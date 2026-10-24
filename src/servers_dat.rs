//! 原版 `servers.dat`：Minecraft 多人服务器列表的 NBT 读写。
//!
//! 服务器列表和 Java 客户端共用同一个文件（`<实例目录>/servers.dat`）：
//! mctl 添加/删除后立即落盘，Java 下一次打开多人游戏就能看到；反过来，
//! Java 里加的服务器，mctl 刷新后也能读到。
//!
//! 只依赖标准库：NBT 是无压缩的树形结构，`servers.dat` 用到的标签类型有限，
//! 但为了不丢 Java 写入的未知字段，这里实现通用的 NBT 树并原样保留。

use anyhow::{anyhow, bail, Context, Result};
use std::path::Path;

use crate::api::mcserver::parse_addr;

const TAG_END: u8 = 0;
const TAG_BYTE: u8 = 1;
const TAG_SHORT: u8 = 2;
const TAG_INT: u8 = 3;
const TAG_LONG: u8 = 4;
const TAG_FLOAT: u8 = 5;
const TAG_DOUBLE: u8 = 6;
const TAG_BYTE_ARRAY: u8 = 7;
const TAG_STRING: u8 = 8;
const TAG_LIST: u8 = 9;
const TAG_COMPOUND: u8 = 10;
const TAG_INT_ARRAY: u8 = 11;
const TAG_LONG_ARRAY: u8 = 12;

/// 一条用户服务器（`servers.dat` 里的一个 compound）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserServer {
    pub name: String,
    pub host: String,
    pub port: u16,
    /// `data:image/png;base64,...`，Java 客户端保存的图标
    pub icon: Option<String>,
    pub accept_textures: bool,
}

