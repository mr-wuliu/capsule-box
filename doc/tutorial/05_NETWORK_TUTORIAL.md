# 网络隔离教程（第 18-19 轮）

> 前置：你已经完成 INTEGRATION_TUTORIAL（第 15-16 轮），容器现在可以
> `run` / `list` / `stop`，并且具备 PID、UTS、Mount namespace 和 cgroup 隔离。
>
> 这份文档给容器加上**最后一块隔离拼图：网络**。

---

## 前提：当前 sandbox 的输入输出模型

在进入网络主题之前，需要先明确当前 sandbox 的输入输出模型，否则后续的验证步骤容易产生误解。

到目前为止，`start_container` 通过 `execvp` 直接启动容器进程，尚未实现以下两项能力：

1. 为容器分配 PTY（伪终端）；
2. 将容器的标准输入、标准输出、标准错误通过 socket 转发回 client。

因此当前实现具有两个特征：

- **不支持交互**：容器进程的标准输入继承自 daemon，而非执行 `mybox run` 的 client 终端，因此 `mybox run /bin/sh 128M` 无法提供一个可输入的交互式 shell。
- **输出归属于 daemon 端**：容器进程的输出会打印在运行 daemon 的终端，而不是执行 `mybox run` 的终端。

基于以上特征，本教程的所有验证均采用**可自行运行结束的非交互式命令**（如 `ip addr`、`ping -c 2 ...`），并统一在 **daemon 终端**查看输出。

> 交互式支持（PTY 与标准流转发）属于独立的较大主题，将在后续章节单独展开。本章聚焦于网络隔离的实现。

---



## 为什么需要网络隔离

现在你的容器虽然有独立的进程视图、主机名、文件系统，但**网络是和宿主机完全共享的**。

用当前代码跑一个非交互命令看看（注意：输出会出现在 **daemon 终端**）：

```bash
# client 终端执行
./target/debug/mybox run ip addr 128M
```

去 daemon 终端看输出，你会看到宿主机的**所有**网卡：`eth0`、WSL 的虚拟网卡、`docker0`…… 这说明：

- 容器能看到宿主机的全部网络接口
- 容器和宿主机抢同一批端口（容器里监听 8080，宿主机就占用了 8080）
- 容器能直接访问宿主机的本地服务

这不是隔离。真正的容器应该有**自己独立的一套网络栈**——独立的网卡、独立的 IP、独立的端口空间、独立的路由表。

这就是 **Network Namespace** 要解决的问题。

---



## 第 18 轮：Network Namespace——给容器独立的网络栈

**这一轮学什么**：用 `CLONE_NEWNET` 隔离网络；理解"新建的网络命名空间里有什么"；把回环接口 `lo` 启动起来。

---



### 概念：网络命名空间里有什么

当你用 `CLONE_NEWNET` 创建一个新的网络命名空间时，内核会给这个命名空间一套**全新的、空的**网络栈：

```
宿主机 netns                    新建的容器 netns
┌────────────────────┐        ┌────────────────────┐
│ lo      (UP)        │        │ lo      (DOWN)      │  ← 只有一个孤零零的
│ eth0    (UP, 有IP)  │        │                     │     回环接口，而且没启动
│ docker0 (UP)        │        │ （没有任何其他网卡） │
│ ...                 │        │                     │
└────────────────────┘        └────────────────────┘
```

注意两个关键事实：

1. **新 netns 里只有一个** `lo`**（loopback 回环接口），而且默认是 DOWN 状态**——连 `127.0.0.1` 都 ping 不通
2. 新 netns 里**没有任何能连外网的网卡**——它和宿主机网络是完全断开的

所以本轮分两步：

- **第一步**：加 `CLONE_NEWNET`，让容器进入独立 netns（立刻可验证：`ip addr` 只剩 `lo`）
- **第二步**：把 `lo` 启动起来，让容器内的 `127.0.0.1` 能用

至于"让容器能访问外网"，需要 veth 虚拟网线 + NAT，留到第 19 轮。

---



### 第一步：加上 `CLONE_NEWNET`

打开 `src/sandbox/mod.rs`，找到 `setup_namespace_and_exec` 里的 `unshare` 调用：

