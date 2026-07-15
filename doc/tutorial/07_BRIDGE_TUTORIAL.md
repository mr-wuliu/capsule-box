# 多容器网络教程（第 21 轮）：Linux bridge 与 IP 分配

> 前置：你已经完成 NAT_TUTORIAL（第 20 轮），单个容器可以访问外网。
>
> 本轮解决"多容器共存"：让多个容器**同时**拥有各自的 IP、互相能通、都能上网。

---

## 现状的硬伤：IP 被写死了

第 19-20 轮里，两端地址是硬编码的：

```rust
run_ip(&["addr", "add", "10.0.0.1/24", "dev", host_if])?;              // 宿主机端
run_nsenter(pid, &["ip", "addr", "add", "10.0.0.2/24", "dev", cont_if])?; // 容器端
```

同时启动**两个**容器时，问题立刻暴露：

- 两个容器的宿主机端 veth 都想配 `10.0.0.1/24` → **地址冲突**
- 就算不冲突，两条点对点 veth 之间也不通 → **容器之间无法互相访问**

根因是我们用的是**点对点（point-to-point）**模型：每个容器和宿主机之间拉一根独立的线，彼此孤立。

---

## 新模型：Linux bridge（虚拟交换机）

Docker 的默认网络用的是**网桥**模型。把 bridge 想象成一台虚拟交换机：

```
                    ┌───────────────── 宿主机 ─────────────────┐
                    │                                          │
                    │   eth0（真实网卡，上网）                   │
                    │      │                                    │
                    │   [ NAT / ip_forward ]                    │
                    │      │                                    │
                    │   mybox0  (网桥, 10.0.0.1/24, 充当网关)    │
                    │    │      │      │                        │
                    │  v<A>   v<B>   v<C>     ← 各容器的宿主机端  │
                    └────┼──────┼──────┼───────────────────────┘
                         │      │      │
                       c<A>   c<B>   c<C>    ← 各容器 netns 内的网卡
                    10.0.0.2 10.0.0.3 10.0.0.4
```

要点：

- 宿主机上建**一个**网桥 `mybox0`，给它配网关地址 `10.0.0.1/24`
- 每个容器分配一个**唯一** IP：`10.0.0.2`、`10.0.0.3`……
- 每个容器的宿主机端 veth **接到网桥上**（而不是直接配 IP）
- 所有容器都挂在同一个网桥、同一个网段，于是**容器之间天然互通**，网关统一是 `10.0.0.1`

第 20 轮的 NAT 规则（针对 `10.0.0.0/24` 整个子网）**完全不用改**，继续复用。

---

## 第一步：`ContainerInfo` 与持久化加 `ip` 字段

先把数据结构改好——第二步的分配与回收逻辑都要读 `info.ip`，所以字段必须**先**就位，否则那一步会编译不过（`no field ip on ContainerInfo`）。

```rust
// src/container/mod.rs
#[derive(Debug, Clone)]
pub struct ContainerInfo {
    pub id: String,
    pub command: Vec<String>,
    pub state: String,
    pub memory_limit: String,
    pub pid: Option<u32>,
    pub ip: Option<String>,   // ← 新增
}
```

同步 `storage/mod.rs` 的 `ContainerMetadata` 和两个 `From` 实现：

```rust
// src/storage/mod.rs
#[derive(Debug, Serialize, Deserialize)]
pub struct ContainerMetadata {
    pub id: String,
    pub command: Vec<String>,
    pub state: String,
    pub memory_limit: String,
    #[serde(default)]
    pub pid: Option<u32>,
    #[serde(default)]
    pub ip: Option<String>,   // ← 新增
}
```

两个 `From` 各加一行 `ip: c.ip.clone()`（`&ContainerInfo → Metadata`）与 `ip: c.ip`（`Metadata → ContainerInfo`）。

---

## 第二步：给容器分配唯一 IP

IP 分配是一份**需要跨容器共享、且要能回收**的状态，放进 `ContainerManager` 最合适。第一步已经给 `ContainerInfo` 补上了 `ip` 字段，下面的逻辑就能直接读写它。

### 2-a：`ContainerManager` 加一个 IP 池

```rust
// src/container/mod.rs —— 顶部
use std::collections::{HashMap, HashSet};

#[derive(Clone)]
pub struct ContainerManager {
    containers: Arc<Mutex<HashMap<String, ContainerInfo>>>,
    ip_pool: Arc<Mutex<HashSet<u8>>>,   // ← 新增：已占用的主机号（2..=254）
}
```

`new()` 里初始化，并把恢复出来的、仍在运行的容器占用的 IP 标记为"已用"：

