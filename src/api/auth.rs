use anyhow::{anyhow, bail, Context, Result};
use md5::{Digest, Md5};
use reqwest::Client;
use serde_json::{json, Value};
use url::Url;

use crate::api::{post_core_bytes, post_core_noauth, LATEST_VERSION};
use crate::domain::Credentials;

/// 4399 OAuth 登录，返回网易 X19 的 userId + userToken。
/// 全程使用一个带 cookie 会话的 Client（模拟 C# NCom4399）。
#[allow(dead_code)]
/// 从 4399 登录页的 HTML 里抠出错误提示（`<p class="login_err_msg">…</p>`）。
/// 带对验证码之后，**失败响应是 HTML 而不是 JSON**——账号不存在、密码错误之类
/// 都渲染在登录页里。也有同名逻辑（`NCom4399.ExtractErrorTip`）。
fn extract_login_err(html: &str) -> Option<String> {
    let s = html.find("login_err_msg\">")? + "login_err_msg\">".len();
    let e = html[s..].find("</p>")? + s;
    let tip = html[s..e].trim();
    (!tip.is_empty()).then(|| tip.to_string())
}

/// 读响应原文，顺带拦掉 4399 的风控软拒绝。返回 `(status, 正文)`。
/// 软拒绝的特征是 HTTP 202 + `text/plain`「请稍后再试~」。已实测它来自
/// **没带验证码**：带对 `captcha_id` + 验证码后同一请求会正常返回 200
/// （成功是 JSON，密码错是 HTML）。所以这个分支现在的文案已按验证码语义改写。
async fn read_body(resp: reqwest::Response, what: &str) -> Result<(reqwest::StatusCode, String)> {
    let status = resp.status();
    let body = resp.text().await.context("读取响应体失败")?;
    let trimmed = body.trim();
    if status == reqwest::StatusCode::ACCEPTED
        || trimmed.contains("请稍再试")
        || trimmed.contains("请稍后再试")
    {
        bail!(
            "4399 拒绝了这次请求（HTTP {status}，正文 {trimmed:?}）。\n\
             在登录场景里，这个响应的含义是：**验证码没通过**——可能是没带\n\
             captcha_id / captcha，也可能是填错了。4399 对这两种情况故意返回同一个\n\
             含糊响应，所以**无法从响应区分是哪一种**（已实测：空值与错误值都是它）。\n\
             验证码是一次性的：填错就得重新登录换一张图。"
        );
    }
    let _ = what;
    Ok((status, body))
}

/// 读 JSON 响应，解析失败时带上 status / Content-Type / 正文。
/// 不要用 `Response::json`：它在解析失败时把响应体丢掉，只剩 serde 一句
/// "expected value at line 1 column 1"——完全看不出服务端返回了什么。
async fn read_json(resp: reqwest::Response, what: &str) -> Result<Value> {
    let (status, body) = read_body(resp, what).await?;
    serde_json::from_str(&body).map_err(|e| {
        let head: String = body.chars().take(500).collect();
        anyhow!("解析{what}失败: {e}\n  HTTP {status}\n  正文前 500 字节: {head:?}")
    })
}

#[cfg(test)]
mod tests {
    use super::{extract_login_err, new_captcha_id};

    /// captcha_id 必须是 96 个小写 hex 字符（实测）
    #[test]
    fn captcha_id_is_96_lowercase_hex() {
        for _ in 0..20 {
            let id = new_captcha_id();
            assert_eq!(id.len(), 96, "长度应为 96，实际 {}", id.len());
            assert!(
                id.chars()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
                "只应含小写 hex: {id}"
            );
        }
    }

    #[test]
    fn captcha_id_is_random() {
        let a = new_captcha_id();
        let b = new_captcha_id();
        assert_ne!(a, b, "两次生成不应相同");
    }

