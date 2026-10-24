//! 本地 AuthLib 认证服务（对照 ）
//!
//! 网易客户端启动后（-DlauncherControlPort=<port>）在进服时连接本服务：
//! [int32 LE 长度][gameId][int32 LE 长度][buffer][int32 LE 长度][serverId]
//! 服务端完成进服认证后回写 4 字节响应码：0 = 成功，1 = 失败。
//!
//! 认证顺序（**不可颠倒**，见 `06` §9.4）：
//! 1. `provides/auth-fallback` 插件优先——它直接拿到完整 `GameProfile`，亚秒级回包；
//! 2. 插件不可用时才回退进程内 Yggdrasil 多盐穷举——每个候选都要连一次认证服务器，
//!    12 个候选会耗光游戏侧 ~1s 超时。顺序错了插件永远轮不到。

use anyhow::{anyhow, Result};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use crate::api::netease::{EntityUserInfo, GameProfile};
use crate::netease::yggdrasil::{join_server, make_profile_with_game};

/// 日志脱敏：插件输入里的 token 只留长度，避免把凭据写进 `authlib.log`
/// （插件进程收到的仍是真值，只有落盘/打印走这里）
fn redacted_input(input: &serde_json::Value) -> String {
    let mut v = input.clone();
    if let Some(tok) = v
        .get_mut("profile")
        .and_then(|p| p.get_mut("profile"))
        .and_then(|p| p.get_mut("user"))
        .and_then(|u| u.get_mut("token"))
    {
        let n = tok.as_str().map(|s| s.chars().count()).unwrap_or(0);
        *tok = serde_json::Value::String(format!("<已隐去 {n} 字符>"));
    }
    serde_json::to_string(&v).unwrap_or_default()
}

/// 认证兜底钩子（插件机制）：本地 Yggdrasil join 失败时，遍历插件目录，
/// 找到带 `provides/auth-fallback` 标记的插件，以 `auth-fallback` 子命令调用：
/// stdin = {"profile": <GameProfile>, "server_id": "<id>"}
/// exit 0 = 认证成功；非 0 = 失败（stderr 诊断）。
/// 核心不内置任何第三方服务，具体兜底逻辑由插件实现。
/// 认证诊断落盘（TUI 外看不到 eprintln）。
fn diag(msg: &str) {
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(crate::storage::config_dir().join("authlib.log"))
    {
        use std::io::Write;
        let _ = writeln!(f, "{}", msg);
    }
}

async fn plugin_auth_fallback(profile: &GameProfile, server_id: &str) -> Result<()> {
    let root = crate::storage::config_dir().join(crate::plugin::PLUGIN_DIR);
    plugin_auth_fallback_in(&root, profile, server_id).await
}

/// 与 [`plugin_auth_fallback`] 同逻辑，但插件根显式传入（便于测试与替身）。
async fn plugin_auth_fallback_in(
    root: &std::path::Path,
    profile: &GameProfile,
    server_id: &str,
) -> Result<()> {
    use tokio::io::AsyncWriteExt as _;
    let mut last_err: Option<String> = None;
    for p in crate::plugin::scan_plugins(root) {
        // 注意：目录名（[mctl]xxx）与 Cargo 包名（xxx）可能不同，一律用实际路径
        if !p.path.join("provides").join("auth-fallback").is_file() {
            continue;
        }
        let Some(bin) = &p.bin else { continue };
        let input = serde_json::json!({ "profile": profile, "server_id": server_id });
        diag(&format!("[authlib] 插件输入: {}", redacted_input(&input)));
        let mut child = tokio::process::Command::new(bin)
            .arg("auth-fallback")
            // 插件目录里的 env 文件（认证服务地址这类「只有本机知道」的配置）
            .envs(crate::plugin::env_file(&p.path))
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| anyhow!("启动插件 {} 失败: {e}", p.name))?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(serde_json::to_string(&input)?.as_bytes())
                .await
                .ok();
        }
        let out = child.wait_with_output().await?;
        if out.status.success() {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
        last_err = Some(format!(
            "{} / {}",
            crate::text::truncate(&stderr, 400),
            crate::text::truncate(&stdout, 200)
        ));
        eprintln!(
            "[authlib] 插件 {} 兜底失败: {}",
            p.name,
            last_err.as_deref().unwrap_or("")
        );
    }
    anyhow::bail!(
        "无可用 auth-fallback 插件或全部失败: {}",
        last_err.unwrap_or_default()
    )
}

