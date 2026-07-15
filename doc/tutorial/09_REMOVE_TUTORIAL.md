# 容器回收教程（第 23 轮）：`remove` 与资源清理

> 前置：你已经完成 INTERACTIVE_TUTORIAL（第 22 轮）。容器具备完整隔离、资源限制、网络、
> 交互能力，`run` / `stop` / `list` 都可用。
>
> 本轮补齐生命周期最后一环——`remove`，并把前面遗留的资源泄漏一次性收口。

---

## 为什么需要 `remove`

目前的命令覆盖了容器的"生"与"控制"，唯独缺了"销毁"：

| 命令 | 作用 | Docker 对应 |
| --- | --- | --- |
| `run` | 创建并启动容器 | `docker run` |
| `stop` | 停止容器进程 | `docker stop` |
| `list` | 列出容器 | `docker ps` |
| **`remove`** | **销毁容器、回收其占用的一切** | **`docker rm`** |

没有 `remove` 会有两个后果：

1. **状态表越积越多**：退出的容器一直挂在 `list` 里，无法清除。
2. **资源泄漏**：每跑一个容器就留下一份 OverlayFS 挂载 + 磁盘目录 + cgroup 目录，永不回收。

---

## 关键设计：区分"退出"与"移除"

一个自然的疑问是：容器进程退出时（第 16 轮的 `on_container_exit`）不就该把所有东西都清理掉吗？

**不该。** Docker 的模型给了很好的答案：容器进程退出后，它依然"存在"——`docker ps -a`
还能看到它，它的文件系统、日志都还在，直到你显式 `docker rm`。这样设计的价值在于：

- 退出后仍可查看容器状态、排查为什么退出、读取其 rootfs 里留下的文件；
- 把"何时彻底销毁"的决定权交给用户，而不是进程一死就毁尸灭迹。

于是资源清理被切成**两个阶段**，各管一段：

| 阶段 | 触发 | 清理的资源 | 保留的资源 |
| --- | --- | --- | --- |
| **退出** `on_container_exit` | 容器进程死亡（SIGCHLD） | 运行态资源：收尸、`state=Exited`、**释放 IP** | 文件系统、目录、cgroup、元数据 |
| **移除** `remove` | 用户执行 `mybox remove <id>` | 持久态资源：**卸载 overlay、删目录、删 cgroup、删元数据、从内存移除** | 无（彻底销毁） |

**判断"是否还在运行"只看 `pid`，不看 `state` 字符串。** `list` 展示 `[Running]`、`[Stopped]`、
`[Exited(0)]` 都只是 `state` 字段；`remove` 的准入条件是 `pid == None`。因此第 16 轮里
`on_container_exit`（进程退出）和 `stop()`（kill 失败时的兜底）都必须把 `pid` 清掉——
否则会出现 `list` 显示已停、但 `remove` 仍报"仍在运行"的矛盾。

> 第 20/21 轮里 IP 是在**退出**时回收的——因为 IP 是"运行态"资源，容器不跑了就该立刻还回池子
> 给别人用。而文件系统属于"持久态"，要留到 `remove` 才清。这条分界线是本轮的核心。

在此之前，容器退出后文件系统一直没人回收，正是因为缺了"移除"这个阶段。本轮补上。

---

## 第一步：文件系统清理——按 id 回收

`remove` 时 daemon 手上没有创建容器时的 `ContainerFs` 对象（它在 `start_container` 里被
`forget` 掉了），所以不能用依赖 `&self` 的实例方法。好在容器的 `merged` 等路径完全能由
`container_id` 重建，因此给 `ContainerFs` 加一个**按 id 清理**的关联函数，一步到位地
"卸载挂载 + 删目录"：

