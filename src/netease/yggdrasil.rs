use anyhow::{anyhow, Result};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use crate::api::netease::{EntityUserInfo, GameProfile};
use crate::netease::crypto::{
    aes_ecb_encrypt, pack_message, rsa_private_encrypt, rsa_public_verify, sha256, skip32_encrypt,
    unpack_message, xor_bytes,
};

const AUTH_SERVER_LIST: &str = "https://x19.update.netease.com/authserver.list";
/// 进服签名盐的兜底候选（历史公开值 + 风格近似常量），命中值缓存到 `crc_salt.txt`。
///
/// 新增候选：登录响应 `entity.sead`（服务端下发的 seed，待实网验证）：
/// - 官方启动器的登录解析器（`mcl.common.dll` 的 `ParseLoginResponse`）逐字读取
///   `entity_id/account/token/sead/...`，`sead` 与 `token` 同级，证明它是登录响应字段。
/// - `sead` 可能随会话轮换，不能写进 `crc_salt.txt`，也不能在日志里打原值
///   （见 `join_server` 的失败分支）。
/// - 旧结论「盐不在任何网络响应里」来自只查了 `authserver.list` 与 Yggdrasil 信令；
///   登录响应是另一条链路，`sead` 是否就是 crc_salt 仍未证实。
///
/// 其余事实不变：
/// - `crc_salt` 原样作为 UTF-8 字节拼进 `BuildSign` / `BuildHashData`，没有 KDF。
/// - 服务端按当前 x19 版本重算 `sha256(BuildSign)`，对不上回 `status=0x11`。
/// - 历史候选随版本失效，只作兜底；`sead` 失败后再走认证插件。
/// - 已剔除 `0eGsBkhl`：实证为 HTTP DynamicToken 的盐，与 crc_salt 无关。
const SALT_CANDIDATES: [&str; 10] = [
    // 历史公开值（版本已过期，通常会被 0x11 拒）
    "B73962A7833192F9CAD0D68A2AA4462E",
    "22AC4B0143EFFC80F2905B267D4D84D3",
    "72e4b414",
    "847e4751",
    "0d48ac91",
    "91C614D1",
    "a5f19620",
    "b16c7cc5",
    "EB193ED8",
    "BDC6B33A",
];
const TCP_SALT: [u8; 16] = [
    47, 132, 174, 163, 153, 33, 41, 38, 218, 191, 149, 163, 171, 175, 55, 224,
];
const MC_VERSION_SALT: [u8; 9] = [1, 0, 4, 128, 210, 62, 247, 17, 1];
const TOKEN_KEY: [u8; 16] = [
    172, 36, 156, 105, 199, 44, 179, 180, 78, 192, 204, 108, 84, 58, 129, 149,
];

fn salt_cache_file() -> std::path::PathBuf {
    crate::storage::config_dir().join("crc_salt.txt")
}

