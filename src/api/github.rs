//! GitHub 仓库搜索：mctl 插件的发现入口（像模组一样在应用内搜索+安装）。
//!
//! ## 为什么用 topic 而不是 `[mctl]` 前缀
//!
//! 想用仓库名 `[mctl]` 前缀做约定，但 GitHub 搜索**不支持**：
//! `q=[mctl] in:name` 恒为 0（分词器会吃掉方括号），而 `q=mctl in:name` 有 133 条。
//! 所以约定改成 GitHub 原生的话题机制：
//!
//! - **主**：`topic:mctl-plugin` —— 为「发现」设计的，搜索可用，且 topics 就在
//! 响应体里，客户端可**零额外请求**做本地过滤。
//! - **次**：用户自定义关键词（纯文本 `in:name,description`，不带方括号）。
//!
//! ## 配额（这是本模块最重要的约束）
//!
//! 未认证 GitHub search API **只有 10 次/分钟**、core 60 次/小时。
//! 所以：**绝不能边打字边搜**。上层只在用户按 Enter 时发一次请求，
//! 结果缓存在本地，后续细化靠本地过滤。带 token 可提到 30/分钟、5000/小时，
//! 但本模块暂不支持（YAGNI），配额用尽时把 GitHub 的提示原样透出。

use anyhow::{anyhow, Result};
use serde::Deserialize;

/// 插件注册话题：作者给仓库打上这个 topic 即可被 mctl 收录
pub const PLUGIN_TOPIC: &str = "mctl-plugin";

/// 注册表为空时的回退查询（首屏只发一次，结果进缓存）
pub const BROAD_QUERY: &str = "mctl";

const API: &str = "https://api.github.com";

/// 搜索结果里的一个仓库
#[derive(Debug, Clone, Deserialize)]
pub struct Repo {
    pub full_name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub stargazers_count: u64,
    #[serde(default)]
    pub updated_at: Option<String>,
    #[serde(default)]
    pub topics: Vec<String>,
    #[serde(default)]
    pub html_url: String,
}

impl Repo {
    /// 克隆地址（shallow）。html_url 一定存在，full_name 兜底拼。
    pub fn clone_url(&self) -> String {
        if self.html_url.is_empty() {
            format!("https://github.com/{}.git", self.full_name)
        } else {
            format!("{}.git", self.html_url)
        }
    }

    /// 是否打了插件注册 topic
    pub fn is_plugin(&self) -> bool {
        self.topics.iter().any(|t| t == PLUGIN_TOPIC)
    }

    /// `owner/repo` → `repo`（安装目录名由 repo_name(url) 决定，这里只用于显示）
    pub fn short_name(&self) -> &str {
        self.full_name.split('/').nth(1).unwrap_or(&self.full_name)
    }

    /// updated_at → `YYYY-MM-DD`
    pub fn updated_day(&self) -> &str {
        self.updated_at
            .as_deref()
            .unwrap_or("")
            .get(..10)
            .unwrap_or("")
    }
}

/// 搜索响应 + 配额信息（配额要透出给用户，否则「搜不出来」会被当成 bug）
#[derive(Debug, Clone)]
pub struct SearchResult {
    pub repos: Vec<Repo>,
    pub total: u64,
    /// 本次请求后剩余的 search 配额；None = 响应头里没有
    pub rate_remaining: Option<String>,
}

#[derive(Deserialize)]
struct RawSearch {
    #[serde(default)]
    total_count: u64,
    #[serde(default)]
    items: Vec<Repo>,
}

/// 构造搜索 query。
/// - 空关键词 → `topic:mctl-plugin`（插件注册表，按最近更新排序）
/// - 有关键词 → 直接透传（上层已保证不带方括号，那会被 GitHub 吃掉）
pub fn build_query(user_query: &str) -> String {
    let q = user_query.trim();
    if q.is_empty() {
        format!("topic:{PLUGIN_TOPIC}")
    } else {
        format!("{q} in:name,description")
    }
}

