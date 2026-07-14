use std::{
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::{container::ContainerInfo, error::AppError};

const STORAGE_DIR: &str = "/var/lib/cb/containers";

#[derive(Debug, Serialize, Deserialize)]
pub struct ContainerMetadata {
    pub id: String,
    pub command: Vec<String>,
    pub state: String,
    pub memory_limit: String,
    #[serde(default)]
    pub pid: Option<u32>,
    #[serde(default)]
    pub ip: Option<String>,
}

impl From<&ContainerInfo> for ContainerMetadata {
    fn from(c: &ContainerInfo) -> Self {
        ContainerMetadata {
            id: c.id.clone(),
            command: c.command.clone(),
            state: c.state.clone(),
            memory_limit: c.memory_limit.clone(),
            pid: c.pid.clone(),
            ip: c.ip.clone(),
        }
    }
}

impl From<ContainerMetadata> for ContainerInfo {
    fn from(c: ContainerMetadata) -> Self {
        ContainerInfo {
            id: c.id.clone(),
            command: c.command.clone(),
            state: c.state.clone(),
            memory_limit: c.memory_limit.clone(),
            pid: c.pid.clone(),
            ip: c.ip.clone(),
        }
    }
}

// 工具函数

fn ensure_dir() -> Result<(), AppError> {
    // 递归创建目录
    fs::create_dir_all(STORAGE_DIR)?;
    Ok(())
}

fn file_path(id: &str) -> PathBuf {
    // 创建保存元数据的json
    Path::new(STORAGE_DIR).join(format!("{}.json", id))
}

pub fn save(info: &ContainerInfo) -> Result<(), AppError> {
    ensure_dir()?;
    let meta = ContainerMetadata::from(info);
    let json = serde_json::to_string_pretty(&meta)?;
    fs::write(file_path(&info.id), json)?;
    Ok(())
}

pub fn delete(id: &str) -> Result<(), AppError> {
    let path = file_path(id);
    if path.exists() {
        fs::remove_file(path)?;
    }
    Ok(())
}

pub fn load_all() -> Result<Vec<ContainerInfo>, AppError> {
    // 一次性加载所有文件
    ensure_dir()?;

    let mut result = vec![];

    for entry in fs::read_dir(STORAGE_DIR)? {
        let entry = entry?;
        let path = entry.path();
        // 注意 Option<T> 中map 和and_then 用法的区别
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            // 只处理.json 后缀的
            continue;
        }
        let content = fs::read_to_string(&path)?;

        match serde_json::from_str::<ContainerMetadata>(&content) {
            Ok(meta) => result.push(ContainerInfo::from(meta)),
            Err(e) => eprintln!("[Stroage] 文件损坏, 已跳过. {:?} : {}", path, e),
        }
    }
    Ok(result)
}
