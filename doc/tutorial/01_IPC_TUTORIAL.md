# 从零实现 IPC 通信框架（螺旋式学习版）

> **核心理念**：每一轮都能跑起来，每一轮只学一件新东西。  
> 我们会把同一个程序反复扩充 7 轮，每轮只在上一轮的基础上加一个概念。  
> 结束时，你得到一个真正可用的 IPC 框架，并且理解其中每一行代码。

---

## 目录

- [第 1 轮：让两个进程用 Socket 说话（不到 50 行）](#第-1-轮让两个进程用-socket-说话不到-50-行)
- [第 2 轮：解决"消息粘连"问题——加长度前缀](#第-2-轮解决消息粘连问题加长度前缀)
- [第 3 轮：不再发字符串——改用 JSON](#第-3-轮不再发字符串改用-json)
- [第 4 轮：支持多种命令——用枚举区分消息类型](#第-4-轮支持多种命令用枚举区分消息类型)
- [第 5 轮：代码太长了——拆分到多个文件](#第-5-轮代码太长了拆分到多个文件)
- [第 6 轮：消灭所有 unwrap()——加入正规错误处理](#第-6-轮消灭所有-unwrap加入正规错误处理)
- [第 7 轮：优雅退出——让 Daemon 能被关掉](#第-7-轮优雅退出让-daemon-能被关掉)
- [完整知识点回顾](#完整知识点回顾)

---

## 准备工作

新建一个 Rust 项目：

```bash
cargo new mybox
cd mybox
```

每一轮结束时都可以 `cargo run -- daemon` 和 `cargo run -- list` 验证效果。

---

## 第 1 轮：让两个进程用 Socket 说话（不到 50 行）

**这一轮学什么**：Unix Socket 的最基础用法——绑定、连接、发字节、收字节。

**目标效果**：

- 终端 A：`cargo run -- daemon`，程序等待
- 终端 B：`cargo run -- list`，程序发一条文字消息给 Daemon，Daemon 打印出来

先不管错误处理，先让它跑起来。所有代码全部写在 `main.rs`：

**`Cargo.toml`**：

```toml
[dependencies]
tokio = { version = "1", features = ["full"] }
```

**`src/main.rs`**：

```rust
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

const SOCKET_PATH: &str = "/tmp/mybox.sock";

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();

    match args.get(1).map(|s| s.as_str()) {
        Some("daemon") => run_daemon().await,
        Some("list")   => run_client("list").await,
        _ => println!("用法: mybox daemon | mybox list"),
    }
}

async fn run_daemon() {
    // 残留的 socket 文件会导致 bind 失败，先删掉
    let _ = std::fs::remove_file(SOCKET_PATH);

    let listener = UnixListener::bind(SOCKET_PATH).unwrap();
    println!("[Daemon] 启动，等待连接...");

    // 只接受一个连接，处理完就退出（第 1 轮先这样，够用）
    let (mut stream, _) = listener.accept().await.unwrap();

    // 读取客户端发来的字节，存入 buf
    let mut buf = vec![0u8; 1024];
    let n = stream.read(&mut buf).await.unwrap();

    // buf[..n] 是实际读到的部分，转成字符串打印
    let msg = String::from_utf8_lossy(&buf[..n]);
    println!("[Daemon] 收到消息: {}", msg);
}

async fn run_client(command: &str) {
    let mut stream = UnixStream::connect(SOCKET_PATH).await.unwrap();

    // 把字符串命令作为字节发送
    stream.write_all(command.as_bytes()).await.unwrap();
    println!("[Client] 已发送: {}", command);
}
```

**运行一下**：

```bash
# 终端 A
cargo run -- daemon
# 输出：[Daemon] 启动，等待连接...

# 终端 B
cargo run -- list
# 输出：[Client] 已发送: list

# 此时终端 A 输出：
# [Daemon] 收到消息: list
```

**本轮收获**：

- `UnixListener::bind(path)` — 创建 socket 文件，开始监听
- `listener.accept()` — 等待一个连接，返回 `UnixStream`
- `UnixStream::connect(path)` — 作为客户端连接 socket 文件
- `stream.read(&mut buf)` — 读字节，返回实际读到了多少字节
- `stream.write_all(bytes)` — 把所有字节写出去

**第 1 轮的问题**：  
`read(&mut buf)` 读到多少算完？如果消息很长，一次 `read` 可能只读到一半。  
如果连续发两条消息，接收方可能把它们粘在一起读出来（这叫"粘包"）。  
下一轮解决这个问题。

---

## 第 2 轮：解决"消息粘连"问题——加长度前缀

**这一轮学什么**：为什么字节流需要"消息边界"，以及用 4 字节长度前缀来标记边界。

**问题复现**：如果 Daemon 要连续发两条消息：

```
"hello"   →  68 65 6C 6C 6F
"world"   →  77 6F 72 6C 64
```

接收方从 socket 收到的字节流是：`68 65 6C 6C 6F 77 6F 72 6C 64`  
它不知道第 5 个字节是第一条消息的末尾还是第二条消息的开头。

**解决方案**：在每条消息前面加一个固定 4 字节的"长度头"，告诉接收方这条消息有多少字节：

```
发送 "hello"（5字节）：
  [00][00][00][05]  [68][65][6C][6C][6F]
   ↑─ 长度头 ──↑   ↑──── 消息体 ────↑

发送 "world"（5字节）：
  [00][00][00][05]  [77][6F][72][6C][64]
```

接收方：先读 4 字节得到长度，再精确读那么多字节，就永远不会混淆。

**这一轮只改两个函数**，其他不动：

```rust
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

const SOCKET_PATH: &str = "/tmp/mybox.sock";

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(|s| s.as_str()) {
        Some("daemon") => run_daemon().await,
        Some("list")   => run_client("list").await,
        _ => println!("用法: mybox daemon | mybox list"),
    }
}

async fn run_daemon() {
    let _ = std::fs::remove_file(SOCKET_PATH);
    let listener = UnixListener::bind(SOCKET_PATH).unwrap();
    println!("[Daemon] 启动，等待连接...");

    let (mut stream, _) = listener.accept().await.unwrap();

    // ★ 改动：调用新的 recv_msg 函数，而不是直接 read
    let msg = recv_msg(&mut stream).await;
    println!("[Daemon] 收到消息: {}", msg);
}

async fn run_client(command: &str) {
    let mut stream = UnixStream::connect(SOCKET_PATH).await.unwrap();

    // ★ 改动：调用新的 send_msg 函数，而不是直接 write_all
    send_msg(&mut stream, command).await;
    println!("[Client] 已发送: {}", command);
}

// ══════════════════════════════════════════
// 新增：带长度前缀的发送函数
// ══════════════════════════════════════════
async fn send_msg(stream: &mut UnixStream, msg: &str) {
    let bytes = msg.as_bytes();

    // 第一步：把消息长度写成 4 字节大端整数
    // write_u32 是 tokio AsyncWriteExt 提供的便捷方法
    // "大端"(big-endian)：高位字节在前，是网络协议的通用约定
    stream.write_u32(bytes.len() as u32).await.unwrap();

    // 第二步：写消息体
    // write_all 保证所有字节都写完，不会只写一半
    stream.write_all(bytes).await.unwrap();

    // 第三步：刷新缓冲区，确保数据真的发出去了
    stream.flush().await.unwrap();
}

// ══════════════════════════════════════════
// 新增：带长度前缀的接收函数
// ══════════════════════════════════════════
async fn recv_msg(stream: &mut UnixStream) -> String {
    // 第一步：读 4 字节，解释为大端 u32，得到消息长度
    let len = stream.read_u32().await.unwrap() as usize;

    // 第二步：按长度分配缓冲区，精确读取那么多字节
    // vec![0u8; len] 创建一个长度为 len、全部为 0 的字节数组
    let mut buf = vec![0u8; len];

    // read_exact 保证读满 buf，哪怕底层 IO 多次才返回足够数据
    stream.read_exact(&mut buf).await.unwrap();

    // 第三步：把字节解释为 UTF-8 字符串
    String::from_utf8(buf).unwrap()
}
```

**验证效果与第 1 轮相同**，但现在发送/接收逻辑是正确的——即使消息很长，也能完整收到。

**本轮收获**：

- 理解"粘包"问题：字节流没有消息边界
- 理解"长度前缀"协议：先发长度，再发内容
- `write_u32` / `read_u32`：读写 4 字节大端整数
- `read_exact`：精确读取指定字节数，不会提前返回

**第 2 轮的问题**：  
现在发送的是纯字符串 `"list"`。Daemon 怎么知道这是一个"列出容器"的命令，而不是其他命令？  
以后有 `stop`、`run`、`inspect` 等命令，每个命令还需要附带参数（比如容器 ID）。  
纯字符串不好处理，下一轮改用结构化的 JSON。

---

## 第 3 轮：不再发字符串——改用 JSON

**这一轮学什么**：用 `serde_json` 把 Rust 结构体序列化成 JSON 字节，以及反序列化。

**加入依赖**：

```toml
[dependencies]
tokio = { version = "1", features = ["full"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
```

**这一轮只做三件事**：

1. 定义两个结构体：`Request` 和 `Response`
2. 把 `send_msg` / `recv_msg` 改成发/收 JSON
3. Daemon 根据请求内容返回真正的响应

```rust
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

const SOCKET_PATH: &str = "/tmp/mybox.sock";

// ══════════════════════════════════════════
// 新增：定义消息结构体
// ══════════════════════════════════════════

// ─────────────────────────────────────────
// 关键语法：#[derive(Serialize, Deserialize)]
// ─────────────────────────────────────────
// 这行告诉 serde 框架：帮我自动生成把这个结构体转换成 JSON（Serialize）
// 以及把 JSON 还原成这个结构体（Deserialize）的代码。
// 不用这个宏，你需要手动实现这两个 trait，代码量巨大。
// ─────────────────────────────────────────
#[derive(Debug, Serialize, Deserialize)]
struct Request {
    command: String,    // "list", "stop", 等
    args: Vec<String>,  // 命令的参数，比如容器 ID
}

#[derive(Debug, Serialize, Deserialize)]
struct Response {
    ok: bool,
    message: String,
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(|s| s.as_str()) {
        Some("daemon") => run_daemon().await,
        Some("list")   => run_client("list", vec![]).await,
        Some("stop")   => {
            let id = args.get(2).cloned().unwrap_or_default();
            run_client("stop", vec![id]).await;
        }
        _ => println!("用法: mybox daemon | mybox list | mybox stop <id>"),
    }
}

async fn run_daemon() {
    let _ = std::fs::remove_file(SOCKET_PATH);
    let listener = UnixListener::bind(SOCKET_PATH).unwrap();
    println!("[Daemon] 启动，等待连接...");

    let (mut stream, _) = listener.accept().await.unwrap();

    // ★ 改动：用泛型版接收函数，直接得到 Request 结构体
    let request: Request = recv_json(&mut stream).await;
    println!("[Daemon] 收到请求: {:?}", request);

    // ★ 新增：根据请求构造响应
    let response = match request.command.as_str() {
        "list" => Response {
            ok: true,
            message: "容器列表（假数据）: [无]".to_string(),
        },
        "stop" => Response {
            ok: true,
            message: format!("已停止容器: {}", request.args.get(0).unwrap_or(&"?".to_string())),
        },
        other => Response {
            ok: false,
            message: format!("未知命令: {}", other),
        },
    };

    // ★ 改动：把响应序列化后发回去
    send_json(&mut stream, &response).await;
}

async fn run_client(command: &str, args: Vec<String>) {
    let mut stream = UnixStream::connect(SOCKET_PATH).await.unwrap();

    let request = Request {
        command: command.to_string(),
        args,
    };

    // 发送请求
    send_json(&mut stream, &request).await;

    // 接收响应
    let response: Response = recv_json(&mut stream).await;
    println!("[Client] 响应: ok={}, message={}", response.ok, response.message);
}

// ══════════════════════════════════════════
// 改造：发送/接收函数变成泛型，处理 JSON
// ══════════════════════════════════════════

// ─────────────────────────────────────────
// 关键语法：泛型函数 <T: Serialize>
// ─────────────────────────────────────────
// T 是一个类型参数，代表"任何实现了 Serialize 的类型"。
// 这样同一个 send_json 函数可以发送 Request，也可以发送 Response。
// 不用泛型的话，你要写两个几乎一样的函数：send_request、send_response。
// ─────────────────────────────────────────
async fn send_json<T: Serialize>(stream: &mut UnixStream, value: &T) {
    // to_vec() 把结构体序列化成 JSON 字节数组
    // 比如 Request { command: "list", args: [] }
    // 变成：{"command":"list","args":[]}
    let json = serde_json::to_vec(value).unwrap();

    stream.write_u32(json.len() as u32).await.unwrap();
    stream.write_all(&json).await.unwrap();
    stream.flush().await.unwrap();
}

// ─────────────────────────────────────────
// 关键语法：泛型函数 <T: DeserializeOwned>
// ─────────────────────────────────────────
// DeserializeOwned 的意思是"反序列化后，结构体不借用原始字节"。
// 几乎所有普通结构体都满足这个约束，不需要深入理解它，记住写法就行。
// ─────────────────────────────────────────
async fn recv_json<T: serde::de::DeserializeOwned>(stream: &mut UnixStream) -> T {
    let len = stream.read_u32().await.unwrap() as usize;
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf).await.unwrap();

    // from_slice() 把字节数组反序列化成目标类型 T
    serde_json::from_slice(&buf).unwrap()
}
```

**运行验证**：

```bash
# 终端 A
cargo run -- daemon

# 终端 B
cargo run -- list
# 输出：[Client] 响应: ok=true, message=容器列表（假数据）: [无]

cargo run -- stop abc123
# 输出：[Client] 响应: ok=true, message=已停止容器: abc123
```

**本轮收获**：

- `#[derive(Serialize, Deserialize)]`：让结构体可以和 JSON 互转
- `serde_json::to_vec(&value)`：结构体 → JSON 字节
- `serde_json::from_slice(&bytes)`：JSON 字节 → 结构体
- 泛型函数 `<T: Serialize>`：一个函数处理多种类型

**第 3 轮的问题**：  
现在用的 `Request { command: "list", ... }` 这种写法有个缺陷：  
`command` 是字符串，如果拼错了（比如 `"lsit"`），编译器发现不了，只有运行时才会出错。  
更好的做法是用 Rust 枚举——枚举变体是编译期检查的，拼错了直接报错。  
下一轮升级到枚举。

---

## 第 4 轮：支持多种命令——用枚举区分消息类型

**这一轮学什么**：`serde` 的标签枚举（tagged enum），让 JSON 能区分不同的消息变体。

**核心改动**：把 `Request` 和 `Response` 从结构体改成枚举。

```rust
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

const SOCKET_PATH: &str = "/tmp/mybox.sock";

// ══════════════════════════════════════════
// 改造：用枚举代替结构体
// ══════════════════════════════════════════

// ─────────────────────────────────────────
// 关键语法：#[serde(tag = "type")]
// ─────────────────────────────────────────
// 问题：如果 Request 是枚举，serde 默认把它序列化成这样：
//   ListRequest { all: false }  →  {"ListRequest":{"all":false}}
//
// 这有个问题：反序列化时，serde 需要猜测 JSON 对应的是哪个枚举变体，
// 上面这种格式是可以的，但不够直观。
//
// #[serde(tag = "type")] 改变序列化格式，加一个 "type" 字段：
//   ListRequest { all: false }  →  {"type":"ListRequest","all":false}
//   StopRequest { id: "abc" }   →  {"type":"StopRequest","id":"abc"}
//
// 反序列化时，serde 先读 "type" 字段，知道应该还原成哪个变体，非常直观。
// ─────────────────────────────────────────
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
enum Request {
    ListRequest {
        // ─────────────────────────────────────────
        // 关键语法：#[serde(default)]
        // ─────────────────────────────────────────
        // 如果 JSON 里没有 "all" 字段，就用 bool 的默认值 false。
        // 这样旧版 CLI 发的消息（没有 all 字段）也能被新版 Daemon 解析。
        // ─────────────────────────────────────────
        #[serde(default)]
        all: bool,
    },
    StopRequest {
        container_id: String,
        #[serde(default = "default_timeout")]
        timeout: u64,
    },
    RunRequest {
        command: Vec<String>,
        memory_limit: String,
    },
}

fn default_timeout() -> u64 { 10 }

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
enum Response {
    ListResponse {
        items: Vec<String>,   // 简化：先用字符串列表
    },
    StopResponse {
        container_id: String,
        state: String,
    },
    RunResponse {
        container_id: String,
    },
    ErrorResponse {
        message: String,
    },
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(|s| s.as_str()) {
        Some("daemon") => run_daemon().await,
        Some("list")   => run_list().await,
        Some("run")    => {
            // 格式：mybox run <command...> <memory_limit>
            // 最后一个参数是内存，其余是命令
            let run_args = &args[2..];
            if let Some((memory_limit, command_parts)) = run_args.split_last() {
                if !command_parts.is_empty() {
                    run_run(command_parts.to_vec(), memory_limit).await;
                }
            }
        }
        Some("stop")   => {
            let id = args.get(2).cloned().unwrap_or_default();
            run_stop(id).await;
        }
        _ => println!("用法: mybox daemon | mybox list | mybox run <cmd...> <memory> | mybox stop <id>"),
    }
}

async fn run_daemon() {
    let _ = std::fs::remove_file(SOCKET_PATH);
    let listener = UnixListener::bind(SOCKET_PATH).unwrap();
    println!("[Daemon] 启动，等待连接...");

    let (mut stream, _) = listener.accept().await.unwrap();

    let request: Request = recv_json(&mut stream).await;
    println!("[Daemon] 收到请求: {:?}", request);

    // ★ 改动：match 枚举变体，每个分支有独立的字段
    let response = match request {
        Request::ListRequest { all } => {
            println!("[Daemon] 处理 list 命令，all={}", all);
            Response::ListResponse { items: vec![] }
        }
        Request::StopRequest { container_id, timeout } => {
            println!("[Daemon] 停止容器 {}，超时 {}s", container_id, timeout);
            Response::StopResponse {
                container_id,
                state: "Stopped".to_string(),
            }
        }
        Request::RunRequest { command, memory_limit } => {
            println!("[Daemon] 运行 {:?}，内存限制 {}", command, memory_limit);
            Response::RunResponse {
                container_id: "fake_id_001".to_string(),
            }
        }
    };

    send_json(&mut stream, &response).await;
}

async fn run_list() {
    let mut stream = UnixStream::connect(SOCKET_PATH).await.unwrap();
    send_json(&mut stream, &Request::ListRequest { all: false }).await;

    let response: Response = recv_json(&mut stream).await;
    match response {
        Response::ListResponse { items } => {
            if items.is_empty() {
                println!("没有容器");
            } else {
                for item in items { println!("  {}", item); }
            }
        }
        Response::ErrorResponse { message } => println!("错误: {}", message),
        _ => println!("意外的响应"),
    }
}

async fn run_stop(container_id: String) {
    let mut stream = UnixStream::connect(SOCKET_PATH).await.unwrap();
    send_json(&mut stream, &Request::StopRequest {
        container_id,
        timeout: 10,
    }).await;

    let response: Response = recv_json(&mut stream).await;
    match response {
        Response::StopResponse { container_id, state } => {
            println!("容器 {} 现在状态: {}", container_id, state);
        }
        Response::ErrorResponse { message } => println!("错误: {}", message),
        _ => println!("意外的响应"),
    }
}

async fn run_run(command: Vec<String>, memory_limit: &str) {
    let mut stream = UnixStream::connect(SOCKET_PATH).await.unwrap();
    send_json(&mut stream, &Request::RunRequest {
        command,
        memory_limit: memory_limit.to_string(),
    }).await;

    let response: Response = recv_json(&mut stream).await;
    match response {
        Response::RunResponse { container_id } => {
            println!("容器已启动: {}", container_id);
        }
        Response::ErrorResponse { message } => println!("错误: {}", message),
        _ => println!("意外的响应"),
    }
}

// send_json / recv_json 与第 3 轮完全相同，不变
async fn send_json<T: Serialize>(stream: &mut UnixStream, value: &T) {
    let json = serde_json::to_vec(value).unwrap();
    stream.write_u32(json.len() as u32).await.unwrap();
    stream.write_all(&json).await.unwrap();
    stream.flush().await.unwrap();
}

async fn recv_json<T: serde::de::DeserializeOwned>(stream: &mut UnixStream) -> T {
    let len = stream.read_u32().await.unwrap() as usize;
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf).await.unwrap();
    serde_json::from_slice(&buf).unwrap()
}
```

**运行验证**：

```bash
cargo run -- list
# 输出：没有容器

cargo run -- run /bin/bash 256M
# 输出：容器已启动: fake_id_001

cargo run -- stop abc123
# 输出：容器 abc123 现在状态: Stopped
```

**本轮收获**：

- 枚举 vs 结构体：枚举更安全，编译期检查，不会拼错命令名
- `#[serde(tag = "type")]`：枚举序列化时加 `"type"` 字段
- `#[serde(default)]`：向后兼容——新字段在旧消息中缺失时用默认值
- `match` 枚举：Rust 要求处理所有变体，不会漏掉某个命令

**第 4 轮的问题**：  
`main.rs` 越来越长了，而且全是 `unwrap()`——任何错误都会直接 panic 崩溃。  
下面两轮分别解决：先拆文件，再加错误处理。

---

## 第 5 轮：代码太长了——拆分到多个文件

**这一轮学什么**：Rust 的模块系统，如何把代码拆分到多个文件而不改变任何逻辑。

**拆分原则**：

- `error.rs` — 错误类型（还是 unwrap，先占坑）
- `ipc/protocol.rs` — `Request`、`Response`、`send_json`、`recv_json`
- `ipc/client.rs` — `run_list`、`run_stop`（CLI 侧逻辑）
- `ipc/server.rs` — `run_daemon`（Daemon 侧逻辑）
- `ipc/mod.rs` — 把上面三个文件组织成一个模块
- `main.rs` — 只剩参数解析和分发

**创建目录**：

```bash
mkdir -p src/ipc
```

**`src/ipc/protocol.rs`**（只是把 `Request`、`Response`、两个函数从 main.rs 移过来）：

```rust
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Request {
    ListRequest {
        #[serde(default)]
        all: bool,
    },
    StopRequest {
        container_id: String,
        #[serde(default = "default_timeout")]
        timeout: u64,
    },
    RunRequest {
        command: Vec<String>,
        memory_limit: String,
    },
}

fn default_timeout() -> u64 { 10 }

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Response {
    ListResponse { items: Vec<String> },
    StopResponse { container_id: String, state: String },
    RunResponse { container_id: String },
    ErrorResponse { message: String },
}

// ─────────────────────────────────────────
// 关键语法：pub async fn 中的泛型约束换了写法
// ─────────────────────────────────────────
// 之前写在尖括号里：async fn send_json<T: Serialize>
// 现在用 where 子句：where T: Serialize
// 两种写法完全等价，但 where 子句在约束复杂时更可读。
// ─────────────────────────────────────────
pub async fn send_json<T>(stream: &mut UnixStream, value: &T)
where
    T: Serialize,
{
    let json = serde_json::to_vec(value).unwrap();
    stream.write_u32(json.len() as u32).await.unwrap();
    stream.write_all(&json).await.unwrap();
    stream.flush().await.unwrap();
}

pub async fn recv_json<T>(stream: &mut UnixStream) -> T
where
    T: serde::de::DeserializeOwned,
{
    let len = stream.read_u32().await.unwrap() as usize;
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf).await.unwrap();
    serde_json::from_slice(&buf).unwrap()
}
```

**`src/ipc/client.rs`**（只是把 CLI 函数移过来，改一下路径）：

```rust
use crate::ipc::protocol::{recv_json, send_json, Request, Response};
use tokio::net::UnixStream;

const SOCKET_PATH: &str = "/tmp/mybox.sock";

pub async fn run_list() {
    let mut stream = UnixStream::connect(SOCKET_PATH).await.unwrap();
    send_json(&mut stream, &Request::ListRequest { all: false }).await;

    let response: Response = recv_json(&mut stream).await;
    match response {
        Response::ListResponse { items } => {
            if items.is_empty() { println!("没有容器"); }
            else { for item in items { println!("  {}", item); } }
        }
        Response::ErrorResponse { message } => println!("错误: {}", message),
        _ => println!("意外的响应"),
    }
}

pub async fn run_stop(container_id: String) {
    let mut stream = UnixStream::connect(SOCKET_PATH).await.unwrap();
    send_json(&mut stream, &Request::StopRequest { container_id, timeout: 10 }).await;

    let response: Response = recv_json(&mut stream).await;
    match response {
        Response::StopResponse { container_id, state } => {
            println!("容器 {} 现在状态: {}", container_id, state);
        }
        Response::ErrorResponse { message } => println!("错误: {}", message),
        _ => println!("意外的响应"),
    }
}

pub async fn run_run(command: Vec<String>, memory_limit: &str) {
    let mut stream = UnixStream::connect(SOCKET_PATH).await.unwrap();
    send_json(&mut stream, &Request::RunRequest {
        command,
        memory_limit: memory_limit.to_string(),
    }).await;

    let response: Response = recv_json(&mut stream).await;
    match response {
        Response::RunResponse { container_id } => {
            println!("容器已启动: {}", container_id);
        }
        Response::ErrorResponse { message } => println!("错误: {}", message),
        _ => println!("意外的响应"),
    }
}
```

**`src/ipc/server.rs`**（只是把 Daemon 逻辑移过来）：

```rust
use crate::ipc::protocol::{recv_json, send_json, Request, Response};
use tokio::net::UnixListener;

const SOCKET_PATH: &str = "/tmp/mybox.sock";

pub async fn run_daemon() {
    let _ = std::fs::remove_file(SOCKET_PATH);
    let listener = UnixListener::bind(SOCKET_PATH).unwrap();
    println!("[Daemon] 启动，等待连接...");

    let (mut stream, _) = listener.accept().await.unwrap();
    let request: Request = recv_json(&mut stream).await;
    println!("[Daemon] 收到请求: {:?}", request);

    let response = match request {
        Request::ListRequest { all } => {
            println!("[Daemon] list，all={}", all);
            Response::ListResponse { items: vec![] }
        }
        Request::StopRequest { container_id, timeout } => {
            println!("[Daemon] 停止 {}，超时 {}s", container_id, timeout);
            Response::StopResponse { container_id, state: "Stopped".to_string() }
        }
        Request::RunRequest { command, memory_limit } => {
            println!("[Daemon] 运行 {:?}，内存 {}", command, memory_limit);
            Response::RunResponse { container_id: "fake_001".to_string() }
        }
    };

    send_json(&mut stream, &response).await;
}
```

**`src/ipc/mod.rs`**：

```rust
// ─────────────────────────────────────────
// 关键语法：pub mod
// ─────────────────────────────────────────
// mod protocol; 告诉 Rust：去找 ipc/protocol.rs 文件，把它作为子模块加载。
// pub mod：这个子模块对外可见，外部可以用 crate::ipc::protocol::Request 访问。
// 不加 pub：只有 ipc 模块内部能用。
// ─────────────────────────────────────────
pub mod protocol;
pub mod client;
pub mod server;
```

**`src/main.rs`**（现在非常干净）：

```rust
// ─────────────────────────────────────────
// 关键语法：mod ipc;
// ─────────────────────────────────────────
// 这行告诉 Rust：去找 src/ipc/mod.rs（因为 ipc 是一个目录），
// 把它加载为名为 ipc 的子模块。
// 之后可以用 crate::ipc::client::run_list() 访问里面的内容。
// ─────────────────────────────────────────
mod ipc;

use ipc::client::{run_list, run_run, run_stop};
use ipc::server::run_daemon;

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(|s| s.as_str()) {
        Some("daemon") => run_daemon().await,
        Some("list")   => run_list().await,
        Some("run")    => {
            // 格式：mybox run <command...> <memory_limit>
            // 最后一个参数是内存限制，其余是命令（支持多个参数）
            // 例如：mybox run /bin/bash 256M
            //       mybox run /usr/bin/python3 script.py 128M
            let run_args = &args[2..];
            if let Some((memory_limit, command_parts)) = run_args.split_last() {
                if !command_parts.is_empty() {
                    run_run(command_parts.to_vec(), memory_limit).await;
                }
            }
        }
        Some("stop")   => {
            let id = args.get(2).cloned().unwrap_or_default();
            run_stop(id).await;
        }
        _ => println!("用法: mybox daemon | mybox list | mybox run <cmd...> <memory> | mybox stop <id>"),
    }
}
```

**运行验证**：行为和第 4 轮完全一样，只是代码分布到了多个文件。

**本轮收获**：

- `mod foo;` 告诉 Rust 去找 `foo.rs` 或 `foo/mod.rs`
- `pub mod`：子模块对外可见
- 目录模块：`src/ipc/mod.rs` 对应模块 `crate::ipc`
- `use crate::ipc::client::run_list` 或更简洁地 `use ipc::client::run_list`

**第 5 轮的问题**：  
代码里到处是 `.unwrap()`，任何 IO 错误都会让整个程序崩溃（panic）。  
真正的程序应该打印错误信息，优雅退出。下一轮加入正规的错误处理。

---

## 第 6 轮：消灭所有 unwrap()——加入正规错误处理

**这一轮学什么**：`thiserror` 库、自定义错误类型、`?` 操作符、`Result` 在调用链中的传递。

**加入依赖**：

```toml
[dependencies]
tokio = { version = "1", features = ["full"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
thiserror = "1"     # ← 新增
```

**新建 `src/error.rs`**：

```rust
use thiserror::Error;

// ─────────────────────────────────────────
// 关键语法：#[derive(Error)]
// ─────────────────────────────────────────
// thiserror 的 #[derive(Error)] 宏帮你做两件事：
// 1. 自动实现 std::error::Error trait（让这个枚举成为合法的错误类型）
// 2. 根据 #[error("...")] 自动实现 Display trait（控制错误怎么被打印）
// ─────────────────────────────────────────
#[derive(Error, Debug)]
pub enum AppError {
    // {0} 指"枚举变体里第一个字段"。
    // 当你 println!("{}", err) 时，std::io::Error 的 Display 就会被嵌入进去。
    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),

    // ─────────────────────────────────────────
    // 关键语法：#[from]
    // ─────────────────────────────────────────
    // #[from] 告诉 thiserror：自动实现 From<serde_json::Error> for AppError。
    // 效果：在返回 Result<_, AppError> 的函数里，
    //        serde_json::from_slice(...)?
    //        如果 serde_json 返回错误，? 会自动把它转换成 AppError::Json(e)。
    // 没有 #[from]，你需要手写：.map_err(AppError::Json)?
    // ─────────────────────────────────────────
    #[error("JSON 错误: {0}")]
    Json(#[from] serde_json::Error),

    #[error("连接 daemon 失败: {0}")]
    ConnectionFailed(std::io::Error),

    #[error("消息过大: {0} 字节")]
    MessageTooLarge(u32),
}
```

**改造 `src/ipc/protocol.rs`**（把 unwrap 换成 `?` 和 `Result`）：

```rust
use crate::error::AppError;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

// Request 和 Response 枚举定义不变（省略）
// ...

// ─────────────────────────────────────────
// 关键语法：Result<(), AppError> 和 ? 操作符
// ─────────────────────────────────────────
// 函数签名从 async fn send_json(...) 变成 async fn send_json(...) -> Result<(), AppError>
// 意思是：这个函数可能成功（返回 Ok(())），也可能失败（返回 Err(AppError::...)）。
//
// ? 操作符是"错误传播"的语法糖：
//   let json = serde_json::to_vec(value)?;
// 等价于：
//   let json = match serde_json::to_vec(value) {
//       Ok(v)  => v,
//       Err(e) => return Err(AppError::Json(e)),  // #[from] 做了类型转换
//   };
//
// 链式传播：调用 send_json 的函数，也返回 Result，也用 ?。
// 错误会沿调用链一路冒泡，直到某个地方真正处理它（打印或 recover）。
// ─────────────────────────────────────────
pub async fn send_json<T>(stream: &mut UnixStream, value: &T) -> Result<(), AppError>
where
    T: Serialize,
{
    let json = serde_json::to_vec(value)?;  // ? 代替 .unwrap()
    stream.write_u32(json.len() as u32).await?;
    stream.write_all(&json).await?;
    stream.flush().await?;
    Ok(())
}

pub async fn recv_json<T>(stream: &mut UnixStream) -> Result<T, AppError>
where
    T: serde::de::DeserializeOwned,
{
    let len = stream.read_u32().await?;

    // 安全检查：防止收到一个超大长度值，导致分配几 GB 内存
    if len > 1_000_000 {
        return Err(AppError::MessageTooLarge(len));
    }

    let mut buf = vec![0u8; len as usize];
    stream.read_exact(&mut buf).await?;
    let value = serde_json::from_slice(&buf)?;
    Ok(value)
}
```

**改造 `src/ipc/client.rs`**：

```rust
use crate::error::AppError;
use crate::ipc::protocol::{recv_json, send_json, Request, Response};
use tokio::net::UnixStream;

const SOCKET_PATH: &str = "/tmp/mybox.sock";

// 返回 Result，让调用方决定怎么处理错误
pub async fn run_list() -> Result<(), AppError> {
    // map_err 把 io::Error 转换为我们自定义的 ConnectionFailed，
    // 让错误信息更友好
    let mut stream = UnixStream::connect(SOCKET_PATH)
        .await
        .map_err(AppError::ConnectionFailed)?;

    send_json(&mut stream, &Request::ListRequest { all: false }).await?;

    let response: Response = recv_json(&mut stream).await?;
    match response {
        Response::ListResponse { items } => {
            if items.is_empty() { println!("没有容器"); }
            else { for item in items { println!("  {}", item); } }
        }
        Response::ErrorResponse { message } => println!("错误: {}", message),
        _ => println!("意外的响应"),
    }
    Ok(())
}

pub async fn run_stop(container_id: String) -> Result<(), AppError> {
    let mut stream = UnixStream::connect(SOCKET_PATH)
        .await
        .map_err(AppError::ConnectionFailed)?;

    send_json(&mut stream, &Request::StopRequest { container_id, timeout: 10 }).await?;

    let response: Response = recv_json(&mut stream).await?;
    match response {
        Response::StopResponse { container_id, state } => {
            println!("容器 {} 现在状态: {}", container_id, state);
        }
        Response::ErrorResponse { message } => println!("错误: {}", message),
        _ => println!("意外的响应"),
    }
    Ok(())
}

pub async fn run_run(command: Vec<String>, memory_limit: &str) -> Result<(), AppError> {
    let mut stream = UnixStream::connect(SOCKET_PATH)
        .await
        .map_err(AppError::ConnectionFailed)?;

    send_json(&mut stream, &Request::RunRequest {
        command,
        memory_limit: memory_limit.to_string(),
    }).await?;

    let response: Response = recv_json(&mut stream).await?;
    match response {
        Response::RunResponse { container_id } => {
            println!("容器已启动: {}", container_id);
        }
        Response::ErrorResponse { message } => println!("错误: {}", message),
        _ => println!("意外的响应"),
    }
    Ok(())
}
```

**改造 `src/main.rs`**（在最顶层统一处理错误）：

```rust
mod error;
mod ipc;

use ipc::client::{run_list, run_run, run_stop};
use ipc::server::run_daemon;

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();

    // ─────────────────────────────────────────
    // 错误处理策略：在 main 里统一 match Result，
    // 打印错误信息，然后以非零退出码退出。
    // 不用在每个子函数里重复写错误处理逻辑。
    // ─────────────────────────────────────────
    let result = match args.get(1).map(|s| s.as_str()) {
        Some("daemon") => run_daemon().await,
        Some("list")   => run_list().await,
        Some("run")    => {
            // 格式：mybox run <command...> <memory_limit>
            // 最后一个参数是内存，其余是命令
            let run_args = &args[2..];
            match run_args.split_last() {
                Some((memory_limit, command_parts)) if !command_parts.is_empty() => {
                    run_run(command_parts.to_vec(), memory_limit).await
                }
                _ => {
                    eprintln!("用法: mybox run <command...> <memory_limit>");
                    eprintln!("示例: mybox run /bin/bash 256M");
                    return;
                }
            }
        }
        Some("stop")   => {
            let id = args.get(2).cloned().unwrap_or_default();
            run_stop(id).await
        }
        _ => {
            println!("用法: mybox daemon | mybox list | mybox run <cmd...> <memory> | mybox stop <id>");
            return;
        }
    };

    if let Err(e) = result {
        // 打印错误给用户看（Display trait：显示 #[error("...")] 里定义的文字）
        eprintln!("错误: {}", e);
        // 以非零退出码退出，让调用者（比如 shell 脚本）知道出错了
        std::process::exit(1);
    }
}
```

**本轮收获**：

- `#[derive(Error)]` + `#[error("...")]`：定义错误类型的显示文字
- `#[from]`：自动类型转换，配合 `?` 使用
- `?` 操作符：成功就取值，失败就返回 `Err(转换后的错误)`
- `Result<T, E>` 在调用链中传递：底层错误冒泡到顶层统一处理
- `map_err`：手动把一种错误类型转换成另一种（没有 `#[from]` 时用）

---

## 第 7 轮：优雅退出——让 Daemon 能被关掉

**这一轮学什么**：`tokio::select!` 同时等多个异步事件；`mpsc` 通道传递信号；让 Daemon 支持持续运行和多连接。

**这一轮的三个改动**：

1. Daemon 改为持续运行（接受多个连接），而不是处理一个就退出
2. 按 Ctrl+C 时优雅关闭（删除 socket 文件）
3. 每个连接用独立的 `tokio::spawn` 处理（支持并发）

**改造 `src/ipc/server.rs`**：

```rust
use crate::error::AppError;
use crate::ipc::protocol::{recv_json, send_json, Request, Response};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;

const SOCKET_PATH: &str = "/tmp/mybox.sock";

pub async fn run_daemon() -> Result<(), AppError> {
    let _ = std::fs::remove_file(SOCKET_PATH);
    let listener = UnixListener::bind(SOCKET_PATH)?;
    println!("[Daemon] 启动，监听 {}", SOCKET_PATH);

    // ─────────────────────────────────────────
    // 关键语法：mpsc 通道
    // ─────────────────────────────────────────
    // mpsc = Multi-Producer Single-Consumer（多生产者，单消费者）
    // channel(1) 创建一个容量为 1 的通道，返回 (发送端, 接收端)
    // 这里用它传递一个"关闭信号"：
    //   Ctrl+C 处理器持有 shutdown_tx（发送端），
    //   主循环持有 shutdown_rx（接收端）。
    // 按 Ctrl+C → shutdown_tx.send(()) → shutdown_rx.recv() 返回
    // ─────────────────────────────────────────
    let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<()>(1);

    // 在后台启动一个异步任务，专门等待 Ctrl+C
    tokio::spawn(async move {
        // ctrl_c() 异步等待用户按 Ctrl+C
        tokio::signal::ctrl_c().await.expect("注册 Ctrl+C 失败");
        println!("\n[Daemon] 收到 Ctrl+C，即将退出...");
        // 发送关闭信号（() 是空消息，只用于通知）
        let _ = shutdown_tx.send(()).await;
    });

    // 主循环：同时等待新连接和关闭信号
    loop {
        // ─────────────────────────────────────────
        // 关键语法：tokio::select!
        // ─────────────────────────────────────────
        // select! 同时"挂起"等待多个异步操作，哪个先完成就执行哪个分支。
        //
        // 形象比喻：你坐在前台，同时等待两件事：
        //   有客人进门（listener.accept）→ 去接待
        //   老板打来电话说下班（shutdown_rx.recv）→ 关门走人
        // 不管哪件事先发生，你都能立刻响应，不会因为等一件事而错过另一件。
        //
        // 注意：select! 每次循环只执行一个分支，执行完后回到 loop 顶部再次等待。
        // ─────────────────────────────────────────
        tokio::select! {
            // 分支 1：有新连接
            result = listener.accept() => {
                match result {
                    Ok((stream, _)) => {
                        // ─────────────────────────────────────────
                        // 关键语法：tokio::spawn
                        // ─────────────────────────────────────────
                        // 把处理这个连接的工作"扔"到后台并发执行。
                        // 主循环立刻回到 select! 等待下一个连接，
                        // 不需要等这个连接处理完。
                        //
                        // 效果：Daemon 可以同时处理多个 CLI 请求。
                        // ─────────────────────────────────────────
                        tokio::spawn(async move {
                            if let Err(e) = handle_one_connection(stream).await {
                                eprintln!("[Daemon] 处理连接出错: {}", e);
                            }
                        });
                    }
                    Err(e) => eprintln!("[Daemon] 接受连接失败: {}", e),
                }
            }

            // 分支 2：收到关闭信号
            _ = shutdown_rx.recv() => {
                println!("[Daemon] 正在清理...");
                let _ = std::fs::remove_file(SOCKET_PATH);
                break;  // 退出 loop，函数返回
            }
        }
    }

    println!("[Daemon] 已关闭");
    Ok(())
}

/// 处理单个连接：读请求 → 处理 → 写响应
async fn handle_one_connection(mut stream: UnixStream) -> Result<(), AppError> {
    let request: Request = recv_json(&mut stream).await?;
    println!("[Daemon] 处理请求: {:?}", request);

    let response = match request {
        Request::ListRequest { all } => {
            println!("[Daemon] list all={}", all);
            Response::ListResponse { items: vec![] }
        }
        Request::StopRequest { container_id, timeout } => {
            println!("[Daemon] 停止 {}，超时 {}s", container_id, timeout);
            Response::StopResponse { container_id, state: "Stopped".to_string() }
        }
        Request::RunRequest { command, memory_limit } => {
            println!("[Daemon] 运行 {:?}，内存 {}", command, memory_limit);
            Response::RunResponse { container_id: "fake_001".to_string() }
        }
    };

    send_json(&mut stream, &response).await?;
    Ok(())
}
```

**最终验证**：

```bash
# 终端 A
cargo run -- daemon
# 输出：[Daemon] 启动，监听 /tmp/mybox.sock

# 终端 B（可以反复执行多次）
cargo run -- list
cargo run -- stop abc
cargo run -- list

# 终端 A 收到 Ctrl+C：
# [Daemon] 收到 Ctrl+C，即将退出...
# [Daemon] 正在清理...
# [Daemon] 已关闭
```

**本轮收获**：

- `mpsc::channel`：多生产者单消费者通道，用来在 task 间传递信号
- `tokio::select!`：同时等多个异步事件，先完成先处理
- `tokio::spawn`：后台并发任务，不阻塞主循环
- `tokio::signal::ctrl_c()`：异步等待 Ctrl+C 信号

---

## 完整知识点回顾


| 第几轮   | 新增概念                                                          | 核心收获                |
| ----- | ------------------------------------------------------------- | ------------------- |
| 第 1 轮 | `UnixListener`、`UnixStream`、`read`、`write_all`                | Socket 通信的最基础用法     |
| 第 2 轮 | `write_u32`、`read_u32`、`read_exact`                           | 为什么需要长度前缀，怎么实现消息帧   |
| 第 3 轮 | `#[derive(Serialize, Deserialize)]`、`serde_json::to_vec`、泛型函数 | 用 JSON 传输结构化数据      |
| 第 4 轮 | `#[serde(tag = "type")]`、`#[serde(default)]`、枚举 match         | 用枚举区分消息类型，编译期安全     |
| 第 5 轮 | `mod foo`、`pub mod`、`use crate::...`                          | Rust 模块系统，代码拆分到多个文件 |
| 第 6 轮 | `#[derive(Error)]`、`#[from]`、`?` 操作符、`Result` 传播              | 正规错误处理，消灭 unwrap    |
| 第 7 轮 | `mpsc::channel`、`tokio::select!`、`tokio::spawn`               | 并发处理多连接，优雅退出        |


### 最终的文件结构

```
src/
├── main.rs            ← 参数解析，统一错误处理
├── error.rs           ← AppError 枚举
└── ipc/
    ├── mod.rs         ← 声明子模块
    ├── protocol.rs    ← Request/Response 枚举 + send_json/recv_json
    ├── client.rs      ← CLI 命令函数
    └── server.rs      ← Daemon 主循环 + 连接处理
```

### 下一步：加入容器管理

IPC 框架完成后，下一个里程碑是让 Daemon 真正管理容器——在 `handle_one_connection` 里，处理 `RunRequest` 时不再返回假数据，而是真正调用 `fork()` + `unshare()` 创建隔离环境。那就是 `container/sandbox.rs` 的工作了。