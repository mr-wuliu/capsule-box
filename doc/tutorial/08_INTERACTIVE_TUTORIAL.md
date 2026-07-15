# 交互式容器教程（第 22 轮）：PTY 与标准流转发

> 前置：你已经完成 BRIDGE_TUTORIAL（第 21 轮）。容器具备完整隔离、资源限制、网络能力，
> 但只能跑"非交互式命令"，输出还打在 daemon 终端。
>
> 本轮是整个系列的收尾：实现 `mybox run -it /bin/sh 128M`，给你一个**真正可交互的 shell**。
> 这是所有轮次里改动面最大的一轮，请对照现有代码逐步来。

---

## 为什么"直接把 stdin 接过去"不够

最朴素的想法是：把 client 的标准输入输出通过 socket 转发给容器进程的 stdin/stdout 不就行了？

对普通管道来说够了，但对**交互式 shell** 远远不够。shell、`vim`、`top` 这类程序会检测"我是不是连着一个终端（TTY）"，并依赖终端特性：

- 方向键、Tab 补全、Ctrl-C 中断、Ctrl-Z 挂起
- 行编辑、回显控制、窗口大小
- `isatty()` 返回真，程序才会进入交互模式

普通管道没有这些能力。要让容器里的程序以为自己连着真实终端，就需要 **PTY（伪终端）**。

---

## 概念：PTY（伪终端）是一对"虚拟串口"

PTY 成对出现，一主一从：

```
        master（主端）                     slave（从端 = /dev/pts/N）
   ┌──────────────────┐              ┌──────────────────────────┐
   │ daemon 持有       │              │ 容器进程把它当作           │
   │ 读写这一端        │◄────内核────►│ stdin/stdout/stderr 和    │
   │ = 终端仿真器的角色 │              │ 控制终端（controlling tty）│
   └──────────────────┘              └──────────────────────────┘
```

- 容器进程往 slave 写 → daemon 从 master 读到（这就是"程序的输出"）
- daemon 往 master 写 → 容器进程从 slave 读到（这就是"用户的输入"）
- slave 是一个功能完整的终端设备，`isatty()` 为真，支持所有终端语义

daemon 扮演"终端仿真器"，把 master 的两端接到 client 的 socket 上，就完成了"用户终端 ↔ 容器终端"的桥接。

---

## 整体数据流

```
用户键盘 ─► client stdin ─► socket ─► daemon ─► pty master ─► pty slave ─► 容器程序 stdin
容器程序 stdout ─► pty slave ─► pty master ─► daemon ─► socket ─► client stdout ─► 用户屏幕
```

要动的四块：

1. **协议**：`RunRequest` 增加 `interactive` 标志；交互模式下连接从"一问一答"变成**双向字节流**
2. **daemon**：分配 PTY，让容器用 slave，daemon 在 master 与 socket 之间转发
3. **容器**：把 slave 设为控制终端并接到 0/1/2
4. **client**：把用户终端设为 raw 模式，双向转发

---

## 第零步：加 nix 的 `term` feature

`openpty` 和 termios 相关函数需要 nix 的 `term` feature：

```toml
# Cargo.toml
nix = { version = "0.29", features = ["process", "hostname", "mount", "sched", "signal", "fs", "term"] }
```

---

## 第一步：协议加 `interactive` 标志

```rust
// src/ipc/protocol.rs
RunRequest {
    command: Vec<String>,
    memory_limit: String,
    #[serde(default)]
    interactive: bool,   // ← 新增
},
```

约定：

- `interactive = false`：行为完全同以前（一问一答，输出在 daemon 终端）
- `interactive = true`：client 发完请求后，**同一条连接**变成 daemon ↔ client 的裸字节流，
  不再走 `send_json` / `recv_json`

---

## 第二步：`SandboxConfig` 带上 stdio fd

容器要用哪个 fd 作为标准流，由 daemon 决定：交互模式传入 PTY slave，否则沿用继承。