```rust
// src/sandbox/mod.rs —— 现在的代码
unshare(
    CloneFlags::CLONE_NEWPID
    | CloneFlags::CLONE_NEWUTS
    | CloneFlags::CLONE_NEWNS
).expect("unshare 失败，需要 CAP_SYS_ADMIN 权限");
```

加一个 `CLONE_NEWNET`：

```rust
// src/sandbox/mod.rs —— 改成这样
unshare(
    CloneFlags::CLONE_NEWPID
    | CloneFlags::CLONE_NEWUTS
    | CloneFlags::CLONE_NEWNS
    | CloneFlags::CLONE_NEWNET   // ← 新增：独立网络栈
).expect("unshare 失败，需要 CAP_SYS_ADMIN 权限");
```

**就这一行**。重新编译运行，跑一个非交互命令验证（输出看 daemon 终端）：

```bash
# client 终端
./target/debug/mybox run ip addr 128M
```

去 daemon 终端看输出：

```
# 之前：看到宿主机一堆网卡
# 现在：只剩一个 lo，而且状态是 DOWN
1: lo: <LOOPBACK> mtu 65536 ... state DOWN
```

隔离已经生效了。但这时候 `lo` 还是 DOWN，`127.0.0.1` 不通。

---



### 第二步：启动回环接口 `lo`

很多程序（数据库、Web 服务）会通过 `127.0.0.1` 和自己的子进程通信，所以 `lo` 必须可用。

启动 `lo` 这件事有两个约束，缺一不可：

1. 必须在**容器的网络命名空间内部**执行（否则配置的是宿主机的 `lo`）；
2. 必须在 `chroot` **之前**执行（`chroot` 进入 busybox rootfs 后，就找不到宿主机的 `ip` 命令了）。

但这里隐藏着一个极易踩中的陷阱，必须先讲清楚，否则程序会直接崩溃。

#### 陷阱：`unshare(CLONE_NEWPID)` 之后不能有额外的 fork

`unshare(CLONE_NEWPID)` 有一个反直觉的语义：调用它的进程**自己并不进入**新的 PID 命名空间，而是它之后 fork 出的**第一个子进程**成为新命名空间里的 PID 1。

与此同时，内核有一条硬性规则（见 `man pid_namespaces`）：

> 一个 PID 命名空间的 **PID 1 一旦退出**，该命名空间即作废，之后任何试图在其中创建进程的 `fork()` 都会失败并返回 **ENOMEM**。

而 `setup_loopback()` 内部的 `Command::new("ip")` 会 **fork + exec** 一个子进程。如果把它放在 `unshare(CLONE_NEWPID)` 与"用于创建容器进程的那次 fork"**之间**，事故链如下：

```
unshare(CLONE_NEWPID)
    ↓
setup_loopback() 内部 fork 出的 ip 进程  →  成为 PID 1
    ↓
ip 执行完毕立即退出               →  PID 1 死亡，命名空间作废
    ↓
随后用于创建容器进程的 fork       →  ENOMEM，进程崩溃
```

**结论**：`setup_loopback()` 必须放在创建容器进程的那次 fork **之后**，也就是在 **PID 1 子进程内部**调用，并且仍然要在 `chroot` 之前。这样 `ip` 进程是 PID 1 的子进程（PID 2），它退出不会影响 PID 1。

正确的执行顺序如下：

```
unshare(..., CLONE_NEWPID | CLONE_NEWNET)
    ↓
sethostname(...)
    ↓
第二次 fork
    ↓
子进程（PID=1）：
    setup_loopback()   ← 在这里启动 lo（仍在 chroot 之前）
    setup_rootfs       ← chroot
    execvp
```

新建一个文件 `src/sandbox/network.rs`：

```rust
// src/sandbox/network.rs

use crate::error::AppError;
use std::process::Command;

/// 在当前网络命名空间里启动回环接口 lo
/// 必须在 PID 1 子进程内部、chroot 之前调用
/// （切勿放在 unshare(CLONE_NEWPID) 与创建容器进程的 fork 之间，否则会让 PID 1 提前死亡）
pub fn setup_loopback() -> Result<(), AppError> {
    // 等价于命令行：ip link set lo up
    let status = Command::new("ip")
        .args(["link", "set", "lo", "up"])
        .status()?;   // std::io::Error 会通过 ? 自动转成 AppError::Io

    if !status.success() {
        return Err(AppError::Io(std::io::Error::new(
            std::io::ErrorKind::Other,
            "启动 lo 失败",
        )));
    }
    Ok(())
}
```