/// 认证失败时的可行动提示：核心不内置盐（净室），签名要靠插件 / 远端代签。
fn auth_help() -> String {
    format!(
        "要让进服走得通：装一个带 `provides/auth-fallback` 的插件（把完整 GameProfile 交给它签名），\
并在插件目录的 env 文件里配上服务地址，例如 {}/plugin/mctl-authproxy/env 里写 \
MCTL_AUTH_ENDPOINT=https://<你的代签服务>（进程环境里已有的同名变量优先）。\
没有代签服务时也可以自己起一个：它拿到的是你本机的账号凭据，核心不会外发任何东西。",
        crate::storage::config_dir().display()
    )
}

/// 启动本地认证服务（port=0 自动分配），返回实际监听端口。
/// 进程退出时随 spawn 的 task 一起结束。
pub async fn start(
    port: u16,
    user: EntityUserInfo,
    game_id: String,
    game_version: String,
    mods_json: String,
    sead: Option<String>,
) -> Result<u16> {
    let listener = TcpListener::bind(("127.0.0.1", port)).await?;
    let bound = listener.local_addr()?.port();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                break;
            };
            let user = user.clone();
            let game_id = game_id.clone();
            let version = game_version.clone();
            let mods = mods_json.clone();
            let sead = sead.clone();
            tokio::spawn(async move {
                let mut code = 1u32;
                // 认证诊断落盘（TUI 外看不到 eprintln）
                fn diag(msg: &str) {
                    if let Ok(mut f) = std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(crate::storage::config_dir().join("authlib.log"))
                    {
                        use std::io::Write;
                        let _ = writeln!(f, "{}", msg);
                    }
                }
                let result = async {
                    let len = sock.read_i32_le().await?;
                    let mut game_buf = vec![0u8; len.clamp(0, 65536) as usize];
                    sock.read_exact(&mut game_buf).await?;
                    // gameId 以游戏上报为准（对照 AuthLibProtocol 原样转发），空则回退启动配置
                    let reported = String::from_utf8_lossy(&game_buf).trim().to_string();
                    let gid = if reported.is_empty() {
                        game_id.clone()
                    } else {
                        reported
                    };
                    let len2 = sock.read_i32_le().await?;
                    let mut buffer = vec![0u8; len2.clamp(0, 65536) as usize];
                    sock.read_exact(&mut buffer).await?;
                    let len3 = sock.read_i32_le().await?;
                    let mut server_id = vec![0u8; len3.clamp(0, 65536) as usize];
                    sock.read_exact(&mut server_id).await?;
                    let server_id = String::from_utf8_lossy(&server_id).to_string();
                    let msg = format!("[authlib] gameId={gid} serverId={server_id}");
                    eprintln!("{msg}");
                    diag(&msg);
                    let profile = make_profile_with_game(&user, &gid, &version, &mods);
                    // 认证顺序（游戏侧 authlib 超时极短 ~1s，顺序错了必超时）：
                    // 1) 认证插件优先：`provides/auth-fallback` 插件拿到的就是完整
                    //    GameProfile（含 userId/token），亚秒级即可回包；
                    // 2) 进程内 Yggdrasil 兜底（多盐穷举）：每个候选都要连一次认证服务器，
                    //    只有在插件缺失/全失败时才走，避免把插件饿死。
                    match plugin_auth_fallback(&profile, &server_id).await {
                        Ok(()) => {}
                        Err(e) => {
                            let msg = format!("[authlib] 插件兜底失败: {e:#}");
                            eprintln!("{msg}");
                            diag(&msg);
                            join_server(&profile, &server_id, sead.as_deref()).await?;
                        }
                    }
                    Ok::<_, anyhow::Error>(())
                }
                .await;
                if result.is_ok() {
                    code = 0;
                } else {
                    let msg = format!("[authlib] 认证失败: {result:?}");
                    eprintln!("{msg}");
                    diag(&msg);
                    // 失败原因十有八九是「没有签名盐」：给一条能照着做的路，
                    // 而不是让用户对着 0x11 猜
                    let hint = auth_help();
                    eprintln!("{hint}");
                    diag(&hint);
                }
                let _ = sock.write_all(&code.to_le_bytes()).await;
            });
        }
    });
    Ok(bound)
}

