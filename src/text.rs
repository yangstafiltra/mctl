//! 文本小工具：**按字符边界**安全截断。
//!
//! 为什么需要它：`&s[..s.len().min(n)]` 这种按字节切的做法在 UTF-8 上会
//! `byte index is not a char boundary` panic —— 中文报错、插件输出、验证码响应
//! 都是多字节文本，服务端一句中文错误就能把 TUI 打崩。

/// 截断到不超过 `max_bytes` 字节（回退到最近的字符边界），超长时补 `…`。
pub fn truncate(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

#[cfg(test)]
mod tests {
    use super::truncate;

    #[test]
    fn short_text_untouched() {
        assert_eq!(truncate("hello", 300), "hello");
        assert_eq!(truncate("", 10), "");
    }

    /// 关键回归：切点落在多字节字符中间也不能 panic
    #[test]
    fn cjk_boundary_safe() {
        let s = "网易服务器错误：请求被拒绝，请重新登录后再试。";
        for n in 0..s.len() + 4 {
            let t = truncate(s, n); // 不许 panic
            assert!(t.is_char_boundary(0));
            assert!(s.starts_with(t.trim_end_matches('…')));
        }
        let t = truncate(s, 3);
        assert!(t.ends_with('…'), "截断要留省略号: {t}");
    }

    /// emoji（4 字节）也要安全
    #[test]
    fn emoji_boundary_safe() {
        let s = "失败🚫重试";
        let t = truncate(s, 7); // 7 落在 🚫 内部
        assert!(s.starts_with(t.trim_end_matches('…')));
    }
}
