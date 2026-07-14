mod container;
mod storage;
mod error;
mod ipc;
mod sandbox;

use ipc::client:: {
    run_list, run_stop, run_run, run_remove,
};
use ipc::server::run_daemon;

use crate::ipc::client::run_run_interactive;

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    println!("[damone] Run mybox daemon process");
    let res = match args.get(1).map(|s| s.as_str()) {
        Some("daemon") => run_daemon().await,
        Some("list") => run_list().await,
        Some("run") => {
            // 格式：mybox run <command...> <memory_limit>
            // args[2..] 是 run 之后的所有参数，最后一个是内存，其余是命令
            let run_args = &args[2..];
            // 检测开头的-it
            let (interactive, run_args) = match run_args.split_first() {
                Some((flag, tail)) if flag == "-it" => (true, tail),
                _ => (false, run_args),
            };

            match run_args.split_last() {
                Some((memory_limit, command_parts)) if !command_parts.is_empty() => {
                    let command = command_parts.to_vec();
                    if interactive {
                        run_run_interactive(command, memory_limit).await
                    } else {
                        run_run(command, memory_limit).await
                    }
                }
                _ => {
                    eprintln!("用法: mybox run <command...> <memory_limit>");
                    eprintln!("示例: mybox run /bin/bash 256M");
                    Ok(())
                }
            }
        }
        Some("stop") => {
            let id = args.get(2).map(|s| s.as_str()).unwrap_or_default();
            run_stop(&id).await
        }
        Some("remove") => {
            let id = args.get(2).map(
                |s| s.as_str()
            ).unwrap_or_default();
            run_remove(id).await
        }
        _ => {
             println!("Not implement");
             Ok(())
        }
    };

    if let Err(e) = res {
        eprintln!("Error! {}",e);
        std::process::exit(1);
    }
}
