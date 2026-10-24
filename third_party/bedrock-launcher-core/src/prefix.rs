//! prefix 准备：注册表 prereqs + GameInput 原生 redist + 代理。
//!
//! 对照 BedrockOnLinux wine_apply_winegdk_prereqs / install_gameinput：
//! - TLS 1.2 强制（Azure 拒绝 Wine GnuTLS 的 TLS 1.3 → 身份服务全断 → 40% 卡死）
//! - ConsoleMode=8（XSAPI 代码路径）
//! - cryptbase/GameInput 原生 redist（wine 内置 gameinput 无鼠标后端）
//! - Windows 代理写入（wine 游戏不走系统代理）

use anyhow::Result;
use std::path::{Path, PathBuf};

/// 代理地址（环境变量 MCTL_PROXY 可覆盖；clash 默认 7890）
fn proxy_addr() -> String {
    std::env::var("MCTL_PROXY").unwrap_or_else(|_| "127.0.0.1:7890".into())
}

/// 启动前准备（幂等）：注册表 prereqs + GameInput。game_dir = 提取的游戏目录。
pub fn prepare(prefix: &Path, game_dir: &Path) -> Result<()> {
    let proxy = proxy_addr();
    reg_ensure(
        prefix, "system.reg",
        &format!("Software\\\\Microsoft\\\\Windows NT\\\\CurrentVersion\\\\OEM"),
        &[("ConsoleMode", RegKind::Dword(8))],
    );
    reg_ensure(
        prefix, "system.reg",
        &format!("Software\\\\Microsoft\\\\WindowsRuntime\\\\ActivatableClassId\\\\Microsoft.Windows.Storage.Pickers.FileOpenPicker"),
        &[("DllPath", RegKind::Sz("C:\\\\windows\\\\system32\\\\windows.storage.dll"))],
    );
    reg_ensure(
        prefix, "system.reg",
        &format!("Software\\\\Microsoft\\\\Windows\\\\CurrentVersion\\\\Internet Settings\\\\WinHttp"),
        &[("DefaultSecureProtocols", RegKind::Dword(0xa00))],
    );
    reg_ensure(
        prefix, "system.reg",
        &format!("Software\\\\Microsoft\\\\SchannelTLS\\\\Protocols\\\\TLS 1.3\\\\Client"),
        &[("DisabledByDefault", RegKind::Dword(1))],
    );
    reg_ensure(
        prefix, "user.reg",
        "Environment",
        &[
            ("MICROSOFT_WINDOWSAPPRUNTIME_BOOTSTRAP_INITIALIZE_SHOWUI", RegKind::Sz("0")),
            ("MICROSOFT_WINDOWSAPPRUNTIME_BOOTSTRAP_INITIALIZE_FAILFAST", RegKind::Sz("0")),
            ("MICROSOFT_WINDOWSAPPRUNTIME_DEPLOYMENT_INITIALIZE_ONERRORSHOWUI", RegKind::Sz("0")),
        ],
    );
    let is = format!("Software\\\\Microsoft\\\\Windows\\\\CurrentVersion\\\\Internet Settings");
    reg_ensure(prefix, "user.reg", &is, &[
        ("ProxyEnable", RegKind::Dword(1)),
        ("ProxyServer", RegKind::Sz(&proxy)),
    ]);
    // GameInput 原生 redist（失败不致命：游戏能跑但鼠标不可用）
    if let Err(e) = install_gameinput(prefix, game_dir) {
        eprintln!("[bedrock] GameInput 安装跳过: {e:#}");
    }
    Ok(())
}

// ---- 注册表离线编辑（wine .reg 文本格式）----

enum RegKind<'a> {
    Dword(u32),
    Sz(&'a str),
}

fn reg_line(name: &str, k: &RegKind) -> String {
    match k {
        RegKind::Dword(v) => format!("\"{name}\"=dword:{v:08x}"),
        RegKind::Sz(v) => format!("\"{name}\"=\"{v}\""),
    }
}

/// 确保 [key] 段含全部值：存在则补缺，不存在则追加新段
fn reg_ensure(prefix: &Path, regfile: &str, key: &str, entries: &[(&str, RegKind)]) {
    let path = prefix.join(regfile);
    let Ok(text) = std::fs::read_to_string(&path) else { return };
    let header = format!("[{key}]");
    let value_lines: Vec<String> = entries.iter().map(|(n, k)| reg_line(n, k)).collect();

    let mut out: Vec<String> = Vec::new();
    let mut done = false;
    if text.contains(&header) {
        let lines: Vec<&str> = text.lines().collect();
        let mut i = 0;
        while i < lines.len() {
            out.push(lines[i].to_string());
            if lines[i].starts_with(&header) {
                i += 1;
                while i < lines.len() && !lines[i].starts_with('[') {
                    out.push(lines[i].to_string());
                    i += 1;
                }
                for (n, k) in entries {
                    let name_eq = format!("\"{n}\"=");
                    if !out[i..].iter().any(|l| l.starts_with(&name_eq)) {
                        out.push(reg_line(n, k));
                    }
                }
                done = true;
                continue;
            }
            i += 1;
        }
    }
    let new_text = if done {
        out.join("\n") + "\n"
    } else {
        format!("{text}\n\n{header} {}/{}\n", chrono_stamp(), value_lines.join("\n"))
    };
    let _ = std::fs::write(&path, new_text);
}

