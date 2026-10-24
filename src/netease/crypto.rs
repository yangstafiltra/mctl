use aes::cipher::{generic_array::GenericArray, BlockEncrypt, KeyInit};
use aes::Aes128;
use anyhow::{anyhow, Result};
use chacha20::cipher::{KeyIvInit, StreamCipher};
use chacha20::ChaCha8;
use rsa::RsaPrivateKey;
use sha2::{Digest, Sha256};

// ---- Skip32 ----

static F_TABLE: [u8; 256] = [
    163, 215, 9, 131, 248, 72, 246, 244, 179, 33, 21, 120, 153, 177, 175, 249, 231, 45, 77, 138,
    206, 76, 202, 46, 82, 149, 217, 30, 78, 56, 68, 40, 10, 223, 2, 160, 23, 241, 96, 104, 18, 183,
    122, 195, 233, 250, 61, 83, 150, 132, 107, 186, 242, 99, 154, 25, 124, 174, 229, 245, 247, 22,
    106, 162, 57, 182, 123, 15, 193, 147, 129, 27, 238, 180, 26, 234, 208, 145, 47, 184, 85, 185,
    218, 133, 63, 65, 191, 224, 90, 88, 128, 95, 102, 11, 216, 144, 53, 213, 192, 167, 51, 6, 101,
    105, 69, 0, 148, 86, 109, 152, 155, 118, 151, 252, 178, 194, 176, 254, 219, 32, 225, 235, 214,
    228, 221, 71, 74, 29, 66, 237, 158, 110, 73, 60, 205, 67, 39, 210, 7, 212, 222, 199, 103, 24,
    137, 203, 48, 31, 141, 198, 143, 170, 200, 116, 220, 201, 93, 92, 49, 164, 112, 136, 97, 44,
    159, 13, 43, 135, 80, 130, 84, 100, 38, 125, 3, 64, 52, 75, 28, 115, 209, 196, 253, 59, 204,
    251, 127, 171, 230, 62, 91, 165, 173, 4, 35, 156, 20, 81, 34, 240, 41, 121, 113, 126, 255, 140,
    14, 226, 12, 239, 188, 114, 117, 111, 55, 161, 236, 211, 142, 98, 139, 134, 16, 232, 8, 119,
    17, 190, 146, 79, 36, 197, 50, 54, 157, 207, 243, 166, 187, 172, 94, 108, 169, 19, 87, 37, 181,
    227, 189, 168, 58, 1, 5, 89, 42, 70,
];

fn g(key: &[u8], k: usize, w: i32) -> i32 {
    let k0 = key[4 * k % 10] as i32;
    let k1 = key[(4 * k + 1) % 10] as i32;
    let k2 = key[(4 * k + 2) % 10] as i32;
    let k3 = key[(4 * k + 3) % 10] as i32;
    let hi = w >> 8;
    let lo = w & 0xFF;
    let g1 = F_TABLE[(lo ^ k0) as usize & 0xFF] as i32 ^ hi;
    let g2 = F_TABLE[(g1 ^ k1) as usize & 0xFF] as i32 ^ lo;
    let g3 = F_TABLE[(g2 ^ k2) as usize & 0xFF] as i32 ^ g1;
    let g4 = F_TABLE[(g3 ^ k3) as usize & 0xFF] as i32 ^ g2;
    (g3 << 8) + g4
}

/// Skip32Cipher.Encrypt（与 实现逐位一致）
pub fn skip32_encrypt(value: i32, key: &[u8]) -> i32 {
    let mut buf = [
        (value >> 24) & 0xFF,
        (value >> 16) & 0xFF,
        (value >> 8) & 0xFF,
        value & 0xFF,
    ];
    let mut w1 = (buf[0] << 8) + buf[1];
    let mut w2 = (buf[2] << 8) + buf[3];
    let mut k = 0usize;
    for _ in 0..12 {
        w2 ^= g(key, k, w1) ^ k as i32;
        k += 1;
        w1 ^= g(key, k, w2) ^ k as i32;
        k += 1;
    }
    buf[0] = w2 >> 8;
    buf[1] = w2 & 0xFF;
    buf[2] = w1 >> 8;
    buf[3] = w1 & 0xFF;
    (buf[0] << 24) | (buf[1] << 16) | (buf[2] << 8) | buf[3]
}