    /// 带对验证码后，账号不存在/密码错 返回的是 HTML，错误在这个标记里
    #[test]
    fn extract_login_err_reads_html_error_tip() {
        // 真实响应片段（2026-09-27 实测）
        let html = r#"<p class="login_err_msg">用户不存在</p><div>其它</div>"#;
        assert_eq!(extract_login_err(html).as_deref(), Some("用户不存在"));

        let html2 = "<p class=\"login_err_msg\">密码错误</p>";
        assert_eq!(extract_login_err(html2).as_deref(), Some("密码错误"));
    }
    #[test]
    fn extract_login_err_absent_or_empty_returns_none() {
        assert_eq!(extract_login_err("<html>正常页面</html>"), None);
        // 有标记但内容为空 → 不该返回空串，否则错误信息会变成 "4399 登录失败: "
        assert_eq!(extract_login_err(r#"<p class="login_err_msg"></p>"#), None);
        assert_eq!(
            extract_login_err(r#"<p class="login_err_msg">   </p>"#),
            None
        );
    }
}

const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) Chrome/120.0";
const OAUTH_CB: &str =
    "https://m.4399api.com/openapi/oauth-callback.html?gamekey=44770&game_key=115716";
const LOGIN_URL: &str =
    "https://ptlogin.4399.com/oauth2/loginAndAuthorize.do?channel=&sdk=op&sdk_version=3.14.5.577";

/// 生成 96 字符十六进制的 `captcha_id`。
/// 按实测行为：`Guid.NewGuid.ToString("N")` 拼三次，
/// 即 3 × 32 个 hex 字符。服务端只拿它当 key 存「这张图对应哪个答案」，内容随机即可。
fn new_captcha_id() -> String {
    use rand::RngCore;
    use std::fmt::Write;
    let mut b = [0u8; 48];
    rand::rng().fill_bytes(&mut b);
    let mut s = String::with_capacity(96);
    for x in b {
        let _ = write!(s, "{x:02x}");
    }
    s
}

/// 用系统默认图片管理器打开验证码图。
/// TUI 画不出图片，CLI 也就只能给个路径——既然图是刚生成的，顺手打开省一步。
/// 打不开不算错（无桌面环境 / 没装 xdg-open），返回 false 让调用方退回"请手动打开"。
fn open_in_viewer(path: &std::path::Path) -> bool {
    use std::process::{Command, Stdio};
    Command::new("xdg-open")
        .arg(path)
        // 图片管理器会独立运行，不能继承我们的 stdout/stderr
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .is_ok()
}

/// 4399 登录的中间态：验证码图已取到，等用户识别后调 [`finish_4399`]。
/// 必须持有 `Client`（cookie 会话）：`captcha_id` 与会话绑定，跨会话复用无效，
/// 所以这个结构不能只存字符串。
pub struct LoginChallenge {
    /// 验证码图片已保存到这里
    pub captcha_path: std::path::PathBuf,
    /// 是否已用系统默认图片管理器打开过（无桌面环境时会失败）
    pub opened: bool,
    pub username: String,
    password: String,
    client: Client,
    client_id: String,
    state: String,
    ref1: String,
    captcha_id: String,
}

/// 登录第一步：建会话 → 取 OAuth 参数 → **下载验证码图**。
/// 走到这里就绪，等用户把图里的字认出来传��� [`finish_4399`]。
pub async fn begin_4399(username: &str, password: &str) -> Result<LoginChallenge> {
    let client = Client::builder()
        .user_agent(UA)
        .cookie_store(true)
        .build()?;

    // 1. 建立会话
    client
        .get("https://ptlogin.4399.com/ptlogin/loginFrame.do")
        .header("Referer", "https://www.4399.com/")
        .send()
        .await
        .context("访问 4399 登录页失败")?;

    // 2. 拿 OAuth 参数 (client_id, state, ref)
    let resp = client
        .get(OAUTH_CB)
        .send()
        .await
        .context("获取 OAuth 参数失败")?;
    let cb = read_json(resp, "OAuth 参数").await?;
    let result = cb
        .get("result")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("OAuth 响应缺少 result"))?;
    let u = Url::parse(result)?;
    let params: std::collections::HashMap<String, String> = u.query_pairs().into_owned().collect();
    let client_id = params.get("client_id").context("缺少 client_id")?.clone();
    let state = params.get("state").context("缺少 state")?.clone();
    let ref1 = params.get("ref").context("缺少 ref")?.clone();

    // 3. 下载验证码图（关键：captcha_id 为空时 4399 会回 202「请稍后再试~」）
    let captcha_id = new_captcha_id();
    let resp = client
        .get(format!(
            "https://ptlogin.4399.com/ptlogin/captcha.do?captchaId={captcha_id}"
        ))
        .header("Referer", "https://ptlogin.4399.com/")
        .send()
        .await
        .context("获取 4399 验证码失败")?;
    let is_png = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.contains("png"));
    let bytes = resp.bytes().await.context("读取验证码图片失败")?;
    if bytes.is_empty() {
        bail!("4399 返回了空的验证码图");
    }
    let path = std::env::temp_dir().join(format!(
        "mctl-4399-captcha-{}.{}",
        &captcha_id[..8],
        if is_png { "png" } else { "jpg" }
    ));
    std::fs::write(&path, &bytes)
        .with_context(|| format!("保存验证码图到 {} 失败", path.display()))?;