fn cached_salt() -> Option<String> {
    std::fs::read_to_string(salt_cache_file())
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn cache_salt(salt: &str) {
    let _ = std::fs::create_dir_all(crate::storage::config_dir());
    let _ = std::fs::write(salt_cache_file(), salt);
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct YggdrasilServer {
    #[serde(rename = "IP", default)]
    pub ip: String,
    #[serde(rename = "Port", default)]
    pub port: u16,
}

/// 拉取认证服务器列表并随机选一台
pub async fn random_auth_server() -> Result<YggdrasilServer> {
    let resp = crate::api::client()?.get(AUTH_SERVER_LIST).send().await?;
    let text = resp.text().await?;
    let servers: Vec<YggdrasilServer> = serde_json::from_str(&text)
        .map_err(|e| anyhow!("解析 authserver.list 失败: {e}\n{text}"))?;
    let mut rng = rand::rng();
    use rand::Rng;
    let idx = rng.random_range(0..servers.len().max(1));
    servers
        .get(idx)
        .cloned()
        .ok_or_else(|| anyhow!("认证服务器列表为空"))
}

fn read_int16_le(stream: &mut TcpStream) -> Result<usize> {
    let mut b = [0u8; 2];
    stream.read_exact(&mut b)?;
    Ok(b[0] as usize | ((b[1] as usize) << 8))
}

fn write_int16_le(stream: &mut TcpStream, v: usize) -> Result<()> {
    stream.write_all(&[(v & 0xFF) as u8, ((v >> 8) & 0xFF) as u8])?;
    Ok(())
}

/// 读取 int16 长度前缀的完整消息
fn read_message(stream: &mut TcpStream) -> Result<Vec<u8>> {
    let len = read_int16_le(stream)?;
    if len == 0 || len > 65535 {
        return Err(anyhow!("非法消息长度: {len}"));
    }
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf)?;
    Ok(buf)
}

fn write_message(stream: &mut TcpStream, data: &[u8]) -> Result<()> {
    write_int16_le(stream, data.len())?;
    stream.write_all(data)?;
    stream.flush()?;
    Ok(())
}

// ---- 字段编码辅助 ----

fn put_int(buf: &mut Vec<u8>, v: i32) {
    buf.extend_from_slice(&v.to_le_bytes());
}

fn put_short(buf: &mut Vec<u8>, v: i32) {
    buf.extend_from_slice(&(v as u16).to_le_bytes());
}

fn put_short_be(buf: &mut Vec<u8>, v: i32) {
    buf.extend_from_slice(&(v as u16).to_be_bytes());
}

fn put_long(buf: &mut Vec<u8>, v: i64) {
    buf.extend_from_slice(&v.to_le_bytes());
}

fn put_byte_string(buf: &mut Vec<u8>, s: &str) {
    let bytes = s.as_bytes();
    buf.push(bytes.len() as u8);
    buf.extend_from_slice(bytes);
}

fn put_short_string(buf: &mut Vec<u8>, s: &str) {
    let bytes = s.as_bytes();
    put_short(buf, bytes.len() as i32);
    buf.extend_from_slice(bytes);
}

fn put_short_string_be(buf: &mut Vec<u8>, s: &str) {
    let bytes = s.as_bytes();
    put_short_be(buf, bytes.len() as i32);
    buf.extend_from_slice(bytes);
}

fn put_short_bytes(buf: &mut Vec<u8>, data: &[u8]) {
    put_short(buf, data.len() as i32);
    buf.extend_from_slice(data);
}

/// BuildSign：authId + AES(loginSeed) + X19版本 + "netease" + crcSalt + gameVersion + McVersionSalt
fn build_sign(
    profile: &GameProfile,
    auth_id: i32,
    seed_enc: &[u8],
    crc_salt: &str,
    x19: &str,
) -> Vec<u8> {
    let mut buf = Vec::new();
    put_int(&mut buf, auth_id);
    buf.extend_from_slice(seed_enc);
    buf.extend_from_slice(x19.as_bytes());
    buf.extend_from_slice(b"netease");
    buf.extend_from_slice(crc_salt.as_bytes());
    buf.extend_from_slice(profile.game_version.as_bytes());
    buf.extend_from_slice(&MC_VERSION_SALT);
    buf
}

/// BuildHashData：sha256(join 内容串 + userId(4B LE) + loginSeed)
fn build_hash_data(
    profile: &GameProfile,
    time: i32,
    user_id: i32,
    login_seed: &[u8],
    crc_salt: &str,
    x19: &str,
) -> Vec<u8> {
    let s = format!(
        "{}{}{}{}{}{}{}{}",
        x19,
        profile.game_version,
        time,
        crc_salt,
        profile.mods,
        profile.bootstrap_md5,
        profile.dat_file_md5,
        "[]"
    );
    let mut buf = s.as_bytes().to_vec();
    buf.extend_from_slice(&user_id.to_le_bytes());
    buf.extend_from_slice(login_seed);
    sha256(&buf)
}

/// InitializeMessage（YggdrasilGenerator.GenerateInitializeMessage）
fn generate_initialize_message(
    profile: &GameProfile,
    login_seed: &[u8],
    sign_content: &[u8],
    crc_salt: &str,
    x19: &str,
) -> Result<Vec<u8>> {
    let user = &profile.profile.user;
    let auth_id = skip32_encrypt(user.user_id.parse::<i32>().unwrap_or(0), b"SaintSteve");
    let auth_token = xor_bytes(user.token.as_bytes(), &TOKEN_KEY);
    let seed_enc = aes_ecb_encrypt(login_seed, &auth_token)?;
    let sign_hash = sha256(&build_sign(profile, auth_id, &seed_enc, crc_salt, x19));
    // RSA 公钥验证 signContent：前 19 字节 + 校验 32 字节 == sha256(loginSeed)
    let verified = rsa_public_verify(sign_content)?;
    if verified.len() < 51 {
        return Err(anyhow!("signContent 长度不足: {}", verified.len()));
    }
    let content = verified[..19].to_vec();
    if verified[19..51].to_vec() != sha256(login_seed) {
        return Err(anyhow!("signContent 校验失败"));
    }
    let signed = rsa_private_encrypt(&[content, sign_hash.clone()].concat())?;
    let mut msg = Vec::new();
    put_int(&mut msg, auth_id);
    msg.extend_from_slice(&seed_enc);
    put_short_string_be(&mut msg, x19);
    put_byte_string(&mut msg, "netease");
    msg.extend_from_slice(&TCP_SALT);
    put_short_bytes(&mut msg, &signed);
    put_byte_string(&mut msg, &profile.game_version);
    msg.extend_from_slice(&MC_VERSION_SALT);
    Ok(msg)
}

/// JoinMessage（YggdrasilGenerator.GenerateJoinMessage）
fn generate_join_message(
    profile: &GameProfile,
    server_id: &str,
    login_seed: &[u8],
    time: i32,
    crc_salt: &str,
    x19: &str,
) -> Vec<u8> {
    let user = &profile.profile.user;
    let user_id = user.user_id.parse::<i32>().unwrap_or(0);
    let data = build_hash_data(profile, time, user_id, login_seed, crc_salt, x19);
    let mut msg = Vec::new();
    put_long(&mut msg, profile.game_id.parse::<i64>().unwrap_or(0));
    put_byte_string(&mut msg, server_id);
    put_byte_string(&mut msg, x19);
    put_byte_string(&mut msg, &profile.game_version);
    put_int(&mut msg, time);
    msg.extend_from_slice(&data);
    put_short_string(&mut msg, &profile.mods.to_string());
    put_short_string(&mut msg, "[]");
    put_short(&mut msg, 0);
    put_byte_string(&mut msg, "netease");
    msg
}

/// 进服签名的尝试顺序：登录下发的 seed → 缓存命中 → 内置候选。
///
/// 登录响应里的 `sead` 是服务端下发的 seed（官方解析器读取该键）；
/// 它是否就是 crc_salt 尚未实网证实，但必须排在历史候选之前：历史候选随版本失效，
/// 而且游戏侧 authlib 的超时只有约 1 秒，先试错值会把正确值饿死。
pub fn salt_attempt_order(preferred: Option<&str>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |s: &str| {
        let s = s.trim();
        if !s.is_empty() && !out.iter().any(|x| x == s) {
            out.push(s.to_string());
        }
    };
    if let Some(s) = preferred {
        push(s);
    }
    if let Some(s) = cached_salt() {
        push(&s);
    }
    for c in SALT_CANDIDATES {
        push(c);
    }
    out
}

/// 完整的 Yggdrasil JoinServer 认证流程（crc_salt 自愈）：
/// 优先用登录 seed，其次缓存命中盐；init 失败（服务端校验不过）自动换下一个候选重试，
/// 成功后把盐写入 ~/.config/mctl/crc_salt.txt 供下次直接命中（登录 seed 除外）。
pub async fn join_server(
    profile: &GameProfile,
    server_id: &str,
    preferred_salt: Option<&str>,
) -> Result<()> {
    let preferred = preferred_salt.map(str::trim).filter(|s| !s.is_empty());
    let tried = salt_attempt_order(preferred);
    let mut last_err = None;
    for salt in &tried {
        let is_login_seed = preferred == Some(salt.as_str());
        match attempt_join(profile, server_id, salt).await {
            Ok(()) => {
                if is_login_seed {
                    // 登录 seed 可能随会话轮换，缓存它只会让下一次登录拿旧值去撞 0x11
                    eprintln!("[ygg] 登录 seed 命中（随会话轮换，不落盘）");
                } else if cached_salt().as_deref() != Some(salt.as_str()) {
                    cache_salt(salt);
                    eprintln!("[ygg] crc_salt 命中并缓存: {salt}");
                }
                return Ok(());
            }
            Err(e) => {
                // 登录 seed 属于会话凭据，日志里不落原值
                let label = if is_login_seed {
                    "<登录 seed>"
                } else {
                    salt.as_str()
                };
                eprintln!("[ygg] crc_salt={label} 认证未过: {e:#}");
                last_err = Some(e);
            }
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow!("无可用 crc_salt 候选")))
}

/// 连上随机认证服务器并跑完 Initialize，返回 `(stream, login_seed)` 供后续 Join 使用。
/// **salt 就是在这一步被校验的**：服务端用自己的 salt 重算 BuildSign 再验 RSA 签名，
/// 对不上就拒（status != 0）。所以「Initialize 是否通过」单独就足以判定 salt 对错，
/// 不必走完整个 Join——这也是 `probe_salt` 的依据。
async fn init_session(profile: &GameProfile, salt: &str) -> Result<(TcpStream, Vec<u8>)> {
    let _ = crate::api::netease::refresh_x19_version().await;
    let x19 = crate::api::netease::x19_version();
    let server = random_auth_server().await?;
    let mut stream = TcpStream::connect_timeout(
        &format!("{}:{}", server.ip, server.port).parse()?,
        Duration::from_secs(10),
    )?;
    stream.set_read_timeout(Some(Duration::from_secs(15)))?;
    stream.set_write_timeout(Some(Duration::from_secs(15)))?;

    // 1. 读握手：int16 LE 长度 + loginSeed(16) + signContent(256)
    let mut head = [0u8; 2];
    stream.read_exact(&mut head)?;
    let len = (head[0] as usize) | ((head[1] as usize) << 8);
    if len < 272 {
        return Err(anyhow!("认证服务器响应过短: {len}"));
    }
    let mut payload = vec![0u8; len];
    stream.read_exact(&mut payload)?;
    let login_seed = payload[..16].to_vec();
    let sign_content = payload[16..272].to_vec();

    // 2. 发送 InitializeMessage
    let init_msg = generate_initialize_message(profile, &login_seed, &sign_content, salt, &x19)?;
    write_message(&mut stream, &init_msg)?;

    // 3. 读初始化结果：1 字节 0 = 成功
    let status = read_message(&mut stream)?;
    if status.first() != Some(&0) {
        return Err(anyhow!(
            "初始化失败 status=0x{:02X}",
            status.first().unwrap_or(&0xFF)
        ));
    }

    Ok((stream, login_seed))
}

/// 单次完整认证（一条 TCP 连接：Initialize → ChaCha8 Join）
async fn attempt_join(profile: &GameProfile, server_id: &str, salt: &str) -> Result<()> {
    let (mut stream, login_seed) = init_session(profile, salt).await?;
    // init_session 里已 refresh 过，这里再取一次（x19_version 只读缓存，不联网）
    let x19 = crate::api::netease::x19_version();

    // 4. ChaCha8 打包 JoinMessage（type 9）
    let user = &profile.profile.user;
    let auth_token = xor_bytes(user.token.as_bytes(), &TOKEN_KEY);
    let pack_key = [auth_token.clone(), login_seed.clone()].concat();
    let time = chrono_unix_now();
    let join_msg = generate_join_message(profile, server_id, &login_seed, time, salt, &x19);
    let packed = pack_message(9, &join_msg, &pack_key)?;
    stream.write_all(&packed)?;
    stream.flush()?;

    // 5. 读响应并校验
    let resp = read_message(&mut stream)?;
    let unpack_key = [login_seed.clone(), auth_token.clone()].concat();
    let (t, body) = unpack_message(&resp, &unpack_key)?;
    if t != 9 || body.first() != Some(&0) {
        let status = *body.first().unwrap_or(&0xFF);
        // 状态码语义（实测取证，别再照抄旧表的「0x11=会话无效」）：
        //   0x11 = 签名校验不过（crc_salt 与当前 x19 版本不匹配）
        //   0xFF = 会话无效（未登录 / 缺 login-start·game-start / 被异地登录踢掉）
        // 0x11 是盐的问题，不是会话的问题——先换盐，别去折腾登录态。
        let hint = match status {
            0x11 => " —— 0x11 表示签名校验失败：当前 crc_salt 与 x19 版本不匹配（换盐，不是会话问题）",
            0xFF => " —— 0xFF 表示会话无效：确认已 login-start/game-start，且账号未被异地登录踢下",
            _ => "",
        };
        return Err(anyhow!("Join 失败: type={t} status=0x{status:02X}{hint}"));
    }
    Ok(())
}

/// 盐探测：只跑到 Initialize。`Ok` = salt 正确；`Err` = 被服务端拒 = salt 不对。
/// 刻意不复用 `attempt_join` 的后半段（Join 需要真实 serverId），也**不写缓存**——
/// 这是只读诊断工具，探测结果该不该固化由人决定。
pub async fn probe_salt(profile: &GameProfile, salt: &str) -> Result<()> {
    init_session(profile, salt).await.map(|_| ())
}

/// 要挨个试的盐列表：缓存命中值优先 → `SALT_CANDIDATES` → 可选的额外值，去重保序。
pub fn salt_candidates(extra: Option<&str>) -> Vec<String> {
    let mut out = salt_attempt_order(None);
    if let Some(e) = extra.map(str::trim).filter(|e| !e.is_empty()) {
        if !out.iter().any(|x| x == e) {
            out.push(e.to_string());
        }
    }
    out
}

fn chrono_unix_now() -> i32 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i32)
        .unwrap_or(0)
}