impl UserServer {
    pub fn addr(&self) -> String {
        if self.host.contains(':') {
            if self.port == 25565 {
                format!("[{}]", self.host)
            } else {
                format!("[{}]:{}", self.host, self.port)
            }
        } else if self.port == 25565 {
            self.host.clone()
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }
}

/// NBT 树。Compound 保留字段顺序，写回时尽量保持原样。
#[derive(Debug, Clone, PartialEq)]
enum Nbt {
    Byte(i8),
    Short(i16),
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    ByteArray(Vec<u8>),
    String(String),
    List { elem: u8, items: Vec<Nbt> },
    Compound(Vec<(String, Nbt)>),
    IntArray(Vec<i32>),
    LongArray(Vec<i64>),
}

impl Nbt {
    fn compound(fields: Vec<(String, Nbt)>) -> Self {
        Nbt::Compound(fields)
    }
}

/// 读取 `servers.dat`。文件不存在或为空 → 空列表；格式损坏 → 报错
/// （宁可报错，也不覆盖 Java 刚写好的文件）。
pub fn load(path: &Path) -> Result<Vec<UserServer>> {
    let fields = read_root(path)?;
    let Some(servers) = fields.iter().find(|(k, _)| k == "servers").map(|(_, v)| v) else {
        return Ok(Vec::new());
    };
    let Nbt::List { items, .. } = servers else {
        bail!("servers.dat 的 servers 不是列表");
    };
    let mut out = Vec::new();
    for item in items {
        let Nbt::Compound(entry) = item else {
            continue;
        };
        let ip = entry
            .iter()
            .find(|(k, _)| k == "ip")
            .and_then(|(_, v)| match v {
                Nbt::String(s) => Some(s.as_str()),
                _ => None,
            })
            .unwrap_or("");
        let Ok((host, port)) = parse_addr(ip) else {
            continue; // 单条坏数据跳过，不拖垮整个列表
        };
        if host.trim().is_empty() {
            continue;
        }
        let name = entry
            .iter()
            .find(|(k, _)| k == "name")
            .and_then(|(_, v)| match v {
                Nbt::String(s) => Some(s.clone()),
                _ => None,
            })
            .unwrap_or_default();
        let icon = entry
            .iter()
            .find(|(k, _)| k == "icon")
            .and_then(|(_, v)| match v {
                Nbt::String(s) if !s.is_empty() => Some(s.clone()),
                _ => None,
            });
        let accept_textures = entry
            .iter()
            .find(|(k, _)| k == "acceptTextures")
            .and_then(|(_, v)| match v {
                Nbt::Byte(b) => Some(*b != 0),
                _ => None,
            })
            .unwrap_or(false);
        out.push(UserServer {
            name: if name.trim().is_empty() {
                host.clone()
            } else {
                name
            },
            host,
            port,
            icon,
            accept_textures,
        });
    }
    Ok(out)
}

/// 添加或更新一条服务器。返回实际写入的条目。
pub fn add(path: &Path, name: &str, input: &str) -> Result<UserServer> {
    let (host, port) = parse_addr(input)?;
    let host = host.trim().to_string();
    if host.is_empty() {
        bail!("服务器地址为空");
    }
    if host.chars().any(char::is_whitespace) {
        bail!("服务器地址不能包含空格");
    }
    let mut root = read_root_or_new(path)?;
    let list = ensure_servers_list(&mut root)?;
    let display_name = if name.trim().is_empty() {
        host.clone()
    } else {
        name.trim().to_string()
    };
    let ip = ip_text(&host, port);

    // 已存在同一个 host:port → 只更新名字，图标/未知字段原样保留。
    if let Nbt::List { items, .. } = list {
        for item in items.iter_mut() {
            let Nbt::Compound(fields) = item else {
                continue;
            };
            let old_ip = fields
                .iter()
                .find(|(k, _)| k == "ip")
                .and_then(|(_, v)| match v {
                    Nbt::String(s) => Some(s.clone()),
                    _ => None,
                })
                .unwrap_or_default();
            let Ok((old_host, old_port)) = parse_addr(&old_ip) else {
                continue;
            };
            if old_host.eq_ignore_ascii_case(&host) && old_port == port {
                upsert(fields, "name", Nbt::String(display_name.clone()));
                let entry = UserServer {
                    name: display_name,
                    host,
                    port,
                    icon: fields.iter().find(|(k, _)| k == "icon").and_then(|(_, v)| match v {
                        Nbt::String(s) if !s.is_empty() => Some(s.clone()),
                        _ => None,
                    }),
                    accept_textures: fields
                        .iter()
                        .find(|(k, _)| k == "acceptTextures")
                        .and_then(|(_, v)| match v {
                            Nbt::Byte(b) => Some(*b != 0),
                            _ => None,
                        })
                        .unwrap_or(false),
                };
                write_root(path, &Nbt::compound(root))?;
                return Ok(entry);
            }
        }
        items.push(Nbt::Compound(vec![
            ("name".to_string(), Nbt::String(display_name.clone())),
            ("ip".to_string(), Nbt::String(ip)),
            ("acceptTextures".to_string(), Nbt::Byte(0)),
        ]));
    }
    write_root(path, &Nbt::compound(root))?;
    Ok(UserServer {
        name: display_name,
        host,
        port,
        icon: None,
        accept_textures: false,
    })
}

/// 删除一条服务器（按 host:port 匹配）。返回是否真的删了。
pub fn remove(path: &Path, input: &str) -> Result<bool> {
    let (host, port) = parse_addr(input)?;
    let mut root = read_root_or_new(path)?;
    let Some(list) = root.iter_mut().find(|(k, _)| k == "servers").map(|(_, v)| v) else {
        return Ok(false);
    };
    let Nbt::List { items, .. } = list else {
        bail!("servers.dat 的 servers 不是列表");
    };
    let before = items.len();
    items.retain(|item| {
        let Nbt::Compound(fields) = item else {
            return true;
        };
        let ip = fields
            .iter()
            .find(|(k, _)| k == "ip")
            .and_then(|(_, v)| match v {
                Nbt::String(s) => Some(s.as_str()),
                _ => None,
            })
            .unwrap_or("");
        match parse_addr(ip) {
            Ok((h, p)) => !(h.eq_ignore_ascii_case(&host) && p == port),
            Err(_) => true,
        }
    });
    let removed = items.len() != before;
    if removed {
        write_root(path, &Nbt::compound(root))?;
    }
    Ok(removed)
}

fn ip_text(host: &str, port: u16) -> String {
    if host.contains(':') {
        if port == 25565 {
            format!("[{host}]")
        } else {
            format!("[{host}]:{port}")
        }
    } else if port == 25565 {
        host.to_string()
    } else {
        format!("{host}:{port}")
    }
}

fn ensure_servers_list<'a>(root: &'a mut Vec<(String, Nbt)>) -> Result<&'a mut Nbt> {
    let pos = root.iter().position(|(k, _)| k == "servers");
    let idx = match pos {
        Some(i) => i,
        None => {
            root.push((
                "servers".to_string(),
                Nbt::List {
                    elem: TAG_COMPOUND,
                    items: Vec::new(),
                },
            ));
            root.len() - 1
        }
    };
    match &mut root[idx].1 {
        Nbt::List { elem, items } if items.is_empty() => {
            // 空列表的类型字节可能是 End（NBT 规范允许），写回时改成 compound
            *elem = TAG_COMPOUND;
            Ok(&mut root[idx].1)
        }
        Nbt::List { elem, .. } if *elem == TAG_COMPOUND => Ok(&mut root[idx].1),
        Nbt::List { .. } => bail!("servers.dat 的 servers 列表元素类型不是 compound"),
        _ => bail!("servers.dat 的 servers 不是列表"),
    }
}