```rust
// src/sandbox/mod.rs
use std::os::fd::RawFd;

pub struct SandboxConfig {
    pub container_id: String,
    pub command: Vec<String>,
    pub memory_limit: String,
    pub hostname: String,
    pub ip: String,
    pub stdio: Option<RawFd>,   // ← 新增：Some(slave) 表示交互式
}
```

---

## 第三步：容器侧——把 slave 变成控制终端

在 PID 1 子进程里、`execvp` 之前，如果 `stdio` 是 `Some(slave)`，就做终端接管。新增一个函数：

```rust
// src/sandbox/mod.rs

/// 把 slave 伪终端设为本进程的控制终端，并接到 0/1/2
fn setup_tty(slave: RawFd) {
    unsafe {
        // 1. 新建会话，成为会话首进程，脱离原有控制终端
        libc::setsid();

        // 2. 把 slave 设为本会话的控制终端
        libc::ioctl(slave, libc::TIOCSCTTY, 0);

        // 3. 把 slave 接到标准输入/输出/错误
        libc::dup2(slave, 0);
        libc::dup2(slave, 1);
        libc::dup2(slave, 2);

        // 4. 原始 slave fd 若大于 2，已经复制完毕，关掉
        if slave > 2 {
            libc::close(slave);
        }
    }
}
```

在 PID 1 分支里调用它（`setup_rootfs`/`execvp` 之前）：

```rust
// src/sandbox/mod.rs —— setup_namespace_and_exec 的 PID 1 分支
ForkResult::Child => {
    network::setup_loopback().expect("启动 lo 失败");

    wait(net_r);
    close_fd(net_r);

    // ← 新增：交互式则接管终端
    if let Some(slave) = cfg.stdio {
        setup_tty(slave);
    }

    setup_rootfs(merged);
    // ... execvp ...
}
```

> master 那一端要保证在容器 `exec` 后自动关闭，否则 daemon 永远读不到 EOF（见第四步的
> `FD_CLOEXEC`）。

---

## 第四步：daemon 侧——分配 PTY 并转发

### 4-a：分配 PTY

交互式请求进来后，先开一对 PTY：

```rust
// src/ipc/server.rs —— 顶部
use nix::pty::openpty;
use std::os::fd::{AsRawFd, IntoRawFd};

// 分配一对 PTY，返回 (master_fd, slave_fd)
fn alloc_pty() -> Result<(RawFd, RawFd), AppError> {
    let pty = openpty(None, None)?;           // OpenptyResult { master, slave }
    let master = pty.master.into_raw_fd();    // 取出裸 fd，自己管理生命周期
    let slave = pty.slave.into_raw_fd();

    // master 打上 FD_CLOEXEC：容器 exec 时自动关闭它继承来的副本，
    // 这样只剩 daemon 持有 master，容器退出后 master 才能读到 EOF
    unsafe {
        let flags = libc::fcntl(master, libc::F_GETFD);
        libc::fcntl(master, libc::F_SETFD, flags | libc::FD_CLOEXEC);
    }
    Ok((master, slave))
}
```

### 4-b：把 PTY 接入第 21 轮的 `create_container`

第 21 轮已经把 `RunRequest` 的主流程抽成了 `create_container(...) -> Result<String, AppError>`，
用 `?` 平铺、单一回滚边界。交互式不另起炉灶，而是**在这个函数上扩展**，让它多做三件事：

1. 入参增加 `interactive: bool`；
2. 交互式时分配 PTY，`slave` 通过 `SandboxConfig.stdio` 交给容器；
3. 返回值多带一个 `Option<RawFd>`——成功且交互式时把 **master** 交回给调用方去转发。

还有两处 fd 生命周期必须处理好，否则会 fd 泄漏或读不到 EOF：

- **成功后关掉 daemon 自己的 slave 副本**：容器是 `start_container` 里 `fork` 出来的子进程，
  已经继承了一份 slave；daemon 手里的这份必须关掉，否则 slave 还有持有者，master 永远等不到 EOF。
- **失败时把 master + slave 一起关掉**：容器没起来，两个 fd 都还在 daemon 手里，要一并回收。