/// 搜仓库。`page` 从 1 起。
pub async fn search_repos(user_query: &str, page: u32) -> Result<SearchResult> {
    let q = build_query(user_query);
    let url = format!("{API}/search/repositories");
    let client = crate::api::client()?;
    let resp = client
        .get(url)
        .query(&[
            ("q", q.as_str()),
            ("per_page", "30"),
            ("page", &page.to_string()),
        ])
        .send()
        .await
        .map_err(|e| anyhow!("请求 GitHub 失败: {e}"))?;

    let remaining = resp
        .headers()
        .get("x-ratelimit-remaining")
        .and_then(|v| v.to_str().ok())
        .map(String::from);

    let status = resp.status();
    if status.as_u16() == 403 || status.as_u16() == 429 {
        let wait = resp
            .headers()
            .get("x-ratelimit-reset")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<i64>().ok())
            .map(|t| (t - now_secs()).max(0))
            .unwrap_or(60);
        return Err(anyhow!(
            "GitHub 搜索配额用尽（未认证 10 次/分钟），约 {wait} 秒后重试"
        ));
    }
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(anyhow!(
            "GitHub 返回 {}: {}",
            status.as_u16(),
            body.chars().take(120).collect::<String>()
        ));
    }

    let raw: RawSearch = resp
        .json()
        .await
        .map_err(|e| anyhow!("解析 GitHub 响应失败: {e}"))?;

    Ok(SearchResult {
        total: raw.total_count,
        repos: raw.items,
        rate_remaining: remaining,
    })
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 实网验证：需要网络 + 消耗 GitHub 配额，默认不跑
    #[tokio::test]
    #[ignore]
    async fn live_github_search() {
        let r = search_repos("", 1).await.expect("搜索应成功");
        println!("query = {:?}", build_query(""));
        println!("total = {}  剩余配额 = {:?}", r.total, r.rate_remaining);
        for repo in r.repos.iter().take(6) {
            println!(
                "  {:<38} ★{:<5} plugin={} {}",
                repo.full_name,
                repo.stargazers_count,
                repo.is_plugin(),
                repo.clone_url()
            );
        }
        // 注册表可能为空（topic 还没人认领），只断言 API 有响应且解析成功
        println!(
            "宽泛回退 {} → total={}",
            BROAD_QUERY,
            search_repos(BROAD_QUERY, 1)
                .await
                .map(|x| x.total)
                .unwrap_or(0)
        );
    }

    #[test]
    fn empty_query_becomes_plugin_topic_registry() {
        assert_eq!(build_query(""), "topic:mctl-plugin");
        assert_eq!(build_query("   "), "topic:mctl-plugin");
    }

    #[test]
    fn broad_query_round_trips() {
        assert_eq!(build_query(BROAD_QUERY), "mctl in:name,description");
    }

    #[test]
    fn user_query_is_passed_through_without_brackets() {
        // 方括号会被 GitHub 分词器吃掉，必须由上层过滤掉
        assert_eq!(build_query("bedrock"), "bedrock in:name,description");
        assert_eq!(build_query("  ratatui "), "ratatui in:name,description");
    }

    #[test]
    fn clone_url_prefers_html_url() {
        let r: Repo = serde_json::from_str(
            r#"{"full_name":"a/b","html_url":"https://github.com/a/b","topics":["mctl-plugin"]}"#,
        )
        .unwrap();
        assert_eq!(r.clone_url(), "https://github.com/a/b.git");
        assert!(r.is_plugin());
        assert_eq!(r.short_name(), "b");
    }

    #[test]
    fn clone_url_falls_back_to_full_name() {
        let r: Repo = serde_json::from_str(r#"{"full_name":"a/b"}"#).unwrap();
        assert_eq!(r.clone_url(), "https://github.com/a/b.git");
        assert!(!r.is_plugin());
        assert_eq!(r.updated_day(), "");
    }
}
