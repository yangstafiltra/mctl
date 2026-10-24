//! Java 国际版服务器：精选列表 + 原版 Server List Ping。
//!
//! 不依赖任何第三方服务器列表 API：列表内置（用户也可在
//! `~/.config/mctl/java_servers.json` 里追加），状态直接按 Minecraft
//! 原版协议 ping 服务器本身。好处是没有 API key、不会被某个站点限流；
//! 代价是墙内访问部分海外服可能超时——超时条目仍会显示地址，只是标成“离线”。

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// 单个服务器的展示状态。`error` 非空表示这次 ping 没成功，
/// 但服务器仍会留在列表里（地址可复制/仍可尝试启动）。
#[derive(Debug, Clone, Default)]
pub struct JavaServer {
    /// 展示名（内置条目用精选名，自定义条目默认 host）
    pub name: String,
    pub host: String,
    pub port: u16,
    /// 服务端声明的版本名，如 `1.8-1.21`；离线时为空
    pub version: String,
    /// 协议号（1.21.8 = 774）
    pub protocol: i64,
    pub online: i64,
    pub max: i64,
    /// MOTD 纯文本（已去掉 § 颜色代码）
    pub motd: String,
    /// `data:image/png;base64,...`（原版协议里的 favicon）
    pub favicon: Option<String>,
    pub latency_ms: Option<u64>,
    pub error: Option<String>,
    /// 内置精选列表（true）还是用户自定义（false）
    pub builtin: bool,
}

impl JavaServer {
    pub fn addr(&self) -> String {
        if self.port == 25565 {
            self.host.clone()
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }

    pub fn online_text(&self) -> String {
        match &self.error {
            Some(_) => "离线".to_string(),
            None => format!("{}/{} 人", self.online, self.max),
        }
    }

    /// 封面 favicon 的 PNG 字节（协议里自带的小图标，64×64）
    pub fn favicon_bytes(&self) -> Option<Vec<u8>> {
        use base64::Engine;
        let raw = self.favicon.as_deref()?;
        let b64 = raw
            .strip_prefix("data:image/png;base64,")
            .or_else(|| raw.split_once(',').map(|(_, b)| b))?;
        base64::engine::general_purpose::STANDARD.decode(b64).ok()
    }
}

/// 内置精选：全球知名度较高、长期在线的 Java 服。
/// 只放公开地址，不含任何需要登录才能拿到的数据。
const BUILTIN: &[(&str, &str, u16)] = &[
    ("Hypixel", "mc.hypixel.net", 25565),
    ("Wynncraft", "play.wynncraft.com", 25565),
    ("2b2t", "2b2t.org", 25565),
    ("CubeCraft", "play.cubecraft.net", 25565),
    ("GommeHD", "mc.gommehd.net", 25565),
    ("PikaNetwork", "play.pika-network.net", 25565),
    ("Purple Prison", "play.purpleprison.net", 25565),
    ("MCC Island", "play.mccisland.net", 25565),
    ("Minehut", "play.minehut.com", 25565),
    ("BlocksMC", "play.blocksmc.com", 25565),
    ("DonutSMP", "play.donutsmp.net", 25565),
    ("Hypixel CN", "cn.hypixel.net", 25565),
];

/// 用户自定义条目（配置文件格式）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedServer {
    #[serde(default)]
    pub name: String,
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
}

fn default_port() -> u16 {
    25565
}

fn custom_path() -> std::path::PathBuf {
    crate::storage::config_dir().join("java_servers.json")
}

/// 读取用户自定义列表；文件不存在/损坏时返回空表（不阻断内置列表）
pub fn custom_servers() -> Vec<SavedServer> {
    let text = match std::fs::read_to_string(custom_path()) {
        Ok(t) => t,
        Err(_) => return Vec::new(),
    };
    serde_json::from_str(&text).unwrap_or_default()
}

fn save_custom(list: &[SavedServer]) -> Result<()> {
    let text = serde_json::to_string_pretty(list)?;
    crate::storage::write_config_file("java_servers.json", &text)
}

