use std::path::{Path, PathBuf};
use std::fs;
use crate::error::AppError;

const CGROUP_BASE: &str = "/sys/fs/cgroup/mybox";

pub struct Cgroup {
    pub path: PathBuf
}

impl Cgroup {
    pub fn new(container_id: &str) -> Result<Self, AppError> {
        // 确保父 cgroup 目录存在（daemon 首次运行或重启后自动创建）
        fs::create_dir_all(CGROUP_BASE)?;

        // 启用 memory controller
        let parent_subtree = Path::new(CGROUP_BASE).join("cgroup.subtree_control");
        fs::write(&parent_subtree, "+memory")?;

        let path = Path::new(CGROUP_BASE).join(container_id);
        fs::create_dir_all(&path)?;
        Ok(Self { path })
    }

    pub fn set_memory_limit(&self, bytes: u64) -> Result<(), AppError> {
        let value = if bytes == u64::MAX {
            "max".to_string()
        } else {
            bytes.to_string()
        };

        fs::write(self.path.join("memory.max"), value)?;
        Ok(())

    }

    pub fn add_process(&self, pid:u32) -> Result<(), AppError> {
        fs::write(self.path.join("cgroup.procs"), pid.to_string())?;
        Ok(())
    }
    pub fn cleanup(&self) -> Result<(), AppError> {
        if self.path.exists() {
            fs::remove_dir(&self.path)?;
        }
        Ok(())
    }

    pub fn remove(container_id: &str) -> Result<(), AppError> {
        let path = Path::new(CGROUP_BASE).join(container_id);
        if path.exists() {
            fs::remove_dir(&path)?;
        }
        Ok(())
    }

}


impl Drop for Cgroup {
    fn drop(&mut self) {
        self.cleanup().ok();
    }
}


pub fn parse_memory_limit(s: &str) -> u64 {
    let s = s.trim();
    if s == "unlimited" || s == "max" {
        return u64::MAX;
    }
    let last = s.chars().last().unwrap_or('0');
    if last.is_alphabetic() {
        let num: u64 = s[..s.len()-1].parse().unwrap_or(0);
        match last {
            'k' | 'K' => num * 1024,
            'm' | 'M' => num * 1024 * 1024,
            'g' | 'G' => num * 1024 * 1024 * 1024,
            _ => num,
        }
    } else {
        // 解析失败，返回默认计算方法
        s.parse().unwrap_or(512 * 1024 * 1024)
    }


}