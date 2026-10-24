//! 网易客户端下载安装（实测）
//!
//! 布局：网易根目录 = <mc_dir>/netease/（本身就是 .minecraft 等价结构）：
//! versions/<id>/<id>.jar|.json|natives/ libraries/ assets/ mods/
//! 流程：
//! 1. CLIENT /game-patch-info {mc_version: 0} → base 包（assets+libraries+versions）
//! 2. CLIENT /game-patch-info {mc_version: <enum>} → 版本包 + core_lib（7z）
//! 3. base 包自带 Linux natives（实测）
//! 4. GATEWAY /game-auth-item-list/query/search-by-game → 联机核心 mod 列表
//! 5. GATEWAY /user-item-download-v2/get-list → mod 下载信息 → jar → mods/
//! 6. CLIENT /user-item-download-v2 {item_id: gameId} → 游戏组件包（7z）→ 合并

use anyhow::{anyhow, Result};
use serde::Deserialize;
use serde_json::json;
use std::path::{Path, PathBuf};

use crate::api::netease::post_signed;
use crate::api::CLIENT;
use crate::api::GATEWAY;

/// 进服链路里，角色 / 游戏组件 / `-DlauncherGameId` 一律用**所选服务器的 item id**
/// （列表项的 `entity_id`，运行时从平台拉取——不要在代码里写死任何一个服的 id）。
const GAME_TYPE_NETGAME: i64 = 2;

/// EnumGameVersion→ game-patch-info mc_version
pub fn version_enum(name: &str) -> u32 {
    match name {
        "1.6.4" => 1006004,
        "1.7.2" => 1007002,
        "1.7.10" => 1007010,
        "1.8" => 1008000,
        "1.8.8" => 1008008,
        "1.8.9" => 1008009,
        "1.9.4" => 1009004,
        "1.10.2" => 1010002,
        "1.11.2" => 1011002,
        "1.12" => 1012000,
        "1.12.2" => 1012002,
        "1.13.2" => 1013002,
        "1.14.3" => 1014003,
        "1.15" => 1015000,
        "1.16" => 1016000,
        "1.18" => 1018000,
        "1.19.2" => 1019002,
        "1.20" => 1020000,
        "1.20.6" => 1020006,
        "1.21" => 1021000,
        "1.21.8" => 1021008,
        _ => 0,
    }
}

/// game-patch-info 响应（EntityCoreLibResponse）
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct CoreLib {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(rename = "mc_version", default)]
    pub mc_version: u32,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub md5: String,
    #[serde(default)]
    pub size: u64,
    #[serde(rename = "core_lib_url", default)]
    pub core_lib_url: String,
    #[serde(rename = "core_lib_md5", default)]
    pub core_lib_md5: String,
    #[serde(rename = "core_lib_name", default)]
    pub core_lib_name: String,
    #[serde(rename = "core_lib_size", default)]
    pub core_lib_size: u64,
}

#[derive(Debug, Clone, Deserialize)]
struct IidList {
    #[serde(rename = "iid_list", default)]
    iid_list: Vec<i64>,
}

/// 联机核心 mod / 游戏组件下载项（EntityUserItemDownloadListItem.SubEntities）
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct SubEntity {
    #[serde(rename = "item_id", default, deserialize_with = "de_i64_or_str")]
    pub item_id: i64,
    #[serde(default)]
    pub name: String,
    #[serde(rename = "res_name", default)]
    pub res_name: String,
    #[serde(rename = "res_url", default)]
    pub res_url: String,
    #[serde(rename = "res_md5", default)]
    pub res_md5: String,
    #[serde(rename = "jar_md5", default)]
    pub jar_md5: String,
    #[serde(default, deserialize_with = "de_i64_or_str")]
    pub mtype: i64,
    #[serde(rename = "entity_id", default, deserialize_with = "de_i64_or_str")]
    pub entity_id: i64,
}

/// 数字字段有时按字符串下发（实测：组件 `entity_id` 是 `"363020"`）→ 两种都收。
/// 用 serde_json 默认的 i64 解析会在实网直接报
/// `invalid type: string "363020", expected i64`，导致整条安装链路失败。
fn de_i64_or_str<'de, D>(d: D) -> std::result::Result<i64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct V;
    impl serde::de::Visitor<'_> for V {
        type Value = i64;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            write!(f, "整数或其十进制字符串")
        }
        fn visit_i64<E: serde::de::Error>(self, v: i64) -> std::result::Result<i64, E> {
            Ok(v)
        }
        fn visit_u64<E: serde::de::Error>(self, v: u64) -> std::result::Result<i64, E> {
            i64::try_from(v).map_err(serde::de::Error::custom)
        }
        fn visit_str<E: serde::de::Error>(self, v: &str) -> std::result::Result<i64, E> {
            v.trim().parse::<i64>().map_err(serde::de::Error::custom)
        }
    }
    d.deserialize_any(V)
}

/// 网易客户端是否已安装（versions/<id>/<id>.jar 存在）
pub fn is_installed(root: &Path, version: &str) -> bool {
    root.join("versions")
        .join(version)
        .join(format!("{version}.jar"))
        .is_file()
}

/// 完整安装网易客户端到 root（.minecraft 等价结构）。
/// progress(阶段描述, 完成数, 总数)。
/// 注意：游戏组件包**不在这里装**——它是「按服务器」隔离的，进服前由
/// `ensure_server_components` 保证当前服务器在位、别的服务器不残留。
pub async fn install_client(
    root: &Path,
    version: &str,
    user_id: &str,
    user_token: &str,
    progress: Progress<'_>,
) -> Result<()> {
    std::fs::create_dir_all(root)?;
    progress.stage("获取网易客户端信息…", 0, 3);
    // 1. base 包
    let base = get_core_lib(0, user_id, user_token).await?;
    if base.url.is_empty() {
        return Err(anyhow!("game-patch-info(base) 未返回下载地址"));
    }
    progress.stage("获取版本包信息…", 1, 3);
    // 2. 版本包 + core lib
    let ver = get_core_lib(version_enum(version), user_id, user_token).await?;
    if ver.url.is_empty() {
        return Err(anyhow!("game-patch-info({version}) 未返回下载地址"));
    }
    progress.stage("下载网易客户端 base…", 1, 3);
    download_extract(&base.url, &base.md5, root, false, progress, " base").await?;
    progress.stage("下载网易客户端版本包…", 2, 3);
    download_extract(&ver.url, &ver.md5, root, true, progress, "版本包").await?;
    // 3. core lib（7z）：jar → libraries 同名覆盖；其他 → versions/<id>/
    if !ver.core_lib_url.is_empty() {
        progress.stage("下载核心库…", 3, 3);
        install_core_lib(
            root,
            version,
            &ver.core_lib_url,
            &ver.core_lib_md5,
            progress,
        )
        .await?;
    }
    // 4. Linux natives：网易包自带（实测）
    progress.stage("解压内置 natives…", 3, 3);
    extract_packaged_natives(root, version)?;
    // 4.5 runtime native 兜底：本地没有就按平台老版本包补齐（**不内嵌官方二进制**）
    if !runtime_native_present(root, version) {
        ensure_runtime_native_from_platform(root, version, user_id, user_token, progress).await?;
    }
    // 5. 联机核心 mod（版本级；进服前还会幂等刷新一次）
    install_core_mods(root, version, user_id, user_token, progress).await?;
    // 6. 游戏组件包：见 `ensure_server_components`（按服务器隔离，进服前处理）
    progress.stage("网易客户端就绪", 3, 3);
    Ok(())
}