/// 网易角色 UUID：`md5(角色名)` 前 12 字节 + `Skip32(userId)` 的 4 字节**小端**，
/// 置 v4/variant 位，输出 **32 位小写无横线**。
/// 官方 `Skip32Cipher.GenerateRoleUuid`：`Convert.ToHexStringLower(array)`，尾 4 字节来自
/// `BitConverter.GetBytes(Encrypt(userId))`（小端）。这个串由 `${auth_uuid}` 原样进 `--uuid`，
/// 带横线/大写都会被客户端 mod 判为无效角色 uuid。
pub fn role_uuid(role_name: &str, user_id: &str) -> String {
    use md5::{Digest as _, Md5 as M};
    let mut h = M::new();
    h.update(role_name.as_bytes());
    let hash = h.finalize();
    // uid 是平台侧 uint32：按 u32 解析（>2^31 的账号也要能算），位模式给 Skip32
    let uid = user_id.parse::<u32>().unwrap_or(0) as i32;
    let skip = skip32_encrypt(uid, b"SaintSteve") as u32;
    let mut b = [0u8; 16];
    b[..12].copy_from_slice(&hash[..12]);
    b[12] = (skip & 0xFF) as u8; // BitConverter.GetBytes(uint)：低字节在前
    b[13] = ((skip >> 8) & 0xFF) as u8;
    b[14] = ((skip >> 16) & 0xFF) as u8;
    b[15] = ((skip >> 24) & 0xFF) as u8;
    b[6] = (b[6] & 0x0F) | 0x40; // version 4
    b[8] = (b[8] & 0x3F) | 0x80; // variant RFC4122
    b.iter().map(|x| format!("{x:02x}")).collect()
}

// ---- AES-ECB 无填充 ----

pub fn aes_ecb_encrypt(data: &[u8], key: &[u8]) -> Result<Vec<u8>> {
    let key_arr = GenericArray::from_slice(key);
    let cipher = Aes128::new(key_arr);
    let mut out = Vec::with_capacity(data.len());
    for chunk in data.chunks(16) {
        if chunk.len() != 16 {
            return Err(anyhow!("AES-ECB 输入不是 16 的倍数: {}", chunk.len()));
        }
        let mut block = GenericArray::clone_from_slice(chunk);
        cipher.encrypt_block(&mut block);
        out.extend_from_slice(&block);
    }
    Ok(out)
}

// ---- AES-128-CBC 零填充（登录报文加解密，实网响应可解密验证）----

pub fn aes_cbc_zeros(data: &[u8], key: &[u8], iv: &[u8]) -> Result<Vec<u8>> {
    if key.len() != 16 || iv.len() != 16 {
        return Err(anyhow!("AES-CBC 密钥/IV 需 16 字节"));
    }
    let cipher = Aes128::new(GenericArray::from_slice(key));
    let mut padded = data.to_vec();
    let rem = padded.len() % 16;
    if rem != 0 {
        padded.resize(padded.len() + 16 - rem, 0);
    }
    let mut out = Vec::with_capacity(padded.len());
    let mut prev = iv.to_vec();
    for chunk in padded.chunks(16) {
        let block: Vec<u8> = chunk.iter().zip(prev.iter()).map(|(a, b)| a ^ b).collect();
        let mut buf = GenericArray::clone_from_slice(&block);
        cipher.encrypt_block(&mut buf);
        out.extend_from_slice(&buf);
        prev = buf.to_vec();
    }
    Ok(out)
}

/// 网易启动 Token 加密（实测）：
/// 8 随机大写 + userToken + 8 随机大写 → AES-128-CBC(zeros) → 十六进制大写
pub fn generate_encrypt_token(user_token: &str) -> Result<String> {
    use rand::Rng;
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ";
    let mut rng = rand::rng();
    let rnd = |r: &mut rand::rngs::ThreadRng| {
        (0..8)
            .map(|_| CHARS[r.random_range(0..26)] as char)
            .collect::<String>()
    };
    let r1 = rnd(&mut rng);
    let r2 = rnd(&mut rng);
    let plain = format!("{r1}{user_token}{r2}");
    let enc = aes_cbc_zeros(plain.as_bytes(), b"debbde3548928fab", b"afd4c5c5a7c456a1")?;
    Ok(enc.iter().map(|b| format!("{b:02X}")).collect())
}