```rust
// src/sandbox/fs.rs —— impl ContainerFs 追加
use std::fs;

impl ContainerFs {
    // ... setup 保持不变 ...

    /// 按容器 id 回收文件系统：卸载 overlay，再删掉整个容器目录。
    /// remove 时调用——此时 daemon 已没有原 ContainerFs 对象，用 id 重建路径即可。
    pub fn remove(container_id: &str) -> Result<(), AppError> {
        let base = Path::new(CONTAINERS_DIR).join(container_id);
        let merged = base.join("merged");

        // 1. 卸载 overlay（MNT_DETACH：即使暂时忙也能懒卸载）。
        //    容器可能从没成功挂载或已被卸载，所以忽略"未挂载"类错误。
        let _ = umount2(&merged, MntFlags::MNT_DETACH);

        // 2. 删除 upper/work/merged 整个目录树
        if base.exists() {
            fs::remove_dir_all(&base)?;
        }
        Ok(())
    }
}
```

要点：

- **卸载 + 删目录是一个完整动作**：先 `umount2(merged, MNT_DETACH)` 卸载 overlay，再
  `remove_dir_all` 删掉 `upper/work/merged` 整棵目录树，一次回收干净。
- **卸载失败要容忍**：容器可能启动到一半就失败、根本没挂上 overlay，或已经被卸载。这种情况下
  `umount2` 会报错，但我们仍要继续删目录，所以用 `let _ =` 忽略它，只对"删目录"失败用 `?` 上报。
- `CONTAINERS_DIR`、`umount2`、`MntFlags` 在 `fs.rs` 里已经有了，无需新增 import（`std::fs` 若
  文件顶部已 `use` 则不必重复）。

---

## 第二步：`Cgroup` 也提供按 id 清理

cgroup 目录同理，`Cgroup` 已有实例方法 `cleanup(&self)`，我们加一个按 id 的版本：

```rust
// src/sandbox/cgroup.rs —— impl Cgroup 追加
impl Cgroup {
    // ... 其余不变 ...

    /// 按容器 id 删除其 cgroup 目录（remove 时调用）
    pub fn remove(container_id: &str) -> Result<(), AppError> {
        let path = Path::new(CGROUP_BASE).join(container_id);
        if path.exists() {
            // cgroup v2 目录要求里面没有活动进程才能 rmdir；
            // 容器已退出时其 cgroup 为空，可以正常删除
            fs::remove_dir(&path)?;
        }
        Ok(())
    }
}
```

> 注意 cgroup v2 的约束：只有当该 cgroup 里**没有存活进程**时 `rmdir` 才会成功。因为 `remove`
> 只处理已停止的容器（见第四步），此时 cgroup 已空，删除没有问题。

---

## 第三步：`ContainerManager` 加 `remove` 方法

`remove` 用 `pid.is_some()` 拦截仍在运行的容器——与 `state` 无关。落地前请先在
`src/error.rs` 补上两个变体（下面 `remove` 会用到）：

```rust
// src/error.rs —— enum AppError 里追加
    #[error("容器不存在: {0}")]
    NotFound(String),

    #[error("容器仍在运行，请先 stop: {0}")]
    StillRunning(String),
```

这是把上面两步串起来、并同时清理元数据与内存状态的地方：

```rust
// src/container/mod.rs —— impl ContainerManager 追加
use crate::sandbox::{ContainerFs, Cgroup};   // 若尚未导出，见文末说明

impl ContainerManager {
    // ... 其余不变 ...

    /// 移除一个已停止的容器，回收其全部持久态资源。
    /// 返回 Err 表示容器不存在或仍在运行。
    pub fn remove(&self, id: &str) -> Result<(), AppError> {
        // 1. 校验：容器必须存在，且不在运行中
        {
            let map = self.containers.lock().unwrap();
            match map.get(id) {
                None => return Err(AppError::NotFound(id.to_string())),
                Some(info) if info.pid.is_some() =>
                    return Err(AppError::StillRunning(id.to_string())),
                Some(_) => {}
            }
        } // 尽早释放锁：下面的卸载/删目录是慢 IO，不该攥着锁做

        // 2. 回收持久态资源（顺序无强依赖，但先卸载再删目录更稳妥）
        ContainerFs::remove(id)?;   // 卸载 overlay + 删容器目录
        Cgroup::remove(id)?;        // 删 cgroup 目录
        storage::delete(id)?;       // 删元数据 json

        // 3. 从内存表移除
        self.containers.lock().unwrap().remove(id);

        println!("[Daemon] 容器 {} 已移除", &id[..8.min(id.len())]);
        Ok(())
    }
}
```

