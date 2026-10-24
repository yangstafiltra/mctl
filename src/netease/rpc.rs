//! 本地 RPC 服务（网易客户端的控制通道）
//!
//! 客户端网络层来自**平台核心库**里的 modlauncher（byte[] 版 NetworkSocket，见
//! `launcher/netease.rs` 的 core lib 安装）：客户端启动后连上本服务收发控制消息。
//! 帧协议：
//! 帧 = [u16 LE 长度][payload]
//! payload = [sid 2B 大端][参数 小端]
//! （sid 大端：客户端 `getSidMid` 读 `data[0] << 8 | data[1]`；参数全部小端：
//!   短整型读到后按字节反转还原）
//!
//! 客户端请求 → 回复（回复 sid 用客户端那侧的 SMID 常量）：
//! 1281(05 01) → 1799(07 07) 创建世界 / ReplyJoinGame [ip][port i32][room]
//! 1282(05 02) → 1796(07 04) 重连 / ReplyReconnect [ip][port i32][room][bool]
//! 4613(12 05) → 4613(12 05) 聊天检查 / ChatBanReply [bool][i64][i64]
//! 2050(08 02) → 2050(08 02) 皮肤 / LoadSkinReplyV2 [name][path][""][skinMode i32]
//! 4608(12 00) → 4608(12 00) 屏蔽词 / FilterWordReply [len i32][msg]
//! 2   (00 02) → 心跳请求：不回。客户端没有注册对应处理器，回什么都会被丢弃；
//!               旧代网络层里未注册 sid 还会把消息推进队列读崩游戏，沉默最安全。
//! 4612(12 04) → 无回复（对端只记日志）
//! 0   (00 00) → 登录帧（gameid + 通道令牌）→ 无回复
//!
//! 参数编码：bool → 1B；short/ushort → 2B LE；int → 4B LE；long → 8B LE；
//! string → [u16 LE 长度][utf8]。
//!
//! 回复 sid 必须是客户端**已注册**的：客户端按两张表查
//! （`replyHashMap` 同步 → 落队列；`replyAsyncHashMap` → 直接派发），两张表都没有就丢弃。
//! 表由随版本下发的核心 mod 初始化时建立（1799/1796/2050/4608/4613…）。

use anyhow::Result;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// 事件出口（一行文本）：客户端接入、请求进服等关键节点
pub type Event = Arc<dyn Fn(&str) + Send + Sync>;

/// 什么都不报的出口（调用方不关心进度时用）
pub fn silent() -> Event {
    Arc::new(|_: &str| {})
}

fn push_str(buf: &mut Vec<u8>, s: &str) {
    let b = s.as_bytes();
    buf.extend_from_slice(&(b.len() as u16).to_le_bytes());
    buf.extend_from_slice(b);
}

