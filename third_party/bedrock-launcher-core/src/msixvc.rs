//! MSIXVC 解密与提取（XTS-AES + 7z/zip 解包）

use anyhow::Result;
use std::fs::File;
use std::io::Read;
use tokio::fs as tokio_fs;
use std::path::{Path, PathBuf};
use zip::ZipArchive;


/// 流式下载 .msixvc，断点续传（Range）+ 重试，完成后校验 MD5。
/// 已有同 MD5 缓存则跳过。
pub async fn download_msixvc(
    client: &reqwest::Client,
    url: &str,
    expected_md5: &str,
    dest_dir: &Path,
) -> Result<PathBuf> {
    use md5::{Digest, Md5};
    use tokio::io::AsyncWriteExt;

    let file_name = url.rsplit('/').next().unwrap_or("package.msixvc");
    let dest = dest_dir.join(file_name);
    tokio_fs::create_dir_all(dest_dir).await?;

    if expected_md5.is_empty() {
        anyhow::bail!("版本数据缺少 MD5，拒绝盲目下载");
    }
    if dest.exists() {
        let mut f = std::fs::File::open(&dest)?;
        let mut hasher = Md5::new();
        std::io::copy(&mut f, &mut hasher)?;
        if hex::encode(hasher.finalize()) == expected_md5 {
            return Ok(dest); // 缓存命中
        }
    }

    let tmp = dest.with_extension("part");
    const MAX_RETRIES: u32 = 20;
    for attempt in 1..=MAX_RETRIES {
        let start = tokio_fs::metadata(&tmp).await.map(|m| m.len()).unwrap_or(0);
        let mut req = client.get(url);
        if start > 0 {
            req = req.header("Range", format!("bytes={start}-"));
            eprintln!("续传 {attempt}: 从 {} 继续（已有 {} MB）", start, start / 1024 / 1024);
        }
        let resp = match req.send().await {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[{attempt}/{MAX_RETRIES}] 连接失败: {e}，5 秒后重试");
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                continue;
            }
        };
        if !resp.status().is_success() {
            anyhow::bail!("下载 HTTP {}", resp.status());
        }
        let resumed = resp.status() == reqwest::StatusCode::PARTIAL_CONTENT;
        let mut file = if resumed {
            tokio_fs::OpenOptions::new().append(true).open(&tmp).await?
        } else {
            tokio_fs::File::create(&tmp).await? // 200 = 服务器不支持 Range，从头下
        };
        let mut stream = resp.bytes_stream();
        use futures_util::StreamExt;
        let mut write_err = None;
        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(c) => {
                    if let Err(e) = file.write_all(&c).await {
                        write_err = Some(e.to_string());
                        break;
                    }
                }
                Err(e) => {
                    write_err = Some(e.to_string());
                    break;
                }
            }
        }
        file.flush().await?;
        drop(file);
        if write_err.is_none() {
            // 完整下载：整文件校验 MD5（续传无法增量算）
            let mut f = std::fs::File::open(&tmp)?;
            let mut hasher = Md5::new();
            std::io::copy(&mut f, &mut hasher)?;
            let got = hex::encode(hasher.finalize());
            if got == expected_md5 {
                tokio_fs::rename(&tmp, &dest).await?;
                return Ok(dest);
            }
            write_err = Some(format!("MD5 不匹配：期望 {expected_md5} 实得 {got}"));
        }
        let err = write_err.unwrap();
        if attempt == MAX_RETRIES {
            anyhow::bail!("下载失败（已重试 {MAX_RETRIES} 次）: {err}");
        }
        eprintln!("[{attempt}/{MAX_RETRIES}] {err}，5 秒后续传重试");
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    }
    unreachable!()
}

/// 是否已是 msft-xvd 容器（0x200 处魔数）
pub fn is_xvd_container(path: &Path) -> bool {
    let Ok(mut f) = File::open(path) else { return false };
    let mut head = [0u8; 0x208];
    if f.read_exact(&mut head).is_err() {
        return false;
    }
    &head[0x200..0x208] == b"msft-xvd"
}

/// 从 MSIX(zip) 中提取 .vc 文件（MSIX 实质是 zip；.vc 可达 1GB+，必须流式拷贝）
pub fn extract_vc_from_msix(msix_path: &Path, out_dir: &Path) -> Result<PathBuf> {
    let file = File::open(msix_path)?;
    let mut archive = ZipArchive::new(file)?;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        let name = entry.name().to_string();
        if name.ends_with(".vc") {
            let file_name = name.rsplit('/').next().unwrap_or(&name).to_string();
            let out_path = out_dir.join(&file_name);
            if let Some(parent) = out_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut out_file = File::create(&out_path)?;
            std::io::copy(&mut entry, &mut out_file)?;
            return Ok(out_path);
        }
    }
    anyhow::bail!("MSIX 中未找到 .vc 文件")
}