//! MsiXVD 容器解析与提取（移植 BedrockLauncher.Core/GdkDecode，对照 C# 结构体 Pack=1 布局）
//!
//! 容器布局：
//!   [0x0000] XVD 头 + 签名（0x3000，有效头 0x1000）
//!   [0x3000 + EmbeddedXvdPages*0x1000]           MutableData
//!   [MutableDataOffset + MutableDataLength]       HashTree（每 0x1000 页 0xAA 条 0x18 hash 项 + 0x10 填充）
//!   [HashTreeOffset + (integrity? HashTreePages*0x1000 : 0)]  UserData（含 SegmentMetadata.bin）
//!   [UserDataOffset + UserDataPages*0x1000]       XvcInfo（regions + update segments）
//!   其后                                            驱动数据（按 region 提取）
//!
//! 页 tweak = LE32(blockNo，加密时取 hash 项[0x14..0x18]) | LE32(regionId) | VdUid[0..8]

use anyhow::{bail, Context, Result};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::keys::KeySet;

const PAGE: u64 = 0x1000;
const XVD_HEADER_INCL_SIGNATURE: u64 = 0x3000;
const HASH_ENTRY: u64 = 0x18;
const ENTRIES_PER_PAGE: u64 = PAGE / HASH_ENTRY; // 0xAA
const CACHE_SIZE: usize = 0x100000;

// ---- 头字段偏移（Pack=1 顺序布局，总 0x1000）----
const OFF_VOLUMES: usize = 0x208;
const OFF_DRIVE_SIZE: usize = 0x218;
const OFF_VD_UID: usize = 0x220;
const OFF_EMBEDDED_XVD_LEN: usize = 0x288;
const OFF_USER_DATA_LEN: usize = 0x28C;
const OFF_XVC_DATA_LEN: usize = 0x290;
const OFF_DYNAMIC_HEADER_LEN: usize = 0x294;
const OFF_MUTABLE_DATA_PAGE_COUNT: usize = 0x470;

const VOLUMES_ENCRYPTION_DISABLED: u32 = 0x2;
const VOLUMES_DATA_INTEGRITY_DISABLED: u32 = 0x4;
const VOLUMES_RESILIENCY_ENABLED: u32 = 0x10;

fn rd_u16(d: &[u8], off: usize) -> u16 {
    let b = d.get(off..off + 2).unwrap_or_else(|| panic!("rd_u16 越界 off={off} len={}", d.len()));
    u16::from_le_bytes([b[0], b[1]])
}
fn rd_u32(d: &[u8], off: usize) -> u32 {
    let b = d.get(off..off + 4).unwrap_or_else(|| panic!("rd_u32 越界 off={off} len={}", d.len()));
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}
fn rd_u64(d: &[u8], off: usize) -> u64 {
    let mut b = [0u8; 8];
    b.copy_from_slice(&d[off..off + 8]);
    u64::from_le_bytes(b)
}

fn ceil_div(a: u64, b: u64) -> u64 {
    (a + b - 1) / b
}
fn bytes_to_pages(bytes: u64) -> u64 {
    ceil_div(bytes, PAGE)
}

/// hash 树总页数（对照 CalculateNumberHashPages）
fn calc_hash_tree_pages(hashed: u64, resilient: bool) -> u64 {
    let mut pages = ceil_div(hashed, ENTRIES_PER_PAGE);
    if pages > 1 {
        let mut levels: u32 = 1;
        let mut result = 2u64;
        while result > 1 {
            // case levels: DATA_BLOCKS_IN_LEVEL{levels} = 0xAA^(levels+1)
            let blocks = ceil_div(hashed, ENTRIES_PER_PAGE.pow(levels + 1));
            result = blocks;
            pages += blocks;
            levels += 1;
        }
    }
    if resilient {
        pages *= 2;
    }
    pages
}

/// 数据页号 → (hash 块页号, 块内 entry 序号)（对照 ComputeHashBlockIndexForDataBlock level=0）
fn compute_hash_block_index(depth: u64, hashed: u64, data_block: u64, resilient: bool) -> (u64, u64) {
    let entry = data_block % ENTRIES_PER_PAGE;
    let mut hbi = data_block / ENTRIES_PER_PAGE;
    let mut depth = depth.saturating_sub(1);
    if depth > 0 {
        hbi += ceil_div(hashed, ENTRIES_PER_PAGE.pow(2));
        depth -= 1;
    }
    if depth > 0 {
        hbi += ceil_div(hashed, ENTRIES_PER_PAGE.pow(3));
        depth -= 1;
    }
    if depth > 0 {
        hbi += ceil_div(hashed, ENTRIES_PER_PAGE.pow(4));
    }
    if resilient {
        hbi *= 2;
    }
    (hbi, entry)
}