async fn get_core_lib(mc_version: u32, user_id: &str, user_token: &str) -> Result<CoreLib> {
    let resp = post_signed(
        CLIENT,
        "/game-patch-info",
        &json!({ "mc_version": mc_version }),
        user_id,
        user_token,
    )
    .await?;
    if resp.code != 0 {
        return Err(anyhow!(
            "game-patch-info 失败: {} ({})",
            resp.message,
            resp.details
        ));
    }
    let ent = resp
        .entity
        .ok_or_else(|| anyhow!("game-patch-info 无 entity"))?;
    serde_json::from_value(ent).map_err(|e| anyhow!("解析 game-patch-info 失败: {e}"))
}

/// 下载 → md5 校验 → 解压（zip/7z）→ 合并进 root
async fn download_extract(
    url: &str,
    expect_md5: &str,
    root: &Path,
    overwrite: bool,
    p: Progress<'_>,
    what: &str,
) -> Result<()> {
    p.stage(&format!("下载网易客户端{what}…"), 0, 1);
    let tmp = temp_dir("mctl-ne");
    let arc_path = tmp.join("pkg");
    download_reported(url, expect_md5, &arc_path, &format!("网易客户端{what}"), p).await?;
    p.stage(&format!("解压{what}…"), 1, 1);
    let out = tmp.join("out");
    std::fs::create_dir_all(&out)?;
    if is_zip_file(&arc_path) {
        extract_zip(&arc_path, &out)?;
    } else {
        extract_7z(&arc_path, &out)?;
    }
    // 顺手捞出 runtime native（官方包里带的话），避免把它内嵌进源码/仓库
    harvest_runtime_native(&out, root);
    merge_into(&out, root, overwrite)?;
    let _ = std::fs::remove_dir_all(&tmp);
    Ok(())
}

fn is_zip(data: &[u8]) -> bool {
    data.len() > 4 && &data[..4] == b"PK\x03\x04"
}

/// 下载进度出口：`stage` 粗粒度阶段（状态行 / 下载中心任务标题），
/// `file` 单文件字节进度（下载中心的细线）。两套输出都往同一个任务上报。
#[derive(Clone, Copy)]
pub struct Progress<'a> {
    pub stage: &'a (dyn Fn(&str, usize, usize) + Send + Sync),
    pub file: &'a (dyn Fn(crate::launcher::FileProgress) + Send + Sync),
}

impl<'a> Progress<'a> {
    fn stage(&self, phase: &str, done: usize, total: usize) {
        (self.stage)(phase, done, total);
    }
}

/// 流式下载到目标路径 + md5 校验（`on_tick` 每块回调「已下, 总长」）。
/// 走 `download_to`：写临时文件再改名，中途失败不留半截文件。
async fn download_verified_to(
    url: &str,
    expect_md5: &str,
    dest: &Path,
    on_tick: &(dyn Fn(u64, u64) + Send + Sync),
) -> Result<()> {
    // 有 md5 的整包走可续传：上次断在半路时从 .tmp 的断点接着下，
    // 避免上百 MB 的客户端包每次重试都从 0 开始。
    let client = crate::launcher::http_client_big();
    if expect_md5.is_empty() {
        crate::launcher::download_to(&client, url, dest, |d, t| on_tick(d, t)).await?;
    } else {
        crate::launcher::download_to_resumable(&client, url, dest, |d, t| on_tick(d, t)).await?;
    }
    if !expect_md5.is_empty() {
        let got = crate::launcher::md5_file(dest)?;
        if !got.eq_ignore_ascii_case(expect_md5) {
            // 续传拼出来的文件也可能是坏的：连半截一起清掉，下次从头下
            let _ = std::fs::remove_file(dest);
            let _ = std::fs::remove_file(dest.with_extension("tmp"));
            return Err(anyhow!(
                "md5 校验失败: 期望 {expect_md5} 实际 {got}（{url}）"
            ));
        }
    }
    Ok(())
}

/// 带细线的下载：`name` 是下载中心里那一行的文件名
async fn download_reported(
    url: &str,
    expect_md5: &str,
    dest: &Path,
    name: &str,
    p: Progress<'_>,
) -> Result<()> {
    let report = |done: u64, total: u64, finished: bool, failed: bool| {
        (p.file)(crate::launcher::FileProgress {
            name: name.to_string(),
            done,
            total,
            finished_at: finished.then(std::time::Instant::now),
            failed,
        });
    };
    match download_verified_to(url, expect_md5, dest, &|d, t| report(d, t, false, false)).await {
        Ok(()) => {
            report(0, 0, true, false);
            Ok(())
        }
        Err(e) => {
            report(0, 0, true, true);
            Err(e)
        }
    }
}

/// 存档/压缩包类型判定（读文件头 4 字节，别整包读内存）
fn is_zip_file(path: &Path) -> bool {
    use std::io::Read;
    // 读 8 字节：is_zip 要求「长度 > 4」，只读 4 字节会被判成非 zip
    let mut head = [0u8; 8];
    match std::fs::File::open(path) {
        Ok(mut f) => f.read(&mut head).unwrap_or(0) >= 4 && is_zip(&head),
        Err(_) => false,
    }
}

/// core lib 包安装：平台用它**覆盖** base/版本包自带的旧文件。
///
/// 包内是按版本分组的目录树（如 `V_1_20_libs/…`），派生规则：
/// - `*.jar` → 启动类路径上的库：在 `libraries/` 下找**同名**文件**原地覆盖**；
///   找不到同名文件（新库）才退回 `libraries/<文件名>`。
/// - 其它文件（如 `1.20.dat`）→ `versions/<版本>/`（指纹里要用它的 md5）。
///
/// 必须覆盖：base/版本包自带的是旧版网络层库，和随版本下发的核心 mod 代次不匹配，
/// 一旦旧库生效，客户端收到任何控制通道回复都会在本 tick 崩掉。
async fn install_core_lib(
    root: &Path,
    version: &str,
    url: &str,
    md5: &str,
    p: Progress<'_>,
) -> Result<()> {
    let tmp = temp_dir("mctl-corelib");
    let arc = tmp.join("core.7z");
    download_reported(url, md5, &arc, &format!("网易客户端 {version} 核心库"), p).await?;
    let out = tmp.join("out");
    std::fs::create_dir_all(&out)?;
    extract_7z(&arc, &out)?;
    // 顺手捞出 runtime native（见 download_extract 同款说明）
    harvest_runtime_native(&out, root);
    apply_core_lib_tree(&out, root, version)?;
    // 记下已落盘的 core lib 版本，启动前据此决定要不要重下（1.20 实测约 1 MB）
    let _ = std::fs::write(core_lib_marker(root, version), md5);
    let _ = std::fs::remove_dir_all(&tmp);
    Ok(())
}

/// core lib 已落盘版本的标记文件（在 `versions/<版本>/` 内，不影响启动参数）
fn core_lib_marker(root: &Path, version: &str) -> PathBuf {
    root.join("versions").join(version).join(".core-lib.md5")
}

/// 把 core lib 解出来的文件树铺到客户端根（返回落盘文件数）
fn apply_core_lib_tree(out: &Path, root: &Path, version: &str) -> Result<usize> {
    let mut files = Vec::new();
    let mut stack = vec![out.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            let path = e.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                files.push(path);
            }
        }
    }
    let libs = root.join("libraries");
    let mut n = 0usize;
    for from in files {
        let Some(name) = from.file_name() else {
            continue;
        };
        let fname = name.to_string_lossy().to_string();
        let target = if fname.ends_with(".jar") {
            find_library_by_name(&libs, &fname).unwrap_or_else(|| libs.join(&fname))
        } else {
            root.join("versions").join(version).join(&fname)
        };
        if let Some(dir) = target.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::copy(&from, &target)?;
        n += 1;
    }
    Ok(n)
}

/// 在 `libraries/` 下递归找同名文件（core lib 要覆盖的就是启动类路径上那个 jar）
fn find_library_by_name(libs: &Path, fname: &str) -> Option<PathBuf> {
    let mut stack = vec![libs.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            let path = e.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.file_name().is_some_and(|n| n == fname) {
                return Some(path);
            }
        }
    }
    None
}

