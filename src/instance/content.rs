//! 实例内容目录：模组 / 光影 / 资源包。
//!
//! 只负责「目录在哪」，具体安装走资源下载页。

use std::path::{Path, PathBuf};

/// 内容类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // 资源包目录暂未接入 UI
pub enum ContentKind {
    Mods,
    Shaders,
    ResourcePacks,
}

impl ContentKind {
    pub fn dir(self) -> &'static str {
        match self {
            ContentKind::Mods => "mods",
            ContentKind::Shaders => "shaderpacks",
            ContentKind::ResourcePacks => "resourcepacks",
        }
    }
}

/// 实例的 mods/shaderpacks/resourcepacks 目录
pub fn content_dir(inst_dir: &Path, kind: ContentKind) -> PathBuf {
    inst_dir.join(kind.dir())
}