```rust
// src/container/mod.rs —— new() 内部
pub fn new() -> Self {
    let manager = Self {
        containers: Arc::new(Mutex::new(HashMap::new())),
        ip_pool: Arc::new(Mutex::new(HashSet::new())),   // ← 新增
    };
    match storage::load_all() {
        Ok(list) => {
            let mut map = manager.containers.lock().unwrap();
            let mut pool = manager.ip_pool.lock().unwrap();
            for info in list {
                println!("[Storage] 恢复容器: {} [{}]", info.id, info.state);
                // 恢复正在运行的容器时，重新占用它的 IP，避免重复分配
                if let Some(n) = info.ip.as_deref().and_then(host_octet) {
                    if info.pid.is_some() {
                        pool.insert(n);
                    }
                }
                map.insert(info.id.clone(), info);
            }
        }
        Err(e) => eprintln!("[Storage] 恢复失败: {}", e),
    }
    manager
}
```

分配与回收：

```rust
// src/container/mod.rs —— impl ContainerManager 追加

/// 分配一个未使用的主机号（2..=254），返回如 "10.0.0.5"
pub fn allocate_ip(&self) -> Option<String> {
    let mut pool = self.ip_pool.lock().unwrap();
    for n in 2..=254u8 {
        if !pool.contains(&n) {
            pool.insert(n);
            return Some(format!("10.0.0.{}", n));
        }
    }
    None // 地址耗尽
}

/// 回收一个 IP
pub fn free_ip(&self, ip: &str) {
    if let Some(n) = host_octet(ip) {
        self.ip_pool.lock().unwrap().remove(&n);
    }
}
```

辅助函数：从 `"10.0.0.5"` 里取最后一段 `5`：

```rust
// src/container/mod.rs —— 文件末尾（模块级函数）
fn host_octet(ip: &str) -> Option<u8> {
    ip.rsplit('.').next()?.parse().ok()
}
```

### 2-b：容器退出时回收 IP

在第 16 轮的 `on_container_exit` 里补一句回收（它正是"专属资源用完即删"的钩子）：

```rust
// src/container/mod.rs —— on_container_exit 内部
pub fn on_container_exit(&self, pid: u32, exit_code: i32) {
    let mut map = self.containers.lock().unwrap();
    if let Some(info) = map.values_mut().find(|c| c.pid == Some(pid)) {
        info.state = format!("Exited({})", exit_code);
        info.pid = None;

        // ← 新增：回收该容器占用的 IP
        if let Some(ip) = info.ip.clone() {
            if let Some(n) = host_octet(&ip) {
                self.ip_pool.lock().unwrap().remove(&n);
            }
        }

        println!("[Deamon] 容器 {} 已退出，退出码 {}", &info.id[..8.min(info.id.len())], exit_code);
        if let Err(e) = crate::storage::save(info) {
            eprintln!("[Storage] 更新容器状态失败 {}", e);
        }
    }
}
```

> 注意：这里在已经持有 `containers` 锁的情况下再去锁 `ip_pool`。只要**全局保持"先 containers 后 ip_pool"的加锁顺序**，就不会死锁。本项目中 `allocate_ip` / `free_ip` 只锁 `ip_pool`，不反向加锁，所以是安全的。

---

## 第三步：`SandboxConfig` 带上分配好的 IP

```rust
// src/sandbox/mod.rs
pub struct SandboxConfig {
    pub container_id: String,
    pub command: Vec<String>,
    pub memory_limit: String,
    pub hostname: String,
    pub ip: String,   // ← 新增：本容器的地址，如 "10.0.0.5"
}
```

---

## 第四步：`network.rs` 改成网桥模型

用"建网桥 + veth 接网桥"替换掉原来的点对点 `setup_veth`：

