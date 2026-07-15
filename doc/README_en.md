# capsule-box

[![License](https://img.shields.io/badge/license-MIT-green.svg)](../LICENSE)
![Platform](https://img.shields.io/badge/platform-Linux-blue.svg)
![Language](https://img.shields.io/badge/language-Rust-orange.svg)

English | [简体中文](../README.md) | [Documentation](README.md)

`capsule-box` is an educational Linux container project written in Rust. The current command name is `cb`. It implements the core path of a minimal container runtime and is designed for learning Linux namespaces, cgroups, OverlayFS, container networking, and daemon/client communication.

> This is a learning project, not a production-grade container engine.

## Highlights

| Area | Description |
| --- | --- |
| Runtime model | daemon/client architecture |
| IPC | Unix Socket + JSON protocol |
| Process isolation | PID, UTS, Mount, and Network namespaces |
| Filesystem | `chroot` with OverlayFS |
| Resource control | cgroup v2 memory limit |
| Networking | veth pair, bridge, and NAT |
| TTY | interactive `-it` mode |
| State | JSON metadata persistence |

## Who Is This For

- Developers learning Rust systems programming
- People who want to understand how containers work under the hood
- People who want a small codebase for experimenting with Linux runtime primitives

## Tutorial Guide

To build the container step by step with the code, start at [doc/tutorial](tutorial/README.md). The tutorials are numbered in learning order: 9 documents, about 23 rounds, each round introducing one new idea.

| Order | Document | What you learn |
| --- | --- | --- |
| 01 | [IPC](tutorial/01_IPC_TUTORIAL.md) | Unix Socket, JSON protocol, error handling (rounds 1-7) |
| 02 | [Container management](tutorial/02_CONTAINER_TUTORIAL.md) | In-memory state and disk persistence (rounds 8-9) |
| 03 | [Sandbox](tutorial/03_SANDBOX_TUTORIAL.md) | namespaces, chroot, cgroup, OverlayFS (rounds 10-14) |
| 04 | [Integration](tutorial/04_INTEGRATION_TUTORIAL.md) | Wire the sandbox into the daemon; lifecycle and error reporting (rounds 15-17) |
| 05 | [Network isolation](tutorial/05_NETWORK_TUTORIAL.md) | Network namespace and veth (rounds 18-19) |
| 06 | [NAT](tutorial/06_NAT_TUTORIAL.md) | External access, IP forwarding, and DNS (round 20) |
| 07 | [Bridge](tutorial/07_BRIDGE_TUTORIAL.md) | Multi-container bridge and IP allocation (round 21) |
| 08 | [Interactive](tutorial/08_INTERACTIVE_TUTORIAL.md) | PTY and stdio forwarding (round 22) |
| 09 | [Cleanup](tutorial/09_REMOVE_TUTORIAL.md) | `remove` and resource teardown (round 23) |

Read them in table order. For the full index and round list, see the [tutorial index](tutorial/README.md).

## Requirements

`capsule-box` depends on Linux kernel container features and should be run on Linux with root privileges.

Required tools and capabilities:

- Rust toolchain
- cgroup v2
- `ip`
- `iptables`
- `nsenter`
- `wget`
- root privileges or equivalent capabilities

If you develop on Windows, use WSL2 or a Linux virtual machine.

## Quick Start

### 1. Initialize rootfs

```bash
sudo bash init.sh
```

### 2. Build

```bash
cargo build
```

### 3. Start daemon

```bash
sudo ./target/debug/cb daemon
```

### 4. Run a container

Open another terminal:

```bash
sudo ./target/debug/cb run ls 128M
```

Interactive shell:

```bash
sudo ./target/debug/cb run -it /bin/sh 256M
```

## Commands

| Command | Description |
| --- | --- |
| `sudo ./target/debug/cb daemon` | Start daemon |
| `sudo ./target/debug/cb run <command...> <memory>` | Run a container command |
| `sudo ./target/debug/cb run -it /bin/sh 256M` | Start an interactive shell |
| `sudo ./target/debug/cb list` | List containers |
| `sudo ./target/debug/cb stop <container_id>` | Stop a container |
| `sudo ./target/debug/cb remove <container_id>` | Remove a stopped container |

Memory limits support raw bytes and `K`, `M`, `G`, `max` values.

```bash
sudo ./target/debug/cb run /bin/sh 128M
sudo ./target/debug/cb run /bin/sh 1G
sudo ./target/debug/cb run /bin/sh max
```

## Architecture

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

Runtime paths:

| Path | Usage |
| --- | --- |
| `/run/cb/ipc.sock` | daemon IPC socket |
| `/run/cb/containers` | runtime container directories |
| `/var/lib/cb/rootfs` | base busybox rootfs |
| `/var/lib/cb/containers` | container metadata |
| `/sys/fs/cgroup/cb` | cgroup v2 hierarchy |
| `cb0` | host bridge device |

## Project Structure

```text
src/
  main.rs              CLI entry point
  container/           container state and lifecycle
  sandbox/
    mod.rs             namespace, fork, exec, TTY setup
    fs.rs              OverlayFS rootfs
    cgroup.rs          cgroup v2 memory limit
    network.rs         bridge, veth, NAT, DNS
  ipc/
    protocol.rs        JSON request/response protocol
    client.rs          CLI client
    server.rs          daemon server and PTY forwarding
  storage/             metadata persistence
  error.rs             shared error type
init.sh                busybox rootfs initialization
```

## Cleanup

Use these commands only when you are sure no important containers are still running.

```bash
sudo rm -f /run/cb/ipc.sock
sudo rm -rf /run/cb/containers
sudo rm -rf /var/lib/cb/containers
sudo ip link delete cb0 2>/dev/null || true
```

Check iptables rules before deleting them:

```bash
sudo iptables -t nat -S
sudo iptables -S FORWARD
```

## Limitations

- Not a production-grade container runtime
- No full OCI runtime/spec support
- No image pulling, registry integration, or image format management
- No user namespace, seccomp, AppArmor, or SELinux integration
- No complete logging, resource stats, port mapping, or volume management
- Networking and cleanup logic are still experimental

## License

This project is licensed under the [MIT License](../LICENSE).
