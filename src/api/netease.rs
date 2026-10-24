use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::api::{client, CLIENT, CORE, GATEWAY};
use crate::token;

/// 网易网关统一响应壳
#[derive(Debug, Deserialize)]
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

/// 带签名头 POST（`user-token` 签名算法见 `crate::token`，已用实网请求核对）
pub async fn post_signed(
    base: &str,
    path: &str,
    body: &Value,
    user_id: &str,
    user_token: &str,
) -> Result<WpfResponse> {
    let body_str = serde_json::to_string(body)?;
    let (uid, tok) = token::compute(path, &body_str, user_id, user_token);
    let resp = client()?
        .post(format!("{base}{path}"))
        .header("Content-Type", "application/json")
        .header("user-id", uid)
        .header("user-token", tok)
        .body(body_str)
        .send()
        .await?;
    let bytes = resp.bytes().await?;
    parse_wpf(&bytes)
}

/// 网关错误码 → 中文可行动提示（诊断表：`建议/02` P2-9）。未知码给 None。
pub fn diagnose_gateway_code(code: i64) -> Option<&'static str> {
    Some(match code {
        10 => "登录态已失效：到 资源(M) 页重新登录后再试",
        11 => "账号被限制（风控/异地登录）：换个网络或稍后再试",
        22 => "同一账号已在别处登录：关掉其它启动器/代理（包括官方启动器）后重试",
        _ => return None,
    })
}

/// 解析网易网关响应壳，失败时附上响应原文（长度 + hex 预览 + 可读文本）便于诊断
pub fn parse_wpf(bytes: &[u8]) -> Result<WpfResponse> {
    // 网关对「不存在的路径」会交给前端返回 HTML（实测 404 Page Not Found）。
    // 直接抛 serde 的报错用户看不懂，这里给能行动的提示：
    // 多半是该接口在当前（OBT）环境没上，或路径只存在于另一个部署环境。
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(200)]);
    if head.trim_start().starts_with('<') {
        let code = if head.contains("404") { "404" } else { "?" };
        return Err(anyhow!(
            "接口在当前环境不可用（HTTP {code}：网关返回网页而不是 JSON）"
        ));
    }
    let mut resp: WpfResponse = serde_json::from_slice(bytes).map_err(|e| {
        let hex_preview: String = bytes.iter().take(64).map(|b| format!("{b:02x}")).collect();
        let text_preview = String::from_utf8_lossy(&bytes[..bytes.len().min(300)]);
        anyhow!(
            "解析响应失败: {e}\nlen={} hex={hex_preview}\n文本={text_preview}",
            bytes.len()
        )
    })?;
    // 诊断表落地：把可行动提示拼进 message，所有调用点的错误文案自动带上，
    // 用户不用查文档也能知道下一步做什么
    if resp.code != 0 {
        if let Some(hint) = diagnose_gateway_code(resp.code) {
            if !resp.message.contains(hint) {
                resp.message = format!("{} —— {hint}", resp.message);
            }
        }
    }
    Ok(resp)
}

/// 租赁服列表来源。抓包核对（官方启动器实机）：五个来源返回的 entities 形状一致，
/// 只有「综合/最新/最热/好友」走 available-by-sort-type 的 sort_type 0/2/3，
/// 「我的收藏」走 rental-server-favorite/get-all，「最近访问」走 query/recent-visit-server。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RentalFeed {
    /// 综合榜（sort_type 0）
    #[default]
    All,
    /// 最新榜（sort_type 2）
    Newest,
    /// 最热榜（sort_type 3）
    Hottest,
    /// 好友的租赁服
    Friend,
    /// 我的收藏
    Favorite,
    /// 最近访问
    Recent,
    /// 我的租赁服（可开服/关服）
    Mine,
}

impl RentalFeed {
    /// 界面上的切换顺序（[ / ] 循环）
    pub const ALL: [RentalFeed; 7] = [
        RentalFeed::All,
        RentalFeed::Newest,
        RentalFeed::Hottest,
        RentalFeed::Friend,
        RentalFeed::Favorite,
        RentalFeed::Recent,
        RentalFeed::Mine,
    ];

    pub fn label(self) -> &'static str {
        match self {
            RentalFeed::All => "综合榜",
            RentalFeed::Newest => "最新榜",
            RentalFeed::Hottest => "最热榜",
            RentalFeed::Friend => "好友的",
            RentalFeed::Favorite => "我的收藏",
            RentalFeed::Recent => "最近访问",
            RentalFeed::Mine => "我的租赁服",
        }
    }

    /// 该来源对应的 (路径, 请求体)
    fn request(self, offset: i64) -> (&'static str, serde_json::Value) {
        match self {
            RentalFeed::All | RentalFeed::Newest | RentalFeed::Hottest => (
                "/rental-server/query/available-by-sort-type",
                json!({
                    "sort_type": match self {
                        RentalFeed::All => 0,
                        RentalFeed::Newest => 2,
                        _ => 3,
                    },
                    "order_type": 0,
                    "offset": offset,
                }),
            ),
            RentalFeed::Friend => (
                "/rental-server/query/available-friend-server",
                json!({"sort_type": 0, "world_name_key": null, "order_type": 0, "offset": offset}),
            ),
            RentalFeed::Favorite => (
                "/rental-server-favorite/get-all",
                json!({"offset": offset}),
            ),
            RentalFeed::Recent => (
                "/rental-server/query/recent-visit-server",
                json!({"offset": offset}),
            ),
            RentalFeed::Mine => (
                "/my-rental-server/query/search-by-user",
                json!({"length": 10, "offset": offset}),
            ),
        }
    }
}

/// 租赁服列表（按来源）。分页沿用 offset，一次一页。
pub async fn rental_feed(
    user_id: &str,
    user_token: &str,
    feed: RentalFeed,
    offset: i64,
) -> Result<Vec<EntityRentalGame>> {
    let (path, body) = feed.request(offset);
    let resp = post_signed(CLIENT, path, &body, user_id, user_token).await?;
    entities_to_rental(resp, &format!("租赁服列表（{}）", feed.label()))
}

/// 把网关响应的 entities 逐个解析成租赁服条目。榜单/收藏/最近访问/我的租赁服
/// 四类来源的条目字段基本一致（我的租赁服多带 world_id/expire_time 等，结构体自动忽略）。
fn entities_to_rental(resp: WpfResponse, what: &str) -> Result<Vec<EntityRentalGame>> {
    if resp.code != 0 {
        return Err(anyhow!("{}失败: {} ({})", what, resp.message, resp.details));
    }
    resp.entities
        .unwrap_or_default()
        .into_iter()
        .map(|v| serde_json::from_value(v).map_err(|e| anyhow!("解析租赁服失败: {e}")))
        .collect()
}

/// 按编号搜索租赁服的请求体。
/// 字段名叫 `server_name`，但实网只匹配**数字 name（玩家说的房号）**，
/// 展示名不参与匹配；抓包与实网响应形状一致（entities + total）。
pub fn rental_search_body(name: &str, offset: i64) -> serde_json::Value {
    json!({ "server_name": name.trim(), "offset": offset })
}

/// 按编号搜索租赁服（`offset` 供以后翻页用，当前 UI 只取第一页）
pub async fn rental_search(
    user_id: &str,
    user_token: &str,
    name: &str,
    offset: i64,
) -> Result<Vec<EntityRentalGame>> {
    let resp = post_signed(
        CLIENT,
        "/rental-server/query/search-by-name",
        &rental_search_body(name, offset),
        user_id,
        user_token,
    )
    .await?;
    entities_to_rental(resp, "搜索租赁服")
}

/// 开服 / 关服 / 重置的文案（`rental-server-control/update` 的 status 取值）
pub fn control_word(status: i64) -> &'static str {
    match status {
        1 => "开服",
        0 => "关服",
        2 => "重置",
        _ => "租赁服操作",
    }
}

/// 开服（1）/ 关服（0）/ 重置（2）：`rental-server-control/update`
/// 抓包实证：关服 = `{"status":0,"server_id":…,"version":null}`，开服 = status 1，重置 = status 2。
/// 重置会清档，mctl 界面上不提供入口。
pub async fn rental_control_set(
    user_id: &str,
    user_token: &str,
    server_id: &str,
    status: i64,
) -> Result<()> {
    let resp = post_signed(
        CLIENT,
        "/rental-server-control/update",
        &control_body(server_id, status),
        user_id,
        user_token,
    )
    .await?;
    if resp.code != 0 {
        return Err(anyhow!(
            "{}失败: {} ({})",
            control_word(status),
            resp.message,
            resp.details
        ));
    }
    Ok(())
}

/// `rental-server-control/update` 的请求体（抽出来便于单测：抓包里 version 恒为 null）
fn control_body(server_id: &str, status: i64) -> Value {
    json!({"status": status, "server_id": server_id, "version": null})
}

/// 收藏 / 取消收藏（`rental-server-favorite/update`：is_favorite 1/0）
pub async fn rental_set_favorite(
    user_id: &str,
    user_token: &str,
    server_id: &str,
    favorite: bool,
) -> Result<()> {
    let resp = post_signed(
        CLIENT,
        "/rental-server-favorite/update",
        &json!({"server_id": server_id, "is_favorite": i32::from(favorite)}),
        user_id,
        user_token,
    )
    .await?;
    if resp.code != 0 {
        return Err(anyhow!(
            "{}失败: {} ({})",
            if favorite { "收藏" } else { "取消收藏" },
            resp.message,
            resp.details
        ));
    }
    Ok(())
}

/// 查询是否已收藏该租赁服（`rental-server-favorite/query-status` → `entity.is_favorite` 0/1）
pub async fn rental_is_favorite(
    user_id: &str,
    user_token: &str,
    server_id: &str,
) -> Result<bool> {
    let resp = post_signed(
        CLIENT,
        "/rental-server-favorite/query-status",
        &json!({"server_id": server_id}),
        user_id,
        user_token,
    )
    .await?;
    if resp.code != 0 {
        return Err(anyhow!(
            "查询收藏状态失败: {} ({})",
            resp.message,
            resp.details
        ));
    }
    Ok(resp
        .entity
        .as_ref()
        .and_then(|e| e.get("is_favorite"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0)
        == 1)
}

/// 切换收藏：先查当前状态再取反，返回操作后的状态（true=已收藏）
pub async fn rental_toggle_favorite(
    user_id: &str,
    user_token: &str,
    server_id: &str,
) -> Result<bool> {
    let cur = rental_is_favorite(user_id, user_token, server_id).await?;
    rental_set_favorite(user_id, user_token, server_id, !cur).await?;
    Ok(!cur)
}

/// 查是否已点赞（`rental-server-like/get` → `entity.is_like`）。
/// 拿不到字段时返回 None（老服务端/字段改名），调用方据此**不要**盲目切换，
/// 否则会把「取消点赞」发成「点赞」（对别人的服是可见的误操作）。
pub async fn rental_like_state(
    user_id: &str,
    user_token: &str,
    server_id: &str,
) -> Result<Option<bool>> {
    let resp = post_signed(
        CLIENT,
        "/rental-server-like/get",
        &json!({"server_id": server_id}),
        user_id,
        user_token,
    )
    .await?;
    if resp.code != 0 {
        return Err(anyhow!(
            "查询点赞状态失败: {} ({})",
            resp.message,
            resp.details
        ));
    }
    Ok(resp
        .entity
        .as_ref()
        .and_then(|e| e.get("is_like"))
        .and_then(|v| v.as_i64())
        .map(|v| v == 1))
}

/// 点赞 / 取消点赞（`rental-server-like/update`：is_like 1/0）
pub async fn rental_set_like(
    user_id: &str,
    user_token: &str,
    server_id: &str,
    like: bool,
) -> Result<()> {
    let resp = post_signed(
        CLIENT,
        "/rental-server-like/update",
        &json!({"server_id": server_id, "is_like": i32::from(like)}),
        user_id,
        user_token,
    )
    .await?;
    if resp.code != 0 {
        return Err(anyhow!(
            "{}失败: {} ({})",
            if like { "点赞" } else { "取消点赞" },
            resp.message,
            resp.details
        ));
    }
    Ok(())
}

/// 切换点赞：先查状态再取反；状态未知（拿不到 is_like）直接报错不做动作，
/// 返回操作后的状态（true=已点赞）
pub async fn rental_toggle_like(
    user_id: &str,
    user_token: &str,
    server_id: &str,
) -> Result<bool> {
    let Some(cur) = rental_like_state(user_id, user_token, server_id).await? else {
        anyhow::bail!("服务端没回点赞状态（字段 is_like 缺失），本次不做改动以免误操作");
    };
    rental_set_like(user_id, user_token, server_id, !cur).await?;
    Ok(!cur)
}

/// 租赁服列表（综合榜）：保留旧入口，等价于 `rental_feed(.., RentalFeed::All, ..)`
pub async fn rental_list(
    user_id: &str,
    user_token: &str,
    offset: i64,
) -> Result<Vec<EntityRentalGame>> {
    rental_feed(user_id, user_token, RentalFeed::All, offset).await
}

/// 租赁服详情
pub async fn rental_detail(
    user_id: &str,
    user_token: &str,
    entity_id: &str,
) -> Result<EntityRentalGameDetails> {
    let resp = post_signed(
        CLIENT,
        "/rental-server-details/get",
        &json!({"entity_id": entity_id}),
        user_id,
        user_token,
    )
    .await?;
    if resp.code != 0 {
        return Err(anyhow!(
            "租赁服详情失败: {} ({})",
            resp.message,
            resp.details
        ));
    }
    let ent = resp.entity.ok_or_else(|| anyhow!("详情无 entity"))?;
    serde_json::from_value(ent).map_err(|e| anyhow!("解析详情失败: {e}"))
}

/// 进入租赁服的请求体。
/// 官方协议里 `pwd` 就是明文密码，没有密码时写字面量 `"none"`（不是省略字段）。
pub fn rental_address_body(server_id: &str, pwd: &str) -> serde_json::Value {
    let pwd = pwd.trim();
    json!({
        "server_id": server_id,
        "pwd": if pwd.is_empty() { "none" } else { pwd },
    })
}

/// 租赁服进入地址（选择线路：默认移动 cmcc / 电信 ctcc / 联通 cucc）。
/// 加密服必须带密码：`pwd` 为空表示无密码，接口对加密服会回 code=65。
pub async fn rental_address(
    user_id: &str,
    user_token: &str,
    server_id: &str,
    pwd: &str,
) -> Result<EntityRentalGameServerAddress> {
    let resp = post_signed(
        CLIENT,
        "/rental-server-world-enter/get",
        &rental_address_body(server_id, pwd),
        user_id,
        user_token,
    )
    .await?;
    if resp.code != 0 {
        return Err(anyhow!(
            "获取服务器地址失败: {} ({})",
            resp.message,
            resp.details
        ));
    }
    let ent = resp.entity.ok_or_else(|| anyhow!("地址无 entity"))?;
    serde_json::from_value(ent).map_err(|e| anyhow!("解析地址失败: {e}"))
}

/// 租赁服角色列表
pub async fn rental_roles(
    user_id: &str,
    user_token: &str,
    server_id: &str,
) -> Result<Vec<EntityRentalGamePlayerList>> {
    let resp = post_signed(
        CLIENT,
        "/rental-server-player/query/search-by-user-server",
        &json!({"server_id": server_id, "offset": 0, "length": 10}),
        user_id,
        user_token,
    )
    .await?;
    if resp.code != 0 {
        return Err(anyhow!("角色列表失败: {} ({})", resp.message, resp.details));
    }
    let entities = resp.entities.unwrap_or_default();
    entities
        .into_iter()
        .map(|v| serde_json::from_value(v).map_err(|e| anyhow!("解析角色失败: {e}")))
        .collect()
}

/// 联机游戏列表（联机大厅）
pub async fn netgame_list(
    user_id: &str,
    user_token: &str,
    offset: i64,
    length: i64,
) -> Result<Vec<EntityNetGameItem>> {
    let resp = post_signed(
        GATEWAY,
        "/item/query/available",
        &json!({
            "available_mc_versions": [],
            "item_type": 1,
            "length": length,
            "offset": offset,
            "master_type_id": "2",
            "secondary_type_id": "",
        }),
        user_id,
        user_token,
    )
    .await?;
    if resp.code != 0 {
        return Err(anyhow!("联机列表失败: {} ({})", resp.message, resp.details));
    }
    let entities = resp.entities.unwrap_or_default();
    entities
        .into_iter()
        .map(|v| serde_json::from_value(v).map_err(|e| anyhow!("解析联机项失败: {e}")))
        .collect()
}

/// 联机游戏详情
pub async fn netgame_detail(
    user_id: &str,
    user_token: &str,
    item_id: &str,
) -> Result<EntityNetGameDetail> {
    let resp = post_signed(
        GATEWAY,
        "/item-details/get_v2",
        &json!({"item_id": item_id}),
        user_id,
        user_token,
    )
    .await?;
    if resp.code != 0 {
        return Err(anyhow!("联机详情失败: {} ({})", resp.message, resp.details));
    }
    let ent = resp.entity.ok_or_else(|| anyhow!("详情无 entity"))?;
    serde_json::from_value(ent).map_err(|e| anyhow!("解析详情失败: {e}"))
}

/// 联机游戏服务器地址
pub async fn netgame_address(
    user_id: &str,
    user_token: &str,
    item_id: &str,
) -> Result<EntityNetGameServerAddress> {
    let resp = post_signed(
        GATEWAY,
        "/item-address/get",
        &json!({"item_id": item_id}),
        user_id,
        user_token,
    )
    .await?;
    if resp.code != 0 {
        return Err(anyhow!(
            "获取联机地址失败: {} ({})",
            resp.message,
            resp.details
        ));
    }
    let ent = resp.entity.ok_or_else(|| anyhow!("地址无 entity"))?;
    serde_json::from_value(ent).map_err(|e| anyhow!("解析地址失败: {e}"))
}

// ---------- 实体 ----------

#[derive(Debug, Clone, Deserialize)]
pub struct EntityRentalGame {
    #[serde(rename = "entity_id", default)]
    pub entity_id: String,
    #[serde(default)]
    pub name: String,
    #[serde(rename = "server_name", default)]
    pub server_name: String,
    #[serde(rename = "has_pwd", default)]
    pub has_pwd: String,
    #[serde(default)]
    pub status: i64,
    #[serde(default)]
    pub capacity: u32,
    #[serde(rename = "mc_version", default)]
    pub mc_version: String,
    #[serde(rename = "player_count", default)]
    pub player_count: u32,
    #[serde(rename = "image_url", default)]
    pub image_url: String,
    /// 点赞数（榜单/收藏/最近访问/我的租赁服都给）
    #[serde(rename = "like_num", default)]
    pub like_num: i64,
}

impl EntityRentalGame {
    /// 卡片标题：公开榜单用 server_name，自己的租赁服常常只填了 name
    pub fn title(&self) -> &str {
        match (self.server_name.is_empty(), self.name.is_empty()) {
            (false, _) => &self.server_name,
            (true, false) => &self.name,
            // 自己的服没起名时官方也回空字符串，界面上给个占位而不是空白卡片
            (true, true) => "（未命名租赁服）",
        }
    }

    /// 「我的租赁服」的运行状态文案（0=已关闭 1=运行中，与 control/update 同一套取值；
    /// 其它值不猜，返回 None 让界面别乱标）
    pub fn power_label(&self) -> Option<&'static str> {
        match self.status {
            0 => Some("已关闭"),
            1 => Some("运行中"),
            _ => None,
        }
    }

    /// 开服/关服要下发的 status（已关闭 → 开服 1；运行中 → 关服 0；其余不动）
    pub fn power_toggle(&self) -> Option<i64> {
        match self.status {
            0 => Some(1),
            1 => Some(0),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct EntityRentalGameDetails {
    #[serde(rename = "entity_id", default)]
    pub entity_id: String,
    #[serde(rename = "brief_summary", default)]
    pub brief_summary: String,
    #[serde(rename = "mc_version", default)]
    pub mc_version: String,
    #[serde(default)]
    pub capacity: u32,
    #[serde(rename = "player_count", default)]
    pub player_count: u32,
    #[serde(rename = "server_name", default)]
    pub server_name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EntityRentalGameServerAddress {
    #[serde(rename = "mcserver_host", default)]
    pub mcserver_host: String,
    #[serde(rename = "mcserver_port", default)]
    pub mcserver_port: u16,
    #[serde(default)]
    pub state: i64,
    #[serde(rename = "cmcc_mcserver_host", default)]
    pub cmcc_mcserver_host: String,
    #[serde(rename = "cmcc_mcserver_port", default)]
    pub cmcc_mcserver_port: u16,
    #[serde(rename = "ctcc_mcserver_host", default)]
    pub ctcc_mcserver_host: String,
    #[serde(rename = "ctcc_mcserver_port", default)]
    pub ctcc_mcserver_port: u16,
    #[serde(rename = "cucc_mcserver_host", default)]
    pub cucc_mcserver_host: String,
    #[serde(rename = "cucc_mcserver_port", default)]
    pub cucc_mcserver_port: u16,
    #[serde(rename = "isp_enable", default)]
    pub isp_enable: bool,
}

impl EntityRentalGameServerAddress {
    /// 选择线路：优先对应运营商，否则回退默认
    pub fn pick(&self) -> Option<(String, u16)> {
        let isp = crate::netease::detect_isp();
        let (host, port) = match isp.as_str() {
            "cmcc" => (self.cmcc_mcserver_host.as_str(), self.cmcc_mcserver_port),
            "ctcc" => (self.ctcc_mcserver_host.as_str(), self.ctcc_mcserver_port),
            "cucc" => (self.cucc_mcserver_host.as_str(), self.cucc_mcserver_port),
            _ => ("", 0),
        };
        if !host.is_empty() && port > 0 {
            return Some((host.to_string(), port));
        }
        if !self.mcserver_host.is_empty() && self.mcserver_port > 0 {
            return Some((self.mcserver_host.clone(), self.mcserver_port));
        }
        None
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct EntityRentalGamePlayerList {
    #[serde(rename = "entity_id", default)]
    pub entity_id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub nick_name: String,
    #[serde(default)]
    pub gameid: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EntityNetGameItem {
    #[serde(rename = "entity_id", default)]
    pub entity_id: String,
    #[serde(rename = "brief_summary", default)]
    pub brief_summary: String,
    #[serde(default)]
    pub name: String,
    #[serde(rename = "online_count", default)]
    pub online_count: String,
    #[serde(rename = "title_image_url", default)]
    pub title_image_url: String,
    #[serde(default)]
    pub version: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EntityNetGameDetail {
    #[serde(rename = "entity_id", default)]
    pub entity_id: String,
    #[serde(rename = "brief_summary", default)]
    pub brief_summary: String,
    #[serde(default)]
    pub name: String,
    #[serde(rename = "server_name", default)]
    pub server_name: String,
    #[serde(rename = "title_image_url", default)]
    pub title_image_url: String,
    /// 宣传图列表（实测：列表页不带图，详情里有；封面取第一张）
    #[serde(rename = "brief_image_urls", default)]
    pub brief_image_urls: Vec<String>,
    #[serde(default)]
    pub version: String,
    /// 服务器支持的 MC 版本列表（实测）
    #[serde(rename = "mc_version_list", default)]
    pub mc_version_list: Vec<EntityMcVersion>,
    #[serde(rename = "server_address", default)]
    pub server_address: String,
    #[serde(rename = "server_port", default)]
    pub server_port: u16,
    #[serde(default)]
    pub player_count: u32,
}

impl EntityNetGameDetail {
    /// 宣传图 URL：优先 `title_image_url`（实测常为空），否则取列表页第一张
    /// `brief_image_urls`——官方启动器列表卡片用的就是后者。
    pub fn cover_url(&self) -> String {
        if !self.title_image_url.is_empty() {
            return self.title_image_url.clone();
        }
        self.brief_image_urls.first().cloned().unwrap_or_default()
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct EntityMcVersion {
    #[serde(rename = "mcversionid", default)]
    pub mcversionid: i64,
    #[serde(default)]
    pub name: String,
}

/// 联机游戏服务器地址（实测）
#[derive(Debug, Clone, Deserialize)]
pub struct EntityNetGameServerAddress {
    #[serde(rename = "ip", default)]
    pub ip: String,
    #[serde(rename = "port", default)]
    pub port: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameProfile {
    #[serde(rename = "gameId")]
    pub game_id: String,
    #[serde(rename = "gameVersion")]
    pub game_version: String,
    #[serde(rename = "bootstrapMd5")]
    pub bootstrap_md5: String,
    #[serde(rename = "datFileMd5")]
    pub dat_file_md5: String,
    #[serde(rename = "mods")]
    pub mods: Value,
    #[serde(rename = "profile")]
    pub profile: UserProfile,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserProfile {
    #[serde(rename = "user")]
    pub user: EntityUserInfo,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityUserInfo {
    #[serde(rename = "token")]
    pub token: String,
    #[serde(rename = "userId")]
    pub user_id: String,
}

/// 联机角色列表（实测）。
/// `game_id` = **服务器自己的 item id**（列表项 `entity_id`），不是任何平台级常量。
/// 角色按 `game_id` 分命名空间：同一账号在不同服务器上是不同的角色集合（实网已验证）。
pub async fn netgame_characters(
    game_id: &str,
    user_id: &str,
    user_token: &str,
) -> Result<Vec<EntityGameCharacter>> {
    let resp = post_signed(
        GATEWAY,
        "/game-character/query/user-game-characters",
        &json!({
            "offset": 0,
            "length": 10,
            "user_id": user_id,
            "game_id": game_id,
            "game_type": "2",
        }),
        user_id,
        user_token,
    )
    .await?;
    if resp.code != 0 {
        return Err(anyhow!("角色列表失败: {} ({})", resp.message, resp.details));
    }
    let entities = resp.entities.unwrap_or_default();
    entities
        .into_iter()
        .map(|v| serde_json::from_value(v).map_err(|e| anyhow!("解析角色失败: {e}")))
        .collect()
}

/// 创建联机角色（实测）
pub async fn create_character(
    game_id: &str,
    user_id: &str,
    user_token: &str,
    name: &str,
) -> Result<()> {
    let resp = post_signed(
        GATEWAY,
        "/game-character",
        &json!({
            "game_id": game_id,
            "game_type": 2,
            "user_id": user_id,
            "name": name,
            "create_time": 555555,
            "expire_time": 0,
        }),
        user_id,
        user_token,
    )
    .await?;
    if resp.code != 0 {
        return Err(anyhow!("创建角色失败: {} ({})", resp.message, resp.details));
    }
    Ok(())
}

#[derive(Debug, Clone, Deserialize)]
pub struct EntityGameCharacter {
    #[serde(rename = "game_id", default)]
    pub game_id: String,
    #[serde(rename = "game_type", default)]
    pub game_type: i64,
    #[serde(rename = "user_id", default)]
    pub user_id: String,
    #[serde(default)]
    pub name: String,
}

/// 角色列表里是否存在该名字（联机语义：名字必须已存在，官方在查不到时按「角色不存在」处理）
pub fn netgame_has_character(chars: &[EntityGameCharacter], name: &str) -> bool {
    chars.iter().any(|c| c.name == name)
}

/// 租赁服玩家列表里是否存在该名字（官方按 `name` 精确匹配）
pub fn rental_has_character(list: &[EntityRentalGamePlayerList], name: &str) -> bool {
    list.iter().any(|p| p.name == name)
}

/// 创建租赁服角色（查询走 Client 主机、创建走 Gateway 主机，与官方一致）。
/// `create_ts`/`is_online`/`status` 是官方固定占位值。
pub async fn create_rental_character(
    user_id: &str,
    user_token: &str,
    server_id: &str,
    name: &str,
) -> Result<()> {
    let resp = post_signed(
        GATEWAY,
        "/rental-server-player",
        &json!({
            "server_id": server_id,
            "user_id": user_id,
            "name": name,
            "create_ts": 555555,
            "is_online": false,
            "status": 0,
        }),
        user_id,
        user_token,
    )
    .await?;
    if resp.code != 0 {
        return Err(anyhow!(
            "创建租赁服角色失败: {} ({})",
            resp.message,
            resp.details
        ));
    }
    Ok(())
}

/// 网易端 Md5 映射（Yggdrasil BuildHashData 使用）
pub fn md5_pair(version: &str) -> (String, String) {
    match version {
        "1.7.10" => (
            "A895FE657915D58F55919CEACD30209D".into(),
            "538D33D5F35EF01736EDA30F94C61DF6".into(),
        ),
        "1.8.9" => (
            "A895FE657915D58F55919CEACD30209D".into(),
            "0CF2074AA7D4B543E35A3D6BB57AF861".into(),
        ),
        "1.12.2" => (
            "A895FE657915D58F55919CEACD30209D".into(),
            "51581ADD89B8AC5A0D8CCDD0E33EE1DE".into(),
        ),
        "1.16" => (
            "7B101583C3965371B89A3C9115B27526".into(),
            "B0712F34B0A584D05D9D29FA68759E29".into(),
        ),
        "1.18" => (
            "C3BD2115F23F6FE4B2ADCC7FC4DEFFEA".into(),
            "56677A2BB31E18246FA241FB02E16D0E".into(),
        ),
        "1.20" => (
            "2A7A476411A1687A56DC6848829C1AE4".into(),
            "D285CBF97D9BA30D3C445DBF1C342634".into(),
        ),
        "1.21" => (
            "684528BF492A84489F825F5599B3E1C6".into(),
            "574033E7E4841D8AC4C14D7FA5E05337".into(),
        ),
        "1.21.8" => (
            "5BF6153C69DD28951A699F7F834EFE1A".into(),
            "C9906B5809A92C73299279E562A78D81".into(),
        ),
        _ => (
            "2A7A476411A1687A56DC6848829C1AE4".into(),
            "D285CBF97D9BA30D3C445DBF1C342634".into(),
        ),
    }
}

fn x19_version_cache() -> std::path::PathBuf {
    crate::storage::config_dir().join("x19_version.txt")
}

/// MC 服务器状态 ping：返回其协议版本号（platform 元数据可能过期，
/// 某热门服实测：元数据 1.20 / 实服协议 774=1.21.8）
pub async fn server_protocol(host: &str, port: u16) -> Result<i64> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    fn varint(buf: &mut Vec<u8>, mut n: i64) {
        loop {
            let b = (n & 0x7F) as u8;
            n >>= 7;
            if n != 0 {
                buf.push(b | 0x80);
            } else {
                buf.push(b);
                return;
            }
        }
    }

    let mut sock = TcpStream::connect((host, port)).await?;
    let mut hs = Vec::new();
    varint(&mut hs, 0); // packet id: handshake
    varint(&mut hs, 0); // protocol: status 用的占位
    varint(&mut hs, host.len() as i64);
    hs.extend_from_slice(host.as_bytes());
    hs.extend_from_slice(&port.to_be_bytes());
    varint(&mut hs, 1); // next state: status
    let mut req = Vec::new();
    varint(&mut req, hs.len() as i64);
    req.extend_from_slice(&hs);
    varint(&mut req, 0); // packet id: status request
    sock.write_all(&req).await?;

    // 读响应：varint 长度 + varint packet id + varint json 长度 + json（8s 超时）
    async fn read_varint(sock: &mut TcpStream) -> Result<i64> {
        let mut n: i64 = 0;
        let mut shift = 0;
        loop {
            let mut b = [0u8; 1];
            tokio::time::timeout(std::time::Duration::from_secs(8), sock.read_exact(&mut b))
                .await
                .map_err(|_| anyhow!("ping 超时"))??;
            n |= ((b[0] & 0x7F) as i64) << shift;
            if b[0] & 0x80 == 0 {
                return Ok(n);
            }
            shift += 7;
        }
    }
    let _len = read_varint(&mut sock).await?;
    let _pid = read_varint(&mut sock).await?;
    let jlen = read_varint(&mut sock).await? as usize;
    let mut buf = vec![0u8; jlen];
    sock.read_exact(&mut buf).await?;
    let v: Value = serde_json::from_slice(&buf)?;
    v.get("version")
        .and_then(|v| v.get("protocol"))
        .and_then(|p| p.as_i64())
        .ok_or_else(|| anyhow!("ping 响应无 protocol"))
}

/// 协议号 → 网易客户端版本（version_enum 可识别的粒度）
pub fn protocol_version(protocol: i64) -> &'static str {
    match protocol {
        p if p >= 774 => "1.21.8",
        p if p >= 767 => "1.21",
        p if p >= 763 => "1.20",
        p if p >= 757 => "1.18",
        p if p >= 751 => "1.16",
        _ => "1.12.2",
    }
}

/// 会话激活（对照 Codexus SDK X19.InterconnectionApi.LoginStart：
/// 登录/进服前调用；缺此步 Yggdrasil 认证服返回 0x11）
pub async fn game_login_start(user_id: &str, user_token: &str) -> Result<()> {
    interconn_post(
        "/interconn/web/game-play-v2/login-start",
        &json!({ "strict_mode": true }),
        user_id,
        user_token,
    )
    .await
}

/// 游戏开局上报（对照 SDK X19.InterconnectionApi.GameStartAsync：
/// 进服前一刻调用，向网易登记 (userId, gameId) 的开局记录）
pub async fn game_play_start(user_id: &str, user_token: &str, game_id: &str) -> Result<()> {
    interconn_post(
        "/interconn/web/game-play-v2/start",
        &json!({
            // 字段名按实测行为 C# InterConnGameStart（camelCase），网易据此绑定会话
            "gameId": game_id,
            "game_type": "2", // NetGame
            "strict_mode": true,
            "itemList": ["10000"], // 默认皮肤
        }),
        user_id,
        user_token,
    )
    .await
}

/// obtcore interconn 签名 POST（DynamicToken 头），响应 {code:0} 视为成功
async fn interconn_post(path: &str, body: &Value, user_id: &str, user_token: &str) -> Result<()> {
    let body_str = serde_json::to_string(body)?;
    let (uid, tok) = crate::token::compute(path, &body_str, user_id, user_token);
    let resp = client()?
        .post(format!("{CORE}{path}"))
        .header("Content-Type", "application/json")
        .header("user-id", uid)
        .header("user-token", tok)
        .body(body_str)
        .send()
        .await?;
    let text = resp.text().await?;
    let code = serde_json::from_str::<Value>(&text)
        .ok()
        .and_then(|v| v.get("code").and_then(|c| c.as_i64()))
        .unwrap_or(0);
    if code != 0 {
        return Err(anyhow!(
            "{} 失败: {}",
            path,
            crate::text::truncate(&text, 200)
        ));
    }
    Ok(())
}

/// 当前 X19.GameVersion（对照 X19.GameVersion）：
/// 优先读 refresh_x19_version 缓存，无缓存回退 crate::api::LATEST_VERSION。
pub fn x19_version() -> String {
    std::fs::read_to_string(x19_version_cache())
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| crate::api::LATEST_VERSION.to_string())
}

/// 从补丁列表解析末位版本号（对照 X19.GetLatestVersion：
/// 找最后一个 `":{"size":`，取其前的最后一个引号后的版本名）
fn parse_latest_version(text: &str) -> Result<String> {
    let idx = text
        .rfind("\":{\"size\":")
        .ok_or_else(|| anyhow!("patchlist 格式异常（无 :{{\"size\":）"))?;
    let head = &text[..idx];
    let start = head
        .rfind('"')
        .ok_or_else(|| anyhow!("patchlist 版本名缺失"))?
        + 1;
    let ver = &head[start..];
    if ver.is_empty() {
        return Err(anyhow!("patchlist 版本名为空"));
    }
    Ok(ver.to_string())
}

/// 拉取最新补丁列表并缓存版本号（启动/认证前调用；失败静默用旧值）
pub async fn refresh_x19_version() -> Result<String> {
    let text = client()?
        .get("https://x19.update.netease.com/pl/x19_java_patchlist")
        .send()
        .await?
        .text()
        .await?;
    let ver = parse_latest_version(&text)?;
    let dir = crate::storage::config_dir();
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(x19_version_cache(), &ver);
    Ok(ver)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 诊断表：错误码 → message 里自动带可行动提示（P2-9）
    #[test]
    fn gateway_code_hints_are_appended() {
        let raw = br#"{"code":22,"message":"invalid session"}"#;
        let resp = parse_wpf(raw).unwrap();
        assert_eq!(resp.code, 22);
        assert!(resp.message.contains("invalid session"), "{}", resp.message);
        assert!(
            resp.message.contains("别处登录"),
            "22 应带双登录提示：{}",
            resp.message
        );

        let ok = parse_wpf(br#"{"code":0,"message":"ok"}"#).unwrap();
        assert_eq!(ok.message, "ok", "成功响应不应被改写");

        let unknown = parse_wpf(br#"{"code":777,"message":"weird"}"#).unwrap();
        assert_eq!(unknown.message, "weird", "未知码不加提示");

        assert!(diagnose_gateway_code(10).unwrap().contains("重新登录"));
        assert!(diagnose_gateway_code(777).is_none());
    }

    /// 角色名匹配必须精确：官方按「列表里存在同名角色」判定，查不到即按角色不存在处理
    #[test]
    fn character_name_matching_is_exact() {
        let chars = vec![EntityGameCharacter {
            game_id: "1".into(),
            game_type: 2,
            user_id: "u".into(),
            name: "甲".into(),
        }];
        assert!(netgame_has_character(&chars, "甲"));
        assert!(!netgame_has_character(&chars, "乙"));
        assert!(!netgame_has_character(&chars, "甲 "), "名字必须精确匹配");

        let players = vec![EntityRentalGamePlayerList {
            entity_id: "9".into(),
            name: "丙".into(),
            nick_name: "丙丙".into(),
            gameid: "1".into(),
        }];
        assert!(rental_has_character(&players, "丙"));
        assert!(
            !rental_has_character(&players, "丙丙"),
            "官方按 name 匹配，不看 nick_name"
        );
    }

    #[test]
    fn netgame_address_parses_ip_port() {
        let raw = br#"{"code":0,"entity":{"ip":"1.2.3.4","port":25565}}"#;
        let resp = parse_wpf(raw).unwrap();
        let ent: EntityNetGameServerAddress = serde_json::from_value(resp.entity.unwrap()).unwrap();
        assert_eq!(ent.ip, "1.2.3.4");
        assert_eq!(ent.port, 25565);
    }

    #[test]
    fn netgame_address_domain_parses() {
        let raw = br#"{"code":0,"entity":{"ip":"mc.example.com","port":25565}}"#;
        let resp = parse_wpf(raw).unwrap();
        let ent: EntityNetGameServerAddress = serde_json::from_value(resp.entity.unwrap()).unwrap();
        assert_eq!(ent.ip, "mc.example.com");
        assert_eq!(ent.port, 25565);
    }

    #[test]
    fn patchlist_last_version_parsed() {
        let text = "{ \"patch\": {\n \
  \"1.15.24.4846\":{\"size\": 97115240, \"url\": \"https://x19.gph.netease.com/patch1.15.24.4846.X19.Release.7z\", \"md5\": \"38d3e027ad1e32f4bdf1a20899009a3a\"},\n \
  \"1.15.25.5792\":{\"size\": 95205650, \"url\": \"https://x19.gph.netease.com/patch1.15.25.5792.X19.Release.7z\", \"md5\": \"5103dba009e3f142c6768c69e9577feb\"}\n \
} }";
        assert_eq!(parse_latest_version(text).unwrap(), "1.15.25.5792");
        assert!(parse_latest_version("no size marker here").is_err());
    }

    /// 换榜的路径/请求体：抓包（官方启动器实机）逐字段核对
    #[test]
    fn rental_feed_maps_path_and_body() {
        let (p, b) = RentalFeed::All.request(40);
        assert_eq!(p, "/rental-server/query/available-by-sort-type");
        assert_eq!(b["sort_type"], 0);
        assert_eq!(b["order_type"], 0);
        assert_eq!(b["offset"], 40);

        assert_eq!(RentalFeed::Newest.request(0).1["sort_type"], 2);
        assert_eq!(RentalFeed::Hottest.request(0).1["sort_type"], 3);

        let (p, b) = RentalFeed::Friend.request(0);
        assert_eq!(p, "/rental-server/query/available-friend-server");
        assert_eq!(b["sort_type"], 0);
        assert!(b["world_name_key"].is_null());

        // 收藏 / 最近访问只带 offset
        let (p, b) = RentalFeed::Favorite.request(7);
        assert_eq!(p, "/rental-server-favorite/get-all");
        assert_eq!(b, json!({"offset": 7}));
        assert_eq!(
            RentalFeed::Recent.request(0).0,
            "/rental-server/query/recent-visit-server"
        );

        // 我的租赁服：只带 length + offset（抓包里 length 恒为 10）
        let (p, b) = RentalFeed::Mine.request(10);
        assert_eq!(p, "/my-rental-server/query/search-by-user");
        assert_eq!(b, json!({"length": 10, "offset": 10}));
    }

    /// 三个来源（榜单/收藏/最近访问）的条目字段完全一致 → 同一结构体吃得下
    #[test]
    fn rental_feed_parses_captured_item_shape() {
        let raw = r#"{"code":0,"message":"正常返回","details":"","entities":[{"entity_id":"4681422345813502335","name":"44809929","owner_id":2272202527,"visibility":0,"status":1,"icon_index":4,"capacity":20,"mc_version":"1.21.2-release","player_count":5,"like_num":7857316,"server_type":"docker_new_guian","offset":null,"has_pwd":"0","image_url":"https://g79-3.fp.ps.netease.com/file/68a71d5e9f6070c47","world_id":"123","min_level":0,"pvp":1,"server_name":"测试服"}],"total":1}"#;
        let resp = parse_wpf(raw.as_bytes()).unwrap();
        let list: Vec<EntityRentalGame> = resp
            .entities
            .unwrap()
            .into_iter()
            .map(|v| serde_json::from_value(v).unwrap())
            .collect();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].entity_id, "4681422345813502335");
        assert_eq!(list[0].server_name, "测试服");
        assert_eq!(list[0].has_pwd, "0");
        assert_eq!(list[0].player_count, 5);
        assert_eq!(list[0].capacity, 20);
    }

    /// 收藏状态查询：entity.is_favorite 0/1（抓包响应形状）
    #[test]
    fn favorite_flag_reads_from_entity() {
        let read = |raw: &[u8]| {
            let resp = parse_wpf(raw).unwrap();
            resp.entity
                .as_ref()
                .and_then(|e| e.get("is_favorite"))
                .and_then(|v| v.as_i64())
                .unwrap_or(0)
                == 1
        };
        assert!(read(
            r#"{"code":0,"message":"正常返回","details":"","entity":{"sid":4681422345813502335,"is_favorite":1}}"#
                .as_bytes()
        ));
        assert!(!read(
            r#"{"code":0,"entity":{"sid":1,"is_favorite":0}}"#.as_bytes()
        ));
        // 老服务端不回字段时按未收藏处理，不能报错
        assert!(!read(r#"{"code":0,"entity":{"sid":1}}"#.as_bytes()));
    }

    #[test]
    fn rental_feed_order_and_labels() {
        assert_eq!(RentalFeed::default(), RentalFeed::All, "默认进综合榜");
        assert_eq!(RentalFeed::ALL.len(), 7);
        assert_eq!(RentalFeed::Mine.label(), "我的租赁服");
        assert_eq!(RentalFeed::ALL[0], RentalFeed::All);
        assert_eq!(RentalFeed::Favorite.label(), "我的收藏");
        assert_eq!(RentalFeed::Recent.label(), "最近访问");
    }

    /// 开服/关服请求体与文案：抓包实证 status 0=关、1=开、2=重置
    #[test]
    fn control_request_matches_capture() {
        assert_eq!(
            control_body("4684271786255223982", 1),
            json!({"status": 1, "server_id": "4684271786255223982", "version": null})
        );
        assert_eq!(control_body("1", 0)["status"], 0);
        assert_eq!(control_word(0), "关服");
        assert_eq!(control_word(1), "开服");
        assert_eq!(control_word(2), "重置");
        // 未知取值不编文案，也别让界面显示空洞
        assert_eq!(control_word(9), "租赁服操作");
    }

    /// 加密服的密码要原样进请求体；没密码时写官方约定的字面量 "none"
    #[test]
    fn rental_address_body_carries_password() {
        let b = rental_address_body("42", "hunter2");
        assert_eq!(b["server_id"], "42");
        assert_eq!(b["pwd"], "hunter2");
        assert_eq!(rental_address_body("42", "")["pwd"], "none");
        assert_eq!(
            rental_address_body("42", "   ")["pwd"],
            "none",
            "空白等同没填"
        );
    }

    /// 搜索请求体：编号去空白，offset 透传（实网只认数字房号）
    #[test]
    fn rental_search_body_uses_number_and_offset() {
        let b = rental_search_body(" 57229664 ", 30);
        assert_eq!(b["server_name"], "57229664");
        assert_eq!(b["offset"], 30);
    }

    /// 「我的租赁服」条目形状（抓包）：多有 world_id/expire_time 等字段，
    /// EntityRentalGame 直接吃下（未声明字段忽略），status 与开停接口同一套取值
    #[test]
    fn mine_item_parses_and_reports_power() {
        let raw = r#"{"code":0,"message":"正常返回","details":"","total":1,"entities":[{"entity_id":"4684271786255223982","world_id":"8xi35esjt76sdfvao7sara","owner_id":"3019702719","name":"38826720","brief_summary":"","icon_index":4,"active_time":1765010156,"expire_time":1767602156,"begin_time":1767324758,"meal_id":"73","mc_version":"1.21.2-release","capacity":4,"status":1,"visibility":2,"active_components":[]}]}"#;
        let resp = parse_wpf(raw.as_bytes()).unwrap();
        let list = entities_to_rental(resp, "我的租赁服").unwrap();
        assert_eq!(list.len(), 1);
        let s = &list[0];
        assert_eq!(s.entity_id, "4684271786255223982");
        assert_eq!(s.status, 1, "运行中");
        assert_eq!(s.power_label(), Some("运行中"));
        assert_eq!(s.power_toggle(), Some(0), "运行中 → 下发关服");
        assert_eq!(s.mc_version, "1.21.2-release");
        // 官方自己的服没填 server_name，标题要回落到 name，不能是空白卡片
        assert_eq!(s.title(), "38826720");
    }

    /// 状态不是「已关闭/运行中」时不开停（重置中/异常状态不猜）
    #[test]
    fn unknown_power_status_is_not_touched() {
        let mut s = EntityRentalGame {
            entity_id: "1".into(),
            name: "甲".into(),
            server_name: String::new(),
            has_pwd: String::new(),
            status: 8,
            capacity: 4,
            mc_version: String::new(),
            player_count: 0,
            image_url: String::new(),
            like_num: 0,
        };
        assert_eq!(s.power_label(), None);
        assert_eq!(s.power_toggle(), None, "状态未知不能乱下发");
        s.status = 0;
        assert_eq!(s.power_label(), Some("已关闭"));
        assert_eq!(s.power_toggle(), Some(1), "已关闭 → 下发开服");
        // 两个名字都空时给占位，界面上不留空卡片
        s.name.clear();
        assert_eq!(s.title(), "（未命名租赁服）");
    }

    /// 网关对不存在的路径返回 HTML → 提示要能行动，而不是 serde 的原始报错
    #[test]
    fn html_404_gives_actionable_error() {
        let html = b"<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<title>404 Page Not Found</title>";
        let err = parse_wpf(html).unwrap_err().to_string();
        assert!(err.contains("当前环境不可用"), "err={err}");
        assert!(err.contains("404"), "err={err}");
        // 正常 JSON 不受影响
        assert!(parse_wpf(br#"{"code":0,"message":"ok"}"#).is_ok());
    }

    /// 点赞状态：entity.is_like 1/0；字段缺失必须返回 None（不许误判成「没点赞」）
    #[test]
    fn like_state_reads_from_entity_and_refuses_unknown() {
        let read = |raw: &[u8]| -> Option<bool> {
            let resp = parse_wpf(raw).unwrap();
            resp.entity
                .as_ref()
                .and_then(|e| e.get("is_like"))
                .and_then(|v| v.as_i64())
                .map(|v| v == 1)
        };
        assert_eq!(
            read(r#"{"code":0,"entity":{"server_id":"1","is_like":1}}"#.as_bytes()),
            Some(true)
        );
        assert_eq!(
            read(r#"{"code":0,"entity":{"server_id":"1","is_like":0}}"#.as_bytes()),
            Some(false)
        );
        // 字段缺失 = 不知道，而不是「没点赞」：调用方据此拒绝动手
        assert_eq!(read(r#"{"code":0,"entity":{"server_id":"1"}}"#.as_bytes()), None);
    }

    /// 列表条目的 like_num 是宽松字段：有就读，没有也不影响解析
    #[test]
    fn like_num_is_optional_in_items() {
        let raw = r#"{"code":0,"entities":[{"entity_id":"1","server_name":"甲","like_num":7857316}]}"#;
        let resp = parse_wpf(raw.as_bytes()).unwrap();
        let list = entities_to_rental(resp, "测试").unwrap();
        assert_eq!(list[0].like_num, 7857316);

        let raw = r#"{"code":0,"entities":[{"entity_id":"1","server_name":"甲"}]}"#;
        let resp = parse_wpf(raw.as_bytes()).unwrap();
        let list = entities_to_rental(resp, "测试").unwrap();
        assert_eq!(list[0].like_num, 0, "缺字段按 0，不能让整条解析失败");
    }

    #[test]
    fn protocol_maps_to_netease_client() {
        // 某热门服实测：774 → 1.21.8；网易 1.20 客户端自报 1.20.6（协议 766）→ 1.20
        assert_eq!(protocol_version(774), "1.21.8");
        assert_eq!(protocol_version(775), "1.21.8");
        assert_eq!(protocol_version(767), "1.21");
        assert_eq!(protocol_version(766), "1.20");
        assert_eq!(protocol_version(763), "1.20");
        assert_eq!(protocol_version(340), "1.12.2");
    }
}
