//! 网易联机本地代理：监听本机端口，转发到网易 mcserver_host:port，
//! 并拦截关键包（握手地址改写 / 登录名替换 / 加密请求触发 Yggdrasil JoinServer）。
//!
//! 对应 `` + `GameConnection`。

use std::sync::Arc;

use anyhow::{anyhow, Result};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::api::netease::EntityUserInfo;
use crate::netease::yggdrasil;

/// 网易联机代理配置。game_id 暂未使用（网易 SDK 后续接入）
/// **没有 crc_salt 字段**：Join 走 `yggdrasil::join_server`，它自己按候选表自愈，
/// 这里再存一个盐只会让人误以为生效（曾经就写死过 "0eGsBkhl"，纯误导）。
#[allow(dead_code)]
pub struct ProxyConfig {
    pub forward_host: String,
    pub forward_port: u16,
    pub nick_name: String,
    pub game_version: String,
    pub game_id: String,
    pub user: EntityUserInfo,
}

/// 在指定端口（默认 25565）启动**本地**代理。返回监听的端口。
/// ponytail: 只绑 127.0.0.1。这个代理会用配置里的 user_token 去和网易租赁服
/// 完成 Yggdrasil 登录再转发流量，入站零认证——绑 0.0.0.0 等于把已认证的
/// 会话开放给整个局域网，咖啡馆 WiFi 上谁都能连进来顶你的角色名进服。
/// 真要监听其它地址就显式改这里，并且必须先加认证。
pub async fn start(port: u16, cfg: Arc<ProxyConfig>) -> Result<u16> {
    let listener = TcpListener::bind(format!("127.0.0.1:{port}")).await?;
    let local_port = listener.local_addr()?.port();
    let cfg_clone = Arc::clone(&cfg);
    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((client, _)) => {
                    let cfg = Arc::clone(&cfg_clone);
                    tokio::spawn(async move {
                        if let Err(e) = handle_connection(client, cfg).await {
                            eprintln!("[proxy] 连接处理失败: {e}");
                        }
                    });
                }
                Err(e) => {
                    eprintln!("[proxy] accept 失败: {e}");
                }
            }
        }
    });
    Ok(local_port)
}

/// 单条客户端连接的完整生命周期
async fn handle_connection(client: TcpStream, cfg: Arc<ProxyConfig>) -> Result<()> {
    let mut client = client;
    client.set_nodelay(true)?;

    // 1. 读握手包（客户端 → 服务端第一包）
    let handshake = read_packet(&mut client).await?;
    let parsed = parse_handshake(&handshake)?;
    let proto = parsed.protocol_version;

    // 2. 连接真实服务器
    let mut server = TcpStream::connect((cfg.forward_host.as_str(), cfg.forward_port)).await?;
    server.set_nodelay(true)?;

    // 3. 改写握手包地址（FML/Forge 标记）
    let rewritten = rewrite_handshake(&handshake, &parsed, &cfg)?;
    eprintln!(
        "[proxy] 握手改写: {} → {}",
        parsed.address, cfg.forward_host
    );
    eprintln!(
        "[proxy] 改写包: {:?}",
        &rewritten[..rewritten.len().min(60)]
    );
    write_packet(&mut server, &rewritten).await?;

    // 4. 登录阶段：读 LoginStart 并替换玩家名
    let login_start = read_packet(&mut client).await?;
    let rewritten_login = rewrite_login_start(&login_start, &cfg)?;
    eprintln!(
        "[proxy] LoginStart 改写: {} 包={:?}",
        cfg.nick_name,
        &rewritten_login[..rewritten_login.len().min(40)]
    );
    write_packet(&mut server, &rewritten_login).await?;

    // 5. 加密请求（服务端 → 客户端）：触发 Yggdrasil JoinServer
    eprintln!("[proxy] 等待服务端 EncryptionRequest…");
    let enc_req = read_packet(&mut server).await?;
    handle_encryption_request(&enc_req, &cfg, proto).await?;
    write_packet(&mut client, &enc_req).await?;

    // 6. 后续流量透明转发（客户端发起 RSA 加密响应后进入密文阶段）
    let (c2s, s2c) = tokio::io::copy_bidirectional(&mut client, &mut server).await?;
    eprintln!("[proxy] 连接关闭 c2s={c2s} s2c={s2c}");
    Ok(())
}

/// 读取一个 MC 协议包（VarInt 长度前缀 + 载荷）
async fn read_packet(stream: &mut TcpStream) -> Result<Vec<u8>> {
    let mut len_buf = [0u8; 5];
    let mut len = 0u32;
    for i in 0..5 {
        let mut b = [0u8; 1];
        stream.read_exact(&mut b).await?;
        len_buf[i] = b[0];
        len |= ((b[0] & 0x7F) as u32) << (7 * i);
        if b[0] & 0x80 == 0 {
            break;
        }
    }
    if len == 0 || len > 0x7FFFFF {
        return Err(anyhow!("非法包长度: {len}"));
    }
    let len = len as usize;
    let mut payload = vec![0u8; len];
    stream.read_exact(&mut payload).await?;
    let mut pkt = len_buf[..varint_len(&len_buf)].to_vec();
    pkt.extend_from_slice(&payload);
    Ok(pkt)
}