// ---- ChaCha8 打包（客户端加入会话的帧格式，按客户端实际行为逐字节核对）----

pub const CHACHA_NONCE: &[u8; 12] = b"163 NetEase\n";

pub fn pack_message(type_byte: u8, data: &[u8], key: &[u8]) -> Result<Vec<u8>> {
    let mut array = vec![0u8; data.len() + 10];
    let len = (array.len() - 2) as u16;
    array[0] = (len & 0xFF) as u8;
    array[1] = (len >> 8) as u8;
    array[6] = type_byte;
    array[7] = 136;
    array[8] = 136;
    array[9] = 136;
    array[10..].copy_from_slice(data);
    let crc = crc32fast::hash(&array[6..]);
    array[2..6].copy_from_slice(&crc.to_le_bytes());
    let mut cipher = ChaCha8::new(
        GenericArray::from_slice(key),
        GenericArray::from_slice(CHACHA_NONCE),
    );
    cipher.apply_keystream(&mut array[2..]);
    Ok(array)
}

pub fn unpack_message(data: &[u8], key: &[u8]) -> Result<(u8, Vec<u8>)> {
    let mut buf = data.to_vec();
    let mut cipher = ChaCha8::new(
        GenericArray::from_slice(key),
        GenericArray::from_slice(CHACHA_NONCE),
    );
    cipher.apply_keystream(&mut buf);
    let mut crc = [0u8; 4];
    let payload = &buf[4..];
    let crc_val = crc32fast::hash(payload);
    crc.copy_from_slice(&crc_val.to_le_bytes());
    for i in 0..4 {
        if crc[i] != buf[i] {
            return Err(anyhow!("UnpackMessage CRC 校验失败"));
        }
    }
    Ok((buf[4], buf[8..].to_vec()))
}

// ---- RSA（BouncyCastle PKCS1 语义）----