/// 已装客户端也要跟上平台核心库：base/版本包自带旧文件，平台侧靠 core lib 覆盖它们。
/// 用落盘标记判断，只有平台侧换包时才重下（幂等、绝大多数启动只多一次接口调用）。
pub async fn refresh_core_lib_if_stale(
    root: &Path,
    version: &str,
    user_id: &str,
    user_token: &str,
    progress: Progress<'_>,
) -> Result<()> {
    let ver = get_core_lib(version_enum(version), user_id, user_token).await?;
    if ver.core_lib_url.is_empty() {
        return Ok(());
    }
    if let Ok(done) = std::fs::read_to_string(core_lib_marker(root, version)) {
        if done.trim() == ver.core_lib_md5 {
            return Ok(());
        }
    }
    progress.stage("更新网易客户端核心库…", 0, 1);
    install_core_lib(
        root,
        version,
        &ver.core_lib_url,
        &ver.core_lib_md5,
        progress,
    )
    .await
}

/// Linux natives：网易包自带（无需任何外部下载）。libraries 内 *-natives-linux.jar
/// 解压到 versions/<id>/natives（实测）；
/// 包内自带 versions/<id>/natives（1.20/1.12.2）已随 base 包合并，此处幂等。
pub fn extract_packaged_natives(root: &Path, version: &str) -> Result<()> {
    let natives_dir = root.join("versions").join(version).join("natives");
    std::fs::create_dir_all(&natives_dir)?;
    let libs_dir = root.join("libraries");
    let mut found = 0usize;
    if libs_dir.is_dir() {
        let mut stack = vec![libs_dir.clone()];
        while let Some(dir) = stack.pop() {
            for ent in std::fs::read_dir(&dir)? {
                let ent = ent?;
                let p = ent.path();
                if p.is_dir() {
                    stack.push(p);
                    continue;
                }
                let Some(name) = p.file_name().and_then(|n| n.to_str()) else {
                    continue;
                };
                if name.ends_with("-natives-linux.jar") {
                    unzip_into(&p, &natives_dir)?;
                    found += 1;
                }
            }
        }
    }
    if found == 0 {
        // 老版本包内自带 natives 目录（随 base 解压合并），无 jar 属正常
        let builtin = root.join("versions").join(version).join("natives");
        if builtin.is_dir() && builtin.read_dir()?.next().is_some() {
            ensure_runtime_native(&natives_dir, root)?;
            return Ok(());
        }
        return Err(anyhow!(
            "未找到内置 Linux natives（libraries 内 *-natives-linux.jar）"
        ));
    }
    ensure_runtime_native(&natives_dir, root)?;
    Ok(())
}

/// 网易 runtime native：客户端固定从 `natives/runtime/` 加载这个文件名，缺失即启动崩溃。
/// mctl **不内嵌官方二进制**：按「目标已有 → 本地缓存 → 其它已安装版本/包（含 jar 内部）」就地补齐；
/// 都没有时由平台侧兜底（`ensure_runtime_native_from_platform`，从网易老版本包取一份）。
pub const RUNTIME_NATIVE_NAME: &str = "api-ms-win-crt-utility-l1-1-1.dll";

/// 下载包解压时顺手捞出来的 runtime native 缓存目录（在客户端根目录下，随卸载一起清）
fn runtime_cache_dir(root: &Path) -> PathBuf {
    root.join(".runtime-native")
}

/// 目标 natives 目录里是否已经有 runtime native
pub fn runtime_native_present(root: &Path, version: &str) -> bool {
    root.join("versions")
        .join(version)
        .join("natives")
        .join("runtime")
        .join(RUNTIME_NATIVE_NAME)
        .is_file()
}

/// 从任意目录树（含 zip/jar 条目）里找 runtime native 的字节
fn find_runtime_native_bytes(dir: &Path) -> Option<Vec<u8>> {
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                stack.push(p);
                continue;
            }
            let name_matches = p
                .file_name()
                .map(|n| n == RUNTIME_NATIVE_NAME)
                .unwrap_or(false);
            if name_matches {
                if let Ok(b) = std::fs::read(&p) {
                    if !b.is_empty() {
                        return Some(b);
                    }
                }
            } else if matches!(
                p.extension().and_then(|x| x.to_str()),
                Some("jar") | Some("zip")
            ) {
                if let Some(b) = read_zip_entry_named(&p, RUNTIME_NATIVE_NAME) {
                    return Some(b);
                }
            }
        }
    }
    None
}

/// 读 zip/jar 里最后一段文件名匹配的条目
fn read_zip_entry_named(archive: &Path, want: &str) -> Option<Vec<u8>> {
    let f = std::fs::File::open(archive).ok()?;
    let mut z = zip::ZipArchive::new(f).ok()?;
    for i in 0..z.len() {
        let Ok(mut e) = z.by_index(i) else { continue };
        let hit = e.name().rsplit('/').next() == Some(want);
        if hit {
            let mut buf = Vec::new();
            std::io::copy(&mut e, &mut buf).ok()?;
            if !buf.is_empty() {
                return Some(buf);
            }
        }
    }
    None
}

/// 下载包解压后调用：树里有 runtime native 就存进缓存（避免以后内嵌官方二进制）
fn harvest_runtime_native(tree: &Path, root: &Path) {
    let target = runtime_cache_dir(root).join(RUNTIME_NATIVE_NAME);
    if target.is_file() {
        return;
    }
    if let Some(bytes) = find_runtime_native_bytes(tree) {
        if std::fs::create_dir_all(runtime_cache_dir(root)).is_ok() {
            let _ = std::fs::write(&target, &bytes);
        }
    }
}

/// 本地补齐 runtime native（幂等）；找不到时给可行动错误。
fn ensure_runtime_native(natives_dir: &Path, root: &Path) -> Result<()> {
    let dir = natives_dir.join("runtime");
    let target = dir.join(RUNTIME_NATIVE_NAME);
    if target.is_file() {
        return Ok(());
    }
    let cached = runtime_cache_dir(root).join(RUNTIME_NATIVE_NAME);
    let bytes = if cached.is_file() {
        std::fs::read(&cached).ok()
    } else {
        None
    }
    .or_else(|| find_runtime_native_bytes(root));

    match bytes {
        Some(b) if !b.is_empty() => {
            std::fs::create_dir_all(&dir)?;
            std::fs::write(&target, &b)?;
            Ok(())
        }
        _ => Err(anyhow!(
            "缺少网易 runtime 文件 {RUNTIME_NATIVE_NAME}（客户端启动必需；本机各处都没有）：\
             请保持网络可用让 mctl 自动补齐，或先安装一次老版本客户端（如 1.12.2 服）"
        )),
    }
}

