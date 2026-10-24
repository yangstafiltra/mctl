//! ProtonGDK / umu-run 启动器

use anyhow::{Context, Result};
use std::path::PathBuf;
use tokio::process::Command as TokioCommand;

/// 启动配置
#[derive(Debug, Clone)]
pub struct LaunchConfig {
    pub exe_path: PathBuf,
    pub proton_prefix: Option<String>,
    pub args: Vec<String>,
}

/// 启动 Bedrock（通过 umu-run / Proton）
pub async fn launch_bedrock(mut config: LaunchConfig) -> Result<()> {
    let exe = config.exe_path.canonicalize().context("EXE 路径无效")?;
    config.exe_path = exe;

    if which("umu-run") {
        launch_with_umu(&config).await
    } else if which("proton") {
        launch_with_proton(&config).await
    } else {
        anyhow::bail!("未找到 umu-run，请先安装: sudo pacman -S umu-launcher")
    }
}

/// umu-run 启动：stdout/stderr 行流式转发（首次运行下载 UMU-Proton 时可见进度），
/// 失败时带回最后一条有效输出。GDK 游戏需 cwd = exe 目录。
async fn launch_with_umu(config: &LaunchConfig) -> Result<()> {
    let mut cmd = TokioCommand::new("umu-run");
    if let Some(prefix) = &config.proton_prefix {
        cmd.env("WINEPREFIX", prefix);
    }
    // PROTONPATH：用户显式设置 > GDK 引擎（WineGDK，GDK 游戏必需）> Steam 自带 Proton
    if std::env::var("PROTONPATH").map(|v| v.is_empty()).unwrap_or(true) {
        match discover_gdk_engine() {
            Some(p) => {
                eprintln!("PROTONPATH = {}（GDK 引擎）", p.display());
                cmd.env("PROTONPATH", &p);
            }
            None => {
                if let Some(p) = discover_steam_proton() {
                    eprintln!("PROTONPATH = {}（Steam Proton，注意：GDK 游戏可能需要 GDK-Proton 引擎）", p.display());
                    cmd.env("PROTONPATH", &p);
                }
            }
        }
    }
    // ponytail: Bedrock 白屏修复——强制 WineD3D（OpenGL）而非 DXVK（Vulkan），
    // Bedrock 的 DX11 渲染在 DXVK 下经常白屏/黑屏；MESA_GL_VERSION_OVERRIDE 保底兼容。
    if std::env::var("MCTL_BEDROCK_USE_DXVK").unwrap_or_default() != "1" {
        cmd.env("PROTON_USE_WINED3D", "1");
    }
    cmd.env("MESA_GL_VERSION_OVERRIDE", "4.5")
        .env("WINEDEBUG", "-all")
        .env("DXVK_LOG_LEVEL", "none")
        .env("VKD3D_LOG_LEVEL", "none");
    let dir = config.exe_path.parent().map(|p| p.to_path_buf());
    cmd.arg(&config.exe_path)
        .args(&config.args)
        .current_dir(dir.unwrap_or_else(|| PathBuf::from(".")))
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = cmd
        .spawn()
        .context("无法启动 umu-run（请确认 umu-launcher 安装完整）")?;

    // 双流转发：行打印到自身 stderr（→ mctl 状态栏），保留最后有效行用于报错
    let last = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    use tokio::io::AsyncRead;
    let mut readers: Vec<Box<dyn AsyncRead + Unpin + Send>> = Vec::new();
    if let Some(out) = child.stdout.take() {
        readers.push(Box::new(out));
    }
    if let Some(err) = child.stderr.take() {
        readers.push(Box::new(err));
    }
    for stream in readers {
        let last = last.clone();
        tokio::spawn(async move {
            use tokio::io::AsyncBufReadExt;
            let mut lines = tokio::io::BufReader::new(stream).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                eprintln!("{line}");
                let t = line.trim_start();
                if !t.is_empty() && !t.starts_with("File \"") && !t.starts_with("~~~") && !t.contains("trace:seh") {
                    *last.lock().unwrap() = line;
                }
            }
        });
    }

    let st = child.wait().await.context("等待 umu-run 失败")?;
    if !st.success() {
        let tail = last.lock().unwrap().clone();
        let tail: String = if tail.is_empty() { "(无输出)".into() } else { tail.chars().take(200).collect() };
        anyhow::bail!("umu-run 失败（{:?}）：{}", st.code(), tail);
    }
    Ok(())
}

