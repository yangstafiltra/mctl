use serde::{Deserialize, Serialize};

/// LittleSkin OAuth 应用登记信息。
/// LittleSkin 已于 2026 年停用传统 Yggdrasil 密码登录 API（实测全 404），
/// 现仅支持 OAuth 设备码流：需先在 https://littleskin.cn/user/oauth/manage
/// 创建 OAuth 应用（回调 URL 填 `https://open.littleskin.cn/oauth/callback`），
/// 再发邮件至 support@littlesk.in 申请「OAuth 设备代码流白名单」，
/// 审核通过后在此填入服务端分配的 client_id 即可启用登录。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct LsConfig {
    pub app_name: String,
    pub callback_url: String,
    /// 白名单审核通过后，LittleSkin 分配的客户端 ID（设备码流登录用）
    #[serde(default)]
    pub client_id: String,
    /// 最近登录的角色 UUID（重启后自动恢复头像用）
    #[serde(default)]
    pub uuid: String,
    /// 角色名（启动外置登录用）
    #[serde(default)]
    pub name: String,
    /// Minecraft 令牌（authlib-injector 外置登录用；过期重登）
    #[serde(default)]
    pub access_token: String,
}

/// OAuth 应用管理页（创建应用 + 查看 client_id）
pub const OAUTH_MANAGE_URL: &str = "https://littleskin.cn/user/oauth/manage";
/// 设备码流白名单要求的回调 URL
pub const OAUTH_CALLBACK_DEFAULT: &str = "https://open.littleskin.cn/oauth/callback";
/// 白名单申请邮箱
pub const SUPPORT_EMAIL: &str = "support@littlesk.in";
// ===== OAuth 设备码流 + Yggdrasil Connect（文档：manual.littleskin.cn） =====

use anyhow::{Context, Result};
use serde_json::Value;

/// 设备代码对（RFC 8628；LittleSkin 返回 snake_case 字段）
#[derive(Debug, Clone, serde::Deserialize)]
pub struct DeviceCodePair {
    pub user_code: String,
    pub device_code: String,
    #[allow(dead_code)]
    pub verification_uri: String,
    pub verification_uri_complete: String,
    #[serde(default = "default_expires")]
    pub expires_in: u64,
    #[serde(default = "default_interval")]
    pub interval: u64,
}

fn default_expires() -> u64 {
    300
}
fn default_interval() -> u64 {
    5
}

/// Yggdrasil 角色
#[derive(Debug, Clone)]
pub struct YgProfile {
    pub uuid: String,
    pub name: String,
}

/// 传统 Yggdrasil 登录响应（/api/yggdrasil/authserver/oauth 同构）
#[derive(Debug, Clone)]
pub struct McToken {
    pub access_token: String,
    #[allow(dead_code)]
    pub client_token: String,
    pub selected_profile: Option<YgProfile>,
}

/// Yggdrasil API 根（authlib-injector -javaagent 注入用）
pub const YGGDRASIL_ROOT: &str = "https://littleskin.cn/api/yggdrasil";