fn chrono_stamp() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs().to_string())
        .unwrap_or_else(|_| "0".into())
}

// ---- GameInput 原生 redist ----

fn gameinput_ok(prefix: &Path) -> bool {
    prefix
        .join("drive_c/Program Files/Microsoft GameInput/x64/GameInputRedist.dll")
        .is_file()
}

fn find_7zz() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("MCTL_7ZZ") {
        if Path::new(&p).is_file() {
            return Some(PathBuf::from(p));
        }
    }
    if let Ok(path) = std::env::var("PATH") {
        for dir in path.split(':') {
            for c in ["7zz", "7z"] {
                let p = Path::new(dir).join(c);
                if p.is_file() {
                    return Some(p);
                }
            }
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        let p = Path::new(&home).join("Fantnel.linux.x64/fantnel/resources/7zz");
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

/// 从游戏包 Installers/GameInputRedist.msi 提取原生 redist 并写注册表。
/// 需要 7zz/7z（两层解包：MSI 流 → PE）。缺工具/MSI 时静默跳过（游戏能跑，鼠标不可用）。
pub fn install_gameinput(prefix: &Path, game_dir: &Path) -> Result<()> {
    if gameinput_ok(prefix) {
        return Ok(());
    }
    let msi = game_dir.join("Installers").join("GameInputRedist.msi");
    if !msi.is_file() {
        eprintln!("[bedrock] GameInputRedist.msi 缺失，跳过 GameInput 安装");
        return Ok(());
    }
    let Some(sz) = find_7zz() else {
        eprintln!("[bedrock] 未找到 7zz/7z，跳过 GameInput 安装（鼠标将不可用）");
        return Ok(());
    };
    let tmp = std::env::temp_dir().join("mctl-gameinput");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp)?;
    let st = std::process::Command::new(&sz)
        .arg("x")
        .arg(&msi)
        .arg(&format!("-o{}", tmp.display()))
        .arg("-y")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()?;
    if !st.success() {
        anyhow::bail!("MSI 解包失败");
    }

    // 收集 PE（MZ + PE 解析），按 BOL 大小排名放置
    let mut dlls: Vec<(usize, Vec<u8>)> = Vec::new();
    let mut exes: Vec<(usize, Vec<u8>)> = Vec::new();
    for e in std::fs::read_dir(&tmp)?.flatten() {
        let p = e.path();
        let Ok(d) = std::fs::read(&p) else { continue };
        if d.len() < 0x40 || &d[..2] != b"MZ" {
            continue;
        }
        let pe = u32_at(&d, 0x3C) as usize;
        if pe + 24 > d.len() || &d[pe..pe + 4] != b"PE\0\0" {
            continue;
        }
        let chars = u16_at(&d, pe + 22);
        let kind = if chars & 0x2000 != 0 { "dll" } else { "exe" };
        match kind {
            "dll" => dlls.push((d.len(), d)),
            _ => exes.push((d.len(), d)),
        }
    }
    dlls.sort_by(|a, b| b.0.cmp(&a.0));
    exes.sort_by(|a, b| b.0.cmp(&a.0));
    if dlls.is_empty() || exes.is_empty() {
        anyhow::bail!("MSI 内未识别出 PE 载荷");
    }

    let x64 = prefix.join("drive_c/Program Files/Microsoft GameInput/x64");
    let sys32 = prefix.join("drive_c/windows/system32");
    std::fs::create_dir_all(&x64)?;
    std::fs::write(x64.join("GameInputRedist.dll"), &dlls[0].1)?;
    std::fs::write(sys32.join("GameInputRedist.dll"), &dlls[0].1)?;
    std::fs::write(x64.join("GameInputRedistService.exe"), &exes[0].1)?;
    if let Some((_, d)) = dlls.get(1) {
        std::fs::write(x64.join("GameInputBridge.dll"), d)?;
    }
    if let Some((_, d)) = exes.get(1) {
        std::fs::write(x64.join("GameInputRawInputProxy.exe"), d)?;
    }

    // 注册表：RedistDir（双视图）+ 服务项
    let redist = "C:\\\\Program Files\\\\Microsoft GameInput\\\\x64";
    let service = "System\\\\CurrentControlSet\\\\Services\\\\GameInputRedistService";
    reg_ensure(prefix, "system.reg", &format!("Software\\\\Microsoft\\\\GameInput"),
        &[("RedistDir", RegKind::Sz(redist))]);
    reg_ensure(prefix, "system.reg", &format!("Software\\\\Wow6432Node\\\\Microsoft\\\\GameInput"),
        &[("RedistDir", RegKind::Sz(redist))]);
    reg_ensure(prefix, "system.reg", service, &[
        ("DisplayName", RegKind::Sz("GameInput Redist Service")),
        ("ImagePath", RegKind::Sz(&format!("{redist}\\\\GameInputRedistService.exe"))),
        ("ObjectName", RegKind::Sz("LocalSystem")),
        ("ErrorControl", RegKind::Dword(0)),
        ("Start", RegKind::Dword(3)),
        ("Type", RegKind::Dword(0x10)),
    ]);
    let _ = std::fs::remove_dir_all(&tmp);
    eprintln!("[bedrock] GameInput 原生 redist 已安装（鼠标可用）");
    Ok(())
}

fn u32_at(d: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([d[off], d[off + 1], d[off + 2], d[off + 3]])
}
fn u16_at(d: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([d[off], d[off + 1]])
}