/// 启动 RPC 服务（port=0 自动分配），返回实际监听端口。
/// `on_event` 是给使用者看的进度/诊断文本（客户端接入、请求进服…）：
/// 进服成功与否的关键节点都发生在这里，不报出来就只能看游戏日志猜。
pub async fn start(
    port: u16,
    host: String,
    server_port: u16,
    role_name: String,
    on_event: Event,
) -> Result<u16> {
    let listener = TcpListener::bind(("127.0.0.1", port)).await?;
    let bound = listener.local_addr()?.port();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                break;
            };
            on_event("网易客户端已接入本地 RPC");
            let host = host.clone();
            let role = role_name.clone();
            let on_event = on_event.clone();
            tokio::spawn(async move {
                loop {
                    let mut len_buf = [0u8; 2];
                    if sock.read_exact(&mut len_buf).await.is_err() {
                        break;
                    }
                    let len = u16::from_le_bytes(len_buf) as usize;
                    let mut payload = vec![0u8; len];
                    if sock.read_exact(&mut payload).await.is_err() {
                        break;
                    }
                    if payload.len() < 2 {
                        continue;
                    }
                    let sid = u16::from_be_bytes([payload[0], payload[1]]);
                    let reply: Option<(u16, Vec<u8>)> = match sid {
                        // 创建世界请求 → 1799 ReplyJoinGame：mod 直接 ConnectScreen 连服
                        1281 => {
                            let mut b = Vec::new();
                            push_str(&mut b, &host);
                            b.extend_from_slice(&(server_port as u32).to_le_bytes());
                            push_str(&mut b, &role);
                            on_event(&format!("客户端请求进服 → 已回复 {host}:{server_port}"));
                            Some((1799, b))
                        }
                        // 重连请求 → 1796 ReplyReconnect
                        1282 => {
                            let mut b = Vec::new();
                            push_str(&mut b, &host);
                            b.extend_from_slice(&(server_port as u32).to_le_bytes());
                            push_str(&mut b, &role);
                            b.push(0); // reconnect = false
                            on_event(&format!("客户端请求重连 → 已回复 {host}:{server_port}"));
                            Some((1796, b))
                        }
                        // 聊天内容检查 → 4613 ChatBanReply（放行）
                        4613 => {
                            let mut b = Vec::new();
                            b.push(0); // not banned
                            b.extend_from_slice(&0u64.to_le_bytes());
                            b.extend_from_slice(&0u64.to_le_bytes());
                            Some((4613, b))
                        }
                        // 皮肤请求 → 2050 LoadSkinReplyV2（不加载皮肤，默认外观）
                        2050 => {
                            let mut b = Vec::new();
                            push_str(&mut b, "");
                            push_str(&mut b, "");
                            push_str(&mut b, "");
                            b.extend_from_slice(&0u32.to_le_bytes()); // skinMode = Default
                            Some((2050, b))
                        }
                        // 屏蔽词检查 → 4608 FilterWordReply（放行）
                        4608 => {
                            let mut b = Vec::new();
                            b.extend_from_slice(&0u32.to_le_bytes()); // length = 0
                            push_str(&mut b, "");
                            Some((4608, b))
                        }
                        // 心跳(2)、登录(0)、4612 等：无回复（对端没注册处理器，或只记日志）
                        _ => None,
                    };
                    if let Some((reply_sid, mut body)) = reply {
                        let mut frame = Vec::with_capacity(2 + 2 + body.len());
                        frame.extend_from_slice(&((2 + body.len()) as u16).to_le_bytes());
                        frame.extend_from_slice(&reply_sid.to_be_bytes());
                        frame.append(&mut body);
                        if sock.write_all(&frame).await.is_err() || sock.flush().await.is_err() {
                            break;
                        }
                    }
                }
            });
        }
    });
    Ok(bound)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpStream;
    use tokio::time::{timeout, Duration};

    async fn send_frame(s: &mut TcpStream, payload: &[u8]) {
        s.write_all(&(payload.len() as u16).to_le_bytes())
            .await
            .unwrap();
        s.write_all(payload).await.unwrap();
        s.flush().await.unwrap();
    }

    async fn recv_frame(s: &mut TcpStream) -> Vec<u8> {
        let mut lb = [0u8; 2];
        timeout(Duration::from_secs(2), s.read_exact(&mut lb))
            .await
            .unwrap()
            .unwrap();
        let len = u16::from_le_bytes(lb) as usize;
        let mut payload = vec![0u8; len];
        timeout(Duration::from_secs(2), s.read_exact(&mut payload))
            .await
            .unwrap()
            .unwrap();
        payload
    }

    #[tokio::test]
    async fn create_world_gets_reply_1799() {
        let port = start(0, "mc.example.com".into(), 25565, "Steve".into(), silent())
            .await
            .unwrap();
        let mut s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        // mod 发送 1281 = [05 01] BE + gameid 参数
        send_frame(&mut s, &[5u8, 1, 0, 0]).await;
        let reply = recv_frame(&mut s).await;
        // 回复 sid 1799 = [07 07] BE
        assert_eq!(reply[0..2], [7u8, 7]);
        // [u16 LE 主机长度][主机][u32 LE 端口][u16 LE 昵称长度][昵称]
        let (host, nick) = (b"mc.example.com".as_slice(), b"Steve".as_slice());
        let mut expect = Vec::new();
        expect.extend_from_slice(&(host.len() as u16).to_le_bytes());
        expect.extend_from_slice(host);
        expect.extend_from_slice(&25565u32.to_le_bytes());
        expect.extend_from_slice(&(nick.len() as u16).to_le_bytes());
        expect.extend_from_slice(nick);
        assert_eq!(&reply[2..], &expect[..]);
    }

    #[tokio::test]
    async fn chat_check_gets_reply_4613() {
        let port = start(0, "mc.example.com".into(), 25565, "Steve".into(), silent())
            .await
            .unwrap();
        let mut s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        send_frame(&mut s, &[0x12u8, 0x05, 0, 0]).await;
        let reply = recv_frame(&mut s).await;
        assert_eq!(reply[0..2], [0x12u8, 0x05]);
        assert_eq!(reply[2], 0); // not banned
        assert_eq!(&reply[3..11], &0u64.to_le_bytes()[..]);
        assert_eq!(&reply[11..19], &0u64.to_le_bytes()[..]);
    }

    #[tokio::test]
    async fn login_gets_no_reply() {
        let port = start(0, "mc.example.com".into(), 25565, "Steve".into(), silent())
            .await
            .unwrap();
        let mut s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        send_frame(&mut s, &[0u8, 0, 0, 0, 0, 0]).await;
        let mut lb = [0u8; 2];
        let r = timeout(Duration::from_millis(500), s.read_exact(&mut lb)).await;
        assert!(r.is_err(), "登录消息不应有回复（与实测一致）");
    }

    /// 心跳（客户端 sid 2）不回：客户端没有注册对应处理器，
    /// 旧代网络层里任何未注册 sid 还会被推进队列读崩游戏。
    #[tokio::test]
    async fn heartbeat_gets_no_reply() {
        let port = start(0, "mc.example.com".into(), 25565, "Steve".into(), silent())
            .await
            .unwrap();
        let mut s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        send_frame(&mut s, &[0u8, 2]).await;
        let mut lb = [0u8; 2];
        let r = timeout(Duration::from_millis(500), s.read_exact(&mut lb)).await;
        assert!(r.is_err(), "心跳不应有回复");
    }
}
