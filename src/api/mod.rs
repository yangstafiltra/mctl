use anyhow::{anyhow, Result};
use futures_util::StreamExt;
use reqwest::Client;
use serde_json::{json, Value};
use std::sync::OnceLock;
use std::time::Duration;

use crate::domain::{Category, Credentials};
use crate::token;

pub mod auth;
pub mod github;
pub mod littleskin;
pub mod mcserver;
pub const GATEWAY: &str = "https://x19apigatewayobt.nie.netease.com";
pub const CLIENT: &str = "https://x19mclobt.nie.netease.com";
pub const CORE: &str = "https://x19obtcore.nie.netease.com:8443";
/// 最新客户端版本（从 patchlist 解析，供登录鉴权使用）
pub const LATEST_VERSION: &str = "1.15.25.5792";

/// ponytail: 全局单例 Client，复用连接池，避免每次请求重建 TCP 连接
static CLIENT_SINGLETON: OnceLock<Client> = OnceLock::new();

pub fn client() -> Result<&'static Client> {
    Ok(CLIENT_SINGLETON.get_or_init(|| {
        Client::builder()
            .user_agent("WPFLauncher/0.0.0.0")
            .timeout(Duration::from_secs(30))
            .pool_max_idle_per_host(8)
            .build()
            .expect("reqwest Client 构建失败")
    }))
}

/// 网易网关统一响应壳 `{ code, message, details, entity/entities }`
#[derive(Debug, serde::Deserialize)]
#[allow(dead_code)]
pub struct WpfResponse {
    pub code: i64,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub details: String,
    #[serde(default)]
    pub entity: Option<Value>,
    #[serde(default)]
    pub entities: Option<Vec<Value>>,
}

/// POST 到指定网关，自动附加 user-token 签名头。
#[allow(dead_code)]
pub async fn post_json(
    base: &str,
    path: &str,
    body: &Value,
    cred: &Credentials,
) -> Result<WpfResponse> {
    let body_str = serde_json::to_string(body)?;
    let (user_id, user_token) = token::compute(path, &body_str, &cred.user_id, &cred.user_token);
    let resp = client()?
        .post(format!("{base}{path}"))
        .header("Content-Type", "application/json")
        .header("user-id", user_id)
        .header("user-token", user_token)
        .body(body_str)
        .send()
        .await?;
    let text = resp.text().await?;
    let parsed: WpfResponse = serde_json::from_str(&text).map_err(|e| {
        anyhow!(
            "解析响应失败: {e}\n原始: {}",
            crate::text::truncate(&text, 300)
        )
    })?;
    Ok(parsed)
}

/// POST 到 Core 网关（无 token 鉴权，明文 JSON，如 /login-otp）
pub async fn post_core_noauth(path: &str, body: &Value) -> Result<WpfResponse> {
    let text = client()?
        .post(format!("{CORE}{path}"))
        .header("Content-Type", "application/json")
        .body(serde_json::to_string(body)?)
        .send()
        .await?
        .text()
        .await?;
    let parsed: WpfResponse = serde_json::from_str(&text).map_err(|e| {
        anyhow!(
            "解析响应失败: {e}\n原始: {}",
            crate::text::truncate(&text, 300)
        )
    })?;
    Ok(parsed)
}

/// POST 原始字节到 Core 网关（AES 加密体，如 /authentication-otp）
pub async fn post_core_bytes(path: &str, body: &[u8]) -> Result<Vec<u8>> {
    let resp = client()?
        .post(format!("{CORE}{path}"))
        .header("Content-Type", "application/octet-stream")
        .body(body.to_vec())
        .send()
        .await?;
    Ok(resp.bytes().await?.to_vec())
}

/// 下载资源二进制到内存（封面图等小文件）
#[allow(dead_code)]
pub async fn get_bytes(url: &str) -> Result<Vec<u8>> {
    let resp = client()?
        .get(url)
        .timeout(Duration::from_secs(30))
        .send()
        .await?;
    Ok(resp.bytes().await?.to_vec())
}

/// 流式下载大文件到磁盘
#[allow(dead_code)]
pub async fn download_to_file(url: &str, dest: &str) -> Result<()> {
    let resp = client()?
        .get(url)
        .timeout(Duration::from_secs(600))
        .send()
        .await?;
    let mut file = tokio::fs::File::create(dest).await?;
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        use tokio::io::AsyncWriteExt;
        let chunk = chunk?;
        file.write_all(&chunk).await?;
    }
    Ok(())
}

/// 列出可用服务器（item_type=1）
#[allow(dead_code)]
pub async fn list_servers(cred: &Credentials, offset: i64, length: i64) -> Result<Vec<Value>> {
    let resp = post_json(
        GATEWAY,
        "/item/query/available",
        &json!({
            "available_mc_versions": [],
            "item_type": 1,
            "length": length,
            "offset": offset,
            "master_type_id": "2",
            "secondary_type_id": "",
            "channel_id": 11,
        }),
        cred,
    )
    .await?;
    if resp.code != 0 {
        return Err(anyhow!("列表获取失败: {} ({})", resp.message, resp.details));
    }
    Ok(resp.entities.unwrap_or_default())
}

/// 资源大厅分类查询（item_type=2, master_type_id 区分模组/地图/皮肤）
/// sort_type：0=默认（下载量主导） 2=最新发布
#[allow(dead_code)]
pub async fn list_category(
    cat: &Category,
    offset: i64,
    length: i64,
    cred: &Credentials,
    sort_type: i64,
) -> Result<Vec<Value>> {
    let resp = post_json(
        GATEWAY,
        "/item/query/available",
        &json!({
            "channel_id": 11,
            "is_has": true,
            "item_type": cat.item_type,
            "length": length,
            "offset": offset,
            "master_type_id": cat.master_type_id,
            "secondary_type_id": cat.secondary_type_id,
            "price_type": 3,
            "sort_type": sort_type,
        }),
        cred,
    )
    .await?;
    if resp.code != 0 {
        return Err(anyhow!("分类查询失败: {} ({})", resp.message, resp.details));
    }
    Ok(resp.entities.unwrap_or_default())
}

/// 资源搜索（item/query/search-by-keyword）
/// sort_type：1=相关度 2=最新发布
#[allow(dead_code)]
pub async fn search_category(
    cat: &Category,
    keyword: &str,
    offset: i64,
    length: i64,
    cred: &Credentials,
    sort_type: i64,
) -> Result<Vec<Value>> {
    let resp = post_json(
        GATEWAY,
        "/item/query/search-by-keyword",
        &json!({
            "is_has": true,
            "is_sync": 0,
            "item_type": cat.item_type,
            "keyword": keyword,
            "length": length,
            "master_type_id": cat.master_type_id,
            "offset": offset,
            "price_type": 3,
            "secondary_type_id": cat.secondary_type_id,
            "sort_type": sort_type,
            "year": 0,
        }),
        cred,
    )
    .await?;
    if resp.code != 0 {
        return Err(anyhow!("搜索失败: {} ({})", resp.message, resp.details));
    }
    Ok(resp.entities.unwrap_or_default())
}

/// 资源详情（含封面图 brief_image_urls）
#[allow(dead_code)]
pub async fn item_detail(entity_id: &str, cred: &Credentials) -> Result<crate::domain::ItemDetail> {
    let resp = post_json(
        GATEWAY,
        "/item-details/get_v2",
        &json!({"item_id": entity_id}),
        cred,
    )
    .await?;
    if resp.code != 0 {
        return Err(anyhow!("详情获取失败: {} ({})", resp.message, resp.details));
    }
    let ent = resp.entity.ok_or_else(|| anyhow!("详情无 entity"))?;
    serde_json::from_value(ent).map_err(|e| anyhow!("解析详情失败: {e}"))
}

/// 下载信息（含多版本 sub_entities）
#[allow(dead_code)]
pub async fn download_info(
    entity_id: &str,
    cred: &Credentials,
) -> Result<Vec<crate::domain::DownloadSub>> {
    let resp = post_json(
        CLIENT,
        "/user-item-download-v2",
        &json!({"item_id": entity_id, "length": 0, "offset": 0}),
        cred,
    )
    .await?;
    if resp.code != 0 {
        return Err(anyhow!(
            "下载信息获取失败: {} ({})",
            resp.message,
            resp.details
        ));
    }
    let ent = resp.entity.ok_or_else(|| anyhow!("下载信息无 entity"))?;
    let subs = ent
        .get("sub_entities")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    subs.into_iter()
        .map(|v| serde_json::from_value(v).map_err(|e| anyhow!("解析下载子项失败: {e}")))
        .collect()
}
