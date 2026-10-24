//! XTS 页级解密（已用真实 msixvc 数据验证：见底部测试）

use crate::keys::KeySet;
use aes::Aes128;
use aes::cipher::KeyInit;
use xts_mode::{Array, Xts128};

#[cfg(test)]
use crate::keys::{get_keys, Channel};
#[cfg(test)]
use std::path::PathBuf;

/// 用与 decrypt_vc 相同的方式解密单页（供测试与主流程共用）
pub fn decrypt_page_xts(page: &mut [u8], keys: &KeySet, block_no: u32) {
    assert_eq!(page.len() % 16, 0, "页大小必须是 16 的倍数");
    let t = KeySet::make_tweak(block_no);
    decrypt_page_tweak(page, keys, t);
}

/// 全量 tweak 变体：tweak = LE32(blockNo) | LE32(regionId) | VdUid[0..8]（容器提取用）
pub fn decrypt_page_tweak(page: &mut [u8], keys: &KeySet, tweak: [u8; 16]) {
    assert_eq!(page.len() % 16, 0, "页大小必须是 16 的倍数");
    let xts_key = keys.xts_key();
    let (dkey, tkey) = xts_key.split_at(16);
    let cipher1 = Aes128::new_from_slice(dkey).expect("DKey 长度错误");
    let cipher2 = Aes128::new_from_slice(tkey).expect("TKey 长度错误");
    let xts = Xts128::<Aes128>::new(cipher1, cipher2);
    // 整个 0x1000 页是一个 sector，tweak 值已含 blockNo，页内每 16 字节 GF(128) 递增
    xts.decrypt_area(page, page.len(), 0, |_| Array::from(tweak));
}

#[cfg(test)]
fn verified_dir() -> Option<PathBuf> {
    let dir = PathBuf::from("/home/yangsta/tempfiles/msixvc_verified");
    if dir.is_dir() { Some(dir) } else { None }
}

#[test]
fn real_page_decrypts_to_known_mz_pe() {
    let Some(dir) = verified_dir() else {
        panic!("缺少验证数据目录 /home/yangsta/tempfiles/msixvc_verified/");
    };
    let enc = std::fs::read(dir.join("encrypted_page0_0x4714000.bin")).expect("读密文失败");
    let expect = std::fs::read(dir.join("decrypted_page0_MZ_pe.bin")).expect("读明文失败");
    assert_eq!(enc.len(), 4096);
    assert_eq!(expect.len(), 4096);

    let mut page = enc;
    decrypt_page_xts(&mut page, &get_keys(Channel::Pre), 0x694AD828);

    // MZ 头必须出现
    assert_eq!(&page[..2], b"MZ", "解密后开头必须是 MZ");
    assert_eq!(page, expect, "解密结果与已知正确明文不一致");
}