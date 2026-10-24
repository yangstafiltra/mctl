//! bedrock-launcher-core：MSIXVC 解密 + Bedrock 启动核心。
//!
//! 密钥来源：keys.md（已实测验证，见 `/home/yangsta/tempfiles/msixvc_verified/keys.md`）
//! 解密流程：MSIX 解包 -> 找到 .vc 文件 -> XTS-AES 解密 -> 提取 Minecraft.Windows.exe -> ProtonGDK/umu 启动

pub mod keys;
pub mod store;
pub mod msixvc;
pub mod launch;
pub mod xts;
pub mod container;
pub mod prefix;
pub mod engines;

pub use keys::{KeySet, Channel};
pub use msixvc::download_msixvc;
pub use launch::{launch_bedrock, LaunchConfig};

use anyhow::{Context, Result};
use std::path::PathBuf;

/// 数据根：~/.local/share/mctl/bedrock（下载缓存 / extracted/<版本> / prefixes/<版本>）
pub fn data_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("mctl/bedrock")
}

/// 已安装（已提取）的版本号列表
pub fn installed_versions() -> Vec<String> {
    let extracted = data_dir().join("extracted");
    let Ok(rd) = std::fs::read_dir(&extracted) else {
        return Vec::new();
    };
    let mut out: Vec<String> = rd
        .flatten()
        .filter(|e| e.path().join("Minecraft.Windows.exe").is_file())
        .filter_map(|e| e.file_name().to_string_lossy().into_owned().into())
        .collect();
    out.sort();
    out
}

/// 确保某版本就绪：下载（幂等）→ 容器提取（幂等）→ 返回 exe 路径
pub async fn prepare_version(client: &reqwest::Client, ver: &store::StoreVersion) -> Result<PathBuf> {
    let keys = keys::get_keys(ver.channel());
    let dest_dir = data_dir();
    let msix_path = msixvc::download_msixvc(&client, &ver.url, &ver.md5, &dest_dir).await?;

    // .msixvc 本身即 msft-xvd 容器；若来源是 zip 包装的 MSIX 则解出 .vc
    let vc_path = if msixvc::is_xvd_container(&msix_path) {
        msix_path
    } else {
        msixvc::extract_vc_from_msix(&msix_path, &dest_dir.join("vc"))?
    };

    let out_dir = dest_dir.join("extracted").join(&ver.version);
    let exe_path = out_dir.join("Minecraft.Windows.exe");
    if !exe_path.is_file() {
        let vc = vc_path.clone();
        let k = keys.clone();
        let o = out_dir.clone();
        let (v, ch) = (ver.version.clone(), ver.channel_type.clone());
        tokio::task::spawn_blocking(move || {
            container::extract(&vc, &k, &o, &mut |cur, total, name| {
                eprintln!("[{cur}/{total}] {name}");
            })
            .map(|_| ())
        })
        .await
        .map_err(|e| anyhow::anyhow!("提取任务失败: {e}"))?
        .with_context(|| format!("容器解密提取失败（{} {}）", v, ch))?;
    }
    if !exe_path.is_file() {
        anyhow::bail!("提取完成但未找到 Minecraft.Windows.exe");
    }
    Ok(exe_path)
}

/// 按版本号启动（每版本独立 WINEPREFIX，HMCL 式隔离）
pub async fn launch_version(version: &str) -> Result<()> {
    let exe = data_dir()
        .join("extracted")
        .join(version)
        .join("Minecraft.Windows.exe");
    if !exe.is_file() {
        anyhow::bail!("版本 {version} 未安装（{} 不存在）", exe.display());
    }
    let prefix = data_dir().join("prefixes").join(version);
    std::fs::create_dir_all(&prefix)?;
    let game_dir = data_dir().join("extracted").join(version);
    prefix::prepare(&prefix, &game_dir)?;
    launch::launch_bedrock(launch::LaunchConfig {
        exe_path: exe,
        proton_prefix: Some(prefix.display().to_string()),
        args: vec![],
    })
    .await
}

/// 统一入口：选最新 → 下载 → 提取 → 启动（mctl 内置 bedrock 命令用）
pub async fn install_and_launch(release: bool, proton_prefix: Option<String>) -> Result<()> {
    let client = reqwest::Client::new();
    let ver = store::pick_latest(&client, release).await?;
    let exe_path = prepare_version(&client, &ver).await?;
    launch::launch_bedrock(launch::LaunchConfig {
        exe_path,
        proton_prefix,
        args: vec![],
    })
    .await
}
