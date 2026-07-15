# 深入容器底层：Linux 技术全景与螺旋实现

> **承接 02_CONTAINER_TUTORIAL.md 的第 9 轮。**  
> IPC 骨架、状态管理、持久化已经完成。  
> 从这里开始，Daemon 不再只是往 HashMap 里插记录——  
> 它要真正调用 Linux 系统调用，创建一个被隔离的进程。

---

## 目录

- [容器到底是什么？](#容器到底是什么)
- [Linux 技术全景图](#linux-技术全景图)
- [第 10 轮：fork + exec——进程创建的本质](#第-10-轮fork--exec进程创建的本质)
- [第 11 轮：PID Namespace——让进程看不到外面的世界](#第-11-轮pid-namespace让进程看不到外面的世界)
- [第 12 轮：Mount Namespace + chroot——让进程看不到外面的文件](#第-12-轮mount-namespace--chroot让进程看不到外面的文件)
- [第 13 轮：cgroup v2——内存限制不能只靠信任](#第-13-轮cgroup-v2内存限制不能只靠信任)
- [第 14 轮：OverlayFS——写时复制的文件系统](#第-14-轮overlayfs写时复制的文件系统)
- [完整架构图（第 10-14 轮结束后）](#完整架构图第-10-14-轮结束后)
- [后续预告：Network Namespace（第 18-19 轮）](#后续预告network-namespace第-18-19-轮)
- [知识点总览（第 1-14 轮）](#知识点总览第-1-14-轮)

---

## 容器到底是什么？

一句话：**容器是一个被精心限制了视野和资源的普通进程。**

它不是虚拟机——没有模拟硬件，没有独立内核，内核还是宿主机的同一个。  
它靠三类 Linux 机制实现隔离：

```
┌─────────────────────────────────────────────┐
│  容器 = 进程 + 受限视野 + 受限资源 + 独立文件系统  │
│                                             │
│  受限视野   ←  Namespace（命名空间）           │
│  受限资源   ←  cgroup（控制组）               │
│  独立文件系统 ←  chroot / OverlayFS           │
└─────────────────────────────────────────────┘
```

Docker 本质上也是这三样东西的组合，只是加了很多工程化封装。

---

## Linux 技术全景图

在开始写代码之前，先建立整体认知：

```
┌─────────────────────────── Linux 内核提供的容器原语 ──────────────────────────┐
│                                                                              │
│  【隔离：Namespace】              【限制：cgroup v2】                          │
│  ├── PID  ns  进程树隔离           ├── memory.max  内存上限                   │
│  ├── MNT  ns  挂载点隔离           ├── cpu.max     CPU 配额                   │
│  ├── NET  ns  网络栈隔离           ├── io.max      磁盘 IO 限速               │
│  ├── UTS  ns  主机名隔离           └── pids.max    进程数上限                  │
│  ├── IPC  ns  信号量/共享内存隔离                                              │
│  └── USER ns  UID/GID 映射                                                   │
│                                                                              │
│  【文件系统：VFS 层】                                                          │
│  ├── chroot       改变进程的根目录                                             │
│  ├── pivot_root   更彻底地替换根文件系统（chroot 的升级版）                      │
│  └── OverlayFS    联合挂载（lowerdir 只读 + upperdir 可写 → merged 视图）       │
│                                                                              │
│  【进程创建：系统调用】                                                         │
│  ├── fork()       复制当前进程                                                 │
│  ├── clone()      fork 的超集，可以在创建时顺便建 namespace                     │
│  ├── unshare()    让当前进程脱离某个 namespace，加入新建的同类 namespace         │
│  └── execv()      用新程序替换当前进程的代码段                                  │
└──────────────────────────────────────────────────────────────────────────────┘
```

接下来的几轮，每轮引入其中一个概念，并写出能跑的代码。

---

## 第 10 轮：fork + exec——进程创建的本质

**这一轮学什么**：`fork()` 和 `exec()` 是所有进程创建的基础，后面每一轮都建立在这两个调用之上。

### fork() 做了什么？

```
调用 fork() 之前：
  Daemon 进程（PID 100）
  内存：[代码段][数据段][堆][栈]

调用 fork() 之后，瞬间变成两个进程：
  父进程（PID 100）  ←─ fork() 返回 子进程的 PID（如 101）
  子进程（PID 101）  ←─ fork() 返回 0
```

两个进程的内存内容完全相同（写时复制，Copy-on-Write）。  
判断自己是父还是子，只需要看 `fork()` 的返回值：

```c
pid_t pid = fork();
if (pid == 0) {
    // 这里是子进程
} else {
    // 这里是父进程，pid 是子进程的 PID
}
```

### exec() 做了什么？

`exec()` 不创建新进程，它**替换**当前进程的程序：

```
子进程执行 execv("/bin/bash", args)
    ↓
子进程的代码段被 /bin/bash 的代码替换
子进程的数据段被清空重建
子进程的 PID 保持不变，但"内容"已经完全是 /bin/bash 了
```

### fork + exec 的配合

```
fork()  → 创建一个进程副本（继承父进程的文件描述符、权限等）
exec()  → 在副本里加载真正要运行的程序
```

这是 Unix 的核心哲学。Shell 执行 `ls` 时就是这样：先 fork 出子进程，再 exec `ls`。

### 在 Rust 里：为什么要 unsafe？

`fork()` 是 C 标准库函数，Rust 的 `std` 没有直接封装它（因为 fork 和 async/多线程配合有严重的安全问题）。  
我们用 `nix` crate 调用，它提供了类型安全的封装，但底层仍是 unsafe 的系统调用。

**加入依赖**：

```toml
[dependencies]
# ... 已有的依赖 ...
nix = { version = "0.29", features = ["process", "unistd", "mount", "sched"] }
libc = "0.2"
```

**新建 `src/sandbox/mod.rs`**：

```bash
mkdir -p src/sandbox
```

```rust
// src/sandbox/mod.rs
// 这一轮先做最简单的版本：fork 一个子进程，exec 指定的命令

use nix::unistd::{fork, ForkResult, execvp};
use nix::sys::wait::{waitpid, WaitStatus};
use std::ffi::CString;
use crate::error::AppError;

/// 在子进程里运行命令，父进程等待子进程结束，返回退出码
pub fn spawn_process(command: &[String]) -> Result<u32, AppError> {
    // ─────────────────────────────────────────
    // 关键语法：unsafe 块
    // ─────────────────────────────────────────
    // fork() 在多线程环境下极度危险（子进程继承了父进程所有锁，
    // 但其他线程消失了，锁可能永远无法解锁）。
    // 我们的 Daemon 用 tokio，是多线程的，所以 fork 之后
    // 子进程里必须立刻 exec，不能做任何复杂操作。
    // ─────────────────────────────────────────
    match unsafe { fork() }? {
        ForkResult::Parent { child } => {
            // 父进程：等待子进程结束
            match waitpid(child, None)? {
                WaitStatus::Exited(_, code) => Ok(code as u32),
                WaitStatus::Signaled(_, sig, _) => {
                    eprintln!("[Sandbox] 子进程被信号 {} 终止", sig);
                    Ok(128 + sig as u32)
                }
                _ => Ok(1),
            }
        }
        ForkResult::Child => {
            // 子进程：立刻 exec，不做任何其他事
            // CString 把 Rust String 转换成 C 风格的以 \0 结尾的字符串
            let prog = CString::new(command[0].as_str()).unwrap();
            let args: Vec<CString> = command.iter()
                .map(|s| CString::new(s.as_str()).unwrap())
                .collect();

            // execvp：exec + PATH 环境变量搜索
            // 执行成功后，这行以下的代码永远不会运行
            execvp(&prog, &args).unwrap();

            // 如果 exec 失败（比如找不到程序），退出子进程
            std::process::exit(127);
        }
    }
}
```

**在 `src/main.rs` 里声明模块**：

```rust
mod container;
mod error;
mod ipc;
mod sandbox;   // ← 新增
mod storage;
```

**本轮收获**：

- `fork()`：复制进程，父子进程返回值不同
- `exec()`：替换进程程序（不创建新进程）
- `CString`：Rust 字符串 → C 字符串（以 `\0` 结尾）
- `waitpid()`：父进程等待子进程退出，回收资源（避免僵尸进程）
- fork 在多线程中的危险性：fork 后子进程里必须立刻 exec

---

## 第 11 轮：PID Namespace——让进程看不到外面的世界

**这一轮学什么**：`unshare(CLONE_NEWPID)` + double fork，让容器进程成为自己 PID namespace 里的 PID 1。

### 为什么需要 PID Namespace？

不隔离时，容器里的进程能看到宿主机上的所有进程：

```bash
# 容器里运行 ps aux
# 会看到：
# PID 1  systemd
# PID 100  mybox daemon
# PID 2341  nginx
# PID 2342  容器自己的 bash   ← 和宿主机共享 PID 空间
```

有了 PID namespace 后：

```bash
# 容器里运行 ps aux
# 只看到：
# PID 1  bash   ← 容器里的 bash 就是 PID 1
```

### unshare 的时机问题（为什么需要 double fork）

```
进程 A 调用 unshare(CLONE_NEWPID)
    ↓
内核创建了一个新的 PID namespace
    ↓
但进程 A 自己的 PID 还是在旧 namespace 里的 PID！
（Linux 规定：unshare(CLONE_NEWPID) 不影响调用者自己）
    ↓
进程 A fork() 出进程 B
    ↓
进程 B 诞生在新的 PID namespace 里，它的 PID = 1 ✓
```

所以必须 unshare，然后再 fork 一次，才能得到真正的 PID 1。

### 进一步理解 PID 1 的特殊性

在 Linux 里，PID 1 是 init 进程。它有两个特权：
1. **孤儿进程会被领养**：容器里任何进程的父进程退出后，孤儿进程会被 PID 1 领养
2. **SIGKILL 默认行为不同**：PID 1 不会被 `SIGTERM` 自动终止，它需要显式处理信号

容器的 PID 1 必须正确处理信号并清理子进程，否则会产生僵尸进程。这就是为什么很多容器镜像使用 `tini` 或 `dumb-init` 作为 PID 1。

### 代码改造

**改造 `src/sandbox/mod.rs`**（加入 namespace 隔离）：

```rust
// src/sandbox/mod.rs

use nix::sched::{unshare, CloneFlags};
use nix::unistd::{fork, ForkResult, execvp, sethostname};
use nix::sys::wait::{waitpid, WaitStatus};
use std::ffi::CString;
use crate::error::AppError;

pub struct SandboxConfig {
    pub command:      Vec<String>,
    pub memory_limit: String,
    pub hostname:     String,   // 容器的主机名
}

pub fn spawn_container(cfg: SandboxConfig) -> Result<u32, AppError> {
    match unsafe { fork() }? {
        // ─────────────────────────────────────────
        // 父进程：等待子进程 A 结束
        // 子进程 A 会再 fork 出子进程 B（真正的容器进程），
        // 然后 A 负责等待 B，A 结束后父进程（Daemon）才解除等待。
        // ─────────────────────────────────────────
        ForkResult::Parent { child } => {
            match waitpid(child, None)? {
                WaitStatus::Exited(_, code) => Ok(code as u32),
                _ => Ok(1),
            }
        }

        ForkResult::Child => {
            // ★ 子进程 A：在这里设置 namespace，然后再 fork
            setup_namespaces_and_exec(cfg);
        }
    }
}

fn setup_namespaces_and_exec(cfg: SandboxConfig) -> ! {
    // ─────────────────────────────────────────
    // 关键系统调用：unshare
    // ─────────────────────────────────────────
    // CLONE_NEWPID：新的 PID namespace（子进程将成为 PID 1）
    // CLONE_NEWUTS：新的 UTS namespace（可以设置独立主机名）
    // CLONE_NEWNS ：新的 Mount namespace（挂载点不影响宿主机）
    // ─────────────────────────────────────────
    unshare(
        CloneFlags::CLONE_NEWPID
        | CloneFlags::CLONE_NEWUTS
        | CloneFlags::CLONE_NEWNS,
    ).expect("unshare 失败，需要 CAP_SYS_ADMIN 权限");

    // 设置容器主机名（需要在 UTS namespace 里才安全）
    sethostname(&cfg.hostname).expect("sethostname 失败");

    // ★ 第二次 fork：新子进程在新的 PID namespace 里，PID = 1
    match unsafe { fork() }.expect("第二次 fork 失败") {
        ForkResult::Parent { child } => {
            // 子进程 A 等待子进程 B
            waitpid(child, None).ok();
            std::process::exit(0);
        }
        ForkResult::Child => {
            // ★ 子进程 B：这才是真正的容器进程，PID = 1
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

**本轮收获**：

- `unshare(flags)`：当前进程脱离指定类型的 namespace，加入新建的同类 namespace
- `CLONE_NEWPID`：新 PID namespace，下次 fork 出来的子进程 PID = 1
- `CLONE_NEWUTS`：新 UTS namespace，主机名独立
- `CLONE_NEWNS`：新 Mount namespace，挂载操作不影响宿主机
- Double fork 的必要性：unshare 不影响调用者自己，必须再 fork 一次

---

## 第 12 轮：Mount Namespace + chroot——让进程看不到外面的文件

**这一轮学什么**：用 mount namespace 和 chroot 给容器一个独立的文件系统视图。

### 问题：没有文件系统隔离会怎样？

```bash
# 没有文件系统隔离，容器里：
ls /           # 看到宿主机的 /
cat /etc/passwd  # 看到宿主机的用户列表
rm -rf /tmp/*   # 会删掉宿主机的 /tmp
```

### chroot 是什么？

`chroot(path)` 把一个进程的"根目录 `/`"重定向到指定目录：

```
调用 chroot("/container-rootfs") 之后：
  进程看到的 /      → 实际是 /container-rootfs/
  进程看到的 /etc   → 实际是 /container-rootfs/etc/
  进程看到的 /bin   → 实际是 /container-rootfs/bin/
  宿主机的真正 /    → 进程完全访问不到
```

### 你需要一个 rootfs

`chroot` 需要一个目录作为容器的根文件系统（rootfs）。最简单的方式是把 BusyBox 的静态二进制文件解压到一个目录：

```bash
# 准备一个最小 rootfs
mkdir -p /tmp/mybox/rootfs/{bin,etc,proc,dev,tmp}

# 下载 BusyBox 静态版（包含 sh, ls, cat 等几百个命令）
wget -O /tmp/mybox/rootfs/bin/busybox \
  https://busybox.net/downloads/binaries/1.35.0-x86_64-linux-musl/busybox

chmod +x /tmp/mybox/rootfs/bin/busybox

# BusyBox 通过符号链接模拟多个命令
cd /tmp/mybox/rootfs/bin
./busybox --install .
```

### Mount Namespace 的作用

`chroot` 有一个著名的安全漏洞：有足够权限的进程可以"越狱"（通过 chroot + chdir 技巧）。`mount namespace` 配合 `pivot_root` 才是更安全的做法。

但我们第 12 轮先用 chroot（简单），第 13 轮升级到 pivot_root（更安全）。

Mount namespace 的另一个作用：在容器里挂载 `/proc`（给 ps 命令用）不会影响宿主机的 `/proc`。

```
宿主机挂载树：
  /         (rootfs)
  /proc     (procfs)
  /dev      (devtmpfs)

容器 mount namespace（独立副本）：
  /         (chroot 后的 rootfs)
  /proc     (容器自己挂载，ps 只看容器内的进程)
  /dev      (容器自己的 dev)
```

### 代码改造

在 `setup_namespaces_and_exec` 的子进程 B 里，exec 之前加入 rootfs 设置：

```rust
// src/sandbox/mod.rs —— 子进程 B 里执行 exec 之前

use nix::mount::{mount, MsFlags};
use nix::unistd::chroot;
use std::path::Path;

const ROOTFS: &str = "/tmp/mybox/rootfs";

fn setup_rootfs() {
    let rootfs = Path::new(ROOTFS);

    // ─────────────────────────────────────────
    // 挂载 /proc：让 ps/top 等命令能看到进程
    // ─────────────────────────────────────────
    // mount(source, target, fstype, flags, data)
    // None 表示该参数不需要（如 source 对虚拟文件系统没意义）
    // ─────────────────────────────────────────
    mount(
        Some("proc"),
        &rootfs.join("proc"),
        Some("proc"),
        MsFlags::empty(),
        None::<&str>,
    ).expect("挂载 /proc 失败");

    // 挂载 /dev（简化版：bind mount 宿主机的 /dev）
    mount(
        Some("/dev"),
        &rootfs.join("dev"),
        None::<&str>,
        MsFlags::MS_BIND | MsFlags::MS_REC,
        None::<&str>,
    ).expect("挂载 /dev 失败");

    // chroot：把 rootfs 设为根目录
    chroot(rootfs).expect("chroot 失败");

    // chdir("/")：chroot 后必须切换到新的根目录
    // 不切换的话，当前工作目录还在旧的根目录层次里
    std::env::set_current_dir("/").expect("chdir 失败");
}
```

**本轮收获**：

- `chroot(path)`：重定向进程的根目录
- `chdir("/")`：切换到新根目录（chroot 后必须做）
- `mount("proc", target, "proc", ...)`：在 mount namespace 里挂载 /proc
- `MS_BIND | MS_REC`：bind mount（把一个已有目录挂载到另一个位置）
- 为什么需要 mount namespace：让容器的挂载操作不影响宿主机

---

## 第 13 轮：cgroup v2——内存限制不能只靠信任

**这一轮学什么**：通过写 cgroup v2 的虚拟文件系统，限制容器进程的内存使用量。

### cgroup 是什么？

cgroup（Control Group）是 Linux 内核提供的资源限制机制。它通过一个**虚拟文件系统**（挂载在 `/sys/fs/cgroup/`）来管理：

```
/sys/fs/cgroup/           ← cgroup v2 根
├── memory.max            ← 所有进程的内存上限（默认无限）
├── cpu.max               ← CPU 配额
├── mybox/                ← 我们创建的 cgroup（mkdir 即可）
│   ├── memory.max        ← 写入 "268435456" 表示限制 256M
│   ├── cpu.max           ← 写入 "50000 100000" 表示限制 50% CPU
│   ├── cgroup.procs      ← 写入 PID，把进程加入这个 cgroup
│   └── mybox_abc123/     ← 每个容器一个子 cgroup
│       ├── memory.max
│       └── cgroup.procs
```

**操作 cgroup 就是读写普通文件**，不需要特殊 API：

```bash
# 手动体验（需要 root）
mkdir /sys/fs/cgroup/mybox_test
echo "67108864" > /sys/fs/cgroup/mybox_test/memory.max   # 限制 64MB
echo $$ > /sys/fs/cgroup/mybox_test/cgroup.procs         # 把当前 shell 加入
```

### 解析内存限制字符串

用户传入的是 `"256M"`、`"1G"` 这样的字符串，要转换成字节数：

```rust
pub fn parse_memory_limit(s: &str) -> u64 {
    let s = s.trim();
    if s == "unlimited" || s == "max" {
        return u64::MAX;
    }
    let last = s.chars().last().unwrap_or('0');
    if last.is_alphabetic() {
        let num: u64 = s[..s.len()-1].parse().unwrap_or(0);
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

### 代码改造

**新建 `src/sandbox/cgroup.rs`**：

```rust
// src/sandbox/cgroup.rs

use std::fs;
use std::path::{Path, PathBuf};
use crate::error::AppError;

const CGROUP_BASE: &str = "/sys/fs/cgroup/mybox";

pub struct Cgroup {
    pub path: PathBuf,
}

impl Cgroup {
    /// 为容器创建一个专属的 cgroup 目录
    pub fn new(container_id: &str) -> Result<Self, AppError> {
        let path = Path::new(CGROUP_BASE).join(container_id);
        fs::create_dir_all(&path)?;
        Ok(Self { path })
    }

    /// 设置内存上限（字节数）
    pub fn set_memory_limit(&self, bytes: u64) -> Result<(), AppError> {
        let value = if bytes == u64::MAX {
            "max".to_string()
        } else {
            bytes.to_string()
        };
        fs::write(self.path.join("memory.max"), value)?;
        Ok(())
    }

    /// 把指定 PID 加入这个 cgroup
    pub fn add_process(&self, pid: u32) -> Result<(), AppError> {
        fs::write(self.path.join("cgroup.procs"), pid.to_string())?;
        Ok(())
    }

    /// 容器退出后清理 cgroup 目录
    pub fn cleanup(&self) -> Result<(), AppError> {
        if self.path.exists() {
            fs::remove_dir(&self.path)?;
        }
        Ok(())
    }
}

impl Drop for Cgroup {
    fn drop(&mut self) {
        // 结构体析构时尝试自动清理
        self.cleanup().ok();
    }
}
```

### cgroup 的设置时机

cgroup 需要在 **fork 之前**由父进程创建好，fork 之后把子进程的 PID 写入 `cgroup.procs`：

```rust
// 正确时序：
let cgroup = Cgroup::new(&container_id)?;            // 1. 父进程创建 cgroup
cgroup.set_memory_limit(parse_memory_limit("256M"))?;

match unsafe { fork() }? {
    ForkResult::Parent { child } => {
        cgroup.add_process(child.as_raw() as u32)?;  // 2. fork 后加入 PID
        waitpid(child, None)?;
    }
    ForkResult::Child => {
        // exec...
    }
}
```

**本轮收获**：

- cgroup v2 通过虚拟文件系统操作，写文件即配置
- `memory.max` 写字节数，进程超出上限会被 OOM Killer 杀死
- `cgroup.procs` 写 PID，把进程加入 cgroup
- cgroup 目录通过 `mkdir`/`rmdir` 创建和删除
- 设置内存限制后，超出上限的进程会收到 SIGKILL

---

## 第 14 轮：OverlayFS——写时复制的文件系统

**这一轮学什么**：用 OverlayFS 让每个容器都有"自己的文件系统"，却共享同一份只读镜像，互不干扰。

### 问题：直接 chroot 到同一个 rootfs 的缺陷

```bash
# 容器 A 和容器 B 都 chroot 到 /tmp/mybox/rootfs
容器 A：echo "hello" > /etc/test.txt   # 修改了 rootfs！
容器 B：cat /etc/test.txt              # 看到了容器 A 的修改
容器 A 退出后：rootfs 已经被污染
```

### OverlayFS 的思路

OverlayFS 把三个目录叠加成一个统一视图：

```
lowerdir（只读层）：/tmp/mybox/rootfs                 ← 共享的基础镜像，永不修改
upperdir（读写层）：/tmp/mybox/containers/abc/upper   ← 每个容器专属
workdir（工作目录）：/tmp/mybox/containers/abc/work    ← OverlayFS 内部使用
                 ↓ 合并
merged（合并视图）：/tmp/mybox/containers/abc/merged   ← 容器看到的 /
```

**读文件**：先看 upperdir 有没有，没有再去 lowerdir 找  
**写文件**：只写到 upperdir，lowerdir 永远不动  
**删文件**：在 upperdir 创建一个"whiteout"标记文件，遮盖 lowerdir 里的对应文件

```bash
# 手动体验（需要 root）
mkdir -p /tmp/ov/{lower,upper,work,merged}
echo "from lower" > /tmp/ov/lower/test.txt

mount -t overlay overlay \
  -o lowerdir=/tmp/ov/lower,upperdir=/tmp/ov/upper,workdir=/tmp/ov/work \
  /tmp/ov/merged

cat /tmp/ov/merged/test.txt        # "from lower"
echo "modified" > /tmp/ov/merged/test.txt
cat /tmp/ov/lower/test.txt         # 仍然是 "from lower"（未被修改）
cat /tmp/ov/upper/test.txt         # "modified"（修改只在 upper 层）
```

### 代码改造

**新建 `src/sandbox/fs.rs`**：

```rust
// src/sandbox/fs.rs

use std::fs;
use std::path::{Path, PathBuf};
use nix::mount::{mount, umount2, MsFlags, MntFlags};
use crate::error::AppError;

const BASE_ROOTFS: &str = "/tmp/mybox/rootfs";
const CONTAINERS_DIR: &str = "/tmp/mybox/containers";

pub struct ContainerFs {
    pub merged: PathBuf,
    upper: PathBuf,
    work:  PathBuf,
}

impl ContainerFs {
    pub fn setup(container_id: &str) -> Result<Self, AppError> {
        let base   = Path::new(CONTAINERS_DIR).join(container_id);
        let upper  = base.join("upper");
        let work   = base.join("work");
        let merged = base.join("merged");

        fs::create_dir_all(&upper)?;
        fs::create_dir_all(&work)?;
        fs::create_dir_all(&merged)?;
        fs::create_dir_all(merged.join("proc"))?;
        fs::create_dir_all(merged.join("dev"))?;

        // ─────────────────────────────────────────
        // 挂载 OverlayFS
        // mount 的 data 参数是选项字符串
        // ─────────────────────────────────────────
        let opts = format!(
            "lowerdir={},upperdir={},workdir={}",
            BASE_ROOTFS,
            upper.display(),
            work.display(),
        );
        mount(
            Some("overlay"),
            &merged,
            Some("overlay"),
            MsFlags::empty(),
            Some(opts.as_str()),
        )?;

        Ok(Self { merged, upper, work })
    }

    /// 容器退出后卸载 OverlayFS
    pub fn teardown(&self) -> Result<(), AppError> {
        // MNT_DETACH：lazy umount，等进程不再使用时再真正卸载
        umount2(&self.merged, MntFlags::MNT_DETACH)?;
        Ok(())
    }
}
```

**本轮收获**：

- OverlayFS 三层：lowerdir（只读）+ upperdir（可写）+ workdir（内部）→ merged（视图）
- 每个容器独占自己的 upperdir，共享同一份 lowerdir 镜像
- 容器对文件系统的任何修改都只影响 upperdir，不影响其他容器
- `umount2(path, MNT_DETACH)`：lazy umount，等进程不再使用时再真正卸载

---

## 完整架构图（第 10-14 轮结束后）

```
mybox run /bin/bash 256M
         │
         ▼
    main.rs         解析参数
         │
         ▼
    client.rs       发送 RunRequest
         │  Unix Socket
         ▼
    server.rs       handle_one_connection
         │
         ▼
    sandbox/        spawn_container(SandboxConfig)
         │
    ┌────┴──────────────────────────────────────┐
    │   父进程（Daemon）                           │
    │   1. ContainerFs::setup()  ← OverlayFS    │
    │   2. Cgroup::new()                        │
    │   3. cgroup.set_memory_limit(256M)        │
    │   4. fork() #1                            │
    │   5. cgroup.add_process(child_pid)        │
    │   6. waitpid()                            │
    └────┬──────────────────────────────────────┘
         │
    ┌────┴──────────────────────────────────────┐
    │   子进程 A（namespaced parent）              │
    │   1. unshare(NEWPID | NEWUTS | NEWNS)     │
    │   2. sethostname("container-abc123")      │
    │   3. fork() #2                            │
    │   4. waitpid(子进程 B)                    │
    │   5. teardown（umount, cleanup）           │
    └────┬──────────────────────────────────────┘
         │
    ┌────┴──────────────────────────────────────┐
    │   子进程 B（容器进程，PID=1）                  │
    │   1. setup_rootfs()                       │
    │      ├── mount /proc                     │
    │      ├── mount /dev (bind)               │
    │      └── chroot(merged_dir)              │
    │   2. execvp("/bin/bash", [])             │
    └───────────────────────────────────────────┘
```

---

## 后续预告：Network Namespace（第 18-19 轮）

目前容器和宿主机共享网络栈，可以直接访问宿主机的所有端口。  
加入 `CLONE_NEWNET` 之后，容器只有一个孤立的 `lo` 接口。  
要让容器能访问外网，还需要：

1. 创建 `veth pair`（虚拟网线，两端各一个）
2. 一端留在宿主机（命名为 `veth0`）
3. 另一端移入容器的 net namespace（命名为 `eth0`）
4. 给两端配 IP，在宿主机配 NAT（iptables masquerade）

这是 Docker bridge 网络模式的基本原理。

---

## 知识点总览（第 1-14 轮）

| 轮次 | 新增概念 | 解决的问题 |
|------|---------|-----------|
| 第 1-9 轮 | IPC 框架 + 状态管理 | Daemon/CLI 通信骨架 |
| 第 10 轮 | `fork()` + `exec()` | 进程创建的基础原语 |
| 第 11 轮 | `unshare()` + PID/UTS/MNT namespace | 进程视野隔离 |
| 第 12 轮 | `chroot()` + mount `/proc` | 文件系统隔离 |
| 第 13 轮 | cgroup v2 文件接口 | 内存资源限制 |
| 第 14 轮 | OverlayFS | 写时复制，多容器共享镜像 |
| 第 18 轮 | NET namespace + veth pair | 网络隔离 |

---

> **下一步** → 继续阅读 [04_INTEGRATION_TUTORIAL.md](./04_INTEGRATION_TUTORIAL.md)  
> 从第 15 轮开始，把沙盒模块串联进 Daemon，让 `run` 真正启动容器。
