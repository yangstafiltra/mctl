use md5::{Digest, Md5};

/// 计算 `user-token` 请求头（协议算法，已用实网请求逐字节核对）。
/// 算法：
/// ```text
/// preimage = md5(user_token).lower
/// + body
/// + "0eGsBkhl"
/// + "/" + request_path
/// text = md5(preimage).hex.lower
/// binary = text 每字符转 8-bit 二进制拼接
/// secret = binary 循环左移 6 位
/// out = text 的 ASCII 字节，逐字节异或 secret 每 8 位(位反转)
/// value = base64(out[0..12]) + "1"，字符 +→m、/→o
/// headers = { user-id, user-token }
/// ```
pub fn compute(
    request_path: &str,
    body: &str,
    user_id: &str,
    user_token: &str,
) -> (String, String) {
    let path = if request_path.starts_with('/') {
        request_path.to_string()
    } else {
        format!("/{request_path}")
    };

    let mut hasher = Md5::new();
    hasher.update(user_token.as_bytes());
    let md5_ut = hex_lower(&hasher.finalize());

    let mut hasher = Md5::new();
    hasher.update(md5_ut.as_bytes());
    hasher.update(body.as_bytes());
    hasher.update(b"0eGsBkhl");
    hasher.update(path.as_bytes());
    let text = hex_lower(&hasher.finalize());

    // HexToBinary
    let mut bin = String::with_capacity(text.len() * 8);
    for c in text.chars() {
        bin.push_str(&format!("{:08b}", c as u32));
    }
    let secret_bin = format!("{}{}", &bin[6..], &bin[..6]);

    let mut out = text.as_bytes().to_vec();
    for i in 0..(secret_bin.len() / 8) {
        let span = &secret_bin[i * 8..i * 8 + 8];
        let mut b: u8 = 0;
        for j in 0..span.len() {
            if span.as_bytes()[7 - j] == b'1' {
                b |= 1 << j;
            }
        }
        out[i] ^= b;
    }

    let mut value = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &out[..12]);
    value.push('1');
    value = value.replace('+', "m").replace('/', "o");

    (user_id.to_string(), value)
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 真实向量（含网易账号 user_token，**不入库**）
    /// /// 仓库里不放这些凭据：拿到仓库就能拿 token 冒充账号调网易 API。
    /// 但这两个向量是本项目**唯一**证明逆向算法正确的证据——把 token 换成
    /// 占位符再重算期望值，测试就变成「自己验自己」，毫无价值。
    /// /// 所以：向量放本地 `token_vectors.json`（已 gitignore），文件在就跑真实
    /// 校验，不在就跳过并说明。`token_vectors.example.json` 是格式模板。
    fn real_vectors() -> Option<Vec<RealVector>> {
        let text = std::fs::read_to_string("token_vectors.json").ok()?;
        serde_json::from_str(&text).ok()
    }

    #[derive(serde::Deserialize)]
    struct RealVector {
        path: String,
        body: String,
        uid: String,
        token: String,
        expect: String,
        /// 向量来源（Python 原型 / C# 真实实现）
        #[serde(default)]
        source: String,
    }

    #[test]
    fn matches_real_vectors() {
        let Some(vectors) = real_vectors() else {
            eprintln!(
                "跳过：未找到 token_vectors.json（真实向量含账号凭据，不入库）。\
                 格式见 token_vectors.example.json"
            );
            return;
        };
        assert!(!vectors.is_empty(), "向量文件为空");
        for v in &vectors {
            let (uid, tok) = compute(&v.path, &v.body, &v.uid, &v.token);
            assert_eq!(uid, v.uid, "{} 向量 uid 应原样回传", v.source);
            assert_eq!(tok, v.expect, "{} 向量不匹配（path={}）", v.source, v.path);
        }
    }

    /// 与凭据无关的回归向量：锁定 base64 后的字符替换与截断行为。
    /// /// 这个能常驻是因为它的期望值来自算法本身的定义（取 out[..12]、'+'→'m'、'/'→'o'），
    /// 不依赖任何外部系统的真实输出。
    #[test]
    fn output_shape_is_stable() {
        let (uid, tok) = compute("/x", "{}", "1", "2");
        assert_eq!(uid, "1");
        assert_eq!(tok.len(), 17, "base64(12B)=16 字符 + 结尾 '1'");
        assert!(tok.ends_with('1'), "末位固定为 '1'");
        assert!(
            !tok.contains('+') && !tok.contains('/'),
            "'+'→'m'、'/'→'o' 必须已替换：{tok}"
        );
    }

    #[test]
    fn path_without_slash_is_normalized() {
        let (_, t1) = compute("item/query/available", "{}", "1", "2");
        let (_, t2) = compute("/item/query/available", "{}", "1", "2");
        assert_eq!(t1, t2);
    }
}
