# 落地：把沙盒串联成真实容器

> **承接 03_SANDBOX_TUTORIAL.md 的第 14 轮。**  
> 三个沙盒模块（`fs.rs` / `cgroup.rs` / `mod.rs`）已经分别写好，  
> 但它们还没有连在一起，`run` 命令还是只往 `HashMap` 里插一条记录。  
> 这一册把所有东西焊接成一条真正能运行的流水线。

---

## 目录

- [第 15 轮：整合——让 run 真正启动容器](#第-15-轮整合让-run-真正启动容器)
- [第 16 轮：生命周期——stop 真正杀进程](#第-16-轮生命周期stop-真正杀进程)
- [第 17 轮：错误回传——fork 后如何传递失败信息](#第-17-轮错误回传fork-后如何传递失败信息)
- [完整验证流程](#完整验证流程)
- [知识点总览（第 1-17 轮）](#知识点总览第-1-17-轮)

---

## 第 15 轮：整合——让 run 真正启动容器

**这一轮学什么**：为什么不能在 `tokio::spawn` 里直接 fork；`spawn_blocking` 的作用；如何把 OverlayFS、cgroup、namespace 三段代码串成一条流水线。

### 问题：tokio + fork = 危险

`run_daemon()` 用 tokio 运行，tokio 是**多线程**运行时。  
在多线程进程里调用 `fork()`，子进程会继承父进程的所有内存，但：

```
父进程里有 N 个线程：
  线程 1（tokio worker）：持有某个 Mutex 的锁
  线程 2（tokio worker）：持有另一个锁
  线程 3（你）：调用 fork()

fork() 之后，子进程里：
  只有"你"这一个线程存活
  线程 1、2 消失了
  但它们持有的那些 Mutex 锁还锁着！
  子进程永远无法解锁 → 任何试图 lock() 的操作都会死锁
```

**规则**：在多线程进程里 fork 之后，子进程只能做两件事：

1. **立刻 exec**（替换进程镜像，锁的问题随之消失）
2. 调用"async-signal-safe"的函数（数量极少）

我们的子进程需要在 exec 之前做 `unshare`、`chroot` 等操作，这些都是 async-signal-safe 的系统调用，没有问题。**绝对不能**在子进程里 lock 任何 Mutex。

### spawn_blocking

`tokio::task::spawn_blocking` 把一个**同步阻塞**任务扔进专用线程池，不阻塞 tokio 的异步调度器：

```rust
// 错误：直接在 async 函数里 fork，阻塞 tokio worker 线程
async fn handle(...) {
    let pid = unsafe { fork() }; // 阻塞！而且在 tokio 多线程里 fork 危险
}

// 正确：在专用线程里 fork
async fn handle(...) {
    tokio::task::spawn_blocking(|| {
        spawn_container(cfg)  // 在独立线程里运行，fork 相对安全
    }).await??;
}
```

`spawn_blocking` 使用的线程池和 tokio worker 线程池**分离**，理论上更安全（但 tokio 仍然是多线程的，这只是工程上的最佳实践，最终安全还是靠"fork 后立刻 exec"）。

### 第一步：给 `cgroup.rs` 补上 `add_process`

`Cgroup` 目前缺少把 PID 加入 cgroup 的方法，先补上：

```rust
// src/sandbox/cgroup.rs —— 在 impl Cgroup 里加一个方法

/// 把指定 PID 加入这个 cgroup（fork 之后调用）
pub fn add_process(&self, pid: u32) -> Result<(), AppError> {
    fs::write(self.path.join("cgroup.procs"), pid.to_string())?;
    Ok(())
}
```

同时补上内存限制字符串的解析函数（放在 `cgroup.rs` 末尾）：

```rust
// src/sandbox/cgroup.rs 末尾

pub fn parse_memory_limit(s: &str) -> u64 {
    let s = s.trim();
    if s == "unlimited" || s == "max" {
        return u64::MAX;
    }
    let last = s.chars().last().unwrap_or('0');
    if last.is_alphabetic() {
        let num: u64 = s[..s.len() - 1].parse().unwrap_or(0);
        match last {
            'K' | 'k' => num * 1024,
            'M' | 'm' => num * 1024 * 1024,
            'G' | 'g' => num * 1024 * 1024 * 1024,
            _ => num,
        }
    } else {
        s.parse().unwrap_or(512 * 1024 * 1024)
    }
}
```



### 第二步：改造 `SandboxConfig` 和 `setup_rootfs`

目前 `setup_rootfs()` 硬编码了 `ROOTFS` 路径，需要改成接收 OverlayFS 的 `merged` 路径：

```rust
// src/sandbox/mod.rs

// 把 setup_rootfs 改成接受路径参数
fn setup_rootfs(merged: &std::path::Path) {
    // 在 OverlayFS merged 目录里挂载 /proc
    mount(
        Some("proc"),
        &merged.join("proc"),
        Some("proc"),
        MsFlags::empty(),
        None::<&str>,
    ).expect("挂载 /proc 失败");

    // bind mount /dev
    mount(
        Some("/dev"),
        &merged.join("dev"),
        None::<&str>,
        MsFlags::MS_BIND | MsFlags::MS_REC,
        None::<&str>,
    ).expect("挂载 /dev 失败");

    // chroot 到 merged（OverlayFS 的合并视图）
    chroot(merged).expect("chroot 失败");
    std::env::set_current_dir("/").expect("chdir 失败");
}
```

`SandboxConfig` 加入 `container_id` 字段：

```rust
pub struct SandboxConfig {
    pub container_id: String,    // ← 新增，用于创建 OverlayFS 和 cgroup
    pub command:      Vec<String>,
    pub memory_limit: String,
    pub hostname:     String,
}
```



### 第三步：改造 `spawn_container`——串联三个模块

```rust
// src/sandbox/mod.rs

use crate::sandbox::cgroup::{Cgroup, parse_memory_limit};
use crate::sandbox::fs::ContainerFs;

pub fn spawn_container(cfg: SandboxConfig) -> Result<u32, AppError> {
    // ════════════════════════════════════════
    // 第一阶段：fork 之前，在父进程里准备资源
    // ════════════════════════════════════════

    // 1. 挂载 OverlayFS，得到容器专属的文件系统视图
    let container_fs = ContainerFs::setup(&cfg.container_id)?;

    // 2. 创建 cgroup，设置内存上限
    let cgroup = Cgroup::new(&cfg.container_id)?;
    let mem_bytes = parse_memory_limit(&cfg.memory_limit);
    cgroup.set_memory_limit(mem_bytes)?;

    // ════════════════════════════════════════
    // 第二阶段：fork #1
    // ════════════════════════════════════════
    let merged = container_fs.merged.clone();

    match unsafe { fork() }? {
        ForkResult::Parent { child } => {
            // ─────────────────────────────────────────
            // 父进程（Daemon）：
            //   把子进程 A 的 PID 加入 cgroup，然后等它退出
            // ─────────────────────────────────────────
            cgroup.add_process(child.as_raw() as u32)?;

            let exit_code = match waitpid(child, None)? {
                WaitStatus::Exited(_, code) => code as u32,
                WaitStatus::Signaled(_, sig, _) => {
                    eprintln!("[Sandbox] 容器被信号 {} 终止", sig);
                    128 + sig as u32
                }
                _ => 1,
            };

            // 子进程全部退出后，卸载 OverlayFS
            if let Err(e) = container_fs.teardown() {
                eprintln!("[Sandbox] OverlayFS 卸载失败: {}", e);
            }

            Ok(exit_code)
        }

        ForkResult::Child => {
            // ─────────────────────────────────────────
            // 子进程 A：
            //   建立 namespace，再 fork 出真正的容器进程（子进程 B）
            // ─────────────────────────────────────────
            setup_namespace_and_exec(cfg, &merged);
        }
    }
}

fn setup_namespace_and_exec(cfg: SandboxConfig, merged: &std::path::Path) -> ! {
    // 创建新的 PID / UTS / Mount namespace
    unshare(
        CloneFlags::CLONE_NEWPID
        | CloneFlags::CLONE_NEWUTS
        | CloneFlags::CLONE_NEWNS,
    ).expect("unshare 失败，需要 root 权限或 CAP_SYS_ADMIN");

    sethostname(&cfg.hostname).expect("sethostname 失败");

    // fork #2：子进程 B 将成为新 PID namespace 里的 PID 1
    match unsafe { fork() }.expect("第二次 fork 失败") {
        ForkResult::Parent { child } => {
            // 子进程 A 等待子进程 B 退出
            waitpid(child, None).ok();
            std::process::exit(0);
        }
        ForkResult::Child => {
            // ★ 子进程 B（PID = 1）：
            //   1. 挂载 /proc、/dev，chroot 到 OverlayFS merged 目录
            //   2. exec 用户指定的命令
            setup_rootfs(merged);

            let prog = CString::new(cfg.command[0].as_str()).unwrap();
            let args: Vec<CString> = cfg.command.iter()
                .map(|s| CString::new(s.as_str()).unwrap())
                .collect();

            execvp(&prog, &args).expect("exec 失败");
            std::process::exit(127);
        }
    }
}
```



### 第四步：改造 `server.rs` 的 RunRequest 处理

把"只插 HashMap"改成"真正启动容器"：

```rust
// src/ipc/server.rs —— RunRequest 分支

Request::RunRequest { command, memory_limit } => {
    let id = generate_id();
    let hostname = format!("mybox-{}", &id[..8]);

    let cfg = crate::sandbox::SandboxConfig {
        container_id: id.clone(),
        command:      command.clone(),
        memory_limit: memory_limit.clone(),
        hostname,
    };

    // ─────────────────────────────────────────
    // 关键语法：spawn_blocking
    // ─────────────────────────────────────────
    // spawn_container 内部会 fork()，是同步阻塞操作。
    // 不能直接在 async 函数里调用（会阻塞 tokio worker）。
    // spawn_blocking 把它扔到专用线程池里执行，
    // await 等待它完成，期间 tokio 可以继续调度其他任务。
    //
    // ?? 是两个 ? 的连用：
    //   外层 ? 处理 spawn_blocking 本身的 JoinError
    //   内层 ? 处理 spawn_container 返回的 AppError
    // ─────────────────────────────────────────
    match tokio::task::spawn_blocking(move || {
        crate::sandbox::spawn_container(cfg)
    }).await {
        Ok(Ok(exit_code)) => {
            // 容器正常退出，更新状态
            manager.insert(ContainerInfo {
                id: id.clone(),
                command,
                state: format!("Exited({})", exit_code),
                memory_limit,
            });
            Response::RunResponse { container_id: id }
        }
        Ok(Err(e)) => {
            Response::ErrorResponse {
                message: format!("容器启动失败: {}", e),
            }
        }
        Err(e) => {
            Response::ErrorResponse {
                message: format!("内部错误: {}", e),
            }
        }
    }
}
```

同时在 `server.rs` 顶部加上 import：

```rust
// src/ipc/server.rs 顶部
use tokio::task;   // 使用 tokio::task::spawn_blocking
```



### 第五步：更新 `AppError`——处理 JoinError

`spawn_blocking` 可能返回 `tokio::task::JoinError`，需要加入 `AppError`：

```rust
// src/error.rs —— 新增一个变体

#[error("任务错误: {0}")]
Join(#[from] tokio::task::JoinError),
```



### 运行验证

```bash
# 准备 BusyBox rootfs（只需要做一次）
mkdir -p /tmp/mybox/rootfs/{bin,etc,proc,dev,tmp}
wget -O /tmp/mybox/rootfs/bin/busybox \
  https://busybox.net/downloads/binaries/1.35.0-x86_64-linux-musl/busybox
chmod +x /tmp/mybox/rootfs/bin/busybox
cd /tmp/mybox/rootfs/bin && ./busybox --install .

# 需要 root 权限（OverlayFS 和 cgroup 需要）
sudo cargo build
sudo ./target/debug/mybox daemon &

# 另一个终端
./target/debug/mybox run /bin/sh 256M
# 容器内：hostname、ps、ls / 都应该是隔离的

./target/debug/mybox list
# 输出：xxx [Exited(0)] /bin/sh
```

**本轮收获**：

- `spawn_blocking`：在专用线程池里运行阻塞代码，不阻塞 tokio
- `??`：连用两个 `?`，同时处理 `JoinError` 和业务错误
- fork 前准备资源（OverlayFS + cgroup）、fork 后立刻加入 cgroup
- 子进程 A 等子进程 B，子进程 A 退出前负责卸载 OverlayFS

---



## 第 16 轮：生命周期——stop 真正杀进程

**这一轮学什么**：跟踪容器进程的 PID；`stop` 命令发 `SIGTERM`，超时后发 `SIGKILL`；用 `SIGCHLD` 信号异步监听容器退出。

---



### 为什么现在的 stop 没有用

目前 `stop` 只是把 `ContainerInfo.state` 改成 `"Stopped"`，**容器进程本身根本没停**。  
要真正停止容器，需要两件事：

1. 知道容器进程的 **PID**
2. 向这个 PID **发信号**（`SIGTERM` / `SIGKILL`）

---



### 为什么还需要改"容器启动"逻辑

第 15 轮的 `spwan_container` 会阻塞 `waitpid`，等容器完全退出才返回。  
这导致一个根本问题：**容器运行期间 Daemon 无法响应任何其他请求**（`list`、`stop` 全部排队等待）。

正确设计应该是：

```
Daemon 收到 RunRequest
    ↓
fork 出容器进程，立刻拿到 PID
    ↓
把 PID 记录到 ContainerInfo，回复 RunResponse
    ↓
容器在后台运行，Daemon 继续接受请求
    ↓
容器退出 → 内核发 SIGCHLD 给 Daemon → Daemon 更新状态
```

---



### 要改哪些文件

本轮需要按顺序改 **4 个文件**：

```
src/container/mod.rs   ← 第一步、第二步
src/sandbox/mod.rs     ← 第三步
src/ipc/server.rs      ← 第四步（改 import + 加 SIGCHLD 任务 + 改 RunRequest + 改 StopRequest）
```

---



### 第一步：`container/mod.rs` 加 `pid` 字段

`ContainerInfo` 已经在第 15 轮加了 `pid: Option<u32>`，如果你还没加，现在补上：

```rust
// src/container/mod.rs

#[derive(Debug, Clone)]
pub struct ContainerInfo {
    pub id:           String,
    pub command:      Vec<String>,
    pub state:        String,
    pub memory_limit: String,
    pub pid:          Option<u32>,   // 容器进程的 PID（None 表示已退出）
}
```

`storage/mod.rs` 的 `ContainerMetadata` 也要加（加 `#[serde(default)]` 是为了兼容旧的没有 `pid` 字段的 JSON 文件）：

```rust
// src/storage/mod.rs

#[derive(Debug, Serialize, Deserialize)]
pub struct ContainerMetadata {
    pub id:           String,
    pub command:      Vec<String>,
    pub state:        String,
    pub memory_limit: String,
    #[serde(default)]
    pub pid:          Option<u32>,
}
```

两个 `From` 实现也要同步更新，加上 `pid` 字段：

```rust
// src/storage/mod.rs

impl From<&ContainerInfo> for ContainerMetadata {
    fn from(c: &ContainerInfo) -> Self {
        ContainerMetadata {
            id:           c.id.clone(),
            command:      c.command.clone(),
            state:        c.state.clone(),
            memory_limit: c.memory_limit.clone(),
            pid:          c.pid,          // ← 新增
        }
    }
}

impl From<ContainerMetadata> for ContainerInfo {
    fn from(c: ContainerMetadata) -> Self {
        ContainerInfo {
            id:           c.id,
            command:      c.command,
            state:        c.state,
            memory_limit: c.memory_limit,
            pid:          c.pid,          // ← 新增
        }
    }
}
```

---



### 第二步：`container/mod.rs` 加两个新方法

在 `impl ContainerManager` 末尾追加：

```rust
// src/container/mod.rs —— impl ContainerManager 末尾追加

/// 容器进程退出时，由 SIGCHLD 任务调用
/// 根据 pid 找到对应容器，把状态改为 Exited 并清除 pid
pub fn on_container_exit(&self, pid: u32, exit_code: i32) {
    let mut map = self.containers.lock().unwrap();
    if let Some(info) = map.values_mut().find(|c| c.pid == Some(pid)) {
        info.state = format!("Exited({})", exit_code);
        info.pid = None;
        println!(
            "[Daemon] 容器 {} 已退出，退出码 {}",
            &info.id[..8.min(info.id.len())],
            exit_code
        );
        if let Err(e) = crate::storage::save(info) {
            eprintln!("[Storage] 更新退出状态失败: {}", e);
        }
    }
}

/// 向容器进程发送信号，用于 stop 命令
/// 返回 None 表示容器不存在，或者 pid 已经是 None（容器已退出）
pub fn kill_container(&self, id: &str, signal: nix::sys::signal::Signal) -> Option<()> {
    let map = self.containers.lock().unwrap();
    let info = map.get(id)?;
    let pid = info.pid?;  // pid 是 None → 返回 None，不发信号

    use nix::sys::signal::kill;
    use nix::unistd::Pid;
    kill(Pid::from_raw(pid as i32), signal).ok()?;
    Some(())
}
```

同时把第 9 轮已有的 `stop()` 补一行——`StopRequest` 在 `kill_container` 失败时会走这条兜底路径，
必须把 **`pid` 一并清掉**，否则 `state` 已是 `Stopped` 但 `pid` 仍残留，后续 `remove` 会误判为仍在运行：

```rust
// src/container/mod.rs —— 更新已有的 stop() 方法

pub fn stop(&self, id: &str) -> Option<String> {
    let mut map = self.containers.lock().unwrap();

    if let Some(info) = map.get_mut(id) {
        info.state = "Stopped".to_string();
        info.pid = None;   // ← 与 on_container_exit 一致：不再运行则 pid 必须清空
        if let Err(e) = storage::save(info) {
            eprintln!("[Storage] 更新容器状态失败 {}", e);
        }
        Some(id.to_string())
    } else {
        None
    }
}
```

> **`pid` 是"是否在运行"的唯一依据**：`list` 里的 `state` 只是展示用字符串；
> 第 23 轮的 `remove` 只看 `pid.is_some()`。凡是把容器标成"已停/已退出"的路径，
> 都必须同步 `pid = None`。

---



### 第三步：`sandbox/mod.rs` 新增 `start_container`

保留原来的 `spwan_container` 不动（后面验证时还用得到），在它**之后**新加一个非阻塞版本：

```rust
// src/sandbox/mod.rs —— 在 spwan_container 之后新增

/// 非阻塞版本：fork 后立刻返回子进程 PID，不等待容器退出
pub fn start_container(cfg: SandboxConfig) -> Result<u32, AppError> {
    let container_fs = ContainerFs::setup(&cfg.container_id)?;
    let cgroup = Cgroup::new(&cfg.container_id)?;
    cgroup.set_memory_limit(parse_memory_limit(&cfg.memory_limit))?;
    let merged = container_fs.merged.clone();

    match unsafe { fork() }? {
        ForkResult::Parent { child } => {
            let child_pid = child.as_raw() as u32;
            cgroup.add_process(child_pid)?;

            // 不调用 waitpid —— 容器退出由 SIGCHLD 任务负责回收
            // 阻止 Drop 自动 cleanup（容器还在运行，OverlayFS 和 cgroup 不能卸载）
            std::mem::forget(cgroup);
            std::mem::forget(container_fs);

            Ok(child_pid)
        }
        ForkResult::Child => {
            setup_namespace_and_exec(cfg, &merged);
        }
    }
}
```

> `std::mem::forget`：让 Rust **不调用** 某个值的 `Drop`。这里用来阻止 `Cgroup` 和 `ContainerFs` 在函数返回时自动卸载——因为容器还在运行，文件系统不能提前拆掉。

---



### 第四步：`server.rs` 改四处

**4-a：修改文件顶部的** `use` **语句**

把现有的 import 里与 `signal`、`wait` 相关的旧内容清理掉，改成：

```rust
// src/ipc/server.rs —— 顶部 use 区域

use crate::container::{ContainerInfo, ContainerManager};
use crate::{
    error::AppError,
    ipc::protocol::{Request, Response, recv_json, send_json},
};
use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
use nix::unistd::Pid;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;
```

注意：`tokio::signal::unix` 的内容放在使用它的地方用 `use` 引入（见下面第 4-b 步），不需要放在文件顶部。

**4-b：在** `run_daemon()` **里加 SIGCHLD 监听任务**

找到 `let manager = ContainerManager::new();` 这一行，在它**之后**、`let (shutdown_tx, ...)` **之前**加：

```rust
// src/ipc/server.rs —— run_daemon() 函数体内，manager 创建之后

// ── SIGCHLD 监听任务 ────────────────────────────────
// 容器进程（子进程）退出时，内核向 Daemon 发送 SIGCHLD 信号。
// tokio 的 signal(SignalKind::child()) 把系统信号包装成异步流。
let manager_for_sigchld = manager.clone();
tokio::spawn(async move {
    use tokio::signal::unix::{SignalKind, signal};
    let mut sigchld = signal(SignalKind::child()).expect("注册 SIGCHLD 失败");

    loop {
        sigchld.recv().await;  // 等到下一个 SIGCHLD

        // 一次 SIGCHLD 可能对应多个子进程同时退出，用内层 loop 全部回收：
        //   Pid::from_raw(-1)    → 等待"任意"子进程
        //   WaitPidFlag::WNOHANG → 非阻塞，没有退出的就立刻返回
        loop {
            match waitpid(Pid::from_raw(-1), Some(WaitPidFlag::WNOHANG)) {
                Ok(WaitStatus::Exited(pid, code)) => {
                    manager_for_sigchld.on_container_exit(pid.as_raw() as u32, code);
                }
                Ok(WaitStatus::Signaled(pid, _, _)) => {
                    // 被信号杀死（如 SIGKILL），退出码记 -1
                    manager_for_sigchld.on_container_exit(pid.as_raw() as u32, -1);
                }
                // WaitStatus::StillAlive → 没有更多退出的子进程，break
                // Err(ECHILD) → 根本没有子进程，break
                _ => break,
            }
        }
    }
});
// ────────────────────────────────────────────────────
```

**为什么两层 loop？**  
外层永远运行，等待每次 SIGCHLD。内层是因为 Linux 可能把多个子进程的退出**合并成一个 SIGCHLD**，必须循环调用 `waitpid` 直到返回 `StillAlive` 为止。

**4-c：修改** `RunRequest` **分支**

把原来用 `spwan_container`（阻塞）的逻辑，替换成用 `start_container`（非阻塞）：

```rust
// src/ipc/server.rs —— handel_one_connection 里，RunRequest 分支

Request::RunRequest { command, memory_limit } => {
    let id = generate_id();
    let hostname = format!("mybox-{}", &id[..8]);

    let cfg = crate::sandbox::SandboxConfig {
        container_id: id.clone(),
        command:      command.clone(),
        memory_limit: memory_limit.clone(),
        hostname,
    };

    // 用 start_container（非阻塞），fork 后立刻返回 PID
    match tokio::task::spawn_blocking(move || crate::sandbox::start_container(cfg)).await {
        Ok(Ok(pid)) => {
            // 容器已成功启动，记录 Running 状态和 PID
            manager.insert(ContainerInfo {
                id: id.clone(),
                command,
                state: "Running".to_string(),
                memory_limit,
                pid: Some(pid),   // ← 记录 PID，stop 命令会用到
            });
            Response::RunResponse { container_id: id }
        }
        Ok(Err(e)) => Response::ErrorResponse {
            message: format!("容器启动失败: {}", e),
        },
        Err(e) => Response::ErrorResponse {
            message: format!("内部错误: {}", e),
        },
    }
}
```

**与之前的区别**：

- 之前：调用 `spwan_container` → 等容器退出 → 记录 `Exited` 状态
- 现在：调用 `start_container` → 立刻拿到 PID → 记录 `Running` 状态 → 容器继续跑

**4-d：修改** `StopRequest` **分支**

把原来只改内存状态的逻辑，换成真正发信号：

```rust
// src/ipc/server.rs —— StopRequest 分支

Request::StopRequest { container_id, .. } => {
    use nix::sys::signal::Signal;

    // kill_container 返回 None 有两种情况：
    //   1. 容器 ID 不存在
    //   2. 容器已经退出（pid 是 None）
    if manager.kill_container(&container_id, Signal::SIGTERM).is_some() {
        // 先发 SIGTERM，给容器 5 秒优雅退出
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;

        // 5 秒后如果还活着，强制 SIGKILL
        manager.kill_container(&container_id, Signal::SIGKILL);

        Response::StopResponse {
            container_id,
            state: "Stopping".to_string(),
        }
    } else {
        // kill 失败：容器不存在，或 pid 已空/进程已消失——走 stop 兜底并清掉残留 pid
        match manager.stop(&container_id) {
            Some(id) => Response::StopResponse {
                container_id: id,
                state: "Stopped".to_string(),
            },
            None => Response::ErrorResponse {
                message: format!("容器不存在: {}", container_id),
            },
        }
    }
}
```

---



### 验证

```bash
# 终端 1：启动 Daemon
sudo cargo run -- daemon

# 终端 2：启动一个容器
cargo run -- run /bin/sh 128M
# 应该立刻返回：Container started: xxxxxxxxxxxx

# 终端 2：此时 Daemon 仍可响应请求（不再阻塞）
cargo run -- list
# 输出：xxxxxxxxxxxx [Running] /bin/sh

# 终端 2：停止容器
cargo run -- stop xxxxxxxxxxxx
# Daemon 日志应出现：[Daemon] 容器 xxxxxxxx 已退出，退出码 -1

# 终端 2：再次 list
cargo run -- list
# 输出：xxxxxxxxxxxx [Exited(-1)] /bin/sh
```

---



### 本轮收获

- `SIGCHLD`：子进程退出时内核向父进程发送此信号，必须调用 `waitpid` 回收，否则子进程变成**僵尸进程**（zombie）
- `waitpid(-1, WNOHANG)`：`-1` = 任意子进程；`WNOHANG` = 非阻塞，没有退出的子进程立刻返回
- 两层 loop：外层等 SIGCHLD，内层循环回收所有同时退出的子进程
- `std::mem::forget`：阻止 `Drop` 析构，用于容器运行期间阻止 OverlayFS / cgroup 提前卸载
- `SIGTERM` → 等待 → `SIGKILL`：标准的优雅停止流程
- **`pid` 表示运行态**：`on_container_exit` 与 `stop()` 都要把 `pid` 置 `None`；`remove` 靠它判断能否删除

---

