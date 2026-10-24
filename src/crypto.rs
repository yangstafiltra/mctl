use anyhow::{anyhow, Result};
use rand::Rng;

const SKEYS: [&str; 16] = [
    "MK6mipwmOUedplb6",
    "OtEylfId6dyhrfdn",
    "VNbhn5mvUaQaeOo9",
    "bIEoQGQYjKd02U0J",
    "fuaJrPwaH2cfXXLP",
    "LEkdyiroouKQ4XN1",
    "jM1h27H4UROu427W",
    "DhReQada7gZybTDk",
    "ZGXfpSTYUvcdKqdY",
    "AZwKf7MWZrJpGR5W",
    "amuvbcHw38TcSyPU",
    "SI4QotspbjhyFdT0",
    "VP4dhjKnDGlSJtbB",
    "UXDZx4KhZywQ2tcn",
    "NIK73ZNvNqzva4kd",
    "WeiW7qU766Q1YQZI",
];
const IV: &[u8; 16] = b"szkgpbyimxavqjcn";

/// HttpUtil.HttpEncrypt：16字节IV + AES-CBC(明文+IV填充) + 1字节(keyIdx<<4|2)
pub fn encrypt(body: &[u8]) -> Vec<u8> {
    let pad_len = (body.len() + 16).div_ceil(16) * 16;
    let mut arr = vec![0u8; pad_len];
    arr[..body.len()].copy_from_slice(body);
    arr[body.len()..body.len() + 16].copy_from_slice(IV);

    let key_idx = rand::rng().random_range(0..15);
    let ci = (key_idx << 4) | 2;
    let key = SKEYS[((ci >> 4) & 0xF) as usize].as_bytes();
    let cipher = cbc_encrypt(key, IV, &arr);
    let mut out = Vec::with_capacity(16 + cipher.len() + 1);
    out.extend_from_slice(IV);
    out.extend_from_slice(&cipher);
    out.push(ci);
    out
}

/// HttpUtil.HttpDecrypt
pub fn decrypt(body: &[u8]) -> Result<Vec<u8>> {
    // 结构：16 字节 IV + N*16 密文 + 1 字节 keyIdx
    if body.len() < 18 {
        return Err(anyhow!("密文过短"));
    }
    // 密文区必须是 16 的整数倍，否则 cbc_decrypt 里的 try_into.unwrap 会 panic
    let ct_len = body.len() - 17;
    if !ct_len.is_multiple_of(16) {
        return Err(anyhow!(
            "密文长度 {} 不是 16 的整数倍（IV 16 + 密文 + keyIdx 1）",
            body.len()
        ));
    }
    let ci = body[body.len() - 1];
    let key = SKEYS[((ci >> 4) & 0xF) as usize].as_bytes();
    let iv: [u8; 16] = body[..16].try_into().unwrap();
    let dec = cbc_decrypt(key, &iv, &body[16..body.len() - 1]);
    // 从末尾去掉填充的 IV 字节（非零计数 16 个）
    // // 边界检查不是可选的：dec 来自网络响应。原文这里 `i -= 1` 会 usize 下溢，
    // 密文被截断/损坏/来自非预期格式时直接 panic（index out of bounds），
    // 而 TUI 一旦 panic 终端就卡在 raw mode。
    let mut n = 0;
    let mut i = dec.len();
    while n < 16 {
        if i == 0 {
            return Err(anyhow!("填充非法：末尾没有 16 个非零填充字节"));
        }
        i -= 1;
        if dec[i] != 0 {
            n += 1;
        }
    }
    Ok(dec[..i].to_vec())
}

fn cbc_encrypt(key: &[u8], iv: &[u8; 16], plain: &[u8]) -> Vec<u8> {
    use aes::cipher::{BlockEncrypt, KeyInit};
    let cipher = aes::Aes128::new_from_slice(key).unwrap();
    let mut prev = *iv;
    let mut out = Vec::with_capacity(plain.len());
    for chunk in plain.chunks(16) {
        let mut block: [u8; 16] = chunk.try_into().unwrap();
        for (b, p) in block.iter_mut().zip(prev.iter()) {
            *b ^= *p;
        }
        cipher.encrypt_block((&mut block).into());
        prev = block;
        out.extend_from_slice(&block);
    }
    out
}

fn cbc_decrypt(key: &[u8], iv: &[u8; 16], cipher_text: &[u8]) -> Vec<u8> {
    use aes::cipher::{BlockDecrypt, KeyInit};
    let cipher = aes::Aes128::new_from_slice(key).unwrap();
    let mut prev = *iv;
    let mut out = Vec::with_capacity(cipher_text.len());
    for chunk in cipher_text.chunks(16) {
        let block: [u8; 16] = chunk.try_into().unwrap();
        let mut dec = block;
        cipher.decrypt_block((&mut dec).into());
        for (d, p) in dec.iter_mut().zip(prev.iter()) {
            *d ^= *p;
        }
        prev = block;
        out.extend_from_slice(&dec);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let plain = br#"{"hello":"world","n":42}"#;
        let enc = encrypt(plain);
        let dec = decrypt(&enc).unwrap();
        assert_eq!(&dec[..], &plain[..]);
    }

    /// 畸形密文必须返回 Err，不能 panic——decrypt 吃的是网络响应，
    /// 而 TUI 里 panic 会把终端卡在 raw mode 回不来。
    #[test]
    fn malformed_ciphertext_errors_instead_of_panicking() {
        // 全零：解出来没有 16 个非零填充字节
        assert!(decrypt(&[0u8; 32]).is_err(), "全零密文应报错");
        // 单块长度、只有 IV 的最小体
        assert!(decrypt(&[0u8; 17]).is_err(), "17 字节应报「过短」");
        assert!(decrypt(&[]).is_err(), "空输入应报错");
        // 随机噪声（长度合法但填充无效）
        assert!(decrypt(&[7u8; 48]).is_err(), "无效填充应报错");
        // 非 16 倍数的密文区 → 曾经在这里 panic（cbc_decrypt 的 try_into.unwrap）
        for len in [18usize, 19, 20, 34, 35, 47, 50] {
            assert!(
                decrypt(&vec![0u8; len]).is_err(),
                "{len} 字节（密文区非 16 倍数）应报错"
            );
        }
        // 核心不变量：任何输入都不能 panic。结构合法（17+16k）的长度解密出垃圾是
        // 可接受的——真正的判据是「不崩」，因为崩了 TUI 终端就回不来了。
        for len in 0..80usize {
            let _ = decrypt(&vec![0xABu8; len]);
        }
        // 确认正常路径没被改坏
        let enc = encrypt(b"x");
        assert_eq!(decrypt(&enc).unwrap(), b"x");
    }
}