fn varint_len(buf: &[u8; 5]) -> usize {
    buf.iter()
        .position(|b| b & 0x80 == 0)
        .map(|i| i + 1)
        .unwrap_or(5)
}

async fn write_packet(stream: &mut TcpStream, pkt: &[u8]) -> Result<()> {
    stream.write_all(pkt).await?;
    stream.flush().await?;
    Ok(())
}

/// 解析握手包：VarInt 协议版本 / String 地址 / u16 端口 / VarInt 下一状态
fn parse_handshake(pkt: &[u8]) -> Result<HandshakeInfo> {
    // 去掉 VarInt 长度前缀 + 包 ID(0x00)
    eprintln!(
        "[parse] lenprefix={} raw={:?}",
        varint_len(&pkt_to_len(pkt)),
        &pkt[..pkt.len().min(20)]
    );
    let mut p = &pkt[varint_len(&pkt_to_len(pkt))..];
    let (_, rest) = read_varint(p)?;
    p = rest;
    let (proto, rest) = read_varint(p)?;
    p = rest;
    let (addr, rest) = read_string(p)?;
    p = rest;
    if p.len() < 2 {
        return Err(anyhow!("握手包缺少端口"));
    }
    let port = u16::from_be_bytes([p[0], p[1]]);
    Ok(HandshakeInfo {
        protocol_version: proto,
        address: addr,
        port,
    })
}

fn pkt_to_len(pkt: &[u8]) -> [u8; 5] {
    let mut b = [0u8; 5];
    let n = varint_len_from_bytes(pkt);
    b[..n].copy_from_slice(&pkt[..n]);
    b
}

fn varint_len_from_bytes(pkt: &[u8]) -> usize {
    pkt.iter()
        .take(5)
        .position(|b| b & 0x80 == 0)
        .map(|i| i + 1)
        .unwrap_or(1)
}

#[allow(dead_code)]
struct HandshakeInfo {
    protocol_version: u32,
    address: String,
    port: u16,
}

/// 改写握手包中的服务器地址 + 端口（FML/Forge 标记，按实测）
fn rewrite_handshake(pkt: &[u8], info: &HandshakeInfo, cfg: &ProxyConfig) -> Result<Vec<u8>> {
    let marker = if info.protocol_version > 763 {
        "\0FORGE"
    } else if info.protocol_version > 340 {
        "\0FML2\0"
    } else {
        "\0FML\0"
    };
    let new_addr = format!("{}{}", cfg.forward_host, marker);
    let mut out = Vec::with_capacity(pkt.len() + 16);
    out.extend_from_slice(&pkt[..varint_len_from_bytes(pkt)]); // 原长度前缀保留
    out.push(0); // 包 ID 0x00（握手）
    out.extend_from_slice(&varint_bytes(info.protocol_version));
    out.extend_from_slice(&string_bytes(&new_addr));
    out.extend_from_slice(&cfg.forward_port.to_be_bytes());
    // 下一状态（原包剩余部分从端口后继续）
    let mut p = &pkt[varint_len_from_bytes(pkt)..];
    let (_, rest) = read_varint(p)?; // 包 ID
    p = rest;
    let (_, rest) = read_varint(p)?; // 协议版本
    p = rest;
    let (_, rest) = read_string(p)?; // 原地址
    p = rest;
    let rest = &p[2..];
    // 重新计算长度前缀（body + 尾部）
    let body = out[varint_len_from_bytes(pkt)..].to_vec();
    let mut final_pkt = varint_bytes((body.len() + rest.len()) as u32);
    final_pkt.extend_from_slice(&body);
    final_pkt.extend_from_slice(rest);
    Ok(final_pkt)
}

/// 替换 LoginStart 玩家名（按实测）
fn rewrite_login_start(pkt: &[u8], cfg: &ProxyConfig) -> Result<Vec<u8>> {
    // 去掉 VarInt 长度前缀，保留包 ID(0x00) + 旧名字
    let mut p = &pkt[varint_len_from_bytes(pkt)..];
    let (packet_id, rest) = read_varint(p)?;
    p = rest;
    let (_, rest) = read_string(p)?;
    p = rest;
    let tail = p.to_vec(); // 原包剩余（1.20.2+ 有 uuid 等字段）
    let mut body = varint_bytes(packet_id);
    body.extend_from_slice(&string_bytes(&cfg.nick_name));
    body.extend_from_slice(&tail);
    let mut final_pkt = varint_bytes(body.len() as u32);
    final_pkt.extend_from_slice(&body);
    Ok(final_pkt)
}

