use std::fs;
use std::path::{Path, PathBuf};
use nix::mount::{mount, umount2, MsFlags, MntFlags};

use crate::error::AppError;


const BASE_ROOTFS: &str = "/var/lib/cb/rootfs";
const CONTAINERS_DIR: &str = "/run/cb/containers";

#[allow(dead_code)]
pub struct ContainerFs {
    pub merged: PathBuf, // 合并视图
    upper: PathBuf, // 读写层, 每个容器专属
    work: PathBuf, // 工作层 OverlayFS 内部使用
}

impl ContainerFs {
    pub fn setup(container_id: &str) -> Result<Self, AppError> {
        let base = Path::new(CONTAINERS_DIR).join(container_id);
        let upper = base.join("upper");
        let work = base.join("work");
        let merged = base.join("merged");

        fs::create_dir_all(&upper)?;
        fs::create_dir_all(&work)?;
        fs::create_dir_all(&merged)?;
        fs::create_dir_all(merged.join("proc"))?;
        fs::create_dir_all(merged.join("dev"))?;

        let opts = format!(
            "lowerdir={},upperdir={},workdir={}",
            BASE_ROOTFS,
            upper.display(),
            work.display(),
        );


        mount(
            Some("overlay"),
            &merged,
            Some("overlay"),
            MsFlags::empty(),
            Some(opts.as_str()),
        )?;

        Ok(Self { merged, upper, work})
    }

    pub fn remove(container_id: &str) -> Result<(), AppError> {
        let base = Path::new(CONTAINERS_DIR).join(container_id);
        let merged = base.join("merged");

        let _ = umount2(&merged, MntFlags::MNT_DETACH);

        if base.exists() {
            fs::remove_dir_all(&base)?;
        }
        Ok(())
    }
}
