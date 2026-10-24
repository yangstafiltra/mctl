//! 网易 API 实网探针（**默认不跑**，需要凭据 + 网络）。
//!
//! 凭据从环境变量读，不写死在源码里：
//! ```sh
//! MCTL_UID=<user_id> MCTL_TOKEN=<user_token> cargo test -- --ignored probe
//! ```
//! 之前这里是硬编码的真实 token —— 拿到仓库就能冒充账号调网易 API。

#[cfg(test)]
mod probe {
    use crate::api::netease::*;

    /// 读探针凭据；缺任一项返回 None（测试自动跳过）
    fn creds() -> Option<(String, String)> {
        let uid = std::env::var("MCTL_UID").ok()?;
        let tok = std::env::var("MCTL_TOKEN").ok()?;
        if uid.is_empty() || tok.is_empty() {
            return None;
        }
        Some((uid, tok))
    }

    #[tokio::test]
    #[ignore = "需要 MCTL_UID / MCTL_TOKEN 环境变量 + 实网"]
    async fn probe_addr() {
        let Some((uid, tok)) = creds() else {
            eprintln!("跳过：未设置 MCTL_UID / MCTL_TOKEN");
            return;
        };
        match netgame_list(&uid, &tok, 0, 5).await {
            Ok(list) => {
                println!("OK {} servers", list.len());
                for s in &list {
                    println!("  {} | {}", s.name, s.entity_id);
                    match netgame_address(&uid, &tok, &s.entity_id).await {
                        Ok(a) => println!("    ADDR: {}:{}", a.ip, a.port),
                        Err(e) => println!("    addr FAIL: {e}"),
                    }
                    match netgame_detail(&uid, &tok, &s.entity_id).await {
                        Ok(d) => println!(
                            "    detail: v{:?}",
                            d.mc_version_list.iter().map(|v| &v.name).collect::<Vec<_>>()
                        ),
                        Err(e) => println!("    detail FAIL: {e}"),
                    }
                }
            }
            Err(e) => println!("list FAIL: {e}"),
        }
    }

    /// 租赁服六种来源 + 点赞状态（**只读**：不点赞、不收藏、不开停）。
    /// 用来核对抓包里的请求体/响应形状在线上还成立。
    #[tokio::test]
    #[ignore = "需要 MCTL_UID / MCTL_TOKEN 环境变量 + 实网"]
    async fn probe_rental_feeds() {
        let Some((uid, tok)) = creds() else {
            eprintln!("跳过：未设置 MCTL_UID / MCTL_TOKEN");
            return;
        };
        for feed in RentalFeed::ALL {
            match rental_feed(&uid, &tok, feed, 0).await {
                Ok(list) => {
                    let like = list.first().map(|s| s.like_num).unwrap_or(0);
                    println!("{}: {} 个（首个 like_num={like}）", feed.label(), list.len());
                    if let Some(s) = list.first() {
                        println!(
                            "    {} | {} | v{} | {}/{} | id={}",
                            s.title(),
                            if feed == RentalFeed::Mine {
                                s.power_label().unwrap_or("状态未知")
                            } else {
                                "-"
                            },
                            s.mc_version,
                            s.player_count,
                            s.capacity,
                            s.entity_id
                        );
                        // 只读探一下点赞状态（不调 update）
                        match rental_like_state(&uid, &tok, &s.entity_id).await {
                            Ok(Some(v)) => println!("    is_like = {v}"),
                            Ok(None) => println!("    is_like 字段缺失（核心会拒绝切换，正确行为）"),
                            Err(e) => println!("    like state FAIL: {e}"),
                        }
                    }
                }
                Err(e) => println!("{} FAIL: {e}", feed.label()),
            }
        }
    }
}
