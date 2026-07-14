use crate::error::AppError;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

#[derive(Debug, Serialize, Deserialize)]
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
        #[serde(default)]
        interactive: bool,
    },
    RemoveRequest {
        container_id: String,
    }
}

fn default_timeout() -> u64 {
    10
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "default")]
pub enum Response {
    ListResponse { items: Vec<String> },
    StopResponse { container_id: String, state: String },
    RunResponse { container_id: String },
    ErrorResponse { message: String },
}

pub async fn send_json<T>(stream: &mut UnixStream, body: &T) -> Result<(), AppError>
where
    T: Serialize,
{
    // 任何可以序列化的东西都可以被发送
    // to_vec可以把json字符串序列化为JSON字节数组
    let json = serde_json::to_vec(body)?;
    stream.write_u32(json.len() as u32).await?;
    stream.write_all(&json).await?;
    // stream.flush().await.unwrap();
    Ok(())
}

pub async fn recv_json<T>(stream: &mut UnixStream) -> Result<T, AppError>
where
    T: serde::de::DeserializeOwned,
{
    let len = stream.read_u32().await?;

    if len > 1_000_000 {
        return Err(AppError::MessageTooLarge(len));
    }

    let mut buf = vec![0u8; len as usize];
    // read_exact会读取固定长度的消息
    // read 流式数据，可读部分
    // read_to_end 文件 读到EOF
    // read_to_string UTF-8 读到EOF
    stream.read_exact(&mut buf).await?;
    let value = serde_json::from_slice(&buf)?;

    Ok(value)
}