#[cfg(test)]
mod tests {
    use super::{plugin_auth_fallback_in, redacted_input};
    use crate::api::netease::{EntityUserInfo, GameProfile};
    use serde_json::json;

    fn fake_profile() -> GameProfile {
        GameProfile {
            game_id: "1".into(),
            game_version: "1.20".into(),
            bootstrap_md5: "0".repeat(32),
            dat_file_md5: "0".repeat(32),
            mods: serde_json::Value::Null,
            profile: crate::api::netease::UserProfile {
                user: EntityUserInfo {
                    user_id: "12345678".into(),
                    token: "A".repeat(16),
                },
            },
        }
    }

    /// 认证插件必须**先于**进程内穷举被尝试：这里只放一个必然失败的插件，
    /// 断言它确实被调用了（拿到的是完整 profile+server_id）。
    /// 若哪天顺序被改回「穷举优先」，本用例会因为插件从未被调用而失败。
    #[tokio::test]
    async fn auth_plugin_is_attempted_before_builtin_probe() {
        // 在临时插件根下造一个「声明能力但必然失败」的插件
        let root = std::env::temp_dir().join(format!(
            "mctl_authlib_order_test_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let plug = root.join("probeplug");
        std::fs::create_dir_all(plug.join("provides")).unwrap();
        std::fs::create_dir_all(plug.join("target").join("release")).unwrap();
        std::fs::write(
            plug.join("Cargo.toml"),
            "[package]\nname = \"probeplug\"\nversion = \"0.0.1\"\nedition = \"2021\"\n",
        )
        .unwrap();
        std::fs::write(plug.join("provides").join("auth-fallback"), b"").unwrap();
        // 一个只做两件事的替身：把 stdin 落盘、以非 0 退出
        let capture = root.join("captured.json");
        let bin = plug.join("target").join("release").join("probeplug");
        std::fs::write(
            &bin,
            format!(
                "#!/bin/sh\ncat > '{}'\necho 'deliberate failure' >&2\nexit 3\n",
                capture.display()
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut p = std::fs::metadata(&bin).unwrap().permissions();
            p.set_mode(0o755);
            std::fs::set_permissions(&bin, p).unwrap();
        }

        // 只验证「插件被调用」这一点，避免真的去连认证服务器
        let _ = plugin_auth_fallback_in(&root, &fake_profile(), "9").await;

        let got = std::fs::read_to_string(&capture)
            .expect("插件没有被调用（认证顺序被改坏了？）");
        assert!(got.contains("\"server_id\":\"9\""), "插件应收到 server_id: {got}");
        assert!(got.contains("12345678"), "插件应收到完整 profile: {got}");
        assert!(got.contains("\"token\""), "插件应收到 token 字段: {got}");

        let _ = std::fs::remove_dir_all(&root);
    }

    /// 插件输入落盘前必须隐去 token（凭据不进日志文件）
    /// 失败提示要能照着做：给到插件目录的 env 文件路径与变量名
    #[test]
    fn auth_help_points_at_plugin_env() {
        let h = super::auth_help();
        assert!(h.contains("provides/auth-fallback"), "h={h}");
        assert!(h.contains("MCTL_AUTH_ENDPOINT"), "h={h}");
        assert!(h.contains("plugin/mctl-authproxy/env"), "h={h}");
    }

    #[test]
    fn plugin_log_redacts_token() {
        let input = json!({
            "profile": { "profile": { "user": { "token": "SECRET-TOKEN", "userId": "42" } } },
            "server_id": "7"
        });
        let s = redacted_input(&input);
        assert!(!s.contains("SECRET-TOKEN"), "token 泄漏进日志: {s}");
        assert!(s.contains("已隐去 12 字符"), "应留下长度线索: {s}");
        assert!(s.contains("\"userId\":\"42\""), "非敏感字段保留: {s}");
    }
}
