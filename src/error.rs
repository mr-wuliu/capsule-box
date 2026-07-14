use thiserror::Error;

#[derive(Error, Debug)]
pub enum AppError {
    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON 错误: {0}")]
    Json(#[from] serde_json::Error),

    #[error("连接 dameon 失败: {0}")]
    ConnectionFailed(std::io::Error),

    #[error("消息过大: {0} 字节")]
    MessageTooLarge(u32),

    #[error("系统调用失败: {0}")]
    Syscall(#[from] nix::errno::Errno),

    #[error("任务错误: {0}")]
    Join(#[from] tokio::task::JoinError),

    #[error("IP 地址已经耗尽")]
    IpExhausted,

    #[error("容器不存在: {0}")]
    NotFound(String),

    #[error("容器仍在运行, 请先执行stop: {0}")]
    StillRunning(String),

}