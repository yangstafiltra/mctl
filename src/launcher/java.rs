use std::path::PathBuf;
use std::process::Command;

/// 识别到的包管理器
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PkgManager {
    Pacman,
    Apt,
    Dnf,
    Zypper,
    Apk,
    /// 认不出（BSD / NixOS / 手工装的 JDK）
    Unknown,
}

/// 探测本机包管理器（按命令是否存在判断，不执行）
pub fn detect_pkg_manager() -> PkgManager {
    let has = |c: &str| {
        Command::new("sh")
            .args(["-c", &format!("command -v {c}")])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    };
    for (cmd, mgr) in [
        ("pacman", PkgManager::Pacman),
        ("apt-get", PkgManager::Apt),
        ("dnf", PkgManager::Dnf),
        ("zypper", PkgManager::Zypper),
        ("apk", PkgManager::Apk),
    ] {
        if has(cmd) {
            return mgr;
        }
    }
    PkgManager::Unknown
}

/// 各系统的 JDK 安装命令（OpenJDK，major 版本对齐）
pub fn install_hint(mgr: PkgManager, major: u32) -> Option<&'static str> {
    Some(match mgr {
        PkgManager::Pacman => {
            // Arch 的包名是 jdk21-openjdk 这类，不是 openjdk-21
            return Some(match major {
                8 => "sudo pacman -S jdk8-openjdk",
                11 => "sudo pacman -S jdk11-openjdk",
                17 => "sudo pacman -S jdk17-openjdk",
                21 => "sudo pacman -S jdk21-openjdk",
                25 => "sudo pacman -S jdk25-openjdk",
                _ => "sudo pacman -S jdk-openjdk",
            });
        }
        PkgManager::Apt => match major {
            8 => "sudo apt install openjdk-8-jdk",
            11 => "sudo apt install openjdk-11-jdk",
            17 => "sudo apt install openjdk-17-jdk",
            21 => "sudo apt install openjdk-21-jdk",
            25 => "sudo apt install openjdk-25-jdk",
            _ => "sudo apt install openjdk-21-jdk",
        },
        PkgManager::Dnf => match major {
            8 => "sudo dnf install java-1.8.0-openjdk-devel",
            11 => "sudo dnf install java-11-openjdk-devel",
            17 => "sudo dnf install java-17-openjdk-devel",
            21 => "sudo dnf install java-21-openjdk-devel",
            25 => "sudo dnf install java-25-openjdk-devel",
            _ => "sudo dnf install java-21-openjdk-devel",
        },
        PkgManager::Zypper => match major {
            8 => "sudo zypper install java-1.8.0-openjdk-devel",
            11 => "sudo zypper install java-11-openjdk-devel",
            17 => "sudo zypper install java-17-openjdk-devel",
            21 => "sudo zypper install java-21-openjdk-devel",
            25 => "sudo zypper install java-25-openjdk-devel",
            _ => "sudo zypper install java-21-openjdk-devel",
        },
        PkgManager::Apk => match major {
            8 => "sudo apk add openjdk8",
            11 => "sudo apk add openjdk11",
            17 => "sudo apk add openjdk17",
            21 => "sudo apk add openjdk21",
            _ => "sudo apk add openjdk21",
        },
        PkgManager::Unknown => return None,
    })
}

/// Java 缺失时的完整提示（含本机对应的安装命令）
/// ponytail: 只给命令让用户自己跑。mctl 不代跑 sudo——
/// 沿用 deps.rs 的安全模型（不碰 sudo 密码，密码交互交给用户在终端做）。
pub fn missing_java_message(required: u32) -> String {
    let mgr = detect_pkg_manager();
    let mut s = format!("未找到 Java {required}+");
    // 已装但版本不够（vs 一个都没装），提示不一样
    if find_any_java().is_some() {
        s.push_str("（已装的版本太旧）");
    }
    s.push_str("，启动 Minecraft 需要它");
    match install_hint(mgr, required) {
        Some(cmd) => s.push_str(&format!("\n  安装：{cmd}")),
        None => s.push_str("\n  未识别包管理器，请自行安装 OpenJDK {required} 或更高版本"),
    }
    s
}

/// 网易客户端各版本的官方 Java 矩阵（社区/官方一致）：
/// `<1.16 → 8`；`1.16–1.20.5 → 17`；`≥1.20.6 → 21`。
/// 输入是网易版本目录名（如 `1.8.9` / `1.12.2` / `1.16` / `1.20` / `1.21.8`）。
pub fn required_major_for(version: &str) -> u32 {
    let mut it = version.split('.');
    let a: u32 = it.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let b: u32 = it.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let c: u32 = it.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    if a == 1 && b < 16 {
        8
    } else if a == 1 && (b > 20 || (b == 20 && c >= 6)) {
        21
    } else if a > 1 {
        21
    } else {
        17
    }
}

/// 所选 Java 与版本的匹配结论（中文、可行动）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JavaFit {
    Ok,
    /// 能启动，但有兼容风险（提示，不阻断）
    Warn(String),
    /// 官方矩阵不支持（阻断）：旧版 Forge 在 Java 9+ 上会崩、新版客户端 JVM 太旧会报 class file 版本错误
    Unsupported(String),
}

/// 按矩阵检查「版本 → Java」。老版本**不能**跑新 Java（Forge 1.8.9/1.12.2 在 Java 9+ 会崩），
/// 所以这里不是简单的 `>=`。
pub fn check_java_fit(version: &str, actual: u32) -> JavaFit {
    let req = required_major_for(version);
    if actual < req {
        return JavaFit::Unsupported(format!(
            "{} 客户端需要 Java {}，当前 Java {} 太旧；升级后重试",
            version, req, actual
        ));
    }
    if req == 8 && actual > 8 {
        let hint = install_hint(detect_pkg_manager(), 8)
            .map(|c| format!("安装：{c}"))
            .unwrap_or_else(|| "请安装 OpenJDK/JRE 8".to_string());
        return JavaFit::Unsupported(format!(
            "{} 客户端需要 Java 8，当前 Java {} 过新（老版 Forge 在 Java 9+ 会崩）。{}",
            version, actual, hint
        ));
    }
    if req == 17 && actual > 17 {
        return JavaFit::Warn(format!(
            "{} 客户端官方矩阵是 Java 17，当前 Java {} 较新：能启动，若 mod/原生库报错请换 Java 17",
            version, actual
        ));
    }
    JavaFit::Ok
}

/// 系统上是否存在任意 java（用于区分「没装」和「版本不够」）
fn find_any_java() -> Option<PathBuf> {
    which_java().or_else(|| {
        std::fs::read_dir("/usr/lib/jvm")
            .ok()?
            .filter_map(|e| e.ok())
            .map(|e| e.path().join("bin/java"))
            .find(|p| p.is_file())
    })
}

/// 查找可用的 Java。
/// 优先 /usr/lib/jvm 下满足版本要求的 JDK，否则回退 PATH 上的 java。
pub fn find_java(required: u32) -> Option<PathBuf> {
    if let Some(jvm) = find_in_jvm_dir(required) {
        return Some(jvm);
    }
    if let Some(java) = which_java() {
        if let Some(major) = detect_major(&java) {
            if major >= required || required == 0 {
                return Some(java);
            }
        } else {
            // 无法探测版本时按可用处理
            return Some(java);
        }
    }
    None
}

fn find_in_jvm_dir(required: u32) -> Option<PathBuf> {
    let mut candidates: Vec<(u32, PathBuf)> = Vec::new();
    for entry in std::fs::read_dir("/usr/lib/jvm").ok()? {
        let Ok(entry) = entry else { continue };
        let dir = entry.path();
        let java = dir.join("bin/java");
        if !java.is_file() {
            continue;
        }
        if let Some(major) = detect_major(&java) {
            candidates.push((major, java));
        }
    }
    // 满足要求的最小版本优先，精确匹配更佳
    candidates.sort();
    candidates
        .into_iter()
        .filter(|(major, _)| *major >= required)
        .map(|(_, p)| p)
        .next()
}

fn which_java() -> Option<PathBuf> {
    let out = Command::new("which").arg("java").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if path.is_empty() {
        None
    } else {
        Some(PathBuf::from(path))
    }
}

/// 运行 `java -version` 解析大版本号（8 -> 8，21 -> 21）
pub fn detect_major(java: &PathBuf) -> Option<u32> {
    let out = Command::new(java).arg("-version").output().ok()?;
    let text = String::from_utf8_lossy(&out.stderr);
    let text = if text.is_empty() {
        String::from_utf8_lossy(&out.stdout)
    } else {
        text
    };
    parse_major(&text)
}

/// 从 `java -version` 输出里解析 major 版本号。
/// 纯函数（不 spawn 进程）才能测。
/// - `openjdk version "21.0.11" 2024-04-16` → 21
/// - `java version "1.8.0_392"` → 8 ← 首段是 1，不是真正的 major
fn parse_major(text: &str) -> Option<u32> {
    let line = text.lines().next()?;
    let quoted = line.split('"').nth(1)?;
    let first = quoted.split('.').next()?;
    let major: u32 = first.parse().ok()?;
    // Java 8 的版本串是 "1.8.0_392"，首段是 1，必须读第二段才是真正的 major。
    // 原实现直接取首段 → Java 8 被当成 Java 1，永远不满足任何要求。
    if major == 1 {
        return quoted.split('.').nth(1)?.parse::<u32>().ok();
    }
    Some(major)
}

#[cfg(test)]
mod tests {
    use super::{
        check_java_fit, install_hint, missing_java_message, parse_major, required_major_for,
        JavaFit,
    };

    /// 官方矩阵：<1.16→8；1.16–1.20.5→17；≥1.20.6→21
    #[test]
    fn required_java_matrix() {
        for (v, want) in [
            ("1.7.10", 8),
            ("1.8.9", 8),
            ("1.12.2", 8),
            ("1.15.2", 8),
            ("1.16", 17),
            ("1.18", 17),
            ("1.20", 17),
            ("1.20.5", 17),
            ("1.20.6", 21),
            ("1.20.7", 21),
            ("1.21", 21),
            ("1.21.8", 21),
        ] {
            assert_eq!(required_major_for(v), want, "矩阵不符: {v}");
        }
    }

    /// 老版本不能跑新 Java（Forge 1.8.9/1.12.2 在 Java 9+ 会崩）→ 必须阻断 + 给安装命令
    #[test]
    fn old_versions_must_not_run_new_java() {
        match check_java_fit("1.12.2", 21) {
            JavaFit::Unsupported(msg) => {
                assert!(msg.contains("Java 8"), "提示缺目标版本: {msg}");
                assert!(
                    msg.contains("sudo ") || msg.contains("OpenJDK"),
                    "提示不可行动: {msg}"
                );
            }
            other => panic!("1.12.2 + Java 21 应被阻断: {other:?}"),
        }
        assert!(matches!(
            check_java_fit("1.8.9", 17),
            JavaFit::Unsupported(_)
        ));
        assert_eq!(check_java_fit("1.8.9", 8), JavaFit::Ok);
    }

    /// 新版本需要 21；Java 17 太旧 → 阻断
    #[test]
    fn new_versions_need_21() {
        assert!(matches!(
            check_java_fit("1.21.8", 17),
            JavaFit::Unsupported(_)
        ));
        assert_eq!(check_java_fit("1.21.8", 21), JavaFit::Ok);
        assert_eq!(check_java_fit("1.20.6", 21), JavaFit::Ok);
    }

    /// 1.16–1.20.5 用 21 能跑但有风险 → 只提示
    #[test]
    fn middle_versions_warn_on_too_new() {
        match check_java_fit("1.20", 21) {
            JavaFit::Warn(msg) => assert!(msg.contains("Java 17"), "提示缺目标版本: {msg}"),
            other => panic!("1.20 + Java 21 应为告警: {other:?}"),
        }
        assert_eq!(check_java_fit("1.20", 17), JavaFit::Ok);
    }

    #[test]
    fn modern_java_major_from_first_segment() {
        assert_eq!(
            parse_major("openjdk version \"21.0.11\" 2024-04-16"),
            Some(21)
        );
        assert_eq!(
            parse_major("openjdk version \"17.0.20.1\" 2025-07-15"),
            Some(17)
        );
    }

    /// Java 8 报 "1.8.0_392"：首段是 1，必须取第二段。
    /// 回归：原实现返回 1，导致 jdk8 永远被判为「版本过低」。
    #[test]
    fn java8_version_one_dot_eight_is_major_8_not_1() {
        assert_eq!(parse_major("java version \"1.8.0_392\""), Some(8));
        assert_eq!(parse_major("java version \"1.8.0_452\""), Some(8));
    }

    #[test]
    fn garbled_input_returns_none() {
        assert_eq!(parse_major(""), None);
        assert_eq!(parse_major("no version here"), None);
    }

    /// 安装命令必须按包管理器给对应包名。Arch 的包名是 jdk21-openjdk
    /// 而不是 openjdk-21 —— 写错的话用户照抄会装不上。
    #[test]
    fn install_hint_per_distro() {
        use super::PkgManager::*;
        // Arch：jdk21-openjdk（不是 openjdk-21）
        assert_eq!(
            install_hint(Pacman, 21).unwrap(),
            "sudo pacman -S jdk21-openjdk"
        );
        assert_eq!(
            install_hint(Pacman, 17).unwrap(),
            "sudo pacman -S jdk17-openjdk"
        );
        assert_eq!(
            install_hint(Pacman, 8).unwrap(),
            "sudo pacman -S jdk8-openjdk"
        );
        // Debian 系：openjdk-21-jdk
        assert_eq!(
            install_hint(Apt, 21).unwrap(),
            "sudo apt install openjdk-21-jdk"
        );
        // Fedora 系：java-21-openjdk-devel
        assert_eq!(
            install_hint(Dnf, 21).unwrap(),
            "sudo dnf install java-21-openjdk-devel"
        );
        // 认不出的系统不给瞎写的命令
        assert_eq!(install_hint(Unknown, 21), None);
    }

    /// 每条命令都必须是 sudo 开头且含对应包管理器
    #[test]
    fn every_hint_is_runnable_sudo_command() {
        use super::PkgManager::*;
        for mgr in [Pacman, Apt, Dnf, Zypper, Apk] {
            for major in [8u32, 17, 21] {
                let cmd = install_hint(mgr, major).unwrap();
                assert!(cmd.starts_with("sudo "), "缺 sudo: {cmd}");
                let tool = match mgr {
                    Pacman => "pacman",
                    Apt => "apt",
                    Dnf => "dnf",
                    Zypper => "zypper",
                    Apk => "apk",
                    Unknown => unreachable!(),
                };
                assert!(cmd.contains(tool), "命令与包管理器不符: {cmd}");
            }
        }
    }

    /// 提示文案必须带版本号；认不出包管理器时不能瞎给命令
    #[test]
    fn message_mentions_required_version() {
        let msg = missing_java_message(21);
        assert!(msg.contains("Java 21"), "缺版本号: {msg}");
    }
}
