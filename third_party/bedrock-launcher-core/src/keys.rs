//! 密钥定义（来自 keys.md，已实测验证）

use serde::{Deserialize, Serialize};

/// 渠道：pre (Beta/Preview) / rel (Release)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Channel {
    Pre,
    Rel,
}

impl Channel {
    pub fn as_str(&self) -> &'static str {
        match self {
            Channel::Pre => "pre",
            Channel::Rel => "rel",
        }
    }
}

/// 单套密钥
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeySet {
    pub channel: Channel,
    pub guid: String,
    pub tkey: String, // 16 bytes hex
    pub dkey: String, // 16 bytes hex
}

impl KeySet {
    /// XTS key = DKey(16) + TKey(16)，DKey 在前
    pub fn xts_key(&self) -> [u8; 32] {
        let mut key = [0u8; 32];
        let dkey = hex::decode(&self.dkey).expect("invalid dkey hex");
        let tkey = hex::decode(&self.tkey).expect("invalid tkey hex");
        key[..16].copy_from_slice(&dkey);
        key[16..].copy_from_slice(&tkey);
        key
    }

    /// tweak 结构（16 字节）：blockNo(u32) | regionId(u32=1) | VdUid[0:8]
    pub fn make_tweak(block_no: u32) -> [u8; 16] {
        let mut tweak = [0u8; 16];
        tweak[..4].copy_from_slice(&block_no.to_le_bytes());
        tweak[4..8].copy_from_slice(&1u32.to_le_bytes()); // regionId = 1
        // VdUid 前 8 字节：3523bd98019b4c4e
        tweak[8..].copy_from_slice(&[0x35, 0x23, 0xbd, 0x98, 0x01, 0x9b, 0x4c, 0x4e]);
        tweak
    }
}

/// 预置密钥（来自 keys.md，已实测验证）
pub fn get_keys(channel: Channel) -> KeySet {
    match channel {
        Channel::Pre => KeySet {
            channel: Channel::Pre,
            guid: "3fd6491ff58b8d1fed7edbd89477dad9".into(),
            tkey: "802814007571f6a353c710ba972ef113".into(),
            dkey: "c6f250c54b315af61a33cca5de85b08a".into(),
        },
        Channel::Rel => KeySet {
            channel: Channel::Rel,
            guid: "91e7b9bd7cc93437e1a8bc602552df06".into(),
            tkey: "c9a969fbfcbbf5f46d71250af226cf6a".into(),
            dkey: "c7d15c25f9546344549391d16857391f".into(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xts_key_pre_correct_len() {
        let ks = get_keys(Channel::Pre);
        assert_eq!(ks.xts_key().len(), 32);
    }

    #[test]
    fn xts_key_rel_correct_len() {
        let ks = get_keys(Channel::Rel);
        assert_eq!(ks.xts_key().len(), 32);
    }

    #[test]
    fn tweak_structure() {
        let t = KeySet::make_tweak(0x694AD828);
        assert_eq!(t.len(), 16);
        // 前 4 字节 = blockNo (little endian)
        assert_eq!(&t[..4], &0x694AD828u32.to_le_bytes());
        // 第 5-8 字节 = regionId = 1
        assert_eq!(&t[4..8], &1u32.to_le_bytes());
        // 后 8 字节 = VdUid 前 8 字节
        assert_eq!(&t[8..], &[0x35, 0x23, 0xbd, 0x98, 0x01, 0x9b, 0x4c, 0x4e]);
    }
}