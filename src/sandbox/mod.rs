mod cgroup;
mod fs;
mod network;

pub use cgroup::Cgroup;
pub use fs::ContainerFs;

use crate::error::AppError;
use nix::mount::{MsFlags, mount};
use nix::sched::{CloneFlags, unshare};
use nix::sys::wait::waitpid;
use nix::unistd::{ForkResult, chroot, execvp, fork, sethostname};
use std::ffi::CString;
use std::os::fd::RawFd;
use std::path::Path;

use crate::sandbox::cgroup::{parse_memory_limit};

// const ROOTFS: &str = "/tmp/cb/rootfs";

fn setup_rootfs(merged: &Path) {
    mount(
        Some("proc"),
        &merged.join("proc"),
        Some("proc"),
        MsFlags::empty(),
        None::<&str>,
    )
    .expect("挂载 /process失败");

    mount(
        Some("/dev"),
        &merged.join("dev"),
        None::<&str>,
        MsFlags::MS_BIND | MsFlags::MS_REC,
        None::<&str>,
    )
    .expect("挂载 /dev 失败");

    chroot(merged).expect("chroot 失败");
    std::env::set_current_dir("/").expect("chdir 失败");
}

pub struct SandboxConfig {
    pub container_id: String,
    pub command: Vec<String>,
    pub memory_limit: String,
    pub hostname: String, //容器主机名
    pub ip: String,
    pub stdio: Option<RawFd>,
}

pub fn start_container(cfg: SandboxConfig) -> Result<u32, AppError> {
    let container_fs = ContainerFs::setup(&cfg.container_id)?;
    let cgroup = Cgroup::new(&cfg.container_id)?;
    cgroup.set_memory_limit(parse_memory_limit(&cfg.memory_limit))?;

    let merged: std::path::PathBuf = container_fs.merged.clone();

    let host_if = format!("v{}", &cfg.container_id[..8]);
    let cont_if = format!("c{}", &cfg.container_id[..8]);

    let (ns_r, ns_w) = make_pipe()?;
    let (net_r, net_w) = make_pipe()?;


    match unsafe { fork() }? {
        ForkResult::Parent { child } => {
            let child_pid = child.as_raw() as u32;

            close_fd(ns_w);
            close_fd(net_r);
            

            cgroup.add_process(child_pid)?;

            wait(ns_r);
            close_fd(ns_r);
            
            // 配置veth
            // network::setup_veth(&host_if, &cont_if, child_pid)?;
            
            network::ensure_bridge()?;
            network::setup_veth_bridge(&host_if, &cont_if, child_pid, &cfg.ip)?;
            network::setup_nat()?;
            network::setup_dns(&merged)?;
            
            // 通知子进程网络就绪
            notify(net_w);
            close_fd(net_w);

            // forget 是为了让Rsut 不再调用某个值的Drop
            std::mem::forget(cgroup);
            std::mem::forget(container_fs);
            Ok(child_pid)
        }
        ForkResult::Child => {
            // 子进程只需要使用ns_w 和 net_r
            close_fd(ns_r);
            close_fd(net_w);

            setup_namespace_and_exec(cfg, &merged, ns_w, net_r);
        }
    }
}

fn setup_namespace_and_exec(
    cfg: SandboxConfig,
    merged: &Path,
    ns_w: RawFd,
    net_r: RawFd,
) -> ! {
    // unshare 是linux系统调用
    // 可以让当前进程脱离某些共享的内核资源，进入独立的而命名空间
    unshare(
        CloneFlags::CLONE_NEWPID
            | CloneFlags::CLONE_NEWUTS
            | CloneFlags::CLONE_NEWNS
            | CloneFlags::CLONE_NEWNET,
    )
    .expect("unshare 失败，需要 CAP_SYS_ADMIN 权限");

    notify(ns_w);
    close_fd(ns_w);

    sethostname(&cfg.hostname).expect("sethostname failure");

    match unsafe { fork() }.expect("第二次fork失败") {
        ForkResult::Parent { child } => {
            waitpid(child, None).ok();
            std::process::exit(0);
        }
        ForkResult::Child => {
            // 真正的容器进程 PID = 1
            network::setup_loopback().expect("启动 lo 失败");

            wait(net_r);
            close_fd(net_r);

            if let Some(slave) = cfg.stdio {
                setup_tty(slave);
            }

            setup_rootfs(merged);
            let prog = CString::new(cfg.command[0].as_str()).unwrap();
            let args: Vec<CString> = cfg
                .command
                .iter()
                .map(|s| CString::new(s.as_str()).unwrap())
                .collect();
            execvp(&prog, &args).expect("exec 失败");
            std::process::exit(127);
        }
    }
}

// tty
fn setup_tty(slave: RawFd) {
    unsafe {
        libc::setsid();
        libc::ioctl(slave, libc::TIOCSCTTY, 0);

        libc::dup2(slave, 0);
        libc::dup2(slave, 1);
        libc::dup2(slave, 2);

        if slave > 2 {
            libc::close(slave);
        }
    }
}

// 网络相关

fn make_pipe() -> Result<(RawFd, RawFd), AppError> {
    let mut fds = [0 as RawFd; 2];
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(AppError::Io(std::io::Error::last_os_error()));
    }
    Ok((fds[0], fds[1]))
}

fn notify(fd: RawFd) {
    let byte = [1u8];
    unsafe { libc::write(fd, byte.as_ptr() as *const libc::c_void, 1) };
}

fn wait(fd: RawFd) {
    let mut byte = [1u8];
    unsafe { libc::read(fd, byte.as_mut_ptr() as *mut libc::c_void, 1) };
}
fn close_fd(fd: RawFd) {
    unsafe { libc::close(fd)};
}
