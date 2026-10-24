//! GDK-Proton 引擎（WineGDK fork）自动供给：GDK 游戏必需，普通 Proton 缺
//! Gaming Services RPC 会静默退出。引擎来自 BedrockOnLinux releases，
//! 镜像轮询 + 断点续传，解包后由 discover_gdk_engine 发现。

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

use crate::launch::discover_gdk_engine;

/// 引擎下载源（直连优先，镜像兜底）
const ENGINE_REMOTE: &str =
    "https://github.com/Wyze3306/BedrockOnLinux/releases/download/engine-wow64-archs-native17/GDK-Proton-xuser-wow64-archs-native17.tar.gz";
const MIRRORS: [&str; 2] = ["https://gh-proxy.com/", "https://ghproxy.cn/"];

fn engines_dir() -> PathBuf {
    crate::data_dir().join("engines")
}

fn archive_path() -> PathBuf {
    engines_dir().join("gdk-proton.tar.gz")
}

/// 确保引擎就绪：已解包 → 直接用；有压缩包 → 解包；都没有 → 下载 + 解包。
/// 返回引擎目录。
pub async fn ensure_gdk_engine() -> Result<PathBuf> {
    if let Some(p) = discover_gdk_engine() {
        return Ok(p);
    }
    let archive = archive_path();
    if !archive.is_file() {
        download_engine(&archive).await?;
    }
    extract_engine(&archive)?;
    discover_gdk_engine().context("引擎解包后未找到 proton 脚本")
}

/// 下载（镜像轮询 + Range 续传，最多 60 轮）
async fn download_engine(dest: &Path) -> Result<()> {
    use tokio::io::AsyncWriteExt;
    let tmp = PathBuf::from(format!("{}.part", dest.display()));
    let client = reqwest::Client::builder()
        .user_agent("mctl-bedrock")
        .timeout(std::time::Duration::from_secs(7200))
        .build()?;
    const MAX_RETRIES: u32 = 60;
    for attempt in 1..=MAX_RETRIES {
        let start = tokio::fs::metadata(&tmp).await.map(|m| m.len()).unwrap_or(0);
        // 每轮轮询一个镜像（含直连）
        let urls = std::iter::once(ENGINE_REMOTE.to_string())
            .chain(MIRRORS.iter().map(|m| format!("{m}{ENGINE_REMOTE}")))
            .collect::<Vec<_>>();
        let url = &urls[(attempt as usize - 1) % urls.len()];
        let mut req = client.get(url);
        if start > 0 {
            req = req.header("Range", format!("bytes={start}-"));
            eprintln!("引擎续传 {attempt}: 已有 {} MB", start / 1024 / 1024);
        }
        let resp = match req.send().await {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[{attempt}/{MAX_RETRIES}] 连接失败: {e}");
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                continue;
            }
        };
        if !resp.status().is_success() {
            eprintln!("[{attempt}/{MAX_RETRIES}] HTTP {}", resp.status());
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            continue;
        }
        let resumed = resp.status() == reqwest::StatusCode::PARTIAL_CONTENT;
        // 期望总大小：优先本次响应 Content-Length + 续传起点
        let expected: Option<u64> = resp
            .content_length()
            .map(|cl| cl + if resumed { start } else { 0 });
        let mut file = if resumed {
            tokio::fs::OpenOptions::new().append(true).open(&tmp).await?
        } else {
            tokio::fs::File::create(&tmp).await?
        };
        let mut stream = resp.bytes_stream();
        let mut broken = false;
        use futures_util::StreamExt;
        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(c) => {
                    if let Err(e) = file.write_all(&c).await {
                        eprintln!("写盘失败: {e}");
                        broken = true;
                        break;
                    }
                }
                Err(e) => {
                    eprintln!("下载中断: {e}");
                    broken = true;
                    break;
                }
            }
        }
        file.flush().await?;
        drop(file);
        // 完整性：流“干净结束”也可能是服务器提前断——必须对上期望大小才 rename
        let downloaded = tokio::fs::metadata(&tmp).await.map(|m| m.len()).unwrap_or(0);
        if !broken {
            match expected {
                Some(total) if downloaded == total => {
                    tokio::fs::rename(&tmp, dest).await?;
                    return Ok(());
                }
                Some(total) => eprintln!(
                    "[{attempt}/{MAX_RETRIES}] 大小不符（{}/{}），续传重试",
                    downloaded / 1024 / 1024,
                    total / 1024 / 1024
                ),
                None => {
                    tokio::fs::rename(&tmp, dest).await?;
                    return Ok(());
                }
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    }
    anyhow::bail!("引擎下载失败（已重试 {MAX_RETRIES} 次）")
}

/// 解包（系统 tar；包内含 GDK-Proton 目录树）
fn extract_engine(archive: &Path) -> Result<()> {
    let out = engines_dir();
    std::fs::create_dir_all(&out)?;
    // 先验 gzip 完整性（截断/双写损坏在这里拦住）
    let gz_ok = std::process::Command::new("gzip")
        .arg("-t")
        .arg(archive)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !gz_ok {
        let _ = std::fs::remove_file(archive);
        anyhow::bail!("引擎压缩包损坏（gzip 校验失败），已删除待重下");
    }
    let st = std::process::Command::new("tar")
        .arg("-xzf")
        .arg(archive)
        .current_dir(&out)
        .status()
        .context("执行 tar 失败（需要 tar）")?;
    if !st.success() {
        let _ = std::fs::remove_file(archive);
        anyhow::bail!("引擎解包失败（已删除待重下）");
    }
    Ok(())
}
