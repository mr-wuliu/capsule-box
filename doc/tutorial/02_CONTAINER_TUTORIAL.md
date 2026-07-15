# 容器管理框架（螺旋式学习续篇）

> **承接 01_IPC_TUTORIAL.md 的第 7 轮。**  
> IPC 骨架已经跑通：Daemon 监听连接，CLI 发命令，双向 JSON 通信，优雅退出。  
> 但 Daemon 处理 `RunRequest` 时始终返回假数据，`ListRequest` 始终返回空列表。  
> 这一册从第 8 轮开始，一步步让这些命令变得真实。

---

## 目录

- [当前代码状态确认](#当前代码状态确认)
- [run 命令的标准实现](#run-命令的标准实现)
- [第 8 轮：给 Daemon 加内存——`Arc<Mutex<HashMap>>`](#第-8-轮给-daemon-加内存arcmutexhashmap)
- [第 9 轮：数据不能丢——持久化到磁盘](#第-9-轮数据不能丢持久化到磁盘)
- [第 10 轮（预告）：真正的容器——fork + unshare + chroot](#第-10-轮预告真正的容器fork--unshare--chroot)

---

## 当前代码状态确认

完成 01_IPC_TUTORIAL.md 后，你的项目结构如下：

```
src/
├── main.rs          ← 参数解析，统一错误处理
├── error.rs         ← AppError 枚举（thiserror）
└── ipc/
    ├── mod.rs       ← pub mod protocol; pub mod client; pub mod server;
    ├── protocol.rs  ← Request/Response 枚举 + send_json/recv_json
    ├── client.rs    ← run_list()、run_stop()
    └── server.rs    ← run_daemon() 主循环 + handle_one_connection()
```

`handle_one_connection` 里 `RunRequest` 返回假 ID，`ListRequest` 返回空列表。这两轮解决这个问题。

---

## 第 8 轮：给 Daemon 加内存——`Arc<Mutex<HashMap>>`

**这一轮学什么**：如何在多个并发 `tokio::spawn` 任务之间安全地共享一块可变数据。

**问题是什么**：

`handle_one_connection` 每次被 `tokio::spawn` 调用时，都是一个独立的异步任务。两次调用之间没有任何共享状态，所以：

- `run` 命令创建了容器，下一个 `list` 命令完全不知道
- 每个连接只能看到"自己这次调用"的局部变量

需要一个所有连接都能读写的"容器注册表"。

**为什么不能直接用 `HashMap`**：

```rust
// ❌ 这样不行
let map = HashMap::new();
tokio::spawn(async move { map.insert(...) }); // 所有权被移走
tokio::spawn(async move { map.get(...) });    // map 已经不在了
```

即使用引用，两个任务可能同时写同一个 `HashMap`——Rust 编译器会直接拒绝（数据竞争）。

**解决方案：`Arc<Mutex<HashMap>>`**

```
HashMap<String, ContainerInfo>
        │
   Mutex<...>     ← 互斥锁：同一时刻只允许一个任务持有访问权
        │
    Arc<...>      ← 原子引用计数：clone 不复制数据，只增加计数器
```

- **`Mutex`**：就像"加锁的门"。你要进去（访问数据）必须先锁门，出来时自动开锁。别人锁着的时候你只能等。
- **`Arc`**（Atomic Reference Counting）：普通 `Rc` 不能跨线程，`Arc` 是线程安全版本。每次 `.clone()` 不复制内部数据，只让计数器 +1；所有 clone 都 drop 后数据才真正释放。

---

### 第 8 轮改动

**第一步：新建 `src/container/mod.rs`**

```bash
mkdir -p src/container
```

```rust
// src/container/mod.rs
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// 一个容器的基本信息（先只存几个字段，后面慢慢扩充）
#[derive(Debug, Clone)]
pub struct ContainerInfo {
    pub id: String,
    pub command: Vec<String>,
    pub state: String,        // "Running" / "Stopped" / "Exited"
    pub memory_limit: String,
}

/// 容器注册表的句柄
///
/// 注意 #[derive(Clone)]——clone ContainerManager 不会复制 HashMap，
/// 只会增加 Arc 的引用计数。所以可以随意 .clone() 传给不同的 tokio::spawn。
#[derive(Clone)]
pub struct ContainerManager {
    // ─────────────────────────────────────────────────
    // Arc<Mutex<T>> 是 Rust 中共享可变状态的标准模式：
    //   Arc  → 让多个任务持有"同一份数据"的句柄（共享所有权）
    //   Mutex → 同一时刻只有一个任务能修改数据（互斥访问）
    // 两者缺一不可：
    //   只有 Arc  → 多个任务能同时写，数据竞争，编译报错
    //   只有 Mutex → 无法在 tokio::spawn 间传递（不满足 Send 约束）
    // ─────────────────────────────────────────────────
    containers: Arc<Mutex<HashMap<String, ContainerInfo>>>,
}

impl ContainerManager {
    pub fn new() -> Self {
        Self {
            containers: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// 添加一个新容器
    pub fn insert(&self, info: ContainerInfo) {
        // .lock() 获取互斥锁，返回 MutexGuard（智能指针）
        // MutexGuard 实现了 Drop：离开作用域时自动释放锁，不需要手动 unlock
        // .unwrap()：如果另一个线程在持有锁时 panic 了，锁会"中毒"（poisoned）
        //            这种情况极罕见，暂时 unwrap 可以接受
        let mut map = self.containers.lock().unwrap();
        map.insert(info.id.clone(), info);
    }

    /// 列出所有容器
    pub fn list(&self) -> Vec<ContainerInfo> {
        let map = self.containers.lock().unwrap();
        // .cloned() 把 &ContainerInfo 变成 ContainerInfo（需要 ContainerInfo: Clone）
        map.values().cloned().collect()
    }

    /// 把容器状态改为 Stopped，返回 None 表示找不到该 ID
    pub fn stop(&self, id: &str) -> Option<String> {
        let mut map = self.containers.lock().unwrap();
        if let Some(info) = map.get_mut(id) {
            info.state = "Stopped".to_string();
            Some(id.to_string())
        } else {
            None
        }
    }
}
```

**第二步：改造 `src/ipc/server.rs`**

把 `ContainerManager` 创建出来，然后 clone 传给每个连接：

```rust
// src/ipc/server.rs 顶部增加两行 use
use crate::container::{ContainerInfo, ContainerManager};

pub async fn run_daemon() -> Result<(), AppError> {
    let _ = std::fs::remove_file(SOCKET_PATH);
    let listener = UnixListener::bind(SOCKET_PATH)?;
    println!("[Daemon] 启动，监听 {}", SOCKET_PATH);

    // ★ 新增：创建注册表（整个 Daemon 只有这一个实例）
    let manager = ContainerManager::new();

    let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<()>(1);
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.expect("注册 Ctrl+C 失败");
        println!("\n[Daemon] 收到 Ctrl+C");
        let _ = shutdown_tx.send(()).await;
    });

    loop {
        tokio::select! {
            res = listener.accept() => {
                if let Ok((stream, _)) = res {
                    // ★ clone 代价极低（只增加引用计数）
                    let manager = manager.clone();
                    tokio::spawn(async move {
                        if let Err(e) = handle_one_connection(stream, manager).await {
                            eprintln!("[Daemon] 连接处理出错: {}", e);
                        }
                    });
                }
            }
            _ = shutdown_rx.recv() => {
                println!("[Daemon] 正在清理...");
                let _ = std::fs::remove_file(SOCKET_PATH);
                break;
            }
        }
    }

    println!("[Daemon] 已关闭");
    Ok(())
}

// ★ 增加 manager 参数
async fn handle_one_connection(
    mut stream: UnixStream,
    manager: ContainerManager,
) -> Result<(), AppError> {
    let request: Request = recv_json(&mut stream).await?;
    println!("[Daemon] 处理请求: {:?}", request);

    let response = match request {
        Request::ListRequest { .. } => {
            // ★ 返回真实数据
            let items = manager
                .list()
                .into_iter()
                .map(|c| format!("{} [{}] {}", &c.id[..8.min(c.id.len())], c.state, c.command.join(" ")))
                .collect();
            Response::ListResponse { items }
        }
        Request::StopRequest { container_id, .. } => {
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
        Request::RunRequest { command, memory_limit } => {
            // ★ 生成真实 ID，存入注册表
            let id = generate_id();
            manager.insert(ContainerInfo {
                id: id.clone(),
                command,
                state: "Running".to_string(),
                memory_limit,
            });
            Response::RunResponse { container_id: id }
        }
    };

    send_json(&mut stream, &response).await?;
    Ok(())
}

/// 用当前时间纳秒部分生成一个 12 位十六进制 ID
/// 生产环境应用随机字节，这里为了简单用时间戳
fn generate_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .subsec_nanos();
    format!("{:012x}", nanos)
}
```

**第三步：在 `src/main.rs` 顶部声明新模块**

```rust
mod container;    // ← 新增这一行
mod error;
mod ipc;
```

---

### 运行验证

```bash
# 终端 A
cargo run -- daemon

# 终端 B（可以反复执行）
cargo run -- run /bin/bash 256M
# 输出：容器已启动: 3a9f2c1b0e7d

cargo run -- run /usr/bin/python3 128M

cargo run -- list
# 输出：
# 3a9f2c1b [Running] /bin/bash
# 1b4d8f2a [Running] /usr/bin/python3

cargo run -- stop 3a9f2c1b0e7d

cargo run -- list
# 输出：
# 3a9f2c1b [Stopped] /bin/bash
# 1b4d8f2a [Running] /usr/bin/python3
```

---

### 本轮收获

| 概念 | 含义 |
|------|------|
| `Arc<T>` | 多个任务共同"拥有"同一份数据（共享所有权），clone 只增加计数 |
| `Mutex<T>` | 同一时刻只允许一个任务访问数据，保证写操作安全 |
| `.lock().unwrap()` | 获取锁，返回 `MutexGuard`，drop 时自动解锁 |
| `#[derive(Clone)]` on struct | 结构体可以 clone，前提是所有字段都实现了 `Clone` |
| `manager.clone()` 传进 `spawn` | 不复制数据，只增加引用计数，代价极低 |

**第 8 轮的问题**：Daemon 重启后，`HashMap` 里的数据全部丢失——进程退出，内存清空。下一轮把容器状态写到磁盘，实现持久化。

---

## 第 9 轮：数据不能丢——持久化到磁盘

**这一轮学什么**：用 `serde_json` + 标准库 `fs` 把结构体写入文件；Daemon 重启时读回来；`impl From<A> for B` 在两个结构体之间做转换。

**目标效果**：

```bash
# 创建容器后重启 Daemon
cargo run -- run /bin/bash 256M
cargo run -- run /bin/sh 128M
# （Ctrl+C 停止 Daemon，再重新启动）
cargo run -- daemon &
cargo run -- list
# 依然显示 2 个容器  ← 这轮要实现的
```

---

### 存储方案

每个容器存成一个独立的 JSON 文件：

```
/tmp/mybox/containers/
    ├── 3a9f2c1b0e7d.json
    └── 1b4d8f2a9c3e.json
```

`3a9f2c1b0e7d.json` 的内容：

```json
{
  "id": "3a9f2c1b0e7d",
  "command": ["/bin/bash"],
  "state": "Running",
  "memory_limit": "256M"
}
```

**为什么每个容器一个文件，而不是一个大 JSON 列表？**

- **并发安全**：两个容器同时更新状态，写不同文件，不会互相覆盖
- **崩溃安全**：写一个文件失败，不影响其他容器
- **查找方便**：按 ID 直接定位文件路径，不需要解析整个列表

---

### 第 9 轮改动

**第一步：新建 `src/storage/mod.rs`**

```bash
mkdir -p src/storage
```

```rust
// src/storage/mod.rs
use std::fs;
use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};
use crate::error::AppError;
use crate::container::ContainerInfo;

const STORAGE_DIR: &str = "/tmp/mybox/containers";

/// 存到磁盘的结构体（加了 Serialize/Deserialize）
///
/// 为什么不直接在 ContainerInfo 上加 Serialize/Deserialize？
/// ContainerInfo 是内存中的运行时结构，以后可能加很多字段（如 pid、pty_fd）
/// 这些字段不应该序列化到磁盘。单独定义 ContainerMetadata 做磁盘格式。
#[derive(Debug, Serialize, Deserialize)]
pub struct ContainerMetadata {
    pub id: String,
    pub command: Vec<String>,
    pub state: String,
    pub memory_limit: String,
}

// ─────────────────────────────────────────────────────────
// 关键语法：impl From<A> for B
// ─────────────────────────────────────────────────────────
// 实现 From trait 之后，就可以用：
//   ContainerMetadata::from(&info)   或者   (&info).into()
// Rust 标准库规定：实现了 From<A> for B，就自动有 Into<B> for A。
//
// 这比写 fn to_metadata(info: &ContainerInfo) -> ContainerMetadata { ... }
// 更符合 Rust 惯例，也能享受 .into() 的语法糖。
// ─────────────────────────────────────────────────────────
impl From<&ContainerInfo> for ContainerMetadata {
    fn from(c: &ContainerInfo) -> Self {
        ContainerMetadata {
            id: c.id.clone(),
            command: c.command.clone(),
            state: c.state.clone(),
            memory_limit: c.memory_limit.clone(),
        }
    }
}

impl From<ContainerMetadata> for ContainerInfo {
    fn from(m: ContainerMetadata) -> Self {
        ContainerInfo {
            id: m.id,
            command: m.command,
            state: m.state,
            memory_limit: m.memory_limit,
        }
    }
}

fn ensure_dir() -> Result<(), AppError> {
    // create_dir_all：递归创建目录，目录已存在不报错（等价于 mkdir -p）
    fs::create_dir_all(STORAGE_DIR)?;
    Ok(())
}

fn file_path(id: &str) -> PathBuf {
    Path::new(STORAGE_DIR).join(format!("{}.json", id))
}

/// 把一个容器的元数据写入磁盘（insert 或 stop 时调用）
pub fn save(info: &ContainerInfo) -> Result<(), AppError> {
    ensure_dir()?;
    let meta = ContainerMetadata::from(info);
    // to_string_pretty：带缩进的 JSON，方便直接 cat 查看
    // to_vec（IPC 用的）更紧凑，但人不好读
    let json = serde_json::to_string_pretty(&meta)?;
    // fs::write：原子覆盖写入，文件不存在则创建
    fs::write(file_path(&info.id), json)?;
    Ok(())
}

/// 从磁盘删除一个容器的元数据文件（remove 命令时调用）
pub fn delete(id: &str) -> Result<(), AppError> {
    let path = file_path(id);
    if path.exists() {
        fs::remove_file(path)?;
    }
    Ok(())
}

/// 读取所有已保存的容器（Daemon 启动时调用一次）
pub fn load_all() -> Result<Vec<ContainerInfo>, AppError> {
    ensure_dir()?;
    let mut result = vec![];

    // read_dir 列出目录下所有条目，每个 entry 是 Result<DirEntry>
    for entry in fs::read_dir(STORAGE_DIR)? {
        let entry = entry?;
        let path = entry.path();

        // 只处理 .json 文件，跳过其他文件（如 .DS_Store）
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }

        let content = fs::read_to_string(&path)?;

        // 如果某个文件损坏，打印警告跳过，不影响其他容器
        match serde_json::from_str::<ContainerMetadata>(&content) {
            Ok(meta) => result.push(ContainerInfo::from(meta)),
            Err(e) => eprintln!("[Storage] 跳过损坏的文件 {:?}: {}", path, e),
        }
    }

    Ok(result)
}
```

**第二步：改造 `src/container/mod.rs`**

在 `new()` 时恢复，在 `insert()` 和 `stop()` 时同步写盘：

```rust
use crate::storage;   // ← 新增

impl ContainerManager {
    pub fn new() -> Self {
        let manager = Self {
            containers: Arc::new(Mutex::new(HashMap::new())),
        };

        // ★ 新增：Daemon 启动时从磁盘恢复
        match storage::load_all() {
            Ok(list) => {
                let mut map = manager.containers.lock().unwrap();
                for info in list {
                    println!("[Storage] 恢复容器: {} [{}]", info.id, info.state);
                    map.insert(info.id.clone(), info);
                }
            }
            Err(e) => eprintln!("[Storage] 恢复失败: {}", e),
        }

        manager
    }

    pub fn insert(&self, info: ContainerInfo) {
        // ★ 先写盘，再写内存（写盘失败只打印，不中断）
        if let Err(e) = storage::save(&info) {
            eprintln!("[Storage] 保存容器失败: {}", e);
        }
        let mut map = self.containers.lock().unwrap();
        map.insert(info.id.clone(), info);
    }

    pub fn stop(&self, id: &str) -> Option<String> {
        let mut map = self.containers.lock().unwrap();
        if let Some(info) = map.get_mut(id) {
            info.state = "Stopped".to_string();
            // ★ 状态变更同步写盘
            if let Err(e) = storage::save(info) {
                eprintln!("[Storage] 更新容器状态失败: {}", e);
            }
            Some(id.to_string())
        } else {
            None
        }
    }
}
```

**第三步：在 `src/main.rs` 顶部声明新模块**

```rust
mod container;
mod error;
mod ipc;
mod storage;    // ← 新增这一行
```

**第四步：`src/storage/mod.rs` 里的 `storage` 也需要声明**

在 `src/container/mod.rs` 顶部加：

```rust
use crate::storage;   // 注意：storage 已在 main.rs 声明，这里直接 use crate::storage 即可
```

---

### 运行验证

```bash
# 终端 A
cargo run -- daemon

# 终端 B
cargo run -- run /bin/bash 256M
cargo run -- run /bin/sh 128M
cargo run -- list
# 显示 2 个容器

# 终端 A：Ctrl+C 停止 Daemon
# 终端 A：重新启动
cargo run -- daemon
# 输出中看到：
# [Storage] 恢复容器: 3a9f2c1b [Running]
# [Storage] 恢复容器: 1b4d8f2a [Running]

# 终端 B
cargo run -- list
# 依然显示 2 个容器 ✓

# 验证磁盘文件
cat /tmp/mybox/containers/*.json
```

---

### 本轮收获

| 概念 | 作用 |
|------|------|
| `fs::create_dir_all` | 递归创建目录，已存在不报错 |
| `fs::write(path, content)` | 原子覆盖写入文件，不存在则创建 |
| `fs::read_to_string(path)` | 把文件内容读成 `String` |
| `fs::read_dir(dir)` | 遍历目录下所有条目 |
| `serde_json::to_string_pretty` | 带缩进的 JSON，方便人工阅读 |
| `impl From<A> for B` | 类型转换 trait，实现后自动获得 `.into()` |
| 恢复逻辑放在 `new()` 里 | 构造时自动恢复，调用方无需关心细节（封装） |

---

## 完整知识点总览（第 1-9 轮）

| 轮次 | 新增概念 | 核心收获 |
|------|----------|----------|
| 第 1 轮 | `UnixListener`、`UnixStream`、`read`、`write_all` | Socket 通信最基础用法 |
| 第 2 轮 | `write_u32`、`read_u32`、`read_exact` | 长度前缀解决粘包 |
| 第 3 轮 | `#[derive(Serialize, Deserialize)]`、`serde_json`、泛型函数 | JSON 传输结构化数据 |
| 第 4 轮 | `#[serde(tag = "type")]`、`#[serde(default)]`、枚举 match | 枚举区分消息类型，编译期安全 |
| 第 5 轮 | `mod`、`pub mod`、`use crate::` | Rust 模块系统，代码分文件 |
| 第 6 轮 | `#[derive(Error)]`、`#[from]`、`?` 操作符 | 正规错误处理，消灭 unwrap |
| 第 7 轮 | `mpsc::channel`、`tokio::select!`、`tokio::spawn` | 并发多连接，优雅退出 |
| 第 8 轮 | `Arc<Mutex<HashMap>>`、`.lock()` | 多任务共享可变状态 |
| 第 9 轮 | `fs::write`、`fs::read_dir`、`impl From` | 状态持久化，Daemon 重启后恢复 |

### 第 9 轮后的项目结构

```
src/
├── main.rs
├── error.rs
├── container/
│   └── mod.rs       ← ContainerInfo + ContainerManager (Arc<Mutex<HashMap>>)
├── storage/
│   └── mod.rs       ← save() / load_all() / delete()（读写 JSON 文件）
└── ipc/
    ├── mod.rs
    ├── protocol.rs
    ├── client.rs
    └── server.rs    ← handle_one_connection 现在操作真实数据
```

---

## run 命令的标准实现

> `list` 和 `stop` 在 01_IPC_TUTORIAL.md 里已经有示例，`run` 命令涉及三个文件的配合，这里给出完整的标准实现。

---

### 数据流回顾

```
用户输入：mybox run /bin/bash 256M
              │
              ▼
        main.rs          解析参数，拆分出 memory_limit 和 command
              │
              ▼
        client.rs        构造 RunRequest，发给 Daemon，打印响应
              │  Unix Socket
              ▼
        server.rs        生成真实 ID，存入 ContainerManager，回复 RunResponse
```

---

### 第一步：`protocol.rs` 里确认消息格式

`RunRequest` 和 `RunResponse` 在 01_IPC_TUTORIAL.md 第 4 轮已经定义好，不需要改动：

```rust
// RunRequest 的字段
RunRequest {
    command: Vec<String>,    // 完整命令，可以包含多个参数
    memory_limit: String,    // 内存上限字符串，如 "256M"
}

// RunResponse 的字段
RunResponse {
    container_id: String,    // Daemon 生成的真实容器 ID
}
```

**关键点**：`command` 是 `Vec<String>` 而不是单个 `String`，因为命令可以带参数：

```
/bin/bash                          → vec!["/bin/bash"]
/usr/bin/python3 script.py         → vec!["/usr/bin/python3", "script.py"]
/bin/sh -c "echo hello"            → vec!["/bin/sh", "-c", "echo hello"]
```

---

### 第二步：`client.rs` 里实现 `run_run`

```rust
// src/ipc/client.rs

/// 发送 RunRequest，打印分配到的容器 ID
pub async fn run_run(command: Vec<String>, memory_limit: &str) -> Result<(), AppError> {
    let mut stream = UnixStream::connect(SOCKET_PATH)
        .await
        .map_err(AppError::ConnectionFailed)?;

    send_json(
        &mut stream,
        &Request::RunRequest {
            command,                              // 字段名和变量名相同，可以省略 "command: command"
            memory_limit: memory_limit.to_string(),
        },
    ).await?;

    let response: Response = recv_json(&mut stream).await?;

    match response {
        Response::RunResponse { container_id } => {
            println!("容器已启动: {}", container_id);
        }
        Response::ErrorResponse { message } => {
            println!("启动失败: {}", message);
        }
        _ => println!("收到意外响应"),
    }

    Ok(())
}
```

---

### 第三步：`server.rs` 里处理 `RunRequest`

```rust
// src/ipc/server.rs —— handle_one_connection 里的 RunRequest 分支

Request::RunRequest { command, memory_limit } => {
    let id = generate_id();
    manager.insert(ContainerInfo {
        id: id.clone(),
        command,
        state: "Running".to_string(),
        memory_limit,
    });
    // 注意：返回的是刚刚生成的 id，而不是任何硬编码字符串
    // 客户端拿到这个 id 才能之后用它来 stop/inspect
    Response::RunResponse { container_id: id }
}
```

---

### 第四步：`main.rs` 里解析参数

命令行约定格式：

```
mybox run <command...> <memory_limit>

示例：
  mybox run /bin/bash 256M
  mybox run /usr/bin/python3 script.py 128M
```

最后一个参数是内存限制，其余所有参数构成命令。

```rust
// src/main.rs —— match 分支里的 "run" 处理

Some("run") => {
    // args[2..] 是 "run" 之后的所有参数
    // split_last() 把切片拆成 (最后一个, 其余所有)
    // 最后一个 = memory_limit，其余 = command
    let run_args = &args[2..];
    match run_args.split_last() {
        Some((memory_limit, command_parts)) if !command_parts.is_empty() => {
            run_run(command_parts.to_vec(), memory_limit).await
        }
        _ => {
            eprintln!("用法: mybox run <command...> <memory_limit>");
            eprintln!("示例: mybox run /bin/bash 256M");
            return Ok(());
        }
    }
}
```

**为什么用 `split_last()` 而不是固定下标？**

`args.get(2)` 固定取第 3 个参数，只能处理单词命令。  
`split_last()` 把切片拆成 `(最后一个元素, 其余所有元素)`，不管命令有几个参数都能正确分离，适合 `run /bin/sh -c "echo hello" 128M` 这样的场景。

---

### 完整数据流验证

```bash
# 终端 A
cargo run -- daemon

# 终端 B
cargo run -- run /bin/bash 256M
# 输出：容器已启动: 3a9f2c1b0e7d

cargo run -- run /bin/sh 128M
# 输出：容器已启动: 1b4d8f2a9c3e

cargo run -- list
# 输出：
# 3a9f2c1b [Running] /bin/bash
# 1b4d8f2a [Running] /bin/sh

cargo run -- stop 3a9f2c1b0e7d
# 输出：容器 3a9f2c1b0e7d 现在状态: Stopped

cargo run -- list
# 输出：
# 3a9f2c1b [Stopped] /bin/bash
# 1b4d8f2a [Running] /bin/sh
```

---

## 第 10 轮（预告）：真正的容器——fork + unshare + chroot

前 9 轮完成了完整的 IPC + 状态管理骨架，处理的都是内存/磁盘数据。第 10 轮进入最核心的部分：

在 `handle_one_connection` 处理 `RunRequest` 时，不再只是往 `HashMap` 里插一条记录，而是真正调用 Linux 系统调用，创建一个隔离的进程。

**Double Fork 执行流程**：

```
RunRequest 到达 Daemon
    │
    └── spawn_blocking()  ← 在专用线程池里运行（fork 是阻塞操作）
            │
            └── fork() #1
                    ├── 【父进程】等待子进程，收集返回的 PID
                    └── 【子进程 A：namespaced parent】
                            ├── unshare(CLONE_NEWPID | CLONE_NEWUTS | CLONE_NEWNS)
                            │   创建新的 PID、UTS、Mount namespace
                            ├── 设置 cgroup（写 /sys/fs/cgroup/rustbox_<pid>/）
                            ├── mount overlayfs（lowerdir + upperdir → merged）
                            └── fork() #2
                                    ├── 【子进程 A】等待子进程 B，负责 unmount 清理
                                    └── 【子进程 B：容器内进程，PID=1】
                                            ├── mount /proc
                                            ├── mount /dev
                                            ├── chroot(merged_dir)
                                            ├── chdir("/")
                                            └── execv("/bin/bash")
```

**为什么要 Double Fork？**

`unshare(CLONE_NEWPID)` 创建新的 PID namespace，但这个调用只对"之后 fork 出来的子进程"生效，调用者自己的 PID 不变。必须再 fork 一次，新子进程才能成为新 PID namespace 里的 PID 1。

这是第 10 轮要学的内容，需要用到 `unsafe { libc::fork() }` 和 `nix` crate 提供的系统调用封装。

---

> **下一步** → 继续阅读 [03_SANDBOX_TUTORIAL.md](./03_SANDBOX_TUTORIAL.md)  
> 从第 10 轮开始，进入真正的容器实现：fork + namespace + cgroup + OverlayFS。