在第 21 轮版本上扩展后的 `create_container`：

```rust
// src/ipc/server.rs —— 在第 21 轮 create_container 基础上扩展交互式
async fn create_container(
    manager: &ContainerManager,
    command: Vec<String>,
    memory_limit: String,
    interactive: bool,                       // ← 新增入参
) -> Result<(String, Option<RawFd>), AppError> {   // ← 返回值多带 master fd
    let id = generate_id();
    let hostname = format!("mybox-{}", &id[..8]);

    let ip = manager.allocate_ip().ok_or(AppError::IpExhausted)?;

    // 交互式才分配 PTY：slave 交给容器，master 留给 daemon
    let pty = if interactive { Some(alloc_pty()?) } else { None };
    let slave_fd = pty.map(|(_, slave)| slave);

    let result: Result<u32, AppError> = async {
        let cfg = crate::sandbox::SandboxConfig {
            container_id: id.clone(),
            command: command.clone(),
            memory_limit: memory_limit.clone(),
            hostname,
            ip: ip.clone(),
            stdio: slave_fd,                 // ← 交互式把 slave 作为容器的 0/1/2
        };
        let pid =
            tokio::task::spawn_blocking(move || crate::sandbox::start_container(cfg)).await??;
        Ok(pid)
    }
    .await;

    match result {
        Ok(pid) => {
            manager.insert(ContainerInfo {
                id: id.clone(),
                command,
                state: "Running".to_string(),
                memory_limit,
                pid: Some(pid),
                ip: Some(ip),
            });
            // 容器已持有 slave；关掉 daemon 自己的 slave 副本，master 才能在容器退出时读到 EOF
            let master = match pty {
                Some((master, slave)) => {
                    unsafe { libc::close(slave); }
                    Some(master)
                }
                None => None,
            };
            Ok((id, master))
        }
        Err(e) => {
            manager.free_ip(&ip);
            // 启动失败：master/slave 都还在 daemon 手里，一并关闭避免 fd 泄漏
            if let Some((master, slave)) = pty {
                unsafe { libc::close(master); libc::close(slave); }
            }
            Err(e)
        }
    }
}
```

`RunRequest` 分支据 master 是否存在，走"转发"或"普通响应"两条路：

```rust
// src/ipc/server.rs —— handel_one_connection 里的 RunRequest 分支
Request::RunRequest { command, memory_limit, interactive } => {
    match create_container(&manager, command, memory_limit, interactive).await {
        // 交互式：拿到 master，进入转发循环，这条连接的生命周期到此为止
        Ok((_id, Some(master))) => {
            forward_pty(stream, master).await?;
            return Ok(());
        }
        // 非交互式：照旧返回 RunResponse，走末尾统一的 send_json
        Ok((id, None)) => Response::RunResponse { container_id: id },
        Err(e) => Response::ErrorResponse { message: e.to_string() },
    }
}
```

> 这里在交互式分支 `forward_pty(stream, master).await?` 之后紧跟 `return Ok(())`：`stream`
> 被移动进 `forward_pty`，而这条分支会提前返回、不会走到末尾的 `send_json(&mut stream, ...)`，
> 所以借用检查通过（发散分支里移动 `stream` 是合法的）。非交互式分支不碰 `stream`，末尾照常发响应。

### 4-c：转发循环（master ↔ socket）

PTY master 是一个普通阻塞 fd。最简单可靠的做法是把这条连接交给**两个阻塞线程**去拷贝，整段放进 `spawn_blocking`：