> 这里我们直接调用宿主机的 `ip` 命令（来自 `iproute2`），而不是自己写底层的
> netlink / ioctl 代码。原因有二：① 代码清晰，一眼能看懂在做什么；
> ② 这正是很多容器运行时初始化网络的真实做法。底层的 netlink 实现留作进阶。

然后在 `src/sandbox/mod.rs` 里注册这个模块并调用它：

```rust
// src/sandbox/mod.rs —— 文件顶部，模块声明区
mod cgroup;
mod fs;
mod network;   // ← 新增
```

```rust
// src/sandbox/mod.rs —— setup_namespace_and_exec 内部
unshare(
    CloneFlags::CLONE_NEWPID
    | CloneFlags::CLONE_NEWUTS
    | CloneFlags::CLONE_NEWNS
    | CloneFlags::CLONE_NEWNET
).expect("unshare 失败，需要 CAP_SYS_ADMIN 权限");

sethostname(&cfg.hostname).expect("sethostname failure");

// 注意：这里【不要】调用 setup_loopback()，
// 否则它内部 fork 的 ip 进程会成为 PID 1 并立即退出，导致后续 fork 报 ENOMEM。

match unsafe { fork() }.expect("第二次 fork 失败") {
    ForkResult::Parent { child } => {
        waitpid(child, None).ok();
        std::process::exit(0);
    }
    ForkResult::Child => {
        // 真正的容器进程，PID = 1

        // ← 在这里启动回环接口：此时 ip 是 PID 1 的子进程，且尚未 chroot
        network::setup_loopback().expect("启动 lo 失败");

        setup_rootfs(merged);   // chroot 进 rootfs
        // ... execvp ...
    }
}
```

---



### 为什么必须放在第二次 fork 之后

关键在于"谁是 PID 1"。`unshare(CLONE_NEWPID)` 之后，第一个被 fork 出来的进程才是新命名空间的 PID 1。我们希望这个 PID 1 是**容器进程本身**，而不是配置网络时临时拉起的 `ip` 进程。

```
unshare(NET | PID) → 当前进程进入新 netns，但 PID 命名空间尚无 PID 1
        ↓
   第二次 fork
        ↓
   PID = 1（容器进程）
        ↓
   setup_loopback()：fork 出的 ip 是 PID 2，执行完退出，PID 1 不受影响
        ↓
   chroot → execvp，PID 1 变身为目标命令
```

由于网络命名空间在 `unshare(CLONE_NEWNET)` 时就已经创建，PID 1 子进程天然继承了它，因此在 PID 1 内部配置 `lo`，配置的正是容器自己的网络栈。

---



### 验证

记住：**所有容器输出都在 daemon 终端**，每个 `run` 都是一个独立的、跑完即退出的容器。

```bash
# 重新编译（改了 sandbox 模块）
cargo build

# 终端 1：启动 daemon（容器输出会打在这个终端）
sudo ./target/debug/mybox daemon
```

在终端 2（client）依次执行三个非交互命令，每次去终端 1 看输出：

```bash
# 1. 确认网络已隔离 + lo 已启动：只看到 lo，且状态 UP
./target/debug/mybox run ip addr 128M
# 终端 1 输出大致：
#   1: lo: <LOOPBACK,UP,LOWER_UP> ...
#       inet 127.0.0.1/8 scope host lo
# 看不到 eth0 等宿主机网卡 → 隔离成功；lo 是 UP → 第二步成功

# 2. 确认回环可用：ping 通 127.0.0.1
./target/debug/mybox run ping -c 2 127.0.0.1 128M
# 终端 1 输出：64 bytes from 127.0.0.1: seq=0 ...  ← 通了

# 3. 确认外网不可达（符合预期，veth 还没做）
./target/debug/mybox run ping -c 2 8.8.8.8 128M
# 终端 1 输出：超时 / 不可达——因为还没连接宿主机网卡
```