struct XvcRegion {
    id: u32,
    key_id: u16,
    first_segment_index: u32,
    offset: u64,
    length: u64,
}

struct SegmentMeta {
    path: String,
    file_size: u64,
}

pub struct Container {
    pub is_encrypted: bool,
    pub data_integrity: bool,
    pub resiliency: bool,
    pub hash_tree_depth: u64,
    pub hashed_pages: u64,
    pub hash_tree_page_offset: u64,
    pub xvd_user_data_offset: u64,
    pub vd_uid: [u8; 16],
    pub first_update_segment_page: u64,
    regions: Vec<XvcRegion>,
    segments: Vec<SegmentMeta>,
}

/// 解析 .vc 容器（头 + UserData + XvcInfo）
pub fn parse(vc: &mut File) -> Result<Container> {
    let mut head = [0u8; 0x1000];
    vc.read_exact(&mut head).context("读 XVD 头失败")?;
    let volumes = rd_u32(&head, OFF_VOLUMES);
    let is_encrypted = volumes & VOLUMES_ENCRYPTION_DISABLED == 0;
    let data_integrity = volumes & VOLUMES_DATA_INTEGRITY_DISABLED == 0;
    let resiliency = volumes & VOLUMES_RESILIENCY_ENABLED != 0;

    let drive_size = rd_u64(&head, OFF_DRIVE_SIZE);
    let mut vd_uid = [0u8; 16];
    vd_uid.copy_from_slice(&head[OFF_VD_UID..OFF_VD_UID + 16]);
    let embedded_xvd_len = rd_u32(&head, OFF_EMBEDDED_XVD_LEN) as u64;
    let user_data_len = rd_u32(&head, OFF_USER_DATA_LEN) as u64;
    let xvc_data_len = rd_u32(&head, OFF_XVC_DATA_LEN) as u64;
    let dynamic_header_len = rd_u32(&head, OFF_DYNAMIC_HEADER_LEN) as u64;
    let mutable_data_page_count = head[OFF_MUTABLE_DATA_PAGE_COUNT] as u64;

    let drive_pages = drive_size / PAGE; // GetPageOffset = floor
    let user_data_pages = bytes_to_pages(user_data_len);
    let xvc_info_pages = bytes_to_pages(xvc_data_len);
    let dynamic_header_pages = bytes_to_pages(dynamic_header_len);
    let hashed_pages = drive_pages + user_data_pages + xvc_info_pages + dynamic_header_pages;

    let hash_tree_pages = calc_hash_tree_pages(hashed_pages, resiliency);
    let hash_tree_depth = if hash_tree_pages > 1 {
        // 与 calc_hash_tree_pages 相同的循环终值（levels）
        let mut levels: u32 = 1;
        let mut result = 2u64;
        while result > 1 {
            result = ceil_div(hashed_pages, ENTRIES_PER_PAGE.pow(levels + 1));
            levels += 1;
        }
        levels as u64
    } else {
        1
    };

    let mutable_data_offset = embedded_xvd_len.div_ceil(PAGE) * PAGE + XVD_HEADER_INCL_SIGNATURE;
    let mutable_data_len = mutable_data_page_count * PAGE;
    let hash_tree_page_offset = mutable_data_len + mutable_data_offset;
    let xvd_user_data_offset =
        (if data_integrity { hash_tree_pages * PAGE } else { 0 }) + hash_tree_page_offset;

    // ---- UserData：找 SegmentMetadata.bin ----
    vc.seek(SeekFrom::Start(xvd_user_data_offset))?;
    let mut ud = vec![0u8; user_data_len as usize];
    vc.read_exact(&mut ud).context("读 UserData 失败")?;
    let ud_header_len = rd_u32(&ud, 0) as usize;
    let ud_type = rd_u32(&ud, 8);
    if ud_type != 0 {
        bail!("UserData 类型 {ud_type} 非 PackageFiles，暂不支持");
    }
    let pkg_files_off = ud_header_len;
    let file_count = rd_u32(&ud, pkg_files_off + 4 + 520) as usize; // Version(4)+Name(520) 后
    let mut segment_meta: Option<Vec<u8>> = None;
    for i in 0..file_count {
        let e = pkg_files_off + 528 + i * 528;
        let name = utf16_at(&ud, e, 260);
        let size = rd_u32(&ud, e + 520) as usize;
        let off = rd_u32(&ud, e + 524) as usize;
        if name == "SegmentMetadata.bin" {
            let start = ud_header_len + off;
            segment_meta = Some(ud[start..start + size].to_vec());
            break;
        }
    }
    let Some(seg_data) = segment_meta else {
        bail!("UserData 中无 SegmentMetadata.bin");
    };

    // ---- SegmentMetadata：段表 + 路径 ----
    let seg_count = rd_u32(&seg_data, 16) as usize;
    let seg_header_len = rd_u32(&seg_data, 12) as usize;
    let mut segments = Vec::with_capacity(seg_count);
    for i in 0..seg_count {
        let e = 0x64 + i * 0x10;
        let path_len = rd_u16(&seg_data, e + 2) as usize;
        let path_off = rd_u32(&seg_data, e + 4) as usize;
        let file_size = rd_u64(&seg_data, e + 8);
        let pstart = seg_header_len + seg_count * 0x10 + path_off;
        let path = utf16_at(&seg_data, pstart, path_len);
        segments.push(SegmentMeta { path, file_size });
    }

    // ---- XvcInfo：regions + update segments ----
    let xvc_info_offset = xvd_user_data_offset + user_data_pages * PAGE;
    vc.seek(SeekFrom::Start(xvc_info_offset))?;
    let mut xi = vec![0u8; xvc_data_len as usize];
    vc.read_exact(&mut xi).context("读 XvcInfo 失败")?;
    let version = rd_u32(&xi, 0xD10);
    if version < 1 {
        bail!("XvcInfo version {version} < 1");
    }
    let region_count = rd_u32(&xi, 0xD14) as usize;
    let update_segment_count = rd_u32(&xi, 0xD3C) as usize;
    let mut regions = Vec::with_capacity(region_count);
    for i in 0..region_count {
        let e = 0xDA8 + i * 0x80; // XvcRegionHeader 尺寸 0x80（Description 为 0x20 个 UTF-16 字符）
        regions.push(XvcRegion {
            id: rd_u32(&xi, e),
            key_id: rd_u16(&xi, e + 4),
            first_segment_index: rd_u32(&xi, e + 0xC),
            offset: rd_u64(&xi, e + 0x50),
            length: rd_u64(&xi, e + 0x58),
        });
    }
    let mut update_segments = Vec::with_capacity(update_segment_count);
    for i in 0..update_segment_count {
        let e = 0xDA8 + region_count * 0x80 + i * 12; // XvcUpdateSegment Pack=1 尺寸 12
        update_segments.push(rd_u32(&xi, e) as u64);
    }
    if update_segments.is_empty() {
        bail!("XvcInfo 无 update segments");
    }

    let Some(&first_update_segment_page) = update_segments.first() else {
        bail!("XvcInfo 无 update segments");
    };

    Ok(Container {
        is_encrypted,
        data_integrity,
        resiliency,
        hash_tree_depth,
        hashed_pages,
        hash_tree_page_offset,
        xvd_user_data_offset,
        vd_uid,
        first_update_segment_page,
        regions,
        segments,
    })
}