```rust
// src/ipc/server.rs
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::FromRawFd;

/// 在 PTY master 和 client 连接之间双向转发，直到任意一端关闭
async fn forward_pty(stream: UnixStream, master: RawFd) -> Result<(), AppError> {
    // tokio 连接转成阻塞的 std 连接
    let std_stream = stream.into_std()?;
    std_stream.set_nonblocking(false)?;

    tokio::task::spawn_blocking(move || {
        // master 包装成 File，方便按 Read/Write 使用
        let mut master_r = unsafe { File::from_raw_fd(master) };
        let mut master_w = master_r.try_clone().expect("clone master");
        let mut sock_r = std_stream.try_clone().expect("clone sock");
        let mut sock_w = std_stream;

        // 线程 A：client → 容器（socket 读，master 写）
        let a = std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match sock_r.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => { if master_w.write_all(&buf[..n]).is_err() { break; } }
                }
            }
        });

        // 线程 B：容器 → client（master 读，socket 写）
        // 容器退出后 master 读到 EOF，本线程结束 → 整个会话结束
        let mut buf = [0u8; 4096];
        loop {
            match master_r.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => { if sock_w.write_all(&buf[..n]).is_err() { break; } }
            }
        }

        // 输出方向结束：关闭连接写端，促使线程 A 退出
        let _ = sock_w.shutdown(std::net::Shutdown::Both);
        let _ = a.join();
    })
    .await?;

    Ok(())
}
```

> 为什么线程 B（master→socket）作为"主判断线"：容器进程退出后，slave 的所有副本关闭，
> master 读到 EOF，线程 B 结束——这就是"会话结束"的准确信号。随后关闭 socket，
> 阻塞在 `stdin` 读取上的线程 A 也会因写入失败而退出。

---

## 第五步：client 侧——raw 模式 + 双向转发

### 5-a：把用户终端设为 raw 模式

正常终端是"行缓冲 + 回显 + Ctrl-C 本地处理"。交互式转发要的是**原始字节**：每敲一个键立刻发走、不本地回显（回显交给容器里的程序）、Ctrl-C 作为字节 `0x03` 发给容器。这就是 raw 模式。

用 RAII 保证异常退出也能恢复终端，否则程序崩了终端会"坏掉"：

```rust
// src/ipc/client.rs
use nix::sys::termios::{self, SetArg, Termios};
use std::os::fd::AsFd;

/// 进入 raw 模式，Drop 时自动恢复
struct RawGuard(Termios);

impl RawGuard {
    fn enter() -> Result<Self, AppError> {
        let stdin = std::io::stdin();
        let original = termios::tcgetattr(stdin.as_fd())?;
        let mut raw = original.clone();
        termios::cfmakeraw(&mut raw);
        termios::tcsetattr(stdin.as_fd(), SetArg::TCSANOW, &raw)?;
        Ok(RawGuard(original))
    }
}

impl Drop for RawGuard {
    fn drop(&mut self) {
        let stdin = std::io::stdin();
        let _ = termios::tcsetattr(stdin.as_fd(), SetArg::TCSANOW, &self.0);
    }
}
```

### 5-b：交互式 run

```rust
// src/ipc/client.rs
use std::io::{Read, Write};

pub async fn run_run_interactive(command: Vec<String>, memory_limit: &str)
    -> Result<(), AppError>
{
    let mut stream = UnixStream::connect(SOCKET_PATH).await?;
    send_json(&mut stream, &Request::RunRequest {
        command,
        memory_limit: memory_limit.to_string(),
        interactive: true,
    }).await?;

    // 进入 raw 模式（Drop 时自动恢复）
    let _guard = RawGuard::enter()?;

    // 转成阻塞 std 连接，用两个线程转发
    let std_stream = stream.into_std()?;
    std_stream.set_nonblocking(false)?;
    let mut sock_w = std_stream.try_clone()?;
    let mut sock_r = std_stream;

    // 线程：本地 stdin → socket
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin();
        let mut buf = [0u8; 4096];
        loop {
            match stdin.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => { if sock_w.write_all(&buf[..n]).is_err() { break; } }
            }
        }
    });

    // 主线程：socket → 本地 stdout（服务端关闭即结束）
    let mut stdout = std::io::stdout();
    let mut buf = [0u8; 4096];
    loop {
        match sock_r.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => { let _ = stdout.write_all(&buf[..n]); let _ = stdout.flush(); }
        }
    }
    // 离开作用域时 RawGuard 恢复终端
    Ok(())
}
```

---

