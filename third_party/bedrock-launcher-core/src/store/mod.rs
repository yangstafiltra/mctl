//! mcappx 版本源：bedrock.json 解析 + 版本选择（GDK x64）

use anyhow::{Context, Result};
use serde::Deserialize;

/// 一个可下载版本
#[derive(Debug, Clone)]
pub struct StoreVersion {
    pub version: String,
    /// Release / Preview / Beta
    pub channel_type: String,
    pub url: String,
    pub md5: String,
}

#[derive(Deserialize)]
struct Root {
    #[serde(rename = "From_mcappx.com")]
    versions: std::collections::BTreeMap<String, VerInfo>,
}

#[derive(Deserialize)]
struct VerInfo {
    #[serde(rename = "Type", default)]
    channel_type: String,
    #[serde(rename = "BuildType", default)]
    build_type: String,
    #[serde(rename = "Variations", default)]
    variations: Vec<Variation>,
}

#[derive(Deserialize)]
struct Variation {
    #[serde(rename = "Arch", default)]
    arch: String,
    #[serde(rename = "MetaData", default)]
    metadata: Option<Vec<String>>,
    #[serde(rename = "MD5", default)]
    md5: Option<String>,
}

impl StoreVersion {
    pub fn is_release(&self) -> bool {
        self.channel_type == "Release"
    }
    pub fn channel(&self) -> crate::keys::Channel {
        if self.is_release() {
            crate::keys::Channel::Rel
        } else {
            crate::keys::Channel::Pre
        }
    }
}

/// 拉取并解析版本列表，返回所有 GDK x64 有直链的版本（新版本在前）
pub async fn fetch_versions(client: &reqwest::Client) -> Result<Vec<StoreVersion>> {
    let root: Root = client
        .get("https://data.mcappx.com/v2/bedrock.json")
        .send()
        .await
        .context("拉取 bedrock.json 失败")?
        .json()
        .await
        .context("解析 bedrock.json 失败")?;

    let mut out = Vec::new();
    for (ver, info) in root.versions {
        if info.build_type != "GDK" {
            continue;
        }
        for v in info.variations {
            if v.arch != "x64" {
                continue;
            }
            let Some(url) = v.metadata.unwrap_or_default().into_iter().find(|u| u.starts_with("http")) else {
                continue;
            };
            out.push(StoreVersion {
                version: ver.clone(),
                channel_type: info.channel_type.clone(),
                url,
                md5: v.md5.unwrap_or_default(),
            });
        }
    }
    Ok(out)
}

/// 选最新版本；release=true 选 Release，否则 Preview
pub async fn pick_latest(client: &reqwest::Client, release: bool) -> Result<StoreVersion> {
    let all = fetch_versions(client).await?;
    all.into_iter()
        .find(|v| v.is_release() == release)
        .context("版本列表中没有匹配的 GDK x64 版本")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn fetches_and_picks_latest_release() {
        let client = reqwest::Client::new();
        let all = fetch_versions(&client).await.expect("拉取失败");
        assert!(!all.is_empty(), "至少应有一个 GDK x64 版本");
        assert!(all.iter().all(|v| v.url.starts_with("http")));
        // 最新 Release 应存在且 URL 指向 msixvc
        let rel = pick_latest(&client, true).await.expect("无 Release");
        assert!(rel.is_release());
        assert!(rel.url.ends_with(".msixvc"), "URL 应指向 .msixvc: {}", rel.url);
    }
}

/// 真实下载+全链路集成测试（手动触发）：cargo test --lib download_real -- --ignored --nocapture
#[tokio::test]
#[ignore = "下载约 1.6GB，需手动触发"]
async fn download_real_package() {
    let client = reqwest::Client::new();
    let ver = pick_latest(&client, true).await.expect("选版本失败");
    eprintln!("下载 {} ({})", ver.version, ver.channel_type);
    let dir = std::env::temp_dir().join("mctl-bedrock-dl-test");
    let path = crate::msixvc::download_msixvc(&client, &ver.url, &ver.md5, &dir)
        .await
        .expect("下载失败");
    let size = std::fs::metadata(&path).unwrap().len();
    eprintln!("完成: {} ({} MB)", path.display(), size / 1024 / 1024);

    // 全链路：容器提取 → 验证 exe（.msixvc 即 msft-xvd 容器）
    let vc = path.clone();
    eprintln!("容器: {} ({} MB)", vc.display(), std::fs::metadata(&vc).unwrap().len() / 1024 / 1024);
    let out = dir.join("extracted");
    let files = crate::container::extract(&vc, &crate::keys::get_keys(ver.channel()), &out, &mut |cur, total, name| {
        eprintln!("[{cur}/{total}] {name}");
    })
    .expect("容器提取失败");
    eprintln!("提取 {} 个文件", files.len());
    let exe = out.join("Minecraft.Windows.exe");
    assert!(exe.is_file(), "Minecraft.Windows.exe 未提取出");
    let head = std::fs::read(&exe).unwrap();
    assert_eq!(&head[..2], b"MZ", "exe 必须是 PE（MZ 头）");
    eprintln!("Minecraft.Windows.exe 验证通过（{} MB）", head.len() / 1024 / 1024);
}