fn utf16_at(d: &[u8], off: usize, chars: usize) -> String {
    let units: Vec<u16> = (0..chars)
        .map(|i| u16::from_le_bytes([d[off + i * 2], d[off + i * 2 + 1]]))
        .collect();
    String::from_utf16_lossy(&units)
        .trim_end_matches('\0')
        .to_string()
}

/// 提取全部 region → 输出目录（流式，页级 XTS 解密）。
/// progress = (当前段序号, 总段数, 文件名)。返回提取的文件路径列表。
pub fn extract(
    vc_path: &Path,
    keys: &KeySet,
    out_dir: &Path,
    progress: &mut dyn FnMut(usize, usize, &str),
) -> Result<Vec<PathBuf>> {
    let mut vc = File::open(vc_path)?;
    let c = parse(&mut vc)?;
    let first_seg_page = c.first_update_segment_page;

    let mut extracted = Vec::new();
    for region in &c.regions {
        let extractable = region.first_segment_index != 0
            || first_seg_page * PAGE == region.offset;
        if !extractable {
            continue;
        }
        let should_decrypt = c.is_encrypted && region.key_id != 0xFFFF;
        let total_pages = region.length / PAGE;
        let mut current_segment = region.first_segment_index as usize;
        let mut processed: u64 = 0;

        // hash 缓存定位（region 首数据页）
        let start_data_page = (region.offset - c.xvd_user_data_offset) / PAGE;
        let (hb_page, mut entry_idx) =
            compute_hash_block_index(c.hash_tree_depth, c.hashed_pages, start_data_page, c.resiliency);
        let mut total_hash_off = c.hash_tree_page_offset + hb_page * PAGE;
        let mut hash_off = entry_idx as usize * HASH_ENTRY as usize;
        let mut hash_cache = vec![0u8; CACHE_SIZE];
        let mut hash_cache_valid = false;

        let mut page_cache = vec![0u8; CACHE_SIZE];
        let mut total_page_off = region.offset;
        let mut page_off = 0usize;
        let mut page_cache_valid = false;

        while c.segments.len() > current_segment && total_pages > processed {
            let seg = &c.segments[current_segment];
            // 段路径是 Windows 分隔符（data\store\x），Linux 上必须归一化为目录树
            let rel = seg.path.replace('\\', "/");
            let out_path = out_dir.join(&rel);
            if let Some(parent) = out_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut out_file = File::create(&out_path)?;
            let mut remaining = seg.file_size;
            progress(current_segment, c.segments.len(), &seg.path);
            loop {
                let chunk = remaining.min(PAGE) as usize;
                if c.data_integrity {
                    if !hash_cache_valid {
                        vc.seek(SeekFrom::Start(total_hash_off))?;
                        hash_cache.fill(0);
                        read_up_to(&mut vc, &mut hash_cache)?;
                        hash_cache_valid = true;
                    }
                }
                if !page_cache_valid {
                    vc.seek(SeekFrom::Start(total_page_off))?;
                    page_cache.fill(0);
                    read_up_to(&mut vc, &mut page_cache)?;
                    page_cache_valid = true;
                }
                let entry_off = hash_off;
                let mut block_no = 0u32;
                if c.data_integrity {
                    if should_decrypt {
                        let he = entry_off + 0x14;
                        block_no = u32::from_le_bytes([
                            hash_cache[he],
                            hash_cache[he + 1],
                            hash_cache[he + 2],
                            hash_cache[he + 3],
                        ]);
                    }
                    hash_off += HASH_ENTRY as usize;
                    entry_idx += 1;
                    if entry_idx == ENTRIES_PER_PAGE {
                        entry_idx = 0;
                        hash_off += 0x10; // 页尾填充
                    }
                    if hash_off as u64 >= PAGE {
                        total_hash_off += hash_off as u64;
                        hash_off = 0;
                        hash_cache_valid = false;
                    }
                }

                let page = &mut page_cache[page_off..page_off + PAGE as usize];
                if should_decrypt {
                    let mut tweak = [0u8; 16];
                    tweak[..4].copy_from_slice(&block_no.to_le_bytes());
                    tweak[4..8].copy_from_slice(&region.id.to_le_bytes());
                    tweak[8..].copy_from_slice(&c.vd_uid[..8]);
                    crate::xts::decrypt_page_tweak(page, keys, tweak);
                }
                out_file.write_all(&page[..chunk])?;

                remaining -= chunk as u64;
                processed += 1;
                page_off += PAGE as usize;
                if page_off == CACHE_SIZE {
                    total_page_off += CACHE_SIZE as u64;
                    page_off = 0;
                    page_cache_valid = false;
                }
                if remaining == 0 {
                    break;
                }
            }
            extracted.push(out_path);
            current_segment += 1;
        }
    }
    Ok(extracted)
}

/// 容忍短读（区域末页可能跨过文件尾；C# 同样允许短读），返回实际读到的字节数
fn read_up_to(f: &mut File, buf: &mut [u8]) -> Result<usize> {
    let mut done = 0;
    while done < buf.len() {
        let n = f.read(&mut buf[done..])?;
        if n == 0 {
            break;
        }
        done += n;
    }
    Ok(done)
}