```rust
// src/sandbox/network.rs —— 新增

const BRIDGE: &str = "mybox0";
const GATEWAY: &str = "10.0.0.1";

/// 创建并启动网桥（幂等，多容器共享同一个网桥）
pub fn ensure_bridge() -> Result<(), AppError> {
    let exists = Command::new("ip")
        .args(["link", "show", BRIDGE])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    if !exists {
        run_ip(&["link", "add", BRIDGE, "type", "bridge"])?;
        run_ip(&["addr", "add", &format!("{}/24", GATEWAY), "dev", BRIDGE])?;
    }
    run_ip(&["link", "set", BRIDGE, "up"])?;
    Ok(())
}

/// 创建 veth，把宿主机端接入网桥，容器端移入 netns 并配置 IP + 默认路由
pub fn setup_veth_bridge(
    host_if: &str,
    cont_if: &str,
    pid: u32,
    ip: &str,
) -> Result<(), AppError> {
    let pid_s = pid.to_string();

    // 1. 创建 veth pair
    run_ip(&["link", "add", host_if, "type", "veth", "peer", "name", cont_if])?;

    // 2. 宿主机端接入网桥并启动
    run_ip(&["link", "set", host_if, "master", BRIDGE])?;
    run_ip(&["link", "set", host_if, "up"])?;

    // 3. 容器端移入容器 netns
    run_ip(&["link", "set", cont_if, "netns", &pid_s])?;

    // 4. 进入容器 netns 配置容器端
    run_nsenter(pid, &["ip", "addr", "add", &format!("{}/24", ip), "dev", cont_if])?;
    run_nsenter(pid, &["ip", "link", "set", cont_if, "up"])?;
    run_nsenter(pid, &["ip", "route", "add", "default", "via", GATEWAY])?;

    Ok(())
}
```

> 旧的 `setup_veth`（点对点）可以删掉，或留着不用。`setup_nat`、`setup_dns`、
> `setup_loopback` 都不用动。

---

## 第五步：串起来

### 5-a：`start_container` 调用新函数

```rust
// src/sandbox/mod.rs —— start_container 父进程分支
            network::ensure_bridge()?;
            network::setup_veth_bridge(&host_if, &cont_if, child_pid, &cfg.ip)?;
            network::setup_nat()?;
            network::setup_dns(&merged)?;

            notify(net_w);
            close_fd(net_w);
```

### 5-b：`server.rs` 的 `RunRequest` 分配 IP

如果直接在 `RunRequest` 这一臂里把"分配 IP → 组 cfg → 启动 → 记录"全用 `match` 串起来，
每多一个可失败步骤就要多嵌一层 `match`，很快就会变成难读的金字塔。更好的做法是把这段主流程
**抽成一个返回 `Result` 的函数**，内部用 `?` 做错误传播；`RunRequest` 分支只负责把
`Result` 转成 `Response`。

先给 `AppError` 加一个表示"IP 池满"的变体（`src/error.rs`）：

```rust
// src/error.rs —— enum AppError 里追加
    #[error("IP 地址已耗尽")]
    IpExhausted,
```

然后新增主流程函数。注意其中的**单一回滚边界**：IP 在 `allocate_ip` 处占用，之后所有可失败
步骤都放进一个内层 `async` 块里用 `?` 平铺；这个块只要有任何一步出错，就在**唯一的一处**
`free_ip` 归还。以后再加资源（端口、volume……），只需在块内多写一行 `?`，回滚点不用动：

```rust
// src/ipc/server.rs —— 新增主流程函数
async fn create_container(
    manager: &ContainerManager,
    command: Vec<String>,
    memory_limit: String,
) -> Result<String, AppError> {
    // 1. 廉价、不可失败的先算好
    let id = generate_id();
    let hostname = format!("mybox-{}", &id[..8]);

    // 2. 获取可失败、需回收的资源；池满 → 转成 IpExhausted 错误向上传播
    let ip = manager.allocate_ip().ok_or(AppError::IpExhausted)?;

    // 3. IP 已持有：后续步骤全用 ? 平铺，出错时在唯一的边界 free_ip
    let result: Result<u32, AppError> = async {
        let cfg = crate::sandbox::SandboxConfig {
            container_id: id.clone(),
            command: command.clone(),
            memory_limit: memory_limit.clone(),
            hostname,
            ip: ip.clone(),
        };
        // .await? 解包 JoinError；再一个 ? 解包 start_container 的 AppError
        let pid = tokio::task::spawn_blocking(move || crate::sandbox::start_container(cfg)).await??;
        // 以后要加资源就在这里继续平铺：
        //   let port = manager.allocate_port()?;
        //   ...
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
                ip: Some(ip),   // ← 记录 IP，退出时据此回收
            });
            Ok(id)
        }
        Err(e) => {
            manager.free_ip(&ip);   // ← 唯一的回滚点：无论块内哪一步失败都归还 IP
            Err(e)
        }
    }
}
```

`RunRequest` 分支现在非常薄，只做 `Result → Response` 的转换：

```rust
// src/ipc/server.rs —— RunRequest 分支
Request::RunRequest { command, memory_limit } => {
    match create_container(&manager, command, memory_limit).await {
        Ok(id) => Response::RunResponse { container_id: id },
        Err(e) => Response::ErrorResponse { message: e.to_string() },
    }
}
```

要点：

