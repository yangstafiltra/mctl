//! 系统依赖检查与安装（插件 deps/<管理器>.txt 约定的执行端）。
//!
//! 安全模型：mctl 不接触 sudo 密码——直接 spawn `sudo pacman -S`（继承 stdio，
//! 密码提示由 sudo 自己在终端处理）；安装前列出包名要求确认。

use anyhow::{bail, Context, Result};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pacman_t_detects_missing_only() {
        // 已安装的包（pacman 自己）不应出现在缺失列表；假包应出现
        let missing = missing_pacman(&["pacman", "definitely-not-a-real-pkg-xyz"]);
        assert!(!missing.iter().any(|p| p == "pacman"));
        assert!(missing.iter().any(|p| p == "definitely-not-a-real-pkg-xyz"));
    }
}

/// pacman 缺失的包（`pacman -T` 安静列出缺失项，退出码非零）
pub fn missing_pacman(pkgs: &[&str]) -> Vec<String> {
    let Ok(out) = std::process::Command::new("pacman")
        .arg("-T")
        .args(pkgs)
        .output()
    else {
        return Vec::new(); // 非 Arch / 无 pacman → 视为不缺，运行时自会报错
    };
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .map(String::from)
        .collect()
}

/// 交互安装（继承 stdio，sudo 自行提示密码）
pub fn install_pacman_interactive(pkgs: &[&str]) -> Result<()> {
    use std::io::Write;
    println!("需要安装系统依赖: {}", pkgs.join(" "));
    print!("回车继续（sudo 可能要求密码），Ctrl+C 取消…");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    let st = std::process::Command::new("sudo")
        .arg("pacman")
        .arg("-S")
        .arg("--needed")
        .args(pkgs)
        .status()
        .context("无法执行 sudo pacman（需要 pacman 与 sudo）")?;
    if !st.success() {
        bail!("pacman 安装失败（退出码 {:?}）", st.code());
    }
    Ok(())
}

/// 缺则装，不缺直接过
pub fn ensure_pacman_interactive(pkgs: &[&str]) -> Result<()> {
    let missing = missing_pacman(pkgs);
    if missing.is_empty() {
        return Ok(());
    }
    install_pacman_interactive(&missing.iter().map(String::as_str).collect::<Vec<_>>())
}
