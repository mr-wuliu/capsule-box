use crate::container::{ContainerInfo, ContainerManager};
use crate::{
    error::AppError,
    ipc::protocol::{Request, Response, recv_json, send_json},
};
use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
use nix::unistd::Pid;
use nix::pty::openpty;

use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;

use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{FromRawFd, IntoRawFd, RawFd};

const SOCKET_PATH: &str = "/run/cb/ipc.sock";

// 准备通信
pub async fn run_daemon() -> Result<(), AppError> {
    // Daemon 启动时确保所有运行时目录就绪（重启后自动重建）
    std::fs::create_dir_all("/run/cb/containers")?;
    std::fs::create_dir_all("/sys/fs/cgroup/cb")?;

    // 移除旧的socket文件
    let _ = std::fs::remove_file(SOCKET_PATH);
    let listener = UnixListener::bind(SOCKET_PATH)?;
    println!("Daemon Started, wait to conneting.");

    let manager = ContainerManager::new();

    let manager_for_sigchld = manager.clone();
    tokio::spawn(async move {
        use tokio::signal::unix::{SignalKind, signal};
        let mut sigchld = signal(SignalKind::child()).expect("注册 SIGCHLD 失败");
        loop {
            sigchld.recv().await;
            loop {
                match waitpid(Pid::from_raw(-1), Some(WaitPidFlag::WNOHANG)) {
                    Ok(WaitStatus::Exited(pid, code)) => {
                        manager_for_sigchld.on_container_exit(pid.as_raw() as u32, code);
                    }
                    Ok(WaitStatus::Signaled(pid, _, _)) => {
                        manager_for_sigchld.on_container_exit(pid.as_raw() as u32, -1);
                    }
                    _ => break,
                }
            }
        }
    });

    let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<()>(1);

    tokio::spawn(async move {
        tokio::signal::ctrl_c()
            .await
            .expect("register Ctrl + C failed");
        println!("\n[Daemon] received Ctrl + C, exiting");
        let _ = shutdown_tx.send(()).await;
    });

    // main loop

    loop {
        tokio::select! {
            res = listener.accept() => {
                if let Ok((stream, _)) = res {
                    let manager = manager.clone();
                    tokio::spawn(async move {
                        if let Err(e) = handel_one_connection(stream, manager).await {
                            eprintln!("[Daemon] 连接处理出错: {}", e);
                        }
                    });
                }
            }
            _ = shutdown_rx.recv() => {
                println!("[Daemon] cleaning...");
                let _ = std::fs::remove_file(SOCKET_PATH);
                break;
            }
        }
    }

    println!("[Daemon] closed");
    Ok(())
}

async fn handel_one_connection(
    mut stream: UnixStream,
    manager: ContainerManager,
) -> Result<(), AppError> {
    let request: Request = recv_json(&mut stream).await?;
    println!("[Daemon] handel request: {:?}", request);

    let response = match request {
        Request::ListRequest { .. } => {
            let items = manager
                .list()
                .into_iter()
                .map(|c| {
                    format!(
                        "{} [{}] {}",
                        &c.id[..12.min(c.id.len())],
                        c.state,
                        c.command.join(" ")
                    )
                })
                .collect();
            Response::ListResponse { items }
        }
        Request::StopRequest { container_id, .. } => {
            use nix::sys::signal::Signal;

            if manager
                .kill_container(&container_id, Signal::SIGTERM)
                .is_some()
            {
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;

                manager.kill_container(&container_id, Signal::SIGKILL);
                Response::StopResponse {
                    container_id,
                    state: "Stopping".to_string(),
                }
            } else {
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
        }
        Request::RunRequest {
            command,
            memory_limit,
            interactive,
        } => {
            match create_container(&manager, command, memory_limit, interactive).await {
                // 分为交互式和非交互式两种
                Ok((_id, Some(master))) => {
                    forward_pty(stream, master).await?;
                    return Ok(());
                }
                Ok((id, None)) => Response::RunResponse { container_id: id },
                Err(e) => Response::ErrorResponse { message: e.to_string() },
            }
        }
        Request::RemoveRequest { container_id } => {
            match manager.remove(&container_id) {
                Ok(()) => Response::StopResponse {
                    container_id,
                    state: "Removed".to_string(),
                },
                Err(e) => Response::ErrorResponse { message: e.to_string() },
            }
        }
    };
    send_json(&mut stream, &response).await?;
    Ok(())
}

fn generate_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .subsec_nanos();
    format!("{:012x}", nanos)
}

async fn create_container(
    manager: &ContainerManager,
    command: Vec<String>,
    memory_limit: String,
    interactive: bool,
) -> Result<(String, Option<RawFd>), AppError> {
    let id = generate_id();
    let hostname = format!("cb-{}", &id[..8]);

    let ip = manager.allocate_ip().ok_or(AppError::IpExhausted)?;

    let pty = if interactive { Some(alloc_pty()?) } else { None };
    let slave_fd = pty.map(| (_, slave)| slave );

    let result: Result<u32, AppError> = async {
        let cfg = crate::sandbox::SandboxConfig {
            container_id: id.clone(),
            command: command.clone(),
            memory_limit: memory_limit.clone(),
            hostname,
            ip: ip.clone(),
            stdio: slave_fd,
        };
        let pid =
            tokio::task::spawn_blocking(move || crate::sandbox::start_container(cfg)).await??;
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
                ip: Some(ip),
            });

            let master = match pty {
                Some((master, slave)) => {
                    unsafe { libc::close(slave); }
                    Some(master)
                }
                None => None,
            };
            Ok((id, master))
        }
        Err(e) => {
            manager.free_ip(&ip);
            Err(e)
        }
    }
}

fn alloc_pty() -> Result<(RawFd, RawFd), AppError> {
    // 获取的结果中，第一个是master， 第二个是slave
    let pty = openpty(None, None)?;

    let master = pty.master.into_raw_fd();
    let slave = pty.slave.into_raw_fd();
    
    //master 添加 FD_CLOEXE，容器在exec是自动关闭它继承来的副本
    // 这样只剩daemon 持有master， 容器退出后master 才能读到EOF
    unsafe {
        let flags = libc::fcntl(master, libc::F_GETFD);
        libc::fcntl(master, libc::F_SETFD, flags | libc::FD_CLOEXEC);
    }

    Ok((master, slave))
}

async fn forward_pty(stream: UnixStream, master: RawFd) -> Result<(), AppError> {
    let std_stream = stream.into_std()?;
    std_stream.set_nonblocking(false)?;

    tokio::task::spawn_blocking(move || {
        let mut master_r = unsafe{ File::from_raw_fd(master) };
        let mut master_w = master_r.try_clone().expect("clone master");
        let mut sock_r = std_stream.try_clone().expect("clone sock");
        let mut sock_w = std_stream;

        // 线程A： client -> container (socket 读， master写)
        let a = std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match sock_r.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => { if master_w.write_all(&buf[..n]).is_err() {break; } }
                }
            }
        });
        let mut buf = [0u8; 4096];
        loop {
            match master_r.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => { if sock_w.write_all(&buf[..n]).is_err() { break; } }
            }
        }

        let _ = sock_w.shutdown(std::net::Shutdown::Both);
        let _ = a.join();
    }).await?;
    Ok(())
}