/// 解析 `host` / `host:port` / `[ipv6]:port`。
pub fn parse_addr(input: &str) -> Result<(String, u16)> {
    let s = input.trim();
    if s.is_empty() {
        return Err(anyhow!("地址为空"));
    }
    if let Some(rest) = s.strip_prefix('[') {
        let (host, tail) = rest
            .split_once(']')
            .ok_or_else(|| anyhow!("IPv6 地址缺少 `]`"))?;
        let port = match tail.strip_prefix(':') {
            Some(p) => p.parse().map_err(|_| anyhow!("端口不是数字"))?,
            None => 25565,
        };
        return Ok((host.to_string(), port));
    }
    match s.rsplit_once(':') {
        Some((host, port)) if !host.is_empty() && port.chars().all(|c| c.is_ascii_digit()) => {
            Ok((host.to_string(), port.parse()?))
        }
        _ => Ok((s.to_string(), 25565)),
    }
}

/// 追加或更新一个自定义服务器。`name` 为空时用 host 当名字。
pub fn add_custom(input: &str, name: &str) -> Result<SavedServer> {
    let (host, port) = parse_addr(input)?;
    let mut list = custom_servers();
    if let Some(item) = list.iter_mut().find(|s| s.host == host && s.port == port) {
        if !name.trim().is_empty() {
            item.name = name.trim().to_string();
        }
        let out = item.clone();
        save_custom(&list)?;
        return Ok(out);
    }
    let item = SavedServer {
        name: if name.trim().is_empty() {
            host.clone()
        } else {
            name.trim().to_string()
        },
        host,
        port,
    };
    list.push(item.clone());
    save_custom(&list)?;
    Ok(item)
}

/// 删除一个自定义服务器（也接受 host:port）。
pub fn remove_custom(input: &str) -> Result<bool> {
    let (host, port) = parse_addr(input)?;
    let mut list = custom_servers();
    let before = list.len();
    list.retain(|s| !(s.host == host && s.port == port));
    let removed = list.len() != before;
    if removed {
        save_custom(&list)?;
    }
    Ok(removed)
}

/// 按名称或地址在内置/自定义列表中查找；找不到时把输入当作 host[:port]。
pub fn lookup(query: &str) -> Option<JavaServer> {
    let q = query.trim();
    if q.is_empty() {
        return None;
    }
    for (name, host, port) in BUILTIN {
        if name.eq_ignore_ascii_case(q)
            || host.eq_ignore_ascii_case(q)
            || format!("{host}:{port}") == q
        {
            return Some(JavaServer {
                name: name.to_string(),
                host: host.to_string(),
                port: *port,
                builtin: true,
                ..Default::default()
            });
        }
    }
    for s in custom_servers() {
        let name = if s.name.trim().is_empty() { &s.host } else { &s.name };
        if name.eq_ignore_ascii_case(q)
            || s.host.eq_ignore_ascii_case(q)
            || format!("{}:{}", s.host, s.port) == q
        {
            return Some(JavaServer {
                name: name.to_string(),
                host: s.host,
                port: s.port,
                builtin: false,
                ..Default::default()
            });
        }
    }
    let (host, port) = parse_addr(q).ok()?;
    Some(JavaServer {
        name: host.clone(),
        host,
        port,
        ..Default::default()
    })
}

/// 查找并立即 ping，返回带状态（或 `error`）的条目。
pub async fn probe(query: &str) -> Result<JavaServer> {
    let mut srv = lookup(query).ok_or_else(|| anyhow!("服务器地址为空"))?;
    match ping(&srv.host, srv.port).await {
        Ok(p) => {
            srv.version = p.version;
            srv.protocol = p.protocol;
            srv.online = p.online;
            srv.max = p.max;
            srv.motd = p.motd;
            srv.favicon = p.favicon;
            srv.latency_ms = Some(p.latency_ms);
        }
        Err(e) => srv.error = Some(format!("{e:#}")),
    }
    Ok(srv)
}

/// 一次 ping 的原始结果
#[derive(Debug, Clone)]
struct Pong {
    version: String,
    protocol: i64,
    online: i64,
    max: i64,
    motd: String,
    favicon: Option<String>,
    latency_ms: u64,
}