/// 构造标准 GameProfile（默认联机 gameId，用于探测工具）
/// 构造标准 GameProfile（CreateAuthenticator 语义：gameId 由调用方传入）
pub fn make_profile_with_game(
    user: &EntityUserInfo,
    game_id: &str,
    game_version: &str,
    mods_json: &str,
) -> GameProfile {
    let (bootstrap, dat) = crate::api::netease::md5_pair(game_version);
    GameProfile {
        game_id: game_id.to_string(),
        game_version: game_version.to_string(),
        bootstrap_md5: bootstrap,
        dat_file_md5: dat,
        mods: serde_json::from_str(mods_json).unwrap_or(serde_json::Value::Null),
        profile: crate::api::netease::UserProfile { user: user.clone() },
    }
}

#[cfg(test)]
mod tests {
    use super::salt_candidates;

    /// 候选表：去重、保序、额外值追加且不重复
    /// /// 注意 salt_candidates 会先读缓存命中值（内容依赖环境），所以这里只断言
    /// 与缓存无关的不变式。
    #[test]
    fn salt_candidates_are_unique_and_keep_extra_last() {
        let all = salt_candidates(None);
        let mut seen = std::collections::HashSet::new();
        for c in &all {
            assert!(seen.insert(c.clone()), "候选表有重复: {c}");
            assert!(!c.is_empty(), "候选表有空串");
        }
        assert!(
            all.len() >= super::SALT_CANDIDATES.len(),
            "候选表不该少于 SALT_CANDIDATES 的长度: {}",
            all.len()
        );
    }