/// 处理服务端加密请求：serverId = sha1(serverId + secret + pubkey)，触发 Yggdrasil JoinServer。
/// 注：代理自身不参与 RSA 握手，仅用服务端公钥+我们生成的 secret 计算 serverId。
/// 真实客户端会用自己的 secret 重算，因此这里生成一个随机 secret 仅用于计算 serverId 并向认证服务器注册。
async fn handle_encryption_request(pkt: &[u8], cfg: &Arc<ProxyConfig>, proto: u32) -> Result<()> {
    // 跳过 VarInt 长度前缀 + 包 ID(0x01)
    let mut p = &pkt[varint_len_from_bytes(pkt)..];
    let (_, rest) = read_varint(p)?;
    p = rest;
    let (server_id, rest) = read_string(p)?;
    p = rest;
    let (pubkey, _) = read_byte_array(p)?;

    // 生成临时 shared secret，构造标准 serverId（Mojang 语义：sha1 十六进制 + 负号处理）
    let mut secret = [0u8; 16];
    use rand::RngCore;
    rand::rng().fill_bytes(&mut secret);
    let digest = mc_server_id(&server_id, &secret, &pubkey)?;

    // 触发 Yggdrasil JoinServer
    let user = cfg.user.clone();
    let profile = yggdrasil::make_profile_with_game(&user, &cfg.game_id, &cfg.game_version, "[]");
    let sid = digest;
    tokio::spawn(async move {
        match yggdrasil::join_server(&profile, &sid, None).await {
            Ok(()) => eprintln!("[proxy] Yggdrasil JoinServer 成功 serverId={sid}"),
            Err(e) => eprintln!("[proxy] Yggdrasil JoinServer 失败: {e}"),
        }
    });
    let _ = proto;
    Ok(())
}

/// Mojang serverId：sha1(serverId + sharedSecret + publicKey)，
/// 负值十六进制处理（原版 authlib 语义）
fn mc_server_id(server_id: &str, secret: &[u8], public_key: &[u8]) -> Result<String> {
    use sha1::Digest;
    let mut hasher = sha1::Sha1::new();
    hasher.update(server_id.as_bytes());
    hasher.update(secret);
    hasher.update(public_key);
    let digest = hasher.finalize();
    let hex = digest
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect::<String>();
    // 若首字节高位为 1，视为负数补码
    if digest[0] & 0x80 != 0 {
        let mut neg = [0u8; 20];
        neg.copy_from_slice(&digest);
        let mut borrow = true;
        for b in neg.iter_mut() {
            let inv = (!*b) as u16;
            let (v, b2) = if borrow {
                let t = inv + 1;
                (t as u8, t > 0xFF)
            } else {
                (inv as u8, false)
            };
            *b = v;
            borrow = b2;
        }
        let hex2 = neg.iter().map(|b| format!("{:02x}", b)).collect::<String>();
        return Ok(format!("-{hex2}"));
    }
    Ok(hex)
}

// ---- VarInt / String / ByteArray 编码 ----

fn varint_bytes(mut v: u32) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let mut b = (v & 0x7F) as u8;
        v >>= 7;
        if v != 0 {
            b |= 0x80;
        }
        out.push(b);
        if v == 0 {
            break;
        }
    }
    out
}

fn read_varint(p: &[u8]) -> Result<(u32, &[u8])> {
    let mut v = 0u32;
    for i in 0..5 {
        let Some(&b) = p.get(i) else {
            return Err(anyhow!("VarInt 截断"));
        };
        v |= ((b & 0x7F) as u32) << (7 * i);
        if b & 0x80 == 0 {
            return Ok((v, &p[i + 1..]));
        }
    }
    Err(anyhow!("VarInt 过长"))
}

fn string_bytes(s: &str) -> Vec<u8> {
    let bytes = s.as_bytes();
    let mut out = varint_bytes(bytes.len() as u32);
    out.extend_from_slice(bytes);
    out
}

fn read_string(p: &[u8]) -> Result<(String, &[u8])> {
    let (len, rest) = read_varint(p)?;
    let len = len as usize;
    if len > 32767 || rest.len() < len {
        return Err(anyhow!("String 长度非法: {len}"));
    }
    let s = String::from_utf8(rest[..len].to_vec())?;
    Ok((s, &rest[len..]))
}

fn read_byte_array(p: &[u8]) -> Result<(Vec<u8>, &[u8])> {
    let (len, rest) = read_varint(p)?;
    let len = len as usize;
    if rest.len() < len {
        return Err(anyhow!("ByteArray 截断"));
    }
    Ok((rest[..len].to_vec(), &rest[len..]))
}