/// Proton/GDK 启动（回退）
async fn launch_with_proton(config: &LaunchConfig) -> Result<()> {
    let proton = std::env::var("PROTON_PATH").unwrap_or_else(|_| {
        std::env::var("STEAM_COMPAT_DATA_PATH")
            .map(|p| {
                let path = PathBuf::from(&p);
                path.parent()
                    .unwrap_or(path.as_path())
                    .join("proton")
                    .display()
                    .to_string()
            })
            .unwrap_or_else(|_| "proton".into())
    });
    let compat = config
        .proton_prefix
        .clone()
        .or_else(|| std::env::var("STEAM_COMPAT_DATA_PATH").ok())
        .unwrap_or_default();
    let dir = config.exe_path.parent().map(|p| p.to_path_buf());
    let out = TokioCommand::new(&proton)
        .env("WINEPREFIX", config.proton_prefix.clone().unwrap_or_default())
        .env("STEAM_COMPAT_DATA_PATH", compat)
        .arg("run")
        .arg(&config.exe_path)
        .args(&config.args)
        .current_dir(dir.unwrap_or_else(|| PathBuf::from(".")))
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .await
        .context("无法启动 proton")?;
    if !out.status.success() {
        anyhow::bail!("proton 失败（{:?}）：{}", out.status.code(), stderr_tail(&out.stderr, 3));
    }
    Ok(())
}

/// GDK 引擎（WineGDK fork）自动发现：data_dir/engines/ 下含 proton 脚本的目录
pub fn discover_gdk_engine() -> Option<PathBuf> {
    let engines = crate::data_dir().join("engines");
    let rd = std::fs::read_dir(&engines).ok()?;
    let mut dirs: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|d| d.is_dir() && d.join("proton").is_file())
        .collect();
    dirs.sort();
    dirs.pop() // 字典序最大 = 最新
}

/// Steam 自带 Proton 自动发现：Experimental > 版本号最大 > 其余
fn discover_steam_proton() -> Option<PathBuf> {
    let base = dirs::data_dir()?.join("Steam").join("steamapps").join("common");
    let rd = std::fs::read_dir(&base).ok()?;
    let mut names: Vec<String> = rd
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("Proton"))
        .collect();
    names.sort_by(|a, b| proton_rank(b).cmp(&proton_rank(a)));
    names.first().map(|n| base.join(n))
}
fn proton_rank(name: &str) -> u32 {
    if name.contains("Experimental") {
        return 1000;
    }
    let num: u32 = name
        .split(|c: char| !c.is_ascii_digit())
        .filter_map(|t| t.parse::<u32>().ok())
        .max()
        .unwrap_or(0);
    if num > 0 { num } else { 1 } // Hotfix 等无数字的排最后但仍在列
}

/// stderr 尾部 n 行合并为单行（状态栏友好；跳过 Python 堆栈行）
fn stderr_tail(stderr: &[u8], n: usize) -> String {
    let text = String::from_utf8_lossy(stderr);
    let lines: Vec<String> = text
        .lines()
        .filter(|l| {
            let t = l.trim_start();
            !t.is_empty() && !t.starts_with("File \"") && !t.starts_with("~~~") && !t.starts_with("\u{1b}")
        })
        .map(String::from)
        .collect();
    if lines.is_empty() {
        return "(无输出)".into();
    }
    let start = lines.len().saturating_sub(n);
    lines[start..].join(" | ").chars().take(200).collect()
}

/// 检查命令是否在 PATH 上（纯路径检查，不依赖 --version 支持）
fn which(cmd: &str) -> bool {
    std::env::var("PATH")
        .unwrap_or_default()
        .split(':')
        .any(|dir| std::path::Path::new(dir).join(cmd).is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_launch_config() {
        let cfg = LaunchConfig {
            exe_path: "/fake/path/Minecraft.Windows.exe".into(),
            proton_prefix: Some("/fake/prefix".into()),
            args: vec!["--arg1".into()],
        };
        assert_eq!(cfg.args.len(), 1);
    }

    #[test]
    fn stderr_tail_joins_last_lines() {
        let err = b"line1\nline2\nline3";
        assert_eq!(stderr_tail(err, 2), "line2 | line3");
        assert_eq!(stderr_tail(b"", 3), "(无输出)");
    }
}