/// 平台兜底：老版本包自带该文件，拉「核心库（体积小）→ 版本包」逐个取一份。
/// 只在本地完全找不到时才会走到这里（下载大包换一个小文件，属于罕见路径）。
pub async fn ensure_runtime_native_from_platform(
    root: &Path,
    version: &str,
    user_id: &str,
    user_token: &str,
    progress: Progress<'_>,
) -> Result<()> {
    let natives_dir = root.join("versions").join(version).join("natives");
    if ensure_runtime_native(&natives_dir, root).is_ok() {
        return Ok(());
    }
    for cand in ["1.12.2", "1.8.9"] {
        progress.stage(&format!("从网易 {cand} 包补齐 runtime 文件…"), 0, 1);
        let Ok(meta) = get_core_lib(version_enum(cand), user_id, user_token).await else {
            continue;
        };
        for (url, md5) in [(meta.core_lib_url, meta.core_lib_md5), (meta.url, meta.md5)] {
            if url.is_empty() {
                continue;
            }
            let tmp = temp_dir("mctl-runtime");
            let arc = tmp.join("pkg");
            let out = tmp.join("out");
            let mut got: Option<Vec<u8>> = None;
            if download_reported(
                &url,
                &md5,
                &arc,
                &format!("网易 {cand} runtime 包"),
                progress,
            )
            .await
            .is_ok()
                && std::fs::create_dir_all(&out).is_ok()
            {
                let ok = if is_zip_file(&arc) {
                    extract_zip(&arc, &out)
                } else {
                    extract_7z(&arc, &out)
                };
                if ok.is_ok() {
                    got = find_runtime_native_bytes(&out);
                }
            }
            let _ = std::fs::remove_dir_all(&tmp);
            if let Some(b) = got.filter(|b| !b.is_empty()) {
                let dir = natives_dir.join("runtime");
                std::fs::create_dir_all(&dir)?;
                std::fs::write(dir.join(RUNTIME_NATIVE_NAME), &b)?;
                let cache = runtime_cache_dir(root);
                let _ = std::fs::create_dir_all(&cache);
                let _ = std::fs::write(cache.join(RUNTIME_NATIVE_NAME), &b);
                progress.stage("runtime 文件已补齐", 1, 1);
                return Ok(());
            }
        }
    }
    Err(anyhow!(
        "无法补齐网易 runtime 文件；请安装一次老版本（1.12.2）客户端后重试"
    ))
}

/// 联机核心 mod：auth-item-list → get-list → 下载 zip → jar → mods/<name>@<mtype>@<entityId>.jar
/// 幂等：同名 jar 已存在直接跳过（服务端更新时补新文件）。
pub async fn install_core_mods(
    root: &Path,
    version: &str,
    user_id: &str,
    user_token: &str,
    progress: Progress<'_>,
) -> Result<()> {
    let resp = post_signed(
        GATEWAY,
        "/game-auth-item-list/query/search-by-game",
        &json!({
            "mc_version_id": version_enum(version) as i64,
            "game_type": GAME_TYPE_NETGAME,
        }),
        user_id,
        user_token,
    )
    .await?;
    if resp.code != 0 {
        return Err(anyhow!(
            "联机核心 mod 列表失败: {} ({})",
            resp.message,
            resp.details
        ));
    }
    let ent = resp
        .entity
        .ok_or_else(|| anyhow!("核心 mod 列表无 entity"))?;
    let list: IidList = serde_json::from_value(ent)?;
    if list.iid_list.is_empty() {
        return Ok(());
    }
    let resp = post_signed(
        GATEWAY,
        "/user-item-download-v2/get-list",
        &json!({ "item_id_list": list.iid_list }),
        user_id,
        user_token,
    )
    .await?;
    if resp.code != 0 {
        return Err(anyhow!(
            "核心 mod 下载信息失败: {} ({})",
            resp.message,
            resp.details
        ));
    }
    let items: Vec<ItemList> = resp
        .entities
        .unwrap_or_default()
        .into_iter()
        .map(serde_json::from_value)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let total: usize = items.iter().map(|it| it.sub_entities.len()).sum();
    let mods_dir = root.join("mods");
    let mut names: Vec<(String, String, String)> = Vec::new();
    let mut i = 0usize;
    for it in &items {
        for sub in &it.sub_entities {
            i += 1;
            progress.stage(&format!("下载联机核心 mod {}…", sub.name), i, total);
            if sub.res_url.is_empty() {
                continue;
            }
            // 官方落盘名 = `{res_name 去后缀}@{实体 MTypeId}@{实体 EntityId}.jar`；
            // 幂等 + 自愈：同名文件 md5 与 jar_md5 一致才跳过（不一致重下）
            let disk = core_mod_disk_name(it, sub);
            let target = mods_dir.join(&disk);
            let fresh = target.is_file()
                && (sub.jar_md5.is_empty()
                    || md5_file_upper(&target)?.eq_ignore_ascii_case(sub.jar_md5.trim()));
            if !fresh {
                let tmp = temp_dir("mctl-mod");
                let arc = tmp.join("m.7z");
                download_reported(&sub.res_url, &sub.res_md5, &arc, &disk, progress).await?;
                let out = tmp.join("out");
                std::fs::create_dir_all(&out)?;
                if is_zip_file(&arc) {
                    extract_zip(&arc, &out)?;
                } else {
                    extract_7z(&arc, &out)?;
                }
                std::fs::create_dir_all(&mods_dir)?;
                let jar = find_jar(&out)
                    .ok_or_else(|| anyhow!("核心 mod 包内没有 jar: {}", sub.res_name))?;
                std::fs::copy(&jar, &target)?;
                let _ = std::fs::remove_dir_all(&tmp);
            }
            // 完整性校验：指纹里的 md5 就是它，错了整条 mod 列表都不可信
            let got = md5_file_upper(&target)?;
            if !sub.jar_md5.is_empty() && !got.eq_ignore_ascii_case(sub.jar_md5.trim()) {
                return Err(anyhow!(
                    "核心 mod 校验失败: {disk} md5 {got} != {}",
                    sub.jar_md5
                ));
            }
            names.push((disk, core_mod_fingerprint_name(it), it.item_id.to_string()));
        }
    }
    write_mod_names(root, &names)?;
    Ok(())
}

/// 官方落盘名：`{res_name 去后缀}@{实体 MTypeId}@{实体 EntityId}.jar`
/// （InstallerService.InstallGameMods；实测核心 mod 的实体 EntityId = 0）
fn core_mod_disk_name(it: &ItemList, sub: &SubEntity) -> String {
    let stem = Path::new(&sub.res_name)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| it.item_id.to_string());
    format!("{stem}@{}@{}.jar", it.mtypeid, it.entity_id)
}

/// 官方指纹里上报的名：`{ItemId}@{MTypeId}@0.jar`（尾部固定 0，与落盘名可能不同）
fn core_mod_fingerprint_name(it: &ItemList) -> String {
    format!("{}@{}@0.jar", it.item_id, it.mtypeid)
}

/// 落盘名 → 上报名 的映射文件（扫描目录拿不到上报名，安装时记录）
pub fn mod_names_path(root: &Path) -> PathBuf {
    root.join(".mods-names.json")
}

/// 把（落盘名, 上报名, iid）合并进映射文件（保留旧条目：核心 mod 是版本级的，不会被清理）
fn write_mod_names(root: &Path, entries: &[(String, String, String)]) -> Result<()> {
    if entries.is_empty() {
        return Ok(());
    }
    let p = mod_names_path(root);
    let mut map: serde_json::Map<String, serde_json::Value> = std::fs::read_to_string(&p)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    for (disk, mod_path, iid) in entries {
        map.insert(
            disk.clone(),
            json!({ "modPath": mod_path, "id": mod_path, "iid": iid }),
        );
    }
    std::fs::write(&p, serde_json::to_string_pretty(&map)?)
        .map_err(|e| anyhow!("写入 mod 名称映射失败: {e}"))?;
    Ok(())
}

/// 文件 md5（大写十六进制；官方 FileUtil.ComputeMd5FromFile 同款）
fn md5_file_upper(p: &Path) -> Result<String> {
    Ok(crate::launcher::md5_file(p)?.to_ascii_uppercase())
}