/// 网易 Yggdrasil 内嵌私钥（YggdrasilGenerator.PrivateKey）
pub const RSA_PRIVATE_KEY_PEM: &str = "-----BEGIN PRIVATE KEY-----MIIEvAIBADANBgkqhkiG9w0BAQEFAASCBKYwggSiAgEAAoIBAQDobJGIddxcs08xzTIVFanc4/84J1DbxiW7wLokIrap3txzyyMXj+AcDa8jLopLJkMg9rZLzL50Dwp+hmTTgiIcjkM1DVREsRBltzqjVNyiYPL2VGHLn+/eEivjhLNWUMcWrlAoJ5JJBi22oGPLlKDVJpg33JPI5nPZpwufdB0ecn2V0CAeeGyswQyAaqIjoiYOoP3HipjEYGQHp1RsADf4ozGRgv+2HGiSOyhlv00ixnF0nRUTzVh18ka0N3LdLMpAN/YAPkO8tmoWp0asyU3X+nyZFd29povvrRy4rL1lYmo8jdpfpSL+Yk+9+RybjBAhXRx6uDoBxaa2kwE4fyrrAgMBAAECggEAWT42szruHfoLkofDjyz+R/6TZLBT788pdeoOjwl1McyMwTlihA2Oc7cdZFjeaPSMGgAhBwHarx2HXgWkeUIibuyBCcHQdX+3WBb+wPA4t3CaWdMUqecDZzV6/KVbZu0lRKQxyvlGxhtFOjZjmyu6hZ2IHQrpA97Y5N2rLNKcy69W+QYJZappmBfbVgWM0NRmgmpg6siQ0Cm6Ryil3SBAPBVv+EUPiD9jdXbtVq7VwN4YmwUGScp2Fib1oUnqEAja1hfihVnRFQ246nKXhIc/YVrNmwBrxAVwaaPFRka6XjKkSF0WVbpqQLXhbY1fS8FoXGpVhiF6o4rTQJbpQxhQgQKBgQD7XysfPNt9G63gTrkZvjvEk5LKsRG42MAYEkuzxEal9E/AQv0jJrk0f1WO09hCcYQeaOQXhM9mezNQv5jXEnmepXqM3NTw1Di5yh3uvjSXQjdUt+7haNw+QjggBqZxyQZjtadYairSzfmWe7OwJIkCmdgaJKxm4qMExk9kUgApGwKBgQDstBdJHU/KEBqVpsIlu185vFFuaAvxiHjXHqGwytJMQ/5aVqaphIiQCAxaEogPSzPSm28UHVQiZeFO059EKOpSJscW4pV95Dr5BAbHuYecacqnZKbQqb69//Cfpne9tGYXlmP6QnYfPoc4wYfTfPyU2x3KtDhVxtEDutpSNU1ycQKBgBabWn92s66uvJZ9vfvotetZ8ku0XQmoxK3lh1Vlg40NSdbar3Vn2CQ2h3VO7BYdq2oouMq8sQJgdh7+/DnreXChJUJh4ey+yVM8MDD2fjhURjGiUSOIkLYwsmd+8Z0uHRr+jUxQUAWhbJ7yBRkEUCYhu+OuBKtEGrElPKKjFUydAoGAQLj9pQBe0OGWY1U1wRt67k6P9aB9o42tfSTjEXRkDHaLFiibab7TmI6a0gY/Le9iPDREKzvZxY4WDXfQFNMbP1tbFObf+Yxuk6iGMhaI/jvvLdZXxrajcVCKex0JoNWzFMAKlmOV6PUwBFTmzu1eI1XGz6Z3wPycKmjtSY1JoAECgYBBOfaUDMaG1xLzv+q1jPPs2U4lXPK2BXFE5RaliUGC+LIQREXPishII2LYFW3gtXj5QWfIGq6x0d6ca6Bja2vYRDDe5tlT/2VbZahiHpb2PYL/2WgeoHl7sT9Bb/nsKyo85Sv+doop6huy4+aeTiQHgrGR9JYMVBSIx6P8Tt5phA==-----END PRIVATE KEY-----";

pub fn rsa_private_encrypt(data: &[u8]) -> Result<Vec<u8>> {
    use base64::Engine as _;
    use rsa::pkcs8::DecodePrivateKey;
    use rsa::traits::{PrivateKeyParts, PublicKeyParts};
    let pem_b64 = RSA_PRIVATE_KEY_PEM
        .replace("-----BEGIN PRIVATE KEY-----", "")
        .replace("-----END PRIVATE KEY-----", "")
        .replace("\n", "");
    let der = base64::engine::general_purpose::STANDARD
        .decode(pem_b64.trim())
        .map_err(|e| anyhow!("私钥 Base64 解码失败: {e}"))?;
    let key = RsaPrivateKey::from_pkcs8_der(&der).map_err(|e| anyhow!("加载 RSA 私钥失败: {e}"))?;
    let k = key.n().to_bytes_be().len();
    if data.len() > k - 11 {
        return Err(anyhow!("RSA 数据过长: {} > {}", data.len(), k - 11));
    }
    // BouncyCastle RsaWithPkcs1(PrivateKey, data, true) 实测：PKCS1 v1.5 type 1 填充 + modpow(d)
    let mut em = vec![0u8; k];
    em[1] = 1;
    for b in em.iter_mut().take(k - data.len() - 1).skip(2) {
        *b = 0xFF;
    }
    em[k - data.len() - 1] = 0;
    em[k - data.len()..].copy_from_slice(data);
    let m = rsa::BigUint::from_bytes_be(&em);
    // 手动 modpow(d) 验证
    let d = key.d();
    let n = key.n();
    let c = m.modpow(d, n);
    let mut out = c.to_bytes_be();
    while out.len() < k {
        out.insert(0, 0);
    }
    Ok(out)
}

