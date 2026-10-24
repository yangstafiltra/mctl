//! 官方包下载：version manifest → 直装 vanilla 实例

use std::path::Path;

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;

use super::version::VersionProfile;

/// 版本清单条目（url/time 待清单下载细化接入）
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
pub struct ManifestVersion {
    pub id: String,
    #[serde(rename = "type")]
    pub version_type: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub time: String,
}

/// 版本清单（镜像源 镜像优先，官方兜底）
pub async fn fetch_manifest() -> Result<Vec<ManifestVersion>> {
    let client = super::http_client();
    let mirror = "https://bmclapi2.bangbang93.com/mc/game/version_manifest_v2.json";
    let official = "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json";
    let mut last_err = anyhow!("no attempt");
    for url in [mirror, official] {
        let resp = client.get(url).send().await;
        let text = match resp {
            Ok(r) if r.status().is_success() => r.text().await?,
            Ok(r) => {
                last_err = anyhow!("HTTP {}: {url}", r.status());
                continue;
            }
            Err(e) => {
                last_err = anyhow!("{e}: {url}");
                continue;
            }
        };
        #[derive(Deserialize)]
        struct Manifest {
            #[serde(default)]
            versions: Vec<ManifestVersion>,
        }
        match serde_json::from_str::<Manifest>(&text) {
            Ok(manifest) => return Ok(manifest.versions),
            Err(e) => last_err = anyhow!("解析失败: {e}: {url}"),
        }
    }
    Err(last_err)
}

/// 下载版本 json 到 versions/<id>/<id>.json（镜像优先，官方兜底；本地已存在则直接读）
async fn fetch_version_json(
    client: &reqwest::Client,
    mc_dir: &Path,
    id: &str,
) -> Result<serde_json::Value> {
    let local = mc_dir.join("versions").join(id).join(format!("{id}.json"));
    if let Ok(text) = std::fs::read_to_string(&local) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
            return Ok(v);
        }
    }
    let mirror = format!("https://bmclapi2.bangbang93.com/versions/{id}/json");
    let official = "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json".to_string();
    // 先从官方 manifest 找版本 json 直链
    let mut urls = vec![mirror];
    if let Ok(resp) = reqwest::Client::new().get(&official).send().await {
        if let Ok(text) = resp.text().await {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                if let Some(versions) = v.get("versions").and_then(|x| x.as_array()) {
                    for e in versions {
                        if e.get("id").and_then(|x| x.as_str()) == Some(id) {
                            if let Some(u) = e.get("url").and_then(|x| x.as_str()) {
                                urls.push(u.to_string());
                            }
                            break;
                        }
                    }
                }
            }
        }
    }
    let mut last_err = anyhow!("no attempt");
    for url in urls {
        let resp = client.get(&url).send().await;
        match resp {
            Ok(r) if r.status().is_success() => {
                let text = r.text().await?;
                match serde_json::from_str::<serde_json::Value>(&text) {
                    Ok(v) => return Ok(v),
                    Err(e) => last_err = anyhow!("{e}"),
                }
            }
            Ok(r) => last_err = anyhow!("HTTP {}: {url}", r.status()),
            Err(e) => last_err = anyhow!("{e}: {url}"),
        }
    }
    Err(last_err)
}

/// 安装官方版本实例。
/// progress(phase, done, total) 报告阶段与进度；
/// on_file 报告单文件字节级进度（驱动下载中心的细线）。
/// 返回实例 id。
pub async fn install_official(
    mc_dir: &Path,
    id: &str,
    progress: impl Fn(&str, usize, usize),
    on_file: &(dyn Fn(super::FileProgress) + Send + Sync),
) -> Result<String> {
    let client = super::http_client();
    let versions_dir = mc_dir.join("versions").join(id);
    std::fs::create_dir_all(&versions_dir)?;

    progress("下载版本描述", 0, 1);
    let json = fetch_version_json(&client, mc_dir, id).await?;
    std::fs::write(
        versions_dir.join(format!("{id}.json")),
        serde_json::to_string_pretty(&json)?,
    )?;
    let profile: VersionProfile = serde_json::from_value(json).context("解析版本 json 失败")?;

    progress("下载依赖库", 0, 1);
    super::ensure_libraries(
        mc_dir,
        &profile,
        |done, total| progress("下载依赖库", done, total),
        on_file,
    )
    .await?;

    progress("下载资源文件", 0, 1);
    super::ensure_assets(
        mc_dir,
        &profile,
        |done, total| progress("下载资源文件", done, total),
        on_file,
    )
    .await?;

    Ok(id.to_string())
}