fn upsert(fields: &mut Vec<(String, Nbt)>, key: &str, value: Nbt) {
    match fields.iter_mut().find(|(k, _)| k == key) {
        Some((_, v)) => *v = value,
        None => fields.push((key.to_string(), value)),
    }
}

fn read_root_or_new(path: &Path) -> Result<Vec<(String, Nbt)>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    read_root(path)
}

fn read_root(path: &Path) -> Result<Vec<(String, Nbt)>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let bytes = std::fs::read(path).with_context(|| format!("读取 {} 失败", path.display()))?;
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    match parse(&bytes)? {
        Nbt::Compound(fields) => Ok(fields),
        _ => bail!("servers.dat 根标签不是 compound"),
    }
}

fn write_root(path: &Path, nbt: &Nbt) -> Result<()> {
    let bytes = encode_root(nbt)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file_name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("servers.dat");
    let tmp = path.with_file_name(format!("{file_name}.tmp"));
    std::fs::write(&tmp, &bytes)?;
    std::fs::rename(&tmp, path).with_context(|| format!("写入 {} 失败", path.display()))?;
    Ok(())
}

// ---------- NBT 编解码 ----------

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self
            .pos
            .checked_add(n)
            .ok_or_else(|| anyhow!("NBT 长度溢出"))?;
        if end > self.data.len() {
            bail!("NBT 数据截断");
        }
        let out = &self.data[self.pos..end];
        self.pos = end;
        Ok(out)
    }

    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    fn i8(&mut self) -> Result<i8> {
        Ok(self.u8()? as i8)
    }

    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_be_bytes(self.take(2)?.try_into().unwrap()))
    }

    fn i16(&mut self) -> Result<i16> {
        Ok(i16::from_be_bytes(self.take(2)?.try_into().unwrap()))
    }

    fn i32(&mut self) -> Result<i32> {
        Ok(i32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn i64(&mut self) -> Result<i64> {
        Ok(i64::from_be_bytes(self.take(8)?.try_into().unwrap()))
    }

    fn f32(&mut self) -> Result<f32> {
        Ok(f32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn f64(&mut self) -> Result<f64> {
        Ok(f64::from_be_bytes(self.take(8)?.try_into().unwrap()))
    }

    fn string(&mut self) -> Result<String> {
        let len = self.u16()? as usize;
        let bytes = self.take(len)?;
        decode_modified_utf8(bytes)
    }

    fn payload(&mut self, tag: u8, depth: usize) -> Result<Nbt> {
        if depth > 64 {
            bail!("NBT 嵌套过深");
        }
        Ok(match tag {
            TAG_BYTE => Nbt::Byte(self.i8()?),
            TAG_SHORT => Nbt::Short(self.i16()?),
            TAG_INT => Nbt::Int(self.i32()?),
            TAG_LONG => Nbt::Long(self.i64()?),
            TAG_FLOAT => Nbt::Float(self.f32()?),
            TAG_DOUBLE => Nbt::Double(self.f64()?),
            TAG_BYTE_ARRAY => {
                let len = self.len()?;
                Nbt::ByteArray(self.take(len)?.to_vec())
            }
            TAG_STRING => Nbt::String(self.string()?),
            TAG_LIST => {
                let elem = self.u8()?;
                let len = self.len()?;
                if elem == TAG_END && len > 0 {
                    bail!("NBT 列表元素类型为 End 但长度非零");
                }
                let mut items = Vec::with_capacity(len.min(4096));
                for _ in 0..len {
                    items.push(self.payload(elem, depth + 1)?);
                }
                Nbt::List { elem, items }
            }
            TAG_COMPOUND => {
                let mut fields = Vec::new();
                loop {
                    let t = self.u8()?;
                    if t == TAG_END {
                        break;
                    }
                    let name = self.string()?;
                    let value = self.payload(t, depth + 1)?;
                    fields.push((name, value));
                    if fields.len() > 1_000_000 {
                        bail!("NBT compound 字段过多");
                    }
                }
                Nbt::Compound(fields)
            }
            TAG_INT_ARRAY => {
                let len = self.len()?;
                let mut out = Vec::with_capacity(len.min(4096));
                for _ in 0..len {
                    out.push(self.i32()?);
                }
                Nbt::IntArray(out)
            }
            TAG_LONG_ARRAY => {
                let len = self.len()?;
                let mut out = Vec::with_capacity(len.min(4096));
                for _ in 0..len {
                    out.push(self.i64()?);
                }
                Nbt::LongArray(out)
            }
            other => bail!("未知 NBT 标签类型 {other}"),
        })
    }

    fn len(&mut self) -> Result<usize> {
        let n = self.i32()?;
        if n < 0 {
            bail!("NBT 长度为负");
        }
        // 长度不可能超过剩余字节数（最紧凑的 ByteArray 也是 1 字节/项）
        let n = n as usize;
        if n > self.data.len().saturating_sub(self.pos) + 1 {
            bail!("NBT 长度超出数据范围");
        }
        Ok(n)
    }
}

fn parse(data: &[u8]) -> Result<Nbt> {
    let mut r = Reader::new(data);
    let tag = r.u8()?;
    if tag != TAG_COMPOUND {
        bail!("NBT 根标签不是 compound（实际 {tag}）");
    }
    let _name = r.string()?;
    let value = r.payload(tag, 0)?;
    if r.pos != data.len() {
        bail!("NBT 末尾有 {} 字节多余数据", data.len() - r.pos);
    }
    Ok(value)
}

fn encode_root(nbt: &Nbt) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    out.push(TAG_COMPOUND);
    write_modified_utf8(&mut out, "")?;
    write_payload(&mut out, nbt)?;
    Ok(out)
}

fn write_payload(out: &mut Vec<u8>, nbt: &Nbt) -> Result<()> {
    match nbt {
        Nbt::Byte(v) => out.push(*v as u8),
        Nbt::Short(v) => out.extend_from_slice(&v.to_be_bytes()),
        Nbt::Int(v) => out.extend_from_slice(&v.to_be_bytes()),
        Nbt::Long(v) => out.extend_from_slice(&v.to_be_bytes()),
        Nbt::Float(v) => out.extend_from_slice(&v.to_be_bytes()),
        Nbt::Double(v) => out.extend_from_slice(&v.to_be_bytes()),
        Nbt::ByteArray(v) => {
            write_len(out, v.len())?;
            out.extend_from_slice(v);
        }
        Nbt::String(s) => write_modified_utf8(out, s)?,
        Nbt::List { elem, items } => {
            out.push(*elem);
            write_len(out, items.len())?;
            for item in items {
                write_payload(out, item)?;
            }
        }
        Nbt::Compound(fields) => {
            for (name, value) in fields {
                out.push(tag_id(value));
                write_modified_utf8(out, name)?;
                write_payload(out, value)?;
            }
            out.push(TAG_END);
        }
        Nbt::IntArray(v) => {
            write_len(out, v.len())?;
            for x in v {
                out.extend_from_slice(&x.to_be_bytes());
            }
        }
        Nbt::LongArray(v) => {
            write_len(out, v.len())?;
            for x in v {
                out.extend_from_slice(&x.to_be_bytes());
            }
        }
    }
    Ok(())
}

fn tag_id(nbt: &Nbt) -> u8 {
    match nbt {
        Nbt::Byte(_) => TAG_BYTE,
        Nbt::Short(_) => TAG_SHORT,
        Nbt::Int(_) => TAG_INT,
        Nbt::Long(_) => TAG_LONG,
        Nbt::Float(_) => TAG_FLOAT,
        Nbt::Double(_) => TAG_DOUBLE,
        Nbt::ByteArray(_) => TAG_BYTE_ARRAY,
        Nbt::String(_) => TAG_STRING,
        Nbt::List { .. } => TAG_LIST,
        Nbt::Compound(_) => TAG_COMPOUND,
        Nbt::IntArray(_) => TAG_INT_ARRAY,
        Nbt::LongArray(_) => TAG_LONG_ARRAY,
    }
}

fn write_len(out: &mut Vec<u8>, len: usize) -> Result<()> {
    let len: i32 = len
        .try_into()
        .map_err(|_| anyhow!("NBT 数组/列表过长"))?;
    out.extend_from_slice(&len.to_be_bytes());
    Ok(())
}

/// NBT 字符串使用 Java 的 Modified UTF-8（`DataInput.readUTF`）：
/// U+0000 写成 C0 80，增补平面字符按 UTF-16 代理对写成两段 3 字节。
fn write_modified_utf8(out: &mut Vec<u8>, s: &str) -> Result<()> {
    let mut body = Vec::new();
    for u in s.encode_utf16() {
        match u {
            0 => body.extend_from_slice(&[0xC0, 0x80]),
            0x01..=0x7F => body.push(u as u8),
            0x80..=0x7FF => {
                body.push(0xC0 | (u >> 6) as u8);
                body.push(0x80 | (u & 0x3F) as u8);
            }
            _ => {
                body.push(0xE0 | (u >> 12) as u8);
                body.push(0x80 | ((u >> 6) & 0x3F) as u8);
                body.push(0x80 | (u & 0x3F) as u8);
            }
        }
    }
    let len: u16 = body
        .len()
        .try_into()
        .map_err(|_| anyhow!("NBT 字符串过长"))?;
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(&body);
    Ok(())
}

fn decode_modified_utf8(bytes: &[u8]) -> Result<String> {
    let mut units: Vec<u16> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        i += 1;
        match b {
            0x01..=0x7F => units.push(b as u16),
            0xC0..=0xDF => {
                let b2 = *bytes.get(i).ok_or_else(|| anyhow!("Modified UTF-8 截断"))?;
                i += 1;
                if b == 0xC0 && b2 == 0x80 {
                    units.push(0);
                } else if b2 & 0xC0 == 0x80 {
                    units.push((((b & 0x1F) as u16) << 6) | (b2 & 0x3F) as u16);
                } else {
                    bail!("Modified UTF-8 非法双字节序列");
                }
            }
            0xE0..=0xEF => {
                let b2 = *bytes.get(i).ok_or_else(|| anyhow!("Modified UTF-8 截断"))?;
                let b3 = *bytes
                    .get(i + 1)
                    .ok_or_else(|| anyhow!("Modified UTF-8 截断"))?;
                i += 2;
                if b2 & 0xC0 != 0x80 || b3 & 0xC0 != 0x80 {
                    bail!("Modified UTF-8 非法三字节序列");
                }
                units.push(
                    (((b & 0x0F) as u16) << 12)
                        | (((b2 & 0x3F) as u16) << 6)
                        | (b3 & 0x3F) as u16,
                );
            }
            // 兼容标准 UTF-8 的 4 字节序列（少数第三方启动器会这么写）
            0xF0..=0xF4 => {
                let b2 = *bytes.get(i).ok_or_else(|| anyhow!("UTF-8 截断"))?;
                let b3 = *bytes.get(i + 1).ok_or_else(|| anyhow!("UTF-8 截断"))?;
                let b4 = *bytes.get(i + 2).ok_or_else(|| anyhow!("UTF-8 截断"))?;
                i += 3;
                let cp = (((b & 0x07) as u32) << 18)
                    | (((b2 & 0x3F) as u32) << 12)
                    | (((b3 & 0x3F) as u32) << 6)
                    | (b4 & 0x3F) as u32;
                let ch = char::from_u32(cp).ok_or_else(|| anyhow!("非法 Unicode 码点"))?;
                let mut buf = [0u16; 2];
                units.extend_from_slice(ch.encode_utf16(&mut buf));
            }
            _ => bail!("Modified UTF-8 非法起始字节 {b:#x}"),
        }
    }
    String::from_utf16(&units).map_err(|_| anyhow!("Modified UTF-8 含孤立代理项"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "mctl_servers_dat_{}_{}",
            std::process::id(),
            name
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("servers.dat")
    }

    #[test]
    fn modified_utf8_roundtrip() {
        for s in ["", "hello", "中文服务器", "a\0b", "emoji 😀", "𝕄ℂ"] {
            let mut out = Vec::new();
            write_modified_utf8(&mut out, s).unwrap();
            // 前两个字节是 u16 长度，decode 只吃 body
            assert_eq!(decode_modified_utf8(&out[2..]).unwrap(), s);
        }
    }

    #[test]
    fn add_load_remove_roundtrip() {
        let path = tmp("roundtrip");
        add(&path, "我的服", "wdsj.net").unwrap();
        add(&path, "带端口", "mc.example.net:25566").unwrap();
        let list = load(&path).unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].name, "我的服");
        assert_eq!(list[0].addr(), "wdsj.net");
        assert_eq!(list[1].port, 25566);

        assert!(remove(&path, "wdsj.net").unwrap());
        let list = load(&path).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].host, "mc.example.net");
        assert!(!remove(&path, "wdsj.net").unwrap());
    }

    #[test]
    fn add_same_addr_updates_name_and_keeps_icon() {
        let path = tmp("update");
        add(&path, "旧名字", "wdsj.net").unwrap();
        // 模拟 Java 写入了 icon
        let mut root = read_root(&path).unwrap();
        if let Some(Nbt::List { items, .. }) = root.iter_mut().find(|(k, _)| k == "servers").map(|(_, v)| v) {
            if let Nbt::Compound(fields) = &mut items[0] {
                fields.push(("icon".into(), Nbt::String("data:image/png;base64,AA==".into())));
            }
        }
        write_root(&path, &Nbt::compound(root)).unwrap();
        add(&path, "新名字", "wdsj.net").unwrap();
        let list = load(&path).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "新名字");
        assert_eq!(list[0].icon.as_deref(), Some("data:image/png;base64,AA=="));
    }

    #[test]
    fn preserves_unknown_root_and_server_fields() {
        let path = tmp("unknown");
        let root = Nbt::Compound(vec![
            ("customRoot".into(), Nbt::Int(42)),
            (
                "servers".into(),
                Nbt::List {
                    elem: TAG_COMPOUND,
                    items: vec![Nbt::Compound(vec![
                        ("name".into(), Nbt::String("Java 加的".into())),
                        ("ip".into(), Nbt::String("java.example.net".into())),
                        ("custom".into(), Nbt::Long(7)),
                        ("acceptTextures".into(), Nbt::Byte(1)),
                    ])],
                },
            ),
        ]);
        write_root(&path, &root).unwrap();
        add(&path, "mctl 加的", "mctl.example.net").unwrap();
        let fields = read_root(&path).unwrap();
        assert_eq!(fields.iter().find(|(k, _)| k == "customRoot").map(|(_, v)| v), Some(&Nbt::Int(42)));
        let Nbt::List { items, .. } = fields.iter().find(|(k, _)| k == "servers").unwrap().1.clone() else {
            panic!("servers 不是列表");
        };
        assert_eq!(items.len(), 2);
        let first = match &items[0] {
            Nbt::Compound(f) => f,
            _ => panic!(),
        };
        assert!(first.iter().any(|(k, v)| k == "custom" && *v == Nbt::Long(7)));
        assert_eq!(load(&path).unwrap()[0].accept_textures, true);
    }

    #[test]
    fn empty_list_with_end_type_can_be_added_to() {
        let path = tmp("end-list");
        let root = Nbt::Compound(vec![(
            "servers".into(),
            Nbt::List {
                elem: TAG_END,
                items: Vec::new(),
            },
        )]);
        write_root(&path, &root).unwrap();
        add(&path, "空表", "empty.example.net").unwrap();
        assert_eq!(load(&path).unwrap().len(), 1);
    }

    #[test]
    fn parses_vanilla_layout_fixture() {
        // 手工按 NBT 规范拼一个最小的原版 servers.dat：
        // root compound -> "servers" list<compound> -> {name, ip, acceptTextures}
        let path = tmp("vanilla-fixture");
        let mut b = vec![
            0x0A, 0x00, 0x00, // root compound, empty name
            0x09, 0x00, 0x07, b's', b'e', b'r', b'v', b'e', b'r', b's', // "servers" list
            0x0A, 0x00, 0x00, 0x00, 0x01, // element type compound, length 1
            0x08, 0x00, 0x04, b'n', b'a', b'm', b'e', 0x00, 0x04, b'T', b'e', b's', b't',
            0x08, 0x00, 0x02, b'i', b'p', 0x00, 0x08, b'w', b'd', b's', b'j', b'.', b'n', b'e', b't',
            0x01, 0x00, 0x0E, b'a', b'c', b'c', b'e', b'p', b't', b'T', b'e', b'x', b't', b'u', b'r', b'e', b's', 0x01,
            0x00, // end server compound
            0x00, // end root compound
        ];
        std::fs::write(&path, &mut b).unwrap();
        let list = load(&path).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "Test");
        assert_eq!(list[0].addr(), "wdsj.net");
        assert!(list[0].accept_textures);
    }

    #[test]
    fn missing_and_empty_file_are_empty() {
        let path = tmp("missing");
        assert!(load(&path).unwrap().is_empty());
        std::fs::write(&path, b"").unwrap();
        assert!(load(&path).unwrap().is_empty());
    }

    #[test]
    fn truncated_file_is_an_error() {
        let path = tmp("truncated");
        std::fs::write(&path, [TAG_COMPOUND, 0, 0, TAG_STRING]).unwrap();
        assert!(load(&path).is_err());
        // 损坏文件不能被 add 静默覆盖
        assert!(add(&path, "x", "y.net").is_err());
    }

    #[test]
    fn ipv6_roundtrip() {
        let path = tmp("ipv6");
        add(&path, "v6", "[::1]:25566").unwrap();
        let list = load(&path).unwrap();
        assert_eq!(list[0].addr(), "[::1]:25566");
        assert!(remove(&path, "[::1]:25566").unwrap());
    }
}