/// 游戏组件包：CLIENT /user-item-download-v2 {item_id: 服务器 item id} → sub_entities → 7z 合并
/// （实测）
/// 游戏组件包：CLIENT /user-item-download-v2 {item_id: 服务器 item id} → sub_entities → 7z 合并。
/// 实测包内是 **`.minecraft/` 前缀**的整份目录（`mods/*.jar`、`config/`、`shaderpacks/`…），
/// 官方把 `.minecraft` 当根用（`compDir/.minecraft/mods`），这里剥掉前缀后合并进 root。
/// 返回包内文件相对 root 的路径列表，供「按服务器清理」使用。
async fn install_game_components(
    root: &Path,
    game_id: &str,
    user_id: &str,
    user_token: &str,
    progress: Progress<'_>,
) -> Result<Vec<String>> {
    let resp = post_signed(
        CLIENT,
        "/user-item-download-v2",
        &json!({ "item_id": game_id, "length": 0, "offset": 0 }),
        user_id,
        user_token,
    )
    .await?;
    if resp.code != 0 {
        return Err(anyhow!(
            "游戏组件列表失败: {} ({})",
            resp.message,
            resp.details
        ));
    }
    let Some(ent) = resp.entity else {
        // 该服务器没有组件包（部分服如此）→ 视为「无需组件」，不是错误
        return Ok(Vec::new());
    };
    if ent.is_null() {
        return Ok(Vec::new());
    }
    let list: ItemList =
        serde_json::from_value(ent).map_err(|e| anyhow!("解析游戏组件列表失败: {e}"))?;
    let total = list.sub_entities.len();
    let mut files = Vec::new();
    for (i, sub) in list.sub_entities.iter().enumerate() {
        progress.stage(&format!("下载游戏组件 {}…", sub.name), i, total);
        if sub.res_url.is_empty() {
            continue;
        }
        let tmp = temp_dir("mctl-comp");
        let arc = tmp.join("c");
        let label = if sub.name.trim().is_empty() {
            format!("游戏组件 {i}/{total}")
        } else {
            sub.name.clone()
        };
        download_reported(&sub.res_url, &sub.res_md5, &arc, &label, progress).await?;
        let out = tmp.join("out");
        std::fs::create_dir_all(&out)?;
        if is_zip_file(&arc) {
            extract_zip(&arc, &out)?;
        } else {
            extract_7z(&arc, &out)?;
        }
        // 官方结构：包内根是 `.minecraft/`；没有这层就按包根当根
        let base = if out.join(".minecraft").is_dir() {
            out.join(".minecraft")
        } else {
            out.clone()
        };
        files.extend(collect_rel_files(&base)?);
        // 覆写式合并：包里的 config/ 等就是服务器要求的样子（官方每次整包重解压）
        merge_into(&base, root, true)?;
        let _ = std::fs::remove_dir_all(&tmp);
    }
    files.sort();
    files.dedup();
    Ok(files)
}

/// 列 `base` 下全部文件的相对路径（`/` 分隔）
fn collect_rel_files(base: &Path) -> Result<Vec<String>> {
    fn walk(base: &Path, cur: &Path, out: &mut Vec<String>) -> Result<()> {
        for e in std::fs::read_dir(cur)?.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(base, &p, out)?;
            } else if let Ok(rel) = p.strip_prefix(base) {
                out.push(rel.to_string_lossy().replace('\\', "/"));
            }
        }
        Ok(())
    }
    let mut v = Vec::new();
    walk(base, base, &mut v)?;
    Ok(v)
}

/// 服务器组件清单：记录「这个服务器装进来的文件相对路径」（jar + config + 光影…）。
/// 官方是每个 (服务器, 角色) 一个 runtime；mctl 共用根目录，用清单做等效隔离：
/// 进 A 服前把 B 服装的文件清掉，B 的 flag 一并清 → 下次进 B 重拉。
fn components_manifest_path(root: &Path, game_id: &str) -> PathBuf {
    root.join(".components").join(format!("{game_id}.mods"))
}

/// components_flag_path
fn components_flag_path(root: &Path, game_id: &str) -> PathBuf {
    root.join(format!(".components.{game_id}.flag"))
}

/// 该服务器的组件是否真的装好：清单 + flag 缺一不可。
/// 只有 flag（旧版本装的、清单缺失）视为未装 → 重拉一次，避免留下无法归属的残留。
fn components_installed(root: &Path, game_id: &str) -> bool {
    components_manifest_path(root, game_id).is_file()
        && components_flag_path(root, game_id).is_file()
}

/// 清掉「不是当前服务器」装的组件文件。当前服务器清单里的文件（两服可能共用）不动。
fn prune_other_components(root: &Path, keep_game_id: &str) {
    let keep: std::collections::BTreeSet<String> =
        std::fs::read_to_string(components_manifest_path(root, keep_game_id))
            .map(|t| {
                t.lines()
                    .map(|l| l.trim().to_string())
                    .filter(|l| !l.is_empty())
                    .collect()
            })
            .unwrap_or_default();
    let comp_dir = root.join(".components");
    let Ok(rd) = std::fs::read_dir(&comp_dir) else {
        return;
    };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let Some(id) = name.strip_suffix(".mods") else {
            continue;
        };
        if id == keep_game_id {
            continue;
        }
        if let Ok(list) = std::fs::read_to_string(e.path()) {
            for f in list.lines().map(str::trim).filter(|l| !l.is_empty()) {
                // 清单是我们自己写的，但纵深防御：越界路径一律不碰
                if keep.contains(f) || crate::launcher::modpack::is_unsafe_rel_path(f) {
                    continue;
                }
                let p = root.join(f);
                let _ = std::fs::remove_file(&p);
                // 顺手收敛空目录（配置都在层级目录里，不清理会留一堆空壳）
                let mut d = p.parent().map(|x| x.to_path_buf());
                while let Some(dir) = d {
                    if dir == root || !dir.starts_with(root) {
                        break;
                    }
                    if std::fs::remove_dir(&dir).is_err() {
                        break; // 非空（还有别的服/用户的文件）就停
                    }
                    d = dir.parent().map(|x| x.to_path_buf());
                }
            }
        }
        let _ = std::fs::remove_file(e.path());
        let _ = std::fs::remove_file(components_flag_path(root, id));
    }
}

/// 进服前保证「当前服务器的组件在位、别的服务器的组件不残留」（官方 runtime 的等效物）。
/// 幂等：装好过（清单 + flag 齐全）就只做一次本地清理，不联网。
pub async fn ensure_server_components(
    root: &Path,
    game_id: &str,
    user_id: &str,
    user_token: &str,
    progress: Progress<'_>,
) -> Result<()> {
    prune_other_components(root, game_id);
    if components_installed(root, game_id) {
        return Ok(());
    }
    // 旧版本只写了 flag：重拉一次并重建清单
    let _ = std::fs::remove_file(components_flag_path(root, game_id));
    let files = install_game_components(root, game_id, user_id, user_token, progress).await?;
    let manifest = components_manifest_path(root, game_id);
    if let Some(p) = manifest.parent() {
        std::fs::create_dir_all(p)?;
    }
    std::fs::write(&manifest, files.join("\n"))?;
    std::fs::write(components_flag_path(root, game_id), b"1")?;
    Ok(())
}

/// 带 md5 校验的下载（大文件专用：长超时，避免 base 150MB 触发 30s 默认超时）
fn md5_hex(data: &[u8]) -> String {
    use md5::{Digest, Md5};
    let mut h = Md5::new();
    h.update(data);
    let d = h.finalize();
    d.iter().map(|b| format!("{b:02x}")).collect()
}

fn temp_dir(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("{name}-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&p);
    p
}

/// 压缩包条目名 → 安全落盘路径：只接受全普通组件的相对路径；
/// 绝对路径、`..`、盘符前缀一律拒绝（zip-slip 防护）。
fn safe_zip_path(dest: &Path, name: &str) -> Option<PathBuf> {
    let p = Path::new(name);
    if p.components()
        .all(|c| matches!(c, std::path::Component::Normal(_)))
    {
        Some(dest.join(p))
    } else {
        None
    }
}

fn extract_zip(zip_path: &Path, dest: &Path) -> Result<()> {
    let file = std::fs::File::open(zip_path)?;
    let mut archive = zip::ZipArchive::new(file)?;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        let name = entry.name().to_string();
        if name.ends_with('/') {
            continue;
        }
        // 越界条目直接跳过（而不是解到目标目录之外）
        let Some(out) = safe_zip_path(dest, &name) else {
            continue;
        };
        if let Some(p) = out.parent() {
            std::fs::create_dir_all(p)?;
        }
        let mut f = std::fs::File::create(&out)?;
        std::io::copy(&mut entry, &mut f)?;
    }
    Ok(())
}

fn extract_7z(seven_zip: &Path, dest: &Path) -> Result<()> {
    let out = std::process::Command::new("7z")
        .arg("x")
        .arg("-y")
        .arg(format!("-o{}", dest.display()))
        .arg(seven_zip)
        .output()?;
    if !out.status.success() {
        return Err(anyhow!(
            "7z 解压失败: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(())
}

/// 解压目录若顶层为 .minecraft / minecraft，取其内容
fn find_minecraft_root(tmp: &Path) -> PathBuf {
    let Ok(entries) = std::fs::read_dir(tmp) else {
        return tmp.to_path_buf();
    };
    let mut dirs = Vec::new();
    for e in entries.flatten() {
        if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            dirs.push(e.path());
        }
    }
    if dirs.len() == 1 {
        let name = dirs[0]
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        if name == ".minecraft" || name == "minecraft" {
            return dirs[0].clone();
        }
    }
    tmp.to_path_buf()
}

/// 递归合并 src 到 dest（overwrite=false 时不覆盖已有文件）
fn merge_into(src_dir: &Path, dest: &Path, overwrite: bool) -> Result<()> {
    copy_dir(&find_minecraft_root(src_dir), dest, overwrite)
}

fn copy_dir(src: &Path, dest: &Path, overwrite: bool) -> Result<()> {
    std::fs::create_dir_all(dest)?;
    for e in std::fs::read_dir(src)? {
        let e = e?;
        let from = e.path();
        let to = dest.join(e.file_name());
        if e.file_type()?.is_dir() {
            copy_dir(&from, &to, overwrite)?;
        } else if e.file_type()?.is_file() {
            if !overwrite && to.exists() {
                continue;
            }
            if let Some(p) = to.parent() {
                std::fs::create_dir_all(p)?;
            }
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

/// 解压 zip 到目录（natives 用：跳过 META-INF，已有文件不覆盖，扁平化落盘）
fn unzip_into(zip_path: &Path, out_dir: &Path) -> Result<()> {
    let file = std::fs::File::open(zip_path)?;
    let mut archive = zip::ZipArchive::new(file)?;
    std::fs::create_dir_all(out_dir)?;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        let name = entry.name().to_string();
        if name.contains("META-INF") || name.ends_with('/') {
            continue;
        }
        // 只取最后一段文件名；拒绝空名、`.`、`..`
        let file_name = name.rsplit('/').next().unwrap_or(&name);
        if file_name.is_empty() || file_name == "." || file_name == ".." {
            continue;
        }
        let target = out_dir.join(file_name);
        if target.is_file() {
            continue;
        }
        let mut out = std::fs::File::create(&target)?;
        std::io::copy(&mut entry, &mut out)?;
    }
    Ok(())
}

fn find_jar(dir: &Path) -> Option<PathBuf> {
    let mut found = None;
    walk(dir, &mut |p| {
        if p.extension().map(|e| e == "jar").unwrap_or(false) {
            found = Some(p.to_path_buf());
        }
    });
    found
}

fn walk(dir: &Path, f: &mut impl FnMut(&Path)) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, f);
        } else {
            f(&p);
        }
    }
}

#[derive(Debug, Deserialize)]
struct ItemList {
    #[serde(rename = "sub_entities", default)]
    sub_entities: Vec<SubEntity>,
    /// 实体级字段（官方落盘名与指纹名都用它们，见 `core_mod_disk_name`）
    #[serde(rename = "item_id", default, deserialize_with = "de_i64_or_str")]
    item_id: i64,
    #[serde(rename = "mtypeid", default, deserialize_with = "de_i64_or_str")]
    mtypeid: i64,
    #[serde(rename = "entity_id", default, deserialize_with = "de_i64_or_str")]
    entity_id: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 压缩包类型判定只看文件头：zip 走 extract_zip、7z 走 extract_7z
    /// （网易包两种都有，判错就是「解压失败」）
    #[test]
    fn is_zip_file_reads_only_header() {
        let dir = crate::storage::tmp_config_dir("is-zip-file");
        let zip = dir.join("a.zip");
        std::fs::write(&zip, b"PK\x03\x04rest-of-file").unwrap();
        assert!(is_zip_file(&zip));
        let seven = dir.join("b.7z");
        std::fs::write(&seven, b"\x37\x7a\xbc\xaf\x27\x1crest").unwrap();
        assert!(!is_zip_file(&seven));
        // 空文件/不存在：不 panic，按「不是 zip」处理
        let empty = dir.join("c.bin");
        std::fs::write(&empty, b"").unwrap();
        assert!(!is_zip_file(&empty));
        assert!(!is_zip_file(&dir.join("nope.bin")));
    }

    /// zip-slip 防护：越界条目名必须被拒（绝对路径 / `..` / 混合）
    /// runtime native 本地补齐：任意已安装版本里已有 → 直接拷给新版本（幂等、不联网）
    #[test]
    fn runtime_native_copies_from_other_version() {
        let root = crate::storage::tmp_config_dir("rt-native");
        let src = root
            .join("versions/1.12.2/natives/runtime")
            .join(RUNTIME_NATIVE_NAME);
        std::fs::create_dir_all(src.parent().unwrap()).unwrap();
        std::fs::write(&src, b"ELF-OLD").unwrap();
        let dst = root.join("versions/1.20/natives");
        ensure_runtime_native(&dst, &root).unwrap();
        let got = std::fs::read(dst.join("runtime").join(RUNTIME_NATIVE_NAME)).unwrap();
        assert_eq!(got, b"ELF-OLD");
        // 幂等：再跑一次不报错
        ensure_runtime_native(&dst, &root).unwrap();
    }

    /// core lib 解出来的目录树要能覆盖启动类路径上的旧库，非 jar 文件落 versions/<版本>/
    #[test]
    fn core_lib_tree_overwrites_library_and_lands_dat() {
        let root = crate::storage::tmp_config_dir("core-lib");
        let out = root.join("pkg/V_1_20_libs");
        std::fs::create_dir_all(&out).unwrap();
        std::fs::write(out.join("modlauncher-10.0.9.jar"), b"NEW").unwrap();
        std::fs::write(out.join("1.20.dat"), b"DAT").unwrap();
        let lib = root.join("client/libraries/cpw/mods/modlauncher/10.0.9");
        std::fs::create_dir_all(&lib).unwrap();
        std::fs::write(lib.join("modlauncher-10.0.9.jar"), b"OLD").unwrap();
        let n = apply_core_lib_tree(&root.join("pkg"), &root.join("client"), "1.20").unwrap();
        assert_eq!(n, 2);
        assert_eq!(
            std::fs::read(lib.join("modlauncher-10.0.9.jar")).unwrap(),
            b"NEW",
            "libraries 下的同名库必须被 core lib 覆盖（base 包自带的是旧代网络层）"
        );
        assert_eq!(
            std::fs::read(root.join("client/versions/1.20/1.20.dat")).unwrap(),
            b"DAT"
        );
        // 没有同名库时退回 libraries/<文件名>
        let out2 = root.join("pkg2/V_1_12_2_libs");
        std::fs::create_dir_all(&out2).unwrap();
        std::fs::write(out2.join("brand-new-1.0.jar"), b"X").unwrap();
        apply_core_lib_tree(&root.join("pkg2"), &root.join("client"), "1.12.2").unwrap();
        assert!(root.join("client/libraries/brand-new-1.0.jar").is_file());
    }

    /// zip/jar 内部也认：包把文件塞在 natives jar 里时也要能捞出来
    #[test]
    fn runtime_native_found_inside_jar() {
        use std::io::Write as _;
        let dir = crate::storage::tmp_config_dir("rt-jar");
        let jar = dir.join("natives-linux.jar");
        let f = std::fs::File::create(&jar).unwrap();
        let mut zw = zip::ZipWriter::new(f);
        let opts = zip::write::SimpleFileOptions::default();
        zw.start_file(format!("runtime/{RUNTIME_NATIVE_NAME}"), opts)
            .unwrap();
        zw.write_all(b"ELF-IN-JAR").unwrap();
        zw.finish().unwrap();

        let got = find_runtime_native_bytes(&dir).expect("jar 内条目应被找到");
        assert_eq!(got, b"ELF-IN-JAR");

        // harvest 应把它写进缓存目录，后续版本安装直接命中缓存
        harvest_runtime_native(&dir, &dir);
        let cached = dir.join(".runtime-native").join(RUNTIME_NATIVE_NAME);
        assert_eq!(std::fs::read(&cached).unwrap(), b"ELF-IN-JAR");
    }

    /// 到处都没有时：错误信息必须可行动（提到 1.12.2 / 网络补齐），不能静默崩溃
    #[test]
    fn runtime_native_missing_error_is_actionable() {
        let root = crate::storage::tmp_config_dir("rt-missing");
        let natives = root.join("versions/1.20/natives");
        let err = ensure_runtime_native(&natives, &root)
            .unwrap_err()
            .to_string();
        assert!(err.contains(RUNTIME_NATIVE_NAME), "{err}");
        assert!(err.contains("1.12.2"), "{err}");
    }

    #[test]
    fn zip_entry_paths_cannot_escape() {
        let base = Path::new("/tmp/mctl_base");
        assert_eq!(safe_zip_path(base, "a/b.txt"), Some(base.join("a/b.txt")));
        assert!(safe_zip_path(base, "../evil.txt").is_none());
        assert!(safe_zip_path(base, "/etc/passwd").is_none());
        assert!(safe_zip_path(base, "a/../../evil").is_none());
        assert!(safe_zip_path(base, "./..").is_none());
    }

    /// 端到端：恶意 zip 不允许把文件写到解压目录之外
    #[test]
    fn extract_zip_rejects_zip_slip() {
        use std::io::Write as _;
        let dir = crate::storage::tmp_config_dir("zip_slip");
        let zip_path = dir.join("evil.zip");
        let file = std::fs::File::create(&zip_path).unwrap();
        let mut zw = zip::ZipWriter::new(file);
        let opts = zip::write::SimpleFileOptions::default();
        zw.start_file("../escaped.txt", opts).unwrap();
        zw.write_all(b"pwned").unwrap();
        zw.start_file("ok/inside.txt", opts).unwrap();
        zw.write_all(b"fine").unwrap();
        zw.finish().unwrap();

        let out = dir.join("out");
        extract_zip(&zip_path, &out).unwrap();
        assert!(
            out.join("ok").join("inside.txt").is_file(),
            "正常条目应解出"
        );
        assert!(
            !dir.join("escaped.txt").exists(),
            "zip-slip：文件逃出了目标目录"
        );
    }

    /// 服务器组件隔离：进 B 服时清掉 A 服装的 jar（A 的 flag 也清掉 → 下次进 A 重拉）
    #[test]
    fn server_components_are_pruned_when_switching() {
        let root = crate::storage::tmp_config_dir("comp-prune");
        let mods = root.join("mods");
        std::fs::create_dir_all(&mods).unwrap();
        std::fs::create_dir_all(root.join("config/eastland")).unwrap();
        std::fs::create_dir_all(root.join(".components")).unwrap();
        // A 服装两个 jar（其中一个与 B 服共用）+ 一个配置文件
        for f in ["mods/a1@3@11.jar", "mods/shared@3@99.jar"] {
            std::fs::write(root.join(f), b"x").unwrap();
        }
        std::fs::write(root.join("config/eastland/a.cfg"), b"a").unwrap();
        std::fs::write(
            root.join(".components").join("111.mods"),
            "mods/a1@3@11.jar\nmods/shared@3@99.jar\nconfig/eastland/a.cfg",
        )
        .unwrap();
        std::fs::write(root.join(".components.111.flag"), b"1").unwrap();
        // B 服（当前要进的）：与 A 服共用 mods/shared@3@99.jar
        std::fs::write(mods.join("b1@3@22.jar"), b"y").unwrap();
        std::fs::write(
            root.join(".components").join("222.mods"),
            "mods/b1@3@22.jar\nmods/shared@3@99.jar",
        )
        .unwrap();
        std::fs::write(root.join(".components.222.flag"), b"1").unwrap();
        // 恶意清单：越界路径必须被忽略
        std::fs::write(root.join(".components").join("333.mods"), "../pwned.txt").unwrap();
        let outside = root.parent().unwrap().join("pwned.txt");
        std::fs::write(&outside, b"bad").unwrap();

        prune_other_components(&root, "222");

        assert!(!mods.join("a1@3@11.jar").exists(), "A 服专有组件应被清掉");
        assert!(mods.join("b1@3@22.jar").exists(), "当前服组件必须保留");
        assert!(
            mods.join("shared@3@99.jar").exists(),
            "两服共用的组件不能删（B 的清单里也有）"
        );
        assert!(
            !root.join("config/eastland/a.cfg").exists(),
            "A 服的配置文件也要清掉"
        );
        assert!(!root.join("config/eastland").exists(), "空目录顺手收敛");
        assert!(outside.exists(), "越界路径不许碰（清单不可信）");
        assert!(
            !root.join(".components.111.flag").exists(),
            "A 的 flag 要清掉 → 下次进 A 重新拉组件"
        );
        assert!(root.join(".components.222.flag").exists());
        assert!(!root.join(".components").join("111.mods").exists());
        assert!(root.join(".components").join("222.mods").exists());
        assert!(!root.join(".components").join("333.mods").exists());
    }

    /// 「装好了」必须是清单 + flag 齐全：只有旧版留下的 flag 视为未装（会重拉）
    #[test]
    fn components_installed_requires_manifest_and_flag() {
        let root = crate::storage::tmp_config_dir("comp-flag");
        assert!(!components_installed(&root, "777"));
        std::fs::write(root.join(".components.777.flag"), b"1").unwrap();
        assert!(
            !components_installed(&root, "777"),
            "只有 flag（旧版）→ 视为未装"
        );
        std::fs::create_dir_all(root.join(".components")).unwrap();
        std::fs::write(root.join(".components").join("777.mods"), "").unwrap();
        assert!(components_installed(&root, "777"));
    }

    /// 实网返回的数字可能是字符串（entity_id "363020"）——解析不能炸
    #[test]
    fn entity_ids_accept_string_or_number() {
        let s = r#"{"item_id":"4633394671597683929","mtypeid":3,"entity_id":0,
            "sub_entities":[{"entity_id":"13105","res_name":"x.7z","res_url":"u","jar_md5":"AB"}]}"#;
        let it: ItemList = serde_json::from_str(s).unwrap();
        assert_eq!(it.item_id, 4633394671597683929);
        assert_eq!(it.mtypeid, 3);
        assert_eq!(it.entity_id, 0);
        assert_eq!(it.sub_entities[0].entity_id, 13105);
        assert_eq!(it.sub_entities[0].jar_md5, "AB");
        // 纯数字形态也要能收
        let it2: ItemList =
            serde_json::from_str(r#"{"item_id":5,"mtypeid":"3","entity_id":"9"}"#).unwrap();
        assert_eq!(it2.item_id, 5);
        assert_eq!(it2.mtypeid, 3);
        assert_eq!(it2.entity_id, 9);
    }

    /// 核心 mod 命名：落盘 `{res_name去后缀}@{MTypeId}@{EntityId}.jar`，
    /// 指纹上报 `{ItemId}@{MTypeId}@0.jar`（官方 InstallerService 规则）
    #[test]
    fn core_mod_names_follow_official_rules() {
        let it: ItemList = serde_json::from_str(
            r#"{"item_id":"4633394671597683929","mtypeid":3,"entity_id":0,
                "sub_entities":[{"entity_id":"13105","res_name":"4633394671597683929.7z"}]}"#,
        )
        .unwrap();
        let sub = &it.sub_entities[0];
        assert_eq!(core_mod_disk_name(&it, sub), "4633394671597683929@3@0.jar");
        assert_eq!(
            core_mod_fingerprint_name(&it),
            "4633394671597683929@3@0.jar"
        );
        // 实体 EntityId 非 0：落盘名与上报名的差别（上报固定 @0.jar）
        let it2: ItemList = serde_json::from_str(
            r#"{"item_id":"7","mtypeid":3,"entity_id":"42","sub_entities":[{"res_name":"7.7z"}]}"#,
        )
        .unwrap();
        assert_eq!(core_mod_disk_name(&it2, &it2.sub_entities[0]), "7@3@42.jar");
        assert_eq!(core_mod_fingerprint_name(&it2), "7@3@0.jar");
    }

    /// 名称映射合并写入（核心 mod 是版本级的，后一次写入不能覆盖前一次）
    #[test]
    fn mod_names_map_merges_and_persists() {
        let root = crate::storage::tmp_config_dir("mod-names");
        write_mod_names(&root, &[("a.jar".into(), "a@1@0.jar".into(), "a".into())]).unwrap();
        write_mod_names(&root, &[("b.jar".into(), "b@1@0.jar".into(), "b".into())]).unwrap();
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(mod_names_path(&root)).unwrap()).unwrap();
        assert_eq!(v["a.jar"]["modPath"], "a@1@0.jar");
        assert_eq!(v["b.jar"]["iid"], "b");
    }

    /// 实网：按服务器装组件 → 清单 → 切服清理 → 幂等。
    /// 需要凭据与网络；为了体积只下一个服（东方大陆 ~44MB）。
    /// ```sh
    /// MCTL_UID=<uid> MCTL_TOKEN=<token> cargo test -- --ignored live_server_components
    /// ```
    #[tokio::test]
    #[ignore = "需要 MCTL_UID / MCTL_TOKEN 环境变量 + 实网"]
    async fn live_server_components() {
        let (Ok(uid), Ok(tok)) = (std::env::var("MCTL_UID"), std::env::var("MCTL_TOKEN")) else {
            eprintln!("跳过：未设置 MCTL_UID / MCTL_TOKEN");
            return;
        };
        // 东方大陆（1.12.2，组件 minecraft.7z ~44MB，实测）
        const GAME: &str = "4619737516439909914";
        let root = crate::storage::tmp_config_dir("live-comp");
        // 进度出口：阶段 + 单文件都打日志（真机排障用）
        let stage_cb = |p: &str, d: usize, t: usize| eprintln!("[{p}] {d}/{t}");
        let file_cb = |f: crate::launcher::FileProgress| {
            eprintln!("[file] {} {}/{}", f.name, f.done, f.total);
        };
        let prog = Progress {
            stage: &stage_cb,
            file: &file_cb,
        };
        ensure_server_components(&root, GAME, &uid, &tok, prog)
            .await
            .unwrap();
        assert!(components_installed(&root, GAME), "装完应有清单 + flag");
        let manifest = std::fs::read_to_string(components_manifest_path(&root, GAME)).unwrap();
        let names: Vec<&str> = manifest.lines().filter(|l| !l.is_empty()).collect();
        eprintln!("组件文件数: {}", names.len());
        assert!(!names.is_empty(), "东方大陆应有组件文件");
        assert!(
            !root.join(".minecraft").exists(),
            "包里的 .minecraft 前缀必须剥掉（否则 mods 不在根上）"
        );
        assert!(
            names
                .iter()
                .any(|n| n.starts_with("mods/") && n.ends_with(".jar")),
            "清单应含 mods/*.jar"
        );
        for n in &names {
            assert!(root.join(n).is_file(), "清单里的 {n} 必须落盘");
        }
        // 幂等：再调一次不该改清单（装了就直接返回，不联网）
        ensure_server_components(&root, GAME, &uid, &tok, prog)
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(components_manifest_path(&root, GAME)).unwrap(),
            manifest,
            "第二次调用不该动清单"
        );
        // 模拟另一个服的残留：进当前服时应被清掉，当前服的文件一个不少
        let mods = root.join("mods");
        std::fs::write(mods.join("other-server.jar"), b"x").unwrap();
        std::fs::create_dir_all(root.join(".components")).unwrap();
        std::fs::write(
            root.join(".components").join("999.mods"),
            "mods/other-server.jar",
        )
        .unwrap();
        std::fs::write(root.join(".components.999.flag"), b"1").unwrap();
        ensure_server_components(&root, GAME, &uid, &tok, prog)
            .await
            .unwrap();
        assert!(!mods.join("other-server.jar").exists(), "别的服残留应清除");
        for n in &names {
            assert!(root.join(n).is_file(), "当前服组件 {n} 不能被误删");
        }
        // 核心 mod（1.12.2 版级列表，7z 都很小）+ 指纹形状
        install_core_mods(&root, "1.12.2", &uid, &tok, prog)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&crate::netease::mods_json(&root)).unwrap();
        let mods_list = v["mods"].as_array().unwrap();
        eprintln!("指纹 mods 条数: {}", mods_list.len());
        assert!(!mods_list.is_empty(), "指纹不该为空");
        for m in mods_list {
            let md5 = m["md5"].as_str().unwrap();
            assert!(
                !md5.chars().any(|c| c.is_ascii_lowercase()) && md5.len() == 32,
                "md5 必须是大写 32 位: {m}"
            );
            let mp = m["modPath"].as_str().unwrap();
            assert!(mp.ends_with(".jar"), "modPath 应以 .jar 结尾: {m}");
        }
        // 核心 mod 的上报名尾部是 @0.jar，且 md5 与落盘文件一致
        assert!(
            mods_list
                .iter()
                .any(|m| m["modPath"].as_str().unwrap().split('@').count() == 3
                    && m["modPath"].as_str().unwrap().ends_with("@0.jar")),
            "应有核心 mod 条目（{{ItemId}}@{{MTypeId}}@0.jar）"
        );
    }

    #[test]
    fn version_enum_mapping() {
        assert_eq!(version_enum("1.12.2"), 1012002);
        assert_eq!(version_enum("1.20"), 1020000);
        assert_eq!(version_enum("1.21.8"), 1021008);
        assert_eq!(version_enum("9.9"), 0);
    }

    #[test]
    fn zip_magic() {
        assert!(is_zip(b"PK\x03\x04abc"));
        assert!(!is_zip(b"\x37\x7a\xbc\xaf\x27\x1c"));
    }

    #[test]
    fn minecraft_root_detection() {
        let tmp = temp_dir("mctl-test-ne");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        // 无 .minecraft 顶层 → 返回自身
        std::fs::create_dir_all(tmp.join("a")).unwrap();
        assert_eq!(find_minecraft_root(&tmp), tmp);
        // 唯一顶层目录 minecraft → 返回其内容
        let mc = tmp.join("b");
        std::fs::create_dir_all(mc.join(".minecraft")).unwrap();
        assert_eq!(find_minecraft_root(&mc), mc.join(".minecraft"));
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