/// 网易 Yggdrasil 内嵌公钥（YggdrasilGenerator.PublicKey）
pub const RSA_PUBLIC_KEY_B64: &str = "MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEA4HJFrYdVTeoSvH6qsnJElfXuf7FnxxFQdz3gRCs66LDrZfaoGoWt2e/aGIOv8uGHliBWnZP42Ike9Qf5aiYVtQ4mlj2bXZjifHG35LlS1Bq6yCA6k1WevWcrGWOuLzny3jo8Wbdi0lIFMTT2hN98sF2k4YcvyE9zhqxfRNFGVI5kLyxm9CeTKAXGBU5mw3yQWJ8cPRR4866jpGGOhBWlJdilWt2NES9bid8SbhTT55wqumnVO5J5/DaMyTgKIQngH7NyZQljAhdK5I23dzpGop322n2eQ+mTNLuquwU453o1cbyQobgC6vh5/F1QT2INBR2qYCnRzzJ6hrhE5kIMZwIDAQAB";

/// RSA 公钥"解密"（RsaWithPkcs1(PublicKey, data, forEncryption:false) 的验证语义）
pub fn rsa_public_verify(data: &[u8]) -> Result<Vec<u8>> {
    use base64::Engine as _;
    use rsa::pkcs8::DecodePublicKey;
    use rsa::traits::PublicKeyParts;
    let der = base64::engine::general_purpose::STANDARD
        .decode(RSA_PUBLIC_KEY_B64)
        .map_err(|e| anyhow!("公钥 Base64 解码失败: {e}"))?;
    let key = rsa::RsaPublicKey::from_public_key_der(&der)
        .map_err(|e| anyhow!("加载 RSA 公钥失败: {e}"))?;
    let n = key.n();
    let e = key.e();
    let k = n.to_bytes_be().len();
    if data.len() != k {
        return Err(anyhow!("RSA 密文长度不符: {} != {}", data.len(), k));
    }
    let c = rsa::BigUint::from_bytes_be(data);
    let m = c.modpow(e, n);
    let mut em = m.to_bytes_be();
    while em.len() < k {
        em.insert(0, 0);
    }
    eprintln!(
        "[rsa] em head={:02x}{:02x}{:02x}{:02x}...",
        em[0], em[1], em[2], em[3]
    );
    // 去掉 PKCS1 type1 填充
    if em.len() < 11 || em[0] != 0 || em[1] != 1 {
        return Err(anyhow!("PKCS1 填充头非法"));
    }
    let sep = em[2..]
        .iter()
        .position(|&b| b == 0)
        .ok_or_else(|| anyhow!("PKCS1 填充无分隔符"))?;
    Ok(em[2 + sep + 1..].to_vec())
}

pub fn sha256(data: &[u8]) -> Vec<u8> {
    let mut h = Sha256::new();
    h.update(data);
    h.finalize().to_vec()
}