设计说明：

- **先校验、早释放锁**：卸载挂载、`remove_dir_all` 都是较慢的文件 IO，不能攥着 `containers`
  这把全局锁去做，否则会阻塞其它请求（`list`、`run` 都要这把锁）。所以校验完就用花括号让
  `MutexGuard` 提前 drop，清理动作在无锁状态下进行，最后再短暂加锁把这条记录删掉。
- **只移除已停止容器**：`info.pid.is_some()` 为真表示进程仍被记录为运行中，直接拒绝。
  `Exited(...)`、`Stopped` 等状态只要 `pid` 已是 `None` 就可以删。想支持"强制移除"可以在
  第四步的协议里加 `force`，先 `kill_container(SIGKILL)` 等它退出、再走 remove（见文末延伸）。
- **IP 不用在这里还**：容器停止时 `on_container_exit` 已经 `free_ip` 过了；`remove` 只管持久态。

---

## 第四步：协议、CLI、daemon 三处接线

### 4-a：协议加 `RemoveRequest`

```rust
// src/ipc/protocol.rs —— enum Request 里追加
    RemoveRequest {
        container_id: String,
    },
```

响应直接复用现有的 `Response`：成功用一个通用的成功响应即可。这里为了少改动，复用
`StopResponse`（带 `state: "Removed"`），也可以自己加一个 `RemoveResponse { container_id }`。
本教程用复用方案：

```rust
// 成功时返回
Response::StopResponse { container_id: id, state: "Removed".to_string() }
```

### 4-b：daemon 处理 `RemoveRequest`

在 `handel_one_connection` 的 `match request` 里加一个分支（和 `StopRequest` 并列）：

```rust
// src/ipc/server.rs —— handel_one_connection 的 match request 内
Request::RemoveRequest { container_id } => {
    match manager.remove(&container_id) {
        Ok(()) => Response::StopResponse {
            container_id,
            state: "Removed".to_string(),
        },
        Err(e) => Response::ErrorResponse { message: e.to_string() },
    }
}
```

### 4-c：client 发起 `remove`

仿照 `run_stop` 写一个 `run_remove`：

```rust
// src/ipc/client.rs —— 追加
pub async fn run_remove(container_id: &str) -> Result<(), AppError> {
    let mut stream = UnixStream::connect(SOCKET_PATH).await?;
    send_json(
        &mut stream,
        &Request::RemoveRequest {
            container_id: container_id.to_string(),
        },
    )
    .await?;

    let response: Response = recv_json(&mut stream).await?;
    match response {
        Response::StopResponse { container_id, state } => {
            println!("container {} {}", container_id, state);
        }
        Response::ErrorResponse { message } => {
            println!("container remove failed: {}", message);
        }
        _ => println!("unexpected response"),
    }
    Ok(())
}
```

### 4-d：`main.rs` 加 `remove` 子命令

```rust
// src/main.rs —— use 里补上 run_remove
use ipc::client::{run_list, run_stop, run_run, run_remove};

// ... main 的 match 里，和 "stop" 并列 ...
Some("remove") => {
    let id = args.get(2).map(|s| s.as_str()).unwrap_or_default();
    run_remove(id).await
}
```

---

## 关于模块导出（可能要补一行 `pub`）

第三步 `ContainerFs::remove` / `Cgroup::remove` 是在 `container/mod.rs` 里调用的，需要这两个
类型对外可见。检查 `src/sandbox/mod.rs` 顶部：`mod fs;` / `mod cgroup;` 若是私有的，改成
`pub` 并重导出，或在 `container/mod.rs` 里用完整路径 `crate::sandbox::fs::ContainerFs`：