如果三条都符合预期，本轮成功：容器有了**独立且隔离**的网络栈，回环可用，外网暂不可达。

> 解析提醒：`run` 的参数里**最后一个是内存限制**，前面全部是命令。例如
> `run ping -c 2 127.0.0.1 128M` 会被解析成命令 `["ping","-c","2","127.0.0.1"]`、
> 内存 `128M`。

---



### WSL2 注意事项

你在 WSL2 上运行。基本的 network namespace 隔离（本轮内容）在 WSL2 内核上是支持的。但下一轮的 veth + iptables NAT 在某些 WSL2 配置下可能需要额外内核模块。到第 19 轮我会专门说明。

---



### 本轮收获

- `CLONE_NEWNET`：创建独立的网络命名空间，容器拥有自己的网卡、IP、端口、路由表
- 新建的 netns 里**只有一个 DOWN 状态的** `lo`，需要手动启动
- `unshare(CLONE_NEWPID)` 后，**第一个 fork 出的子进程才是 PID 1**；PID 1 一旦退出，命名空间作废，后续 fork 报 `ENOMEM`
- 因此 `setup_loopback()`（内部会 fork `ip`）必须放在创建容器进程的 fork **之后**、`chroot` **之前**，即在 **PID 1 子进程内部**调用
- 推论：`unshare(CLONE_NEWPID)` 与"创建 PID 1 的 fork"之间，**绝不能出现任何额外的 fork**（包括 `Command`、`std::process` 等）
- 调用 `ip` 命令是配置网络最清晰的方式，底层是 netlink

---



## 第 19 轮：veth pair——把容器接到宿主机

**这一轮学什么**：用 veth pair（虚拟网线）把容器的网络命名空间和宿主机连起来；理解"按 PID 引用网络命名空间"；解决一个隐藏的双向同步问题。完成后，容器能 ping 通宿主机，宿主机也能 ping 通容器。

> 本轮只打通"容器 ↔ 宿主机"。让容器访问**外网**（如 `8.8.8.8`）需要额外的 NAT 和路由转发，放在第 20 轮。

---



### 概念：veth pair 是一根虚拟网线

第 18 轮里，容器的 netns 是一座**孤岛**——除了 `lo`，没有任何网卡能和外界通信。

**veth pair** 就是连接这座孤岛和宿主机的"网线"。它总是成对出现，一端进、另一端出，从一端进入的数据包会从另一端原样出来：

```
宿主机 netns                              容器 netns
┌────────────────────────┐              ┌────────────────────────┐
│  eth0（真实网卡）        │              │                        │
│                         │              │                        │
│  v<id> ─────────────────┼──虚拟网线────┼───────────── c<id>      │
│  10.0.0.1/24            │              │            10.0.0.2/24  │
└────────────────────────┘              └────────────────────────┘
```

我们的计划：

1. 在宿主机侧创建一对 veth：`v<id>` 和 `c<id>`
2. 把 `c<id>` 这一端**移入容器的 netns**
3. 给宿主机端配 `10.0.0.1/24`，容器端配 `10.0.0.2/24`
4. 容器内加一条默认路由，指向宿主机端 `10.0.0.1`

> 接口名有长度限制（最多 15 字符），所以我们用容器 ID 的前 8 位拼成 `v<8位>` /
> `c<8位>`，既唯一又不超长。

---



### 怎么"按 PID 引用"容器的网络命名空间

veth 必须在**宿主机侧**创建（容器自己没有权限、也看不到宿主机网卡）。问题来了：宿主机怎么把 `c<id>` 这一端"放进"容器的 netns？

`ip` 命令支持按 **PID** 引用命名空间：

```bash
ip link set c<id> netns <PID>
```

它的原理是读取 `/proc/<PID>/ns/net`——也就是"和这个进程同属一个网络命名空间"。

那用哪个 PID？回顾 `start_container` 返回的 `child_pid`：它正是那个执行了 `unshare(CLONE_NEWNET)` 的中间进程。这个进程：