    #[test]
    fn salt_candidates_appends_extra_without_duplicating() {
        let extra = "ZZ_EXTRA_TEST_SALT";
        let with = salt_candidates(Some(extra));
        assert_eq!(
            with.iter().filter(|c| c.as_str() == extra).count(),
            1,
            "额外值应恰好出现一次"
        );
        assert_eq!(
            with.last().map(String::as_str),
            Some(extra),
            "额外值应排在最后"
        );

        // 已在表里的值不应被追加第二份
        let dup = with[0].clone();
        let again = salt_candidates(Some(&dup));
        assert_eq!(
            again.iter().filter(|c| *c == dup.as_str()).count(),
            1,
            "重复的额外值不应产生第二份"
        );
    }

    #[test]
    fn salt_candidates_ignores_blank_extra() {
        for blank in ["", "   "] {
            assert_eq!(
                salt_candidates(Some(blank)),
                salt_candidates(None),
                "空白额外值应被忽略"
            );
        }
    }

    #[test]
    fn login_seed_is_tried_before_history() {
        let seed = "ZZ_LOGIN_SEED_TEST";
        let all = salt_attempt_order(Some(seed));
        assert_eq!(
            all.first().map(String::as_str),
            Some(seed),
            "登录下发的 seed 必须最先尝试，否则 1 秒超时内轮不到它"
        );
        assert_eq!(
            all.iter().filter(|s| s.as_str() == seed).count(),
            1,
            "登录 seed 不应重复"
        );
    }

    #[test]
    fn blank_login_seed_is_ignored() {
        assert_eq!(salt_attempt_order(Some("  ")), salt_attempt_order(None));
    }

    use super::*;

    #[test]
    fn hash_data_format() {
        // 简单结构测试：长度 = 字符串 + 4 + 16 + 32
        let profile = make_profile_with_game(
            &EntityUserInfo {
                token: "t".into(),
                user_id: "1".into(),
            },
            "1234567890",
            "1.20",
            "[]",
        );
        let h = build_hash_data(
            &profile,
            12345,
            1,
            &[0u8; 16],
            SALT_CANDIDATES[0],
            crate::api::LATEST_VERSION,
        );
        assert_eq!(h.len(), 32);
    }
}