/// 下载 authlib-injector（已有则跳过），返回 jar 路径
pub async fn ensure_authlib_injector(mc_dir: &std::path::Path) -> Result<std::path::PathBuf> {
    let jar = mc_dir.join("authlib-injector.jar");
    if jar.is_file() {
        return Ok(jar);
    }
    let http = reqwest::Client::new();
    let url = "https://authlib-injector.yushi.moe/artifact/latest.json";
    let meta: Value = http
        .get(url)
        .send()
        .await
        .context("获取 authlib-injector 版本信息失败")?
        .error_for_status()
        .context("authlib-injector 版本接口被拒")?
        .json()
        .await
        .context("解析 authlib-injector 版本信息失败")?;
    let path = meta["download_url"]
        .as_str()
        .context("版本信息缺少 download_url")?;
    let sha = meta["sha256"].as_str().unwrap_or("");
    let bytes = http
        .get(path)
        .send()
        .await
        .context("下载 authlib-injector 失败")?
        .error_for_status()?
        .bytes()
        .await?;
    if !sha.is_empty() {
        use sha2::Digest;
        let digest = hex_encode(&sha2::Sha256::digest(&bytes));
        anyhow::ensure!(digest == sha, "authlib-injector sha256 校验失败");
    }
    std::fs::write(&jar, &bytes).context("写入 authlib-injector.jar 失败")?;
    Ok(jar)
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

const DEVICE_CODE_URL: &str = "https://open.littleskin.cn/oauth/device_code";
const TOKEN_URL: &str = "https://open.littleskin.cn/oauth/token";
const PROFILES_URL: &str =
    "https://littleskin.cn/api/yggdrasil/sessionserver/session/minecraft/profile";
const MC_TOKEN_URL: &str = "https://littleskin.cn/api/yggdrasil/authserver/oauth";

/// 启动器所需 scope：读角色档案 + 创建 Minecraft 令牌
pub const OAUTH_SCOPE: &str = "Yggdrasil.PlayerProfiles.Read Yggdrasil.MinecraftToken.Create";

/// 第 1 步：请求设备代码对（应用需在设备码流白名单内）
pub async fn request_device_code(
    http: &reqwest::Client,
    client_id: &str,
) -> Result<DeviceCodePair> {
    let resp = http
        .post(DEVICE_CODE_URL)
        .header("Accept", "application/json")
        .form(&[("client_id", client_id), ("scope", OAUTH_SCOPE)])
        .send()
        .await
        .context("请求设备代码失败")?;
    let status = resp.status();
    let body: Value = resp.json().await.context("解析设备代码响应失败")?;
    if !status.is_success() {
        let err = body["error"].as_str().unwrap_or("unknown");
        anyhow::bail!("设备代码请求被拒（{err}）：应用是否已加入白名单？");
    }
    serde_json::from_value(body.clone())
        .map_err(|e| anyhow::anyhow!("设备代码响应解析失败: {e}；原始响应: {body}"))
}

/// 第 3 步：轮询授权结果。Ok(Some(token))=完成；Ok(None)=等待用户；
/// Err=过期/被拒/网络错误。
pub async fn poll_token(
    http: &reqwest::Client,
    client_id: &str,
    device_code: &str,
) -> Result<Option<String>> {
    let resp = http
        .post(TOKEN_URL)
        .header("Accept", "application/json")
        .form(&[
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ("client_id", client_id),
            ("device_code", device_code),
        ])
        .send()
        .await
        .context("轮询令牌失败")?;
    let body: Value = resp.json().await.context("解析令牌响应失败")?;
    if let Some(err) = body["error"].as_str() {
        return match err {
            // 等待用户授权 / 轮询过快（按 interval 重试即可）
            "authorization_pending" | "slow_down" => Ok(None),
            other => anyhow::bail!("授权失败: {other}"),
        };
    }
    Ok(Some(
        body["access_token"]
            .as_str()
            .context("响应缺少 access_token")?
            .to_string(),
    ))
}

/// 第 4 步：获取用户名下所有角色的 Yggdrasil 档案
pub async fn fetch_profiles(http: &reqwest::Client, access_token: &str) -> Result<Vec<YgProfile>> {
    let resp = http
        .get(PROFILES_URL)
        .header("Accept", "application/json")
        .bearer_auth(access_token)
        .send()
        .await
        .context("获取角色档案失败")?
        .error_for_status()
        .context("获取角色档案被拒")?;
    let arr: Vec<Value> = resp.json().await.context("解析角色档案失败")?;
    Ok(arr
        .into_iter()
        .filter_map(|p| {
            Some(YgProfile {
                uuid: p["id"].as_str()?.to_string(),
                name: p["name"].as_str()?.to_string(),
            })
        })
        .collect())
}

/// 第 5 步：换取 Minecraft 令牌（响应与传统 Yggdrasil 登录一致）
pub async fn fetch_mc_token(
    http: &reqwest::Client,
    access_token: &str,
    uuid: &str,
) -> Result<McToken> {
    let resp = http
        .post(MC_TOKEN_URL)
        .header("Accept", "application/json")
        .bearer_auth(access_token)
        .json(&serde_json::json!({ "uuid": uuid }))
        .send()
        .await
        .context("获取 Minecraft 令牌失败")?
        .error_for_status()
        .context("获取 Minecraft 令牌被拒")?;
    let body: Value = resp.json().await.context("解析 Minecraft 令牌失败")?;
    Ok(McToken {
        access_token: body["accessToken"]
            .as_str()
            .context("缺少 accessToken")?
            .to_string(),
        client_token: body["clientToken"].as_str().unwrap_or("").to_string(),
        selected_profile: body["selectedProfile"].as_object().map(|p| YgProfile {
            uuid: p["id"].as_str().unwrap_or(uuid).to_string(),
            name: p["name"].as_str().unwrap_or("").to_string(),
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 真实端点连通性：假 client_id 不在白名单 → 应返回 invalid_client 类错误
    #[tokio::test]
    async fn device_code_rejects_unknown_client() {
        let http = reqwest::Client::new();
        let err = request_device_code(&http, "0000000000000000000")
            .await
            .expect_err("假 client_id 应被拒绝");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("invalid_client") || msg.contains("白名单"),
            "应提示 invalid_client/白名单: {msg}"
        );
    }
}