    let opened = open_in_viewer(&path);
    Ok(LoginChallenge {
        captcha_path: path,
        opened,
        username: username.to_string(),
        password: password.to_string(),
        client,
        client_id,
        state,
        ref1,
        captcha_id,
    })
}

/// 登录第二步：带 `captcha_id` + 用户识别的验证码提交，然后走完 SAuth/AuthSession/otp。
pub async fn finish_4399(ch: &LoginChallenge, captcha: &str) -> Result<Credentials> {
    let captcha = captcha.trim();
    if captcha.is_empty() {
        bail!("验证码不能为空");
    }
    let resp = ch
        .client
        .post(LOGIN_URL)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .form(&[
            ("isInputRealname", "false"),
            ("isVaildRealname", "false"),
            ("sec", "0"),
            ("client_id", &ch.client_id),
            ("state", &ch.state),
            ("ref", &ch.ref1),
            ("response_type", "TOKEN"),
            ("scope", "basic"),
            ("bizId", "2100001792"),
            ("auth_action", "ORILOGIN"),
            ("redirect_uri", OAUTH_CB),
            ("username", &ch.username),
            ("password", &ch.password),
            ("captcha_id", &ch.captcha_id),
            ("captcha", captcha),
        ])
        .send()
        .await
        .context("4399 登录失败")?;
    let (status, body) = read_body(resp, "登录响应").await?;

    // 成功是 JSON；**失败是 HTML**（账号不存在 / 密码错 都渲染在登录页里）
    let resp: Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(_) => {
            if let Some(tip) = extract_login_err(&body) {
                bail!("4399 登录失败: {tip}（验证码已消耗，重新登录会取新图）");
            }
            let head: String = body.chars().take(300).collect();
            bail!("登录响应无法解析（HTTP {status}，既非 JSON 也无 login_err_msg）: {head:?}");
        }
    };
    let code = resp
        .get("code")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    if code != "100" {
        let msg = resp
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("未知错误");
        bail!("4399 登录失败: {msg}");
    }
    let r = resp.get("result").context("登录响应缺少 result")?;
    let uid = r
        .get("uid")
        .and_then(|v| v.as_u64())
        .context("缺少 uid")?
        .to_string();
    let session = r
        .get("state")
        .and_then(|v| v.as_str())
        .context("缺少 state")?
        .to_string();

    // 4. 构造 SAuth
    let sn = {
        let mut hasher = Md5::new();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        hasher.update(format!("{uid}:{now}").as_bytes());
        format!("{:x}", hasher.finalize())
    };
    let sauth = json!({
        "aim_info": "{\"aim\":\"127.0.0.1\",\"tz\":\"+0800\",\"tzid\":\"\",\"country\":\"CN\"}",
        "app_channel": "4399com",
        "client_login_sn": sn,
        "deviceid": sn,
        "gameid": "x19",
        "gas_token": "",
        "ip": "127.0.0.1",
        "login_channel": "4399com",
        "platform": "ad",
        "realname": "{\"realname_type\":\"0\"}",
        "sdk_version": "1.0.0",
        "sdkuid": uid,
        "sessionid": session,
        "source_platform": "ad",
        "timestamp": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_millis()
            .to_string(),
        "udid": sn,
        "userid": "",
    });
    let sauth_json = serde_json::to_string(&sauth)?;

    // 5. AuthSession (上报给网易 SDK)
    // // 这里**复用 4399 的 client**（带浏览器 UA）。曾改成不带 UA + 强制 HTTP/1.1
    // 去按实测行为 的 .NET HttpWrapper，理由是「UA 说是 Chrome 但 TLS 指纹不是」
    // 属于自动化特征——但那个理由的前提（实测）已被
    // 证伪：同样需要短信。前提不成立，改动也就没有依据，已回滚。
    let resp = ch
        .client
        .post("https://mgbsdk.matrix.netease.com/x19/sdk/uni_sauth")
        .header("Content-Type", "application/json")
        .body(sauth_json.clone())
        .send()
        .await
        .context("AuthSession 失败")?;
    let auth = read_json(resp, "AuthSession 响应").await?;
    let auth_code = auth
        .get("code")
        .and_then(|v| v.as_u64())
        .unwrap_or_default();
    if auth_code != 200 {
        let msg = auth
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("未知错误");
        // uni_sauth 只会回一句 "login abnormally"，看不出到底哪一项被拒。
        // 把整个响应带上——否则每次失败都只能猜。
        bail!("AuthSession 失败: {msg}\n  完整响应: {auth}");
    }
    let aid = auth
        .get("aid")
        .and_then(|v| v.as_u64())
        .unwrap_or_default()
        .to_string();

    // 6. login-otp (Core 网关, 无 token)
    let core = post_core_noauth("/login-otp", &json!({"sauth_json": sauth_json})).await?;
    let otp_entity = core
        .entity
        .as_ref()
        .ok_or_else(|| anyhow!("login-otp 无 entity"))?;
    let otp_token = otp_entity
        .get("otp_token")
        .and_then(|v| v.as_str())
        .context("缺少 otp_token")?;

    // 7. authentication-otp (AES 加密)
    let sa_data = json!({
        "os_name": "windows",
        "os_ver": "Microsoft Windows 11 专业版",
        "mac_addr": "00:11:22:33:44:55",
        "udid": format!("0000000000000000{}", &sn[..4]),
        "app_ver": LATEST_VERSION,
        "sdk_ver": "",
        "network": "",
        "disk": &sn[..4],
        "is64bit": "1",
        "video_card1": "Microsoft Hyper-V 视频",
        "video_card2": "",
        "video_card3": "",
        "video_card4": "",
        "launcher_type": "PC_java",
        "pay_channel": "4399com",
        "dotnet_ver": "4.8.0",
        "cpu_type": "Intel(R) Core(TM) i7",
        "ram_size": "8192",
        "device_width": "1920",
        "device_height": "1080",
        "os_detail": "10.0.26100",
    });
    let value = json!({
        "sa_data": serde_json::to_string(&sa_data)?,
        "sauth_json": sauth_json,
        "version": { "version": LATEST_VERSION },
        "sdkuid": uid,
        "aid": aid,
        "has_message": false,
        "has_gmail": false,
        "otp_token": otp_token,
        "otp_pwd": Value::Null,
        "lock_time": 0,
        "env": "",
        "min_engine_version": "",
        "min_patch_version": "",
        "verify_status": 0,
        "unisdk_login_json": Value::Null,
        "token": Value::Null,
    });
    let encrypted = crate::crypto::encrypt(serde_json::to_string(&value)?.as_bytes());
    let raw = post_core_bytes("/authentication-otp", &encrypted).await?;
    let decrypted = crate::crypto::decrypt(&raw)?;
    let auth_resp: Value =
        serde_json::from_slice(&decrypted).context("解析 authentication-otp 响应失败")?;
    let ent = auth_resp
        .get("entity")
        .context("authentication-otp 无 entity")?;
    let user_id = ent
        .get("entity_id")
        .and_then(|v| v.as_str())
        .context("缺少 entity_id")?
        .to_string();
    let user_token = ent
        .get("token")
        .and_then(|v| v.as_str())
        .context("缺少 token")?
        .to_string();
    // 官方登录响应里的 `sead` 是服务端下发的 seed（官方解析器逐字使用该键名）。
    // 第三方实现并未把它当 crc_salt，因此这里只作为首选候选；失败会回退旧候选表。
    let sead = ent
        .get("sead")
        .and_then(|v| v.as_str())
        .map(str::to_string);

    Ok(Credentials {
        user_id,
        user_token,
        aid,
        sdkuid: uid,
        sead,
    })
}