## 第六步：命令行加 `-it`

让 `mybox run -it <command...> <memory>` 走交互式：

```rust
// src/main.rs —— run 分支
Some("run") => {
    let rest = &args[2..];
    // 检测开头的 -it（简化：只认这一个组合标志）
    let (interactive, rest) = match rest.split_first() {
        Some((flag, tail)) if flag == "-it" => (true, tail),
        _ => (false, rest),
    };

    match rest.split_last() {
        Some((memory_limit, command_parts)) if !command_parts.is_empty() => {
            let command = command_parts.to_vec();
            if interactive {
                run_run_interactive(command, memory_limit).await
            } else {
                run_run(command, memory_limit).await
            }
        }
        _ => {
            eprintln!("用法: mybox run [-it] <command...> <memory_limit>");
            Ok(())
        }
    }
}
```

---

## 验证

```bash
cargo build
sudo ./target/debug/mybox daemon
```

另一个终端：

```bash
./target/debug/mybox run -it /bin/sh 128M
```

现在你应该进入了容器内的 shell，且：

```sh
hostname          # mybox-xxxxxxxx（独立主机名）
ps                # 只有 sh 和 ps（PID 隔离）
ip addr           # 只有 lo 和 c<id>（网络隔离）
ping -c 2 8.8.8.8 # 通（NAT 生效）
ls /              # busybox rootfs（文件系统隔离）
```

交互特性验证：

- 敲命令有回显、能用退格、方向键
- `Ctrl-C` 能中断前台命令（raw 模式把 `0x03` 发给容器，容器 pty 的 `ISIG` 生成 SIGINT）
- 输入 `exit` 退出 shell → 容器进程结束 → master EOF → 连接关闭 → 终端自动恢复正常

---

## 已知局限与延伸（不在本轮实现）

- **窗口大小**：没有同步终端行列数，`vim`、`top` 的画面可能按默认 80x24 排版。完善做法：
  client 用 `TIOCGWINSZ` 读本地窗口大小发给 daemon，daemon 用 `TIOCSWINSZ` 设到 master，
  并监听 `SIGWINCH` 在窗口变化时重设。
- **`stop` 与交互**：交互式容器由用户 `exit` 结束；`stop` 命令仍可用（第 16 轮的 SIGTERM 路径）。
- **多路复用**：当前一条连接对应一个交互会话，足够学习使用。

---

## 本轮收获

- **PTY**：一对主/从虚拟终端，slave 是功能完整的终端设备，让容器内程序 `isatty()` 为真
- **控制终端**：`setsid` + `ioctl(TIOCSCTTY)` + `dup2` 三件套，把 slave 变成容器进程的控制终端
- **`FD_CLOEXEC`**：给 master 打标记，容器 `exec` 时自动关闭继承副本，daemon 才能靠 EOF 感知退出
- **流式协议**：交互连接从"一问一答"切换为双向裸字节流
- **raw 模式**：`cfmakeraw` + `tcsetattr`，用 RAII 保证终端一定被恢复
- **阻塞线程转发**：PTY 是阻塞 fd，用 `spawn_blocking` + 两个线程做双向拷贝，简单可靠

---

## 第 23 轮（预告）：容器回收——`remove` 与资源清理

到这里，`run` / `stop` / `list` 齐了，但还缺"销毁"这一环——`run` 的对偶 `remove`。
更重要的是，前面留了两处**资源泄漏**没人管：

- 容器退出后，OverlayFS 的 `merged` 挂载和 `upper/work/merged` 目录一直残留（`ContainerFs::teardown`
  至今是死代码）；
- cgroup 目录 `/sys/fs/cgroup/mybox/<id>` 也没被删。

第 23 轮补上 `remove` 命令，把 `teardown` 用起来，并厘清"退出（保留文件系统）"与
"移除（彻底清理）"的职责边界——这正是第 20/21 轮"资源生命周期"主线的收尾。

完整内容见单独文档：[09_REMOVE_TUTORIAL.md](./09_REMOVE_TUTORIAL.md)。