- 在**宿主机的 PID 空间**里可见（所以 `/proc/<child_pid>` 存在）
- 处于**容器的网络命名空间**内（它亲自 unshare 出来的）
- 在整个容器生命周期内存活（它在 `waitpid` 等容器进程）

所以 `child_pid` 正是我们要的 netns 引用——这也正好用上了第 16 轮存进 `ContainerInfo.pid` 的那个 PID。

---



### 隐藏的难题：两个时序必须对上

把网络配置加进来后，会冒出一个 18 轮没有的新问题：**时序**。

宿主机侧配置 veth，依赖"容器的 netns 已经存在"；而容器进程一旦 `execvp`，就立刻开始跑用户命令（比如 `ping`）。这中间有两个必须卡~~住的时间点：~~

```
时序点 ①：宿主机要配 veth，必须等容器进程【已经 unshare(NET)】
          —— 否则 /proc/<pid>/ns/net 还是宿主机的 netns，veth 放错地方

时序点 ②：容器要 exec 用户命令，必须等宿主机【已经配好 veth】
          —— 否则 ping 启动时网络还没就绪，直接失败
```

这是一个典型的**双向同步**：父进程要等子进程到达某一步，子进程也要等父进程到达某一步。用**两根管道**解决：

- `ns_ready`：子进程 unshare 完，写一个字节通知父进程
- `net_ready`：父进程配好 veth，写一个字节通知子进程

```
父进程（daemon）                     子进程（容器侧）
   │                                    │
   │                          unshare(NET | PID | ...)
   │   ◄────── ns_ready ──────  写：命名空间好了
   │                                    │（阻塞等 net_ready）
 配置 veth（用 child_pid）              │
   │  ─────── net_ready ──────►  读到：网络好了
   │                              chroot → execvp
```

---



### 第一步：在 `network.rs` 里加 veth 配置函数

```rust
// src/sandbox/network.rs —— 新增

/// 在宿主机侧创建 veth pair，把容器端移入容器 netns，并配好两端地址与路由
/// host_if / cont_if：两端接口名；pid：容器进程在宿主机 PID 空间里的 PID
pub fn setup_veth(host_if: &str, cont_if: &str, pid: u32) -> Result<(), AppError> {
    let pid_s = pid.to_string();

    // 1. 创建 veth pair
    run_ip(&["link", "add", host_if, "type", "veth", "peer", "name", cont_if])?;

    // 2. 把容器端移入容器 netns（按 PID 引用）
    run_ip(&["link", "set", cont_if, "netns", &pid_s])?;

    // 3. 配置宿主机端
    run_ip(&["addr", "add", "10.0.0.1/24", "dev", host_if])?;
    run_ip(&["link", "set", host_if, "up"])?;

    // 4. 进入容器 netns，配置容器端 + 默认路由
    run_nsenter(pid, &["ip", "addr", "add", "10.0.0.2/24", "dev", cont_if])?;
    run_nsenter(pid, &["ip", "link", "set", cont_if, "up"])?;
    run_nsenter(pid, &["ip", "route", "add", "default", "via", "10.0.0.1"])?;

    Ok(())
}

/// 在宿主机当前 netns 执行一条 ip 命令
fn run_ip(args: &[&str]) -> Result<(), AppError> {
    run_cmd("ip", args)
}

/// 借助 nsenter 进入指定 PID 的网络命名空间执行命令
/// 等价于：nsenter -t <pid> -n <args...>
fn run_nsenter(pid: u32, args: &[&str]) -> Result<(), AppError> {
    let mut full = vec!["-t".to_string(), pid.to_string(), "-n".to_string()];
    full.extend(args.iter().map(|s| s.to_string()));
    let refs: Vec<&str> = full.iter().map(|s| s.as_str()).collect();
    run_cmd("nsenter", &refs)
}

fn run_cmd(bin: &str, args: &[&str]) -> Result<(), AppError> {
    let status = Command::new(bin).args(args).status()?;
    if !status.success() {
        return Err(AppError::Io(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("命令失败: {} {:?}", bin, args),
        )));
    }
    Ok(())
}
```

> 容器端 veth 会随容器 netns 一起消失：容器进程退出 → netns 销毁 → `c<id>` 自动删除，
> 它在宿主机的对端 `v<id>` 也会一起消失。所以本轮**不需要手动清理 veth**。