- **`?` 负责传播，函数负责收口**：主流程用 `?` 一行行平铺，避免了"每个资源嵌一层 `match`"
  的金字塔。`RunRequest` 分支只剩一个 `Ok/Err → Response` 的转换，整臂仍求值成 `Response`，
  末尾统一的 `send_json` 一处不用改。
- **`.await??` 两个问号**：`spawn_blocking(...).await` 的类型是
  `Result<Result<u32, AppError>, JoinError>`。第一个 `?` 靠 `AppError::Join(#[from] JoinError)`
  解包外层，第二个 `?` 解包 `start_container` 返回的 `AppError`。
- **单一回滚边界**：IP 占用后，可失败步骤都收进内层 `async` 块，只在块外**一处** `free_ip`。
  资源变多时只在块内加 `?`，不必再为每个资源单独写清理，从根上解决了"嵌套膨胀"。
- **`ok_or(AppError::IpExhausted)?`**：把"池满"这种 `Option::None` 用 `ok_or` 转成错误，
  就能和其它步骤一样用 `?` 统一传播。
- 成功时把 IP 写进 `ContainerInfo`（`ip: Some(ip)`），容器退出时第二步 2-b 的
  `on_container_exit` 才能据此回收。

> 局限：`?` + 函数解决了"传播"和"收口"，但回滚仍是**手写**的那一处 `free_ip`——只是从
> "每条失败路径各写一次"收敛成了"整段只写一次"。若将来资源进一步增多、且希望回滚**完全自动**
> （任何提前返回都自动逆序归还），可以再进一步引入 RAII guard：让 `allocate_ip` 返回一个持有
> IP 的哨兵，`Drop` 时自动 `free_ip`，成功后再 `commit()` 放弃回收。那是本方案的自然演进方向。

---

## 验证：两个容器同时在线

因为交互式还没做（第 22 轮），这里用一个"长时间运行"的命令占住容器，方便同时观察。用 busybox 的 `sleep`：

```bash
cargo build
sudo ./target/debug/mybox daemon
```

```bash
# 启动两个后台容器（各睡 300 秒）
./target/debug/mybox run sleep 300 128M      # 记下返回的 ID：容器 A
./target/debug/mybox run sleep 300 128M      # 容器 B

# 看看它们各自的 IP（输出在 daemon 终端）
./target/debug/mybox run ip addr 128M
# 多跑几次，会看到 10.0.0.2、10.0.0.3、10.0.0.4 …… 依次分配

# 列表里两个 sleep 容器都在 Running
./target/debug/mybox list
```

容器互通性验证（假设 A=10.0.0.2、B=10.0.0.3）：在 B 里 ping A：

```bash
./target/debug/mybox run ping -c 2 10.0.0.2 128M
# daemon 终端：通 —— 说明两个容器挂在同一网桥、彼此可达
```

外网依旧可达（NAT 未变）：

```bash
./target/debug/mybox run ping -c 2 8.8.8.8 128M
```

停掉容器后，其 IP 会被 `on_container_exit` 回收，可被后续容器复用。

---

## 本轮收获

- **点对点 veth 无法扩展**：地址写死、容器间不通
- **Linux bridge = 虚拟交换机**：所有容器挂到同一网桥、同一网段，天然互通，网关统一
- **IP 分配是共享且可回收的状态**：放在 `ContainerManager` 里，用 `HashSet` 记录占用
- **分配在 daemon 侧完成**（`RunRequest` 处理里），通过 `SandboxConfig` 传给 `start_container`
- **回收挂在 `on_container_exit`**：呼应第 20 轮"专属资源用完即删"，删除钩子就在 SIGCHLD 路径
- **加锁顺序**：多把锁时固定顺序（先 `containers` 后 `ip_pool`）以避免死锁
- NAT 规则针对整个子网，多容器无需改动

---

## 第 22 轮（预告）：交互式容器——PTY 与标准流转发

到目前为止容器只能跑"非交互式命令"，输出还打在 daemon 终端。最后一块拼图是让
`mybox run -it /bin/sh` 真正给你一个**可交互的 shell**。这需要：

1. **PTY（伪终端）**：daemon 用 `openpty` 分配一对主/从设备，容器把**从设备**作为自己的
   stdin/stdout/stderr 和控制终端
2. **流式协议**：客户端连接在发出请求后不再是"一问一答"，而是变成 daemon 与 client
   之间的**双向字节流**
3. **client 端 raw 模式**：把用户终端设为 raw，逐字节转发，这样方向键、Ctrl-C、Tab 补全才正常

完整内容见单独文档：[08_INTERACTIVE_TUTORIAL.md](./08_INTERACTIVE_TUTORIAL.md)。
