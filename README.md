# capsule-box

[![License](https://img.shields.io/badge/license-MIT-green.svg)](LICENSE)
![Platform](https://img.shields.io/badge/platform-Linux-blue.svg)
![Language](https://img.shields.io/badge/language-Rust-orange.svg)

[English](doc/README_en.md) | 简体中文 | [文档索引](doc/README.md)

`capsule-box` 是一个用 Rust 编写的教学性质 Linux 容器项目，当前命令名为 `cb`。它实现了一个最小容器运行时的核心路径，适合用来学习 Linux namespace、cgroup、OverlayFS、容器网络和 daemon/client 通信。

> 这是一个学习项目，不是生产级容器引擎。

## 项目亮点

| 模块 | 说明 |
| --- | --- |
| 运行模型 | daemon/client 架构 |
| IPC | Unix Socket + JSON 协议 |
| 进程隔离 | PID、UTS、Mount、Network namespace |
| 文件系统 | `chroot` + OverlayFS |
| 资源限制 | cgroup v2 内存限制 |
| 网络 | veth pair、bridge、NAT |
| 终端 | 支持 `-it` 交互式终端 |
| 状态管理 | JSON 元数据持久化 |

## 适合谁

- 正在学习 Rust 系统编程的开发者
- 想理解容器底层原理的人
- 想用小项目实验 Linux 运行时基础能力的人

## 教程指引

想跟着代码一步步实现容器，请从 [doc/tutorial](doc/tutorial/README.md) 开始。教程按学习顺序编号，共 9 篇、约 23 轮，每轮只引入一个新概念：

| 顺序 | 文档 | 你会学到 |
| --- | --- | --- |
| 01 | [IPC](doc/tutorial/01_IPC_TUTORIAL.md) | Unix Socket、JSON 协议、错误处理（第 1-7 轮） |
| 02 | [容器管理](doc/tutorial/02_CONTAINER_TUTORIAL.md) | 内存状态与磁盘持久化（第 8-9 轮） |
| 03 | [沙盒](doc/tutorial/03_SANDBOX_TUTORIAL.md) | namespace、chroot、cgroup、OverlayFS（第 10-14 轮） |
| 04 | [整合](doc/tutorial/04_INTEGRATION_TUTORIAL.md) | 把沙盒接进 daemon，生命周期与错误回传（第 15-17 轮） |
| 05 | [网络隔离](doc/tutorial/05_NETWORK_TUTORIAL.md) | Network namespace 与 veth（第 18-19 轮） |
| 06 | [NAT](doc/tutorial/06_NAT_TUTORIAL.md) | 外网访问、IP 转发与 DNS（第 20 轮） |
| 07 | [Bridge](doc/tutorial/07_BRIDGE_TUTORIAL.md) | 多容器 bridge 与 IP 分配（第 21 轮） |
| 08 | [交互式](doc/tutorial/08_INTERACTIVE_TUTORIAL.md) | PTY 与标准流转发（第 22 轮） |
| 09 | [回收](doc/tutorial/09_REMOVE_TUTORIAL.md) | `remove` 与资源清理（第 23 轮） |

建议按表中顺序阅读；完整目录与轮次说明见 [教程索引](doc/tutorial/README.md)。

## 运行环境

`capsule-box` 依赖 Linux 内核容器能力，需要在 Linux 环境中运行，并且多数操作需要 root 权限。

需要的系统工具与能力：

- Rust toolchain
- cgroup v2
- `ip`
- `iptables`
- `nsenter`
- `wget`
- root 权限或等价 capability

如果你在 Windows 上开发，建议使用 WSL2 或 Linux 虚拟机。

## 快速开始

### 1. 初始化 rootfs

```bash
sudo bash init.sh
```

### 2. 构建

```bash
cargo build
```

### 3. 启动 daemon

```bash
sudo ./target/debug/cb daemon
```

### 4. 运行容器

另开一个终端：

```bash
sudo ./target/debug/cb run ls 128M
```

交互式 shell：

```bash
sudo ./target/debug/cb run -it /bin/sh 256M
```

## 命令

| 命令 | 说明 |
| --- | --- |
| `sudo ./target/debug/cb daemon` | 启动 daemon |
| `sudo ./target/debug/cb run <command...> <memory>` | 运行容器命令 |
| `sudo ./target/debug/cb run -it /bin/sh 256M` | 启动交互式 shell |
| `sudo ./target/debug/cb list` | 查看容器 |
| `sudo ./target/debug/cb stop <container_id>` | 停止容器 |
| `sudo ./target/debug/cb remove <container_id>` | 删除已停止容器 |

内存限制支持普通字节数，也支持 `K`、`M`、`G`、`max`。

```bash
sudo ./target/debug/cb run /bin/sh 128M
sudo ./target/debug/cb run /bin/sh 1G
sudo ./target/debug/cb run /bin/sh max
```

## 架构

```text
client
  |
  | Unix Socket: /run/cb/ipc.sock
  v
daemon
  |
  +-- container manager
  |     - container state
  |     - IP allocation
  |     - lifecycle operations
  |
  +-- sandbox
  |     - namespace
  |     - cgroup v2
  |     - OverlayFS rootfs
  |     - veth/bridge/NAT
  |
  +-- storage
        - JSON metadata
```

运行时路径：

| 路径 | 用途 |
| --- | --- |
| `/run/cb/ipc.sock` | daemon IPC socket |
| `/run/cb/containers` | 容器运行时目录 |
| `/var/lib/cb/rootfs` | busybox 基础 rootfs |
| `/var/lib/cb/containers` | 容器元数据 |
| `/sys/fs/cgroup/cb` | cgroup v2 层级 |
| `cb0` | 宿主机 bridge 设备 |

## 项目结构

```text
src/
  main.rs              CLI 入口
  container/           容器状态与生命周期
  sandbox/
    mod.rs             namespace、fork、exec、TTY
    fs.rs              OverlayFS 文件系统
    cgroup.rs          cgroup v2 内存限制
    network.rs         bridge、veth、NAT、DNS
  ipc/
    protocol.rs        JSON 请求响应协议
    client.rs          CLI 客户端
    server.rs          daemon 与 PTY 转发
  storage/             元数据持久化
  error.rs             统一错误类型
init.sh                busybox rootfs 初始化
```

## 清理环境

请在确认没有重要容器仍在运行后，再执行这些清理命令。

```bash
sudo rm -f /run/cb/ipc.sock
sudo rm -rf /run/cb/containers
sudo rm -rf /var/lib/cb/containers
sudo ip link delete cb0 2>/dev/null || true
```

清理 iptables 规则前请先检查实际规则：

```bash
sudo iptables -t nat -S
sudo iptables -S FORWARD
```

## 当前限制

- 不是生产级容器运行时
- 没有完整 OCI runtime/spec 支持
- 没有镜像拉取、镜像仓库和镜像格式管理
- 没有用户 namespace、seccomp、AppArmor、SELinux 集成
- 没有完整日志、资源统计、端口映射和 volume 管理
- 网络和清理逻辑仍偏实验性质

## License

本项目使用 [MIT License](LICENSE)。
