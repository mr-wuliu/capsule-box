use crate::error::AppError;
use std::process::Command;
use std::path::Path;

const BRIDGE: &str = "mybox0";
const GATEWAY: &str = "10.0.0.1";
const CONTAINER_SUBNET: &str = "10.0.0.0/24";


pub fn setup_loopback() -> Result<(), AppError> {
    let status = Command::new("ip")
        .args(["link", "set", "lo","up"])
        .status()?;

    if ! status.success() {
        return Err(AppError::Io(std::io::Error::new(
            std::io::ErrorKind::Other,
            "启动 lo 失败",
        )));
    }
    Ok(())
}

pub fn setup_nat() -> Result<(), AppError> {
    std::fs::write("/proc/sys/net/ipv4/ip_forward", "1")?;
    ensure_iptables(&[
        "-t", "nat", "-A", "POSTROUTING",
        "-s", CONTAINER_SUBNET, "-j", "MASQUERADE",
    ])?;

    ensure_iptables(&["-A", "FORWARD", "-s", CONTAINER_SUBNET, "-j", "ACCEPT"])?;
    ensure_iptables(&["-A", "FORWARD", "-d", CONTAINER_SUBNET, "-j", "ACCEPT"])?;

    Ok(())
}

pub fn setup_dns(merged: &Path) -> Result<(), AppError> {
    let resolve = merged.join("etc/resolv.conf");
    std::fs::write(resolve, "nameserver 8.8.8.8\n")?;
    Ok(())
}

fn run_ip(args: &[&str]) -> Result<(), AppError> {
    run_cmd("ip", args)
}

fn run_nsenter(pid: u32, args: &[&str]) -> Result<(), AppError> {
    let mut full = vec!["-t".to_string(), pid.to_string(), "-n".to_string()];
    full.extend(args.iter().map(|s| s.to_string()));
    let refs: Vec<&str> = full.iter().map(|s| s.as_str()).collect();
    run_cmd("nsenter", &refs)
}

fn run_cmd(bin: &str, args: &[&str]) -> Result<(), AppError> {
    let status = Command::new(bin).args(args).status()?;
    
    if !status.success() {
        return Err(AppError::Io(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("命令失败: {} {:?}", bin, args),
        )));
    }
    Ok(())
}

fn ensure_iptables(add_args: &[&str]) -> Result<(), AppError> {
    let check_args: Vec<&str> = add_args
        .iter()
        .map(|a| if *a == "-A" { "-C" } else { *a  })
        .collect();
    let exists = Command::new("iptables")
        .args(&check_args)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    if exists {
        return Ok(());
    }

    run_cmd("iptables", add_args)
}

pub fn ensure_bridge() -> Result<(), AppError> {
    // 幂等，所有容器共享同一个网桥
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

pub fn setup_veth_bridge(
    host_if: &str,
    cont_if: &str,
    pid: u32,
    ip: &str,
) -> Result<(), AppError> {
    let pid_s = pid.to_string();

    run_ip(&["link", "add", host_if, "type", "veth", "peer", "name", cont_if])?;

    run_ip(&["link", "set", host_if, "master", BRIDGE])?;
    run_ip(&["link", "set", host_if, "up"])?;

    run_ip(&["link", "set", cont_if, "netns", &pid_s])?;


    run_nsenter(pid, &["ip", "addr", "add", &format!("{}/24", ip), "dev", cont_if])?;
    run_nsenter(pid, &["ip", "link", "set", cont_if, "up"])?;
    run_nsenter(pid, &["ip", "route", "add", "default", "via", GATEWAY])?;

    Ok(())
}