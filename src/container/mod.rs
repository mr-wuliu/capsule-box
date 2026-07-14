use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use crate::error::AppError;
use crate::storage;
use crate::sandbox::{ContainerFs, Cgroup};

// 容器的基本定义
#[derive(Debug, Clone)]
pub struct ContainerInfo {
    pub id: String,
    pub command: Vec<String>,
    pub state: String,
    pub memory_limit: String,
    pub pid: Option<u32>,
    pub ip: Option<String>,
}

#[derive(Clone)]
pub struct ContainerManager {
    containers: Arc<Mutex<HashMap<String, ContainerInfo>>>,
    ip_pool: Arc<Mutex<HashSet<u8>>>,
}

impl ContainerManager {
    pub fn new() -> Self {
        let manager = Self {
            containers: Arc::new(Mutex::new(HashMap::new())),
            ip_pool: Arc::new(Mutex::new(HashSet::new())),
        };
        match storage::load_all() {
            Ok(list) => {
                let mut map = manager.containers.lock().unwrap();
                let mut pool = manager.ip_pool.lock().unwrap();

                for info in list {
                    println!("[Storage] 恢复容器: {} [{}]", info.id, info.state);

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
    pub fn insert(&self, info: ContainerInfo) {
        // 先写盘， 再写内存
        if let Err(e) = storage::save(&info) {
            eprintln!("[Storage] 保存容器失败: {}", e);
        }
        let mut map = self.containers.lock().unwrap();
        map.insert(info.id.clone(), info);
    }

    // 列出所有容器
    pub fn list(&self) -> Vec<ContainerInfo> {
        let map = self.containers.lock().unwrap();
        map.values().cloned().collect()
    }

    pub fn stop(&self, id: &str) -> Option<String> {
        let mut map = self.containers.lock().unwrap();

        if let Some(info) = map.get_mut(id) {
            info.state = "Stopped".to_string();
            info.pid = None;
            if let Err(e) = storage::save(info) {
                eprintln!("[Storage] 更新容器状态失败 {}", e);
            }
            Some(id.to_string())
        } else {
            None
        }
    }

    pub fn on_container_exit(&self, pid: u32, exit_code: i32) {
        let mut map = self.containers.lock().unwrap();
        if let Some(info) = map.values_mut().find(|c| c.pid == Some(pid)) {
            info.state = format!("Exited({})", exit_code);
            info.pid = None;

            if let Some(ip) = info.ip.clone() {
                if let Some(n) = host_octet(&ip) {
                    self.ip_pool.lock().unwrap().remove(&n);
                }
            }

            println!(
                "[Daemon] 容器 {} 已退出，退出码 {}",
                &info.id[..8.min(info.id.len())],
                exit_code
            );
            if let Err(e) = crate::storage::save(info) {
                eprintln!("[Storage] 更新容器状态失败 {}", e);
            }
        }
    }

    pub fn kill_container(&self, id: &str, signal: nix::sys::signal::Signal) -> Option<()> {
        let map = self.containers.lock().unwrap();
        let info = map.get(id)?;
        let pid = info.pid?;

        use nix::sys::signal::kill;
        use nix::unistd::Pid;
        kill(Pid::from_raw(pid as i32), signal).ok()?;
        Some(())
    }
    pub fn allocate_ip(&self) -> Option<String> {
        let mut pool = self.ip_pool.lock().unwrap();
        for n in 2..=254u8 {
            if !pool.contains(&n) {
                pool.insert(n);
                return Some(format!("10.0.0.{}", n))
            }
        }
        None // 地址耗尽
    }

    pub fn free_ip(&self, ip: &str) {
        if let Some(n) = host_octet(ip) {
            self.ip_pool.lock().unwrap().remove(&n);
        }
    }

    pub fn remove(&self, id: &str) -> Result<(), AppError> {
        {
            // 校验：容器存在且不在运行中
            let map = self.containers.lock().unwrap();
            match map.get(id) {
                None => return Err(AppError::NotFound(id.to_string())),
                Some(info) if info.pid.is_some() =>
                    return Err(AppError::StillRunning(id.to_string())),
                Some(_) => {}
            }
        }

        ContainerFs::remove(id)?;
        Cgroup::remove(id)?;
        storage::delete(id)?;

        self.containers.lock().unwrap().remove(id);

        println!("[Daemon] 容器 {} 已移除", &id[..8.min(id.len())]);
        Ok(())
    }
}


// 辅助函数， 用于解析ip字符串最后一位地址。
fn host_octet(ip: &str) -> Option<u8> {
    ip.rsplit('.').next()?.parse().ok()
}
