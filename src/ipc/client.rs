use crate::{
    error::AppError,
    ipc::protocol::{Request, Response, recv_json, send_json},
};
use nix::sys::termios::{self, SetArg, Termios};
use std::io::{Read, Write};
use std::os::fd::AsFd;
use tokio::net::UnixStream;

const SOCKET_PATH: &str = "/run/mybox/ipc.sock";

struct RawGuard(Termios);

impl RawGuard {
    // 进入raw 模式
    fn enter() -> Result<Self, AppError> {
        let stdin = std::io::stdin();
        let original = termios::tcgetattr(stdin.as_fd())?;
        let mut raw = original.clone();
        termios::cfmakeraw(&mut raw);
        termios::tcsetattr(stdin.as_fd(), SetArg::TCSANOW, &raw)?;
        Ok(RawGuard(original))
    }
}

impl Drop for RawGuard {
    fn drop(&mut self) {
        // 在离开作用域时，恢复终端原先的设置
        let stdin = std::io::stdin();
        let _ = termios::tcsetattr(stdin.as_fd(), SetArg::TCSANOW, &self.0);
    }
}

pub async fn run_list() -> Result<(), AppError> {
    let mut stream = UnixStream::connect(SOCKET_PATH)
        .await
        .map_err(AppError::ConnectionFailed)?;
    send_json(&mut stream, &Request::ListRequest { all: true }).await?;
    let response: Response = recv_json(&mut stream).await?;
    match response {
        Response::ListResponse { items } => {
            if items.is_empty() {
                println!("no container");
            } else {
                for item in items {
                    println!("  {}", item);
                }
            }
        }
        Response::ErrorResponse { message } => {
            println!("Error: {}", message);
        }
        _ => println!("unexpected response"),
    }

    Ok(())
}

pub async fn run_run(command: Vec<String>, memory_limit: &str) -> Result<(), AppError> {
    let mut stream: UnixStream = UnixStream::connect(SOCKET_PATH).await?;
    send_json(
        &mut stream,
        &Request::RunRequest {
            command: command,
            memory_limit: memory_limit.to_string(),
            interactive: false,
        },
    )
    .await?;

    let response: Response = recv_json(&mut stream).await?;
    match response {
        Response::RunResponse { container_id } => {
            println!("container {} is runnning.", container_id);
        }
        Response::ErrorResponse { message } => {
            println!("container running faield: {}", message);
        }
        _ => println!("unexpected response"),
    }
    Ok(())
}

pub async fn run_run_interactive(command: Vec<String>, memory_limit: &str) -> Result<(), AppError> {
    let mut stream = UnixStream::connect(SOCKET_PATH).await?;
    send_json(
        &mut stream,
        &Request::RunRequest {
            command,
            memory_limit: memory_limit.to_string(),
            interactive: true,
        },
    )
    .await?;
    let _guard = RawGuard::enter()?;

    // 转成阻塞std连接
    let std_stream = stream.into_std()?;
    std_stream.set_nonblocking(false)?;
    let mut sock_w = std_stream.try_clone()?;
    let mut sock_r = std_stream;

    std::thread::spawn(move || {
        let mut stdin = std::io::stdin();
        let mut buf = [0u8; 4096];
        loop {
            match stdin.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if sock_w.write_all(&buf[..n]).is_err() {
                        break;
                    }
                }
            }
        }
    });

    // 主线程 socket -> 本地 stdout （服务端关闭结束）
    let mut stdout = std::io::stdout();
    let mut buf = [0u8; 4096];

    loop {
        match sock_r.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let _ = stdout.write_all(&buf[..n]);
                let _ = stdout.flush();
            }
        }
    }

    Ok(())
}

pub async fn run_stop(container_id: &str) -> Result<(), AppError> {
    let mut stream: UnixStream = UnixStream::connect(SOCKET_PATH).await?;

    send_json(
        &mut stream,
        &Request::StopRequest {
            container_id: container_id.to_string(),
            timeout: 10,
        },
    )
    .await?;

    let response: Response = recv_json(&mut stream).await?;
    match response {
        Response::StopResponse {
            container_id,
            state,
        } => {
            println!(
                "container {} has been stopped. state {}",
                container_id, state
            );
        }
        Response::ErrorResponse { message } => {
            println!("container stop faield: {}", message);
        }
        _ => println!("unexpected response"),
    }

    Ok(())
}

pub async fn run_remove(container_id: &str) -> Result<(), AppError> {
    let mut stream = UnixStream::connect(SOCKET_PATH).await?;
    send_json(
        &mut stream,
        &Request::RemoveRequest {
            container_id: container_id.to_string(),
        },
    ).await?;

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