/// 加载内置 + 自定义列表，并发 ping。永远返回条目：
/// ping 失败的服务器 `error` 有值，但地址仍在。
pub async fn load_all() -> Vec<JavaServer> {
    let mut targets: Vec<(String, String, u16, bool)> = BUILTIN
        .iter()
        .map(|(n, h, p)| (n.to_string(), h.to_string(), *p, true))
        .collect();
    for s in custom_servers() {
        let name = if s.name.trim().is_empty() {
            s.host.clone()
        } else {
            s.name
        };
        targets.push((name, s.host, s.port, false));
    }

    let mut set = tokio::task::JoinSet::new();
    for (idx, (name, host, port, builtin)) in targets.into_iter().enumerate() {
        set.spawn(async move {
            let mut srv = JavaServer {
                name,
                host: host.clone(),
                port,
                builtin,
                ..Default::default()
            };
            match ping(&host, port).await {
                Ok(p) => {
                    srv.version = p.version;
                    srv.protocol = p.protocol;
                    srv.online = p.online;
                    srv.max = p.max;
                    srv.motd = p.motd;
                    srv.favicon = p.favicon;
                    srv.latency_ms = Some(p.latency_ms);
                }
                Err(e) => srv.error = Some(format!("{e:#}")),
            }
            (idx, srv)
        });
    }
    let mut got = Vec::new();
    while let Some(joined) = set.join_next().await {
        match joined {
            Ok(item) => got.push(item),
            Err(e) => eprintln!("java server ping task panicked: {e}"),
        }
    }
    got.sort_by_key(|(idx, _)| *idx);
    got.into_iter().map(|(_, s)| s).collect()
}

/// 原版 Server List Ping（握手 + status）。5 秒超时。
pub async fn ping(host: &str, port: u16) -> Result<Pong> {
    let began = Instant::now();
    let mut sock = tokio::time::timeout(Duration::from_secs(5), TcpStream::connect((host, port)))
        .await
        .map_err(|_| anyhow!("连接超时"))?
        .map_err(|e| anyhow!("连接失败: {e}"))?;

    // handshake: packet id 0, protocol 767(1.21), host, port, next_state=status(1)
    let mut pkt = Vec::new();
    write_varint(&mut pkt, 0)?;
    write_varint(&mut pkt, 767)?;
    write_string(&mut pkt, host)?;
    pkt.extend_from_slice(&port.to_be_bytes());
    write_varint(&mut pkt, 1)?;

    let mut out = Vec::with_capacity(pkt.len() + 5);
    write_varint(&mut out, pkt.len() as i64)?;
    out.extend_from_slice(&pkt);
    // status request: 长度 1，packet id 0
    out.extend_from_slice(&[0x01, 0x00]);
    sock.write_all(&out).await?;

    let len = read_varint(&mut sock).await?;
    if !(1..=2 * 1024 * 1024).contains(&len) {
        return Err(anyhow!("响应长度异常: {len}"));
    }
    let mut body = vec![0u8; len as usize];
    tokio::time::timeout(Duration::from_secs(5), sock.read_exact(&mut body))
        .await
        .map_err(|_| anyhow!("读取响应超时"))??;
    let latency_ms = began.elapsed().as_millis() as u64;

    let mut cur = std::io::Cursor::new(body);
    let packet_id = read_varint_sync(&mut cur)?;
    if packet_id != 0 {
        return Err(anyhow!("意外的 packet id: {packet_id}"));
    }
    let json_len = read_varint_sync(&mut cur)?;
    if !(0..=2 * 1024 * 1024).contains(&json_len) {
        return Err(anyhow!("JSON 长度异常: {json_len}"));
    }
    let pos = cur.position() as usize;
    let json = cur.get_ref();
    if pos + json_len as usize > json.len() {
        return Err(anyhow!("响应被截断"));
    }
    let v: Value = serde_json::from_slice(&json[pos..pos + json_len as usize])?;

    let version = v
        .get("version")
        .and_then(|x| x.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("未知")
        .to_string();
    let protocol = v
        .get("version")
        .and_then(|x| x.get("protocol"))
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let players = v.get("players");
    let online = players
        .and_then(|p| p.get("online"))
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let max = players
        .and_then(|p| p.get("max"))
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let mut motd = String::new();
    if let Some(desc) = v.get("description") {
        flatten_chat(desc, &mut motd);
    }
    let motd = clean_motd(&motd);
    let favicon = v
        .get("favicon")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    Ok(Pong {
        version,
        protocol,
        online,
        max,
        motd,
        favicon,
        latency_ms,
    })
}

/// 把聊天组件（string / {text, extra} / 数组）拍平成纯文本
fn flatten_chat(v: &Value, out: &mut String) {
    match v {
        Value::String(s) => out.push_str(s),
        Value::Array(items) => {
            for item in items {
                flatten_chat(item, out);
            }
        }
        Value::Object(obj) => {
            if let Some(t) = obj.get("text").and_then(Value::as_str) {
                out.push_str(t);
            }
            if let Some(extra) = obj.get("extra") {
                flatten_chat(extra, out);
            }
        }
        _ => {}
    }
}

/// 去掉 § 颜色代码、折叠空白、限制长度（MOTD 是外部数据，不全信）
fn clean_motd(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '§' {
            chars.next();
        } else if c == '\n' || c == '\r' {
            if !out.ends_with(' ') {
                out.push(' ');
            }
        } else {
            out.push(c);
        }
    }
    let out = out.split_whitespace().collect::<Vec<_>>().join(" ");
    out.chars().take(120).collect()
}