---



### 第二步：在 `start_container` 里加同步管道 + 调用 veth

**关于管道 API 的选择**：`nix` 0.29 里 `pipe()` 返回 `OwnedFd`（Drop 时自动关闭），而 `read`/`close` 收 `RawFd`、`write` 收 `AsFd`——签名不统一，且 `OwnedFd` 的"Drop 自动关闭"在 fork 之后语义微妙，容易误关。这里直接用 `libc`（裸 `i32` 文件描述符），行为最明确。先在 `mod.rs` 里加几个小工具函数：

```rust
// src/sandbox/mod.rs —— 顶部补充导入
use std::os::fd::RawFd;
```

```rust
// src/sandbox/mod.rs —— 管道小工具（基于 libc，行为明确）

/// 创建一根管道，返回 (读端 fd, 写端 fd)
fn make_pipe() -> Result<(RawFd, RawFd), AppError> {
    let mut fds = [0 as RawFd; 2];
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(AppError::Io(std::io::Error::last_os_error()));
    }
    Ok((fds[0], fds[1]))
}

/// 往管道写一个字节，表示"就绪"
fn notify(fd: RawFd) {
    let byte = [1u8];
    unsafe { libc::write(fd, byte.as_ptr() as *const libc::c_void, 1) };
}

/// 阻塞读一个字节，等待对端"就绪"信号
fn wait(fd: RawFd) {
    let mut byte = [0u8; 1];
    unsafe { libc::read(fd, byte.as_mut_ptr() as *mut libc::c_void, 1) };
}

/// 关闭一个 fd
fn close_fd(fd: RawFd) {
    unsafe { libc::close(fd) };
}
```

```rust
// src/sandbox/mod.rs —— 改写 start_container
pub fn start_container(cfg: SandboxConfig) -> Result<u32, AppError> {
    let container_fs = ContainerFs::setup(&cfg.container_id)?;
    let cgroup = Cgroup::new(&cfg.container_id)?;
    cgroup.set_memory_limit(parse_memory_limit(&cfg.memory_limit))?;
    let merged = container_fs.merged.clone();

    // 接口名：v/c + 容器 ID 前 8 位（不超过 15 字符上限）
    let host_if = format!("v{}", &cfg.container_id[..8]);
    let cont_if = format!("c{}", &cfg.container_id[..8]);

    // 两根同步管道
    let (ns_r, ns_w) = make_pipe()?;     // 子 → 父：命名空间就绪
    let (net_r, net_w) = make_pipe()?;   // 父 → 子：网络就绪

    match unsafe { fork() }? {
        ForkResult::Parent { child } => {
            let child_pid = child.as_raw() as u32;

            // 父进程只用 ns_r（读）和 net_w（写），关掉另外两个
            close_fd(ns_w);
            close_fd(net_r);

            cgroup.add_process(child_pid)?;

            // 时序点①：等子进程 unshare 完
            wait(ns_r);
            close_fd(ns_r);

            // 此时容器 netns 已存在，配置 veth
            network::setup_veth(&host_if, &cont_if, child_pid)?;

            // 时序点②：通知子进程网络就绪
            notify(net_w);
            close_fd(net_w);

            std::mem::forget(cgroup);
            std::mem::forget(container_fs);
            Ok(child_pid)
        }
        ForkResult::Child => {
            // 子进程只用 ns_w（写）和 net_r（读）
            close_fd(ns_r);
            close_fd(net_w);
            setup_namespace_and_exec(cfg, &merged, ns_w, net_r);
        }
    }
}
```

---



### 第三步：改 `setup_namespace_and_exec`，加两个同步点