pub fn xor_bytes(a: &[u8], b: &[u8]) -> Vec<u8> {
    a.iter().zip(b.iter().cycle()).map(|(x, y)| x ^ y).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skip32_roundtrip() {
        // 对照 行为：Encrypt 是可逆的
        assert_eq!(skip32_encrypt(123456789, b"SaintSteve"), 347728316);
        let key = b"SaintSteve";
        for v in [1i32, 123456789, 929065056, 253547207] {
            let e = skip32_encrypt(v, key);
            assert_ne!(e as u32, v as u32, "加密不应等于原文");
        }
    }

    /// AES-ECB 解密——**仅作测试参照物**。
    /// /// 生产路径用不到（解密由网易服务端做），但没有它就无法验证
    /// `aes_ecb_encrypt` 是否正确：加密没有参照就只是「和自己一致」。
    #[cfg(test)]
    pub fn aes_ecb_decrypt(data: &[u8], key: &[u8]) -> Result<Vec<u8>> {
        use aes::cipher::BlockDecrypt;
        let cipher = Aes128::new(GenericArray::from_slice(key));
        let mut out = Vec::with_capacity(data.len());
        for chunk in data.chunks(16) {
            if chunk.len() != 16 {
                return Err(anyhow!("AES-ECB 输入不是 16 的倍数: {}", chunk.len()));
            }
            let mut block = GenericArray::clone_from_slice(chunk);
            cipher.decrypt_block(&mut block);
            out.extend_from_slice(&block);
        }
        Ok(out)
    }

    #[test]
    fn aes_ecb_roundtrip() {
        let key = [0x11u8; 16];
        let data = b"0123456789abcdef";
        let enc = aes_ecb_encrypt(data, &key).unwrap();
        let dec = aes_ecb_decrypt(&enc, &key).unwrap();
        assert_eq!(dec, data);
    }

    #[test]
    fn chacha_pack_unpack_roundtrip() {
        let key = b"0123456789abcdefghijklmnopqrstuv";
        let msg = b"hello yggdrasil";
        let packed = pack_message(9, msg, key).unwrap();
        // 网络帧 = int16 长度前缀 + 加密负载；UnpackMessage 输入不含前缀
        let (t, out) = unpack_message(&packed[2..], key).unwrap();
        assert_eq!(t, 9);
        assert_eq!(out, msg);
    }

    #[test]
    fn role_uuid_format() {
        let u = role_uuid("Steve", "123456789");
        // 官方 GenerateRoleUuid → Convert.ToHexStringLower：32 位小写无横线（`--uuid` 原样用它）
        assert_eq!(u.len(), 32, "--uuid 必须 32 位无横线");
        assert!(
            u.bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()),
            "必须小写十六进制：{u}"
        );
        assert_eq!(u.as_bytes()[12], b'4'); // version 4（第 13 个字符是版本位）
                                            // 末尾 4 字节 = BitConverter.GetBytes(Skip32(userId))：小端
        let skip = skip32_encrypt(123456789, b"SaintSteve") as u32;
        let hex_tail = format!(
            "{:02x}{:02x}{:02x}{:02x}",
            skip & 0xFF,
            (skip >> 8) & 0xFF,
            (skip >> 16) & 0xFF,
            (skip >> 24) & 0xFF
        );
        assert!(
            u.ends_with(&hex_tail),
            "{u} 应含 Skip32 小端尾部 {hex_tail}"
        );
        // 反向：客户端 mod 用尾部 4 字节小端 → Skip32 解密还原 uid
        let raw = u32::from_le_bytes([
            u8::from_str_radix(&u[24..26], 16).unwrap(),
            u8::from_str_radix(&u[26..28], 16).unwrap(),
            u8::from_str_radix(&u[28..30], 16).unwrap(),
            u8::from_str_radix(&u[30..32], 16).unwrap(),
        ]);
        assert_eq!(raw, skip, "尾部必须是 Skip32 值的小端序");
    }

    #[test]
    fn rsa_sign_verify_self() {
        // 客户端私钥(PKCS1 type1 + modpow d) 的签名，应能用其自身派生的公钥(modpow e) 解回。
        // 注意：RSA_PUBLIC_KEY_B64 是网易服务器公钥（另一密钥对），只用于验证服务器 signContent，
        // 故此处用私钥派生的公钥做自验，而非 rsa_public_verify。
        use base64::Engine as _;
        use rsa::pkcs8::DecodePrivateKey;
        use rsa::traits::PublicKeyParts;
        let pem_b64 = RSA_PRIVATE_KEY_PEM
            .replace("-----BEGIN PRIVATE KEY-----", "")
            .replace("-----END PRIVATE KEY-----", "")
            .replace("\n", "");
        let der = base64::engine::general_purpose::STANDARD
            .decode(pem_b64.trim())
            .unwrap();
        let key = RsaPrivateKey::from_pkcs8_der(&der).unwrap();
        let pubk = rsa::RsaPublicKey::from(&key);
        let n = pubk.n();
        let e = pubk.e();
        let k = n.to_bytes_be().len();
        let data = b"test payload for rsa";
        let sig = rsa_private_encrypt(data).unwrap();
        let m = rsa::BigUint::from_bytes_be(&sig).modpow(e, n);
        let mut em = m.to_bytes_be();
        while em.len() < k {
            em.insert(0, 0);
        }
        assert_eq!(&em[..2], &[0, 1], "PKCS1 type1 头");
        let sep = em[2..].iter().position(|&b| b == 0).unwrap();
        assert_eq!(&em[2 + sep + 1..], data);
    }
}