```rust
// src/sandbox/mod.rs —— 视情况导出
pub mod fs;
pub mod cgroup;
// 或者只重导出需要的类型：
// pub use fs::ContainerFs;
// pub use cgroup::Cgroup;
```

按你现有代码的可见性选一种即可，目标是让 `container/mod.rs` 能引用到这两个 `::remove`。

---

## 验证

```bash
cargo build
sudo ./target/debug/mybox daemon
```

另一个终端：

```bash
# 1. 跑一个短命令，让它很快退出
./target/debug/mybox run ls / 128M
# 记下返回的容器 id（假设 000012ab...）

# 2. list 里能看到它，状态 Exited
./target/debug/mybox list

# 3. 移除它
./target/debug/mybox remove 000012ab...
# 输出：container 000012ab... Removed

# 4. 再 list，已经消失
./target/debug/mybox list
```

资源确实被回收的旁证：

```bash
# 容器目录已删除（remove 前存在，remove 后消失）
ls /run/mybox/containers/          # 不再有该 id 目录
# overlay 挂载已卸载
mount | grep <id>                  # 无输出
# cgroup 目录已删除
ls /sys/fs/cgroup/mybox/           # 不再有该 id 目录
```

拒绝移除运行中容器：

```bash
./target/debug/mybox run sleep 300 128M   # 启动一个长命令，记下 id
./target/debug/mybox remove <id>          # 报错：容器仍在运行（pid 非空）
./target/debug/mybox stop <id>            # 先停：SIGCHLD 后 pid 清空，或 stop 兜底清 pid
./target/debug/mybox remove <id>          # 现在可以移除
```

`Exited(0)` 的容器天然满足 `pid == None`，可直接 remove，无需先 stop。

---

## 本轮收获

- **`remove` 是 `run` 的对偶**，补齐了容器生命周期的最后一环
- **两阶段清理**：退出（`on_container_exit`）只回收运行态资源（IP），移除（`remove`）才清理
  持久态资源（overlay 挂载、磁盘目录、cgroup、元数据）——对应 Docker 里 `exited` 容器仍存在、
  直到 `docker rm` 的模型
- **文件系统回收**：`umount2` 卸载 overlay + `remove_dir_all` 删目录，合成一个完整的清理动作
- **按 id 重建路径**：即便原资源对象被 `forget`，也能靠 `container_id` 重建路径来清理
- **锁的粒度**：慢 IO（卸载、删目录）不能攥着全局锁做，先校验再放锁，清理完再短暂加锁删记录
- **cgroup v2 约束**：cgroup 目录要空（无存活进程）才能 `rmdir`，所以只移除已停止容器
- **`pid` 是运行态判据**：`remove` 认 `pid` 不认 `state`；`stop` / `on_container_exit` 负责把 `pid` 清干净

---

## 系列完结

到这里，`mybox` 具备了一个容器运行时的核心能力：

| 能力 | 轮次 |
| --- | --- |
| Daemon / CLI 通信（IPC） | 1-7 |
| 容器状态管理与持久化 | 8-9 |
| 进程 / 文件系统 / 主机名隔离（namespace + chroot） | 10-12 |
| 内存资源限制（cgroup v2） | 13 |
| 写时复制文件系统（OverlayFS） | 14 |
| 整合、生命周期、优雅停止（SIGCHLD / 信号） | 16-17 |
| 网络隔离与回环、veth 打通宿主机 | 18-19 |
| NAT 访问外网、DNS | 20 |
| 多容器网桥与 IP 分配 | 21 |
| 交互式容器（PTY） | 22 |
| 容器回收（remove / 资源清理） | 23 |

你已经从零实现了一个"麻雀虽小、五脏俱全"的容器运行时——创建、隔离、限额、联网、交互、回收
一应俱全。后续若想继续深入，可以探索：镜像分发与打包（OCI 格式）、端口映射（DNAT）、
用户命名空间（rootless）、seccomp / capabilities 安全加固、cgroup 的 CPU / IO 限制。