```rust
// src/sandbox/mod.rs —— 给函数加两个 fd 参数
fn setup_namespace_and_exec(
    cfg: SandboxConfig,
    merged: &Path,
    ns_w: RawFd,    // 写端：通知父进程"命名空间就绪"
    net_r: RawFd,   // 读端：等待父进程"网络就绪"
) -> ! {
    unshare(
        CloneFlags::CLONE_NEWPID
        | CloneFlags::CLONE_NEWUTS
        | CloneFlags::CLONE_NEWNS
        | CloneFlags::CLONE_NEWNET
    ).expect("unshare 失败，需要 CAP_SYS_ADMIN 权限");

    // 时序点①：命名空间已创建，通知父进程可以配 veth
    notify(ns_w);
    close_fd(ns_w);

    sethostname(&cfg.hostname).expect("sethostname failure");

    match unsafe { fork() }.expect("第二次 fork 失败") {
        ForkResult::Parent { child } => {
            waitpid(child, None).ok();
            std::process::exit(0);
        }
        ForkResult::Child => {
            // 真正的容器进程，PID = 1
            network::setup_loopback().expect("启动 lo 失败");

            // 时序点②：等父进程把 veth 配好，再继续
            wait(net_r);
            close_fd(net_r);

            setup_rootfs(merged);   // chroot
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

> 注意 `net_r` 的读取放在 `setup_loopback()` 之后、`setup_rootfs`（chroot）之前。
> 因为同步信号要在 chroot 之前收到——chroot 之后文件描述符还在，但语义上我们希望
> "网络就绪 → 进 rootfs → exec"这个顺序清晰。

---



### 验证

```bash
# 确保宿主机装了 iproute2 和 util-linux（提供 nsenter）
which ip nsenter

# 重新编译 + 重启 daemon（务必重启！）
cargo build
sudo ./target/debug/mybox daemon
```

容器 → 宿主机（输出看 daemon 终端）：

```bash
./target/debug/mybox run ping -c 2 10.0.0.1 128M
# daemon 终端：64 bytes from 10.0.0.1: seq=0 ...  ← 容器能 ping 通宿主机
```

确认容器拿到了 IP 和路由：

```bash
./target/debug/mybox run ip addr 128M
# daemon 终端：能看到 c<id>，inet 10.0.0.2/24

./target/debug/mybox run ip route 128M
# daemon 终端：default via 10.0.0.1 ...
```

此时容器还**不能**访问外网（`ping 8.8.8.8` 不通），因为还没做 NAT——这是下一轮的内容。

---



### 本轮收获

- **veth pair**：成对的虚拟网卡，一端在宿主机、一端在容器，是连接两个 netns 的"网线"
- **按 PID 引用 netns**：`ip link set <if> netns <pid>` 通过 `/proc/<pid>/ns/net` 定位命名空间
- `start_container` 返回的 `child_pid` 既在宿主机 PID 空间可见、又在容器 netns 内，是天然的引用点
- `nsenter -t <pid> -n`：在指定进程的网络命名空间里执行命令，用来配置容器端
- **双向同步**：父子进程互相等待对方就绪，用两根单向管道实现（`ns_ready` / `net_ready`）
- 容器端 veth 随 netns 销毁自动清理，无需手动删除

---



## 第 20 轮（预告）：NAT + 路由转发——让容器访问外网

本轮容器只能和宿主机互通（`10.0.0.x` 网段内）。要访问公网，数据包从 `10.0.0.2` 出发，需要宿主机帮它做**地址转换**并**转发**出去：

```
容器 10.0.0.2  ──►  宿主机 v<id> (10.0.0.1)  ──►  内核转发  ──►  iptables NAT  ──►  eth0  ──►  外网
                                                  (ip_forward)   (源地址改成宿主机IP)
```

下一轮要做的三件事：

1. **打开内核转发**：`echo 1 > /proc/sys/net/ipv4/ip_forward`（默认关闭，否则内核不会替别人转包）
2. **配置 NAT**：`iptables -t nat -A POSTROUTING -s 1 0.0.0.0/24 -j MASQUERADE`（把容器源地址伪装成宿主机地址，回包才找得回来）
3. **解决 DNS**：往容器 rootfs 写一个 `/etc/resolv.conf`，否则只能 `ping IP`、不能 `ping 域名`

新难点：这些规则是**全局副作用**（改了宿主机的 iptables 和内核参数），所以第 20 轮还要处理**清理**——容器退出后如何撤销 NAT 规则，避免规则越积越多。

> 第 20 轮的完整内容见单独文档：[06_NAT_TUTORIAL.md](./06_NAT_TUTORIAL.md)。