fn write_varint(buf: &mut Vec<u8>, value: i64) -> Result<()> {
    if !(0..=u32::MAX as i64).contains(&value) {
        return Err(anyhow!("VarInt 越界: {value}"));
    }
    let mut n = value as u32;
    loop {
        let b = (n & 0x7F) as u8;
        n >>= 7;
        if n != 0 {
            buf.push(b | 0x80);
        } else {
            buf.push(b);
            return Ok(());
        }
    }
}

fn write_string(buf: &mut Vec<u8>, s: &str) -> Result<()> {
    write_varint(buf, s.len() as i64)?;
    buf.extend_from_slice(s.as_bytes());
    Ok(())
}

async fn read_varint<R: AsyncReadExt + Unpin>(r: &mut R) -> Result<i64> {
    let mut result: u32 = 0;
    for shift in (0..35).step_by(7) {
        let mut b = [0u8; 1];
        r.read_exact(&mut b).await?;
        result |= ((b[0] & 0x7F) as u32) << shift;
        if b[0] & 0x80 == 0 {
            return Ok(result as i64);
        }
    }
    Err(anyhow!("VarInt 过长"))
}

fn read_varint_sync<R: std::io::Read>(r: &mut R) -> Result<i64> {
    let mut result: u32 = 0;
    for shift in (0..35).step_by(7) {
        let mut b = [0u8; 1];
        r.read_exact(&mut b)?;
        result |= ((b[0] & 0x7F) as u32) << shift;
        if b[0] & 0x80 == 0 {
            return Ok(result as i64);
        }
    }
    Err(anyhow!("VarInt 过长"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_addr_forms() {
        assert_eq!(parse_addr("mc.hypixel.net").unwrap(), ("mc.hypixel.net".into(), 25565));
        assert_eq!(parse_addr("example.com:25566").unwrap(), ("example.com".into(), 25566));
        assert_eq!(parse_addr("[::1]:25567").unwrap(), ("::1".into(), 25567));
        assert_eq!(parse_addr("[::1]").unwrap(), ("::1".into(), 25565));
        assert!(parse_addr("  ").is_err());
    }

    #[test]
    fn flattens_chat_components() {
        let v: Value = serde_json::json!({
            "text": "Hello ",
            "extra": [{"text": "world", "extra": [{"text": "!"}]}, " ok"]
        });
        let mut out = String::new();
        flatten_chat(&v, &mut out);
        assert_eq!(out, "Hello world! ok");
    }

    #[test]
    fn cleans_motd_and_strips_codes() {
        assert_eq!(clean_motd("§aA §lb\n\n c"), "A b c");
        assert_eq!(clean_motd(&"x".repeat(300)).len(), 120);
    }

    #[test]
    fn varint_roundtrip() {
        for n in [0i64, 1, 127, 128, 25565, 767, 774, 2_097_151, 2_147_483_647] {
            let mut buf = Vec::new();
            write_varint(&mut buf, n).unwrap();
            let mut cur = std::io::Cursor::new(buf);
            assert_eq!(read_varint_sync(&mut cur).unwrap(), n);
        }
    }

    #[test]
    fn favicon_decodes_data_url() {
        let s = JavaServer {
            favicon: Some("data:image/png;base64,iVBORw0KGgo=".into()),
            ..Default::default()
        };
        assert_eq!(s.favicon_bytes().unwrap(), b"\x89PNG\r\n\x1a\n");
    }

    /// 真实网络测试：`cargo test -- --ignored live_ping`
    #[tokio::test]
    #[ignore]
    async fn live_ping() {
        let p = ping("mc.hypixel.net", 25565).await.expect("ping 失败");
        assert!(!p.version.is_empty());
    }
}
