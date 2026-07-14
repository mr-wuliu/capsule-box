# capsule-box

`capsule-box` 是一个用 Rust 编写的教学性质 Linux 容器项目。只实现了容器服务一些基本的功能。

## 适合谁

- 正在学习Rust
- 想理解容器底层原理。
- 想尝试系统编程。

## 项目结构

```text
src/
  main.rs              CLI 入口，分发 daemon/list/run/stop/remove 命令
  container/           容器状态管理、IP 分配、生命周期操作
  sandbox/
    mod.rs             namespace、fork、exec、TTY、rootfs 组装入口
    fs.rs              OverlayFS 与容器文件系统目录管理
    cgroup.rs          cgroup v2 内存限制
    network.rs         bridge、veth、NAT、DNS 配置
  ipc/
    protocol.rs        请求/响应协议与 JSON 编解码
    client.rs          CLI 客户端逻辑
    server.rs          daemon、请求处理、PTY 转发
  storage/             容器元数据持久化
  error.rs             统一错误类型
init.sh                初始化 busybox rootfs
```

## 运行环境

这个项目依赖 Linux 内核容器能力，需要在 Linux 环境中运行，并且多数操作需要 root 权限。

需要的系统能力和工具：

- Rust toolchain
- cgroup v2
- `ip`
- `iptables`
- `nsenter`
- `wget`
- root 权限或等价 capability

如果你在 Windows 上开发，建议放到 WSL2 或 Linux 虚拟机中运行。

## 快速开始

初始化 rootfs：

```bash
sudo bash init.sh
```

构建项目：

```bash
cargo build
```

启动 daemon：

```bash
sudo ./target/debug/cb daemon
```

另开一个终端运行容器：

```bash
sudo ./target/debug/cb run ls 128M
```

交互式运行：

```bash
sudo ./target/debug/cb run -it /bin/sh 256M
```

查看容器：

```bash
sudo ./target/debug/cb list
```

停止容器：

```bash
sudo ./target/debug/cb stop <container_id>
```

删除已停止容器：

```bash
sudo ./target/debug/cb remove <container_id>
```

内存限制支持普通字节数，也支持 `K`、`M`、`G` 后缀：

```bash
sudo ./target/debug/cb run /bin/sh 128M
sudo ./target/debug/cb run /bin/sh 1G
sudo ./target/debug/cb run /bin/sh max
```

## 清理环境

实验过程中如果需要手动清理，可以参考下面的命令。执行前请确认没有重要容器还在运行。

```bash
sudo rm -f /run/cb/ipc.sock
sudo rm -rf /run/cb/containers
sudo rm -rf /var/lib/cb/containers
sudo ip link delete cb0 2>/dev/null || true
```

iptables 规则需要根据实际环境检查后清理：

```bash
sudo iptables -t nat -S
sudo iptables -S FORWARD
```

## 项目定位

`capsule-box` 的目标是帮助你用较少代码理解容器的核心机制。它刻意保持简单，方便阅读、调试和修改。你可以把它当作一个容器运行时实验室：先跑起来，再逐个替换或增强 namespace、cgroup、rootfs、network、IPC 等模块。
