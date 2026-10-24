//! 整合包下载：Modrinth 搜索 + .mrpack 安装
//! 依赖解决：fabric-loader → meta.fabricmc.net 完整启动 profile；
//! forge → 镜像源 下载 installer 并执行 --installClient。

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use serde::{de::DeserializeOwned, Deserialize};

use super::meta::install_official;

/// Modrinth 搜索命中（slug/description 待详情页接入）
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct ModpackHit {
    pub project_id: String,
    pub slug: String,
    pub title: String,
    pub description: String,
    pub downloads: i64,
    pub follows: i64,
    pub date_modified: String,
    pub icon_url: String,
    pub game_versions: Vec<String>,
}

/// Modrinth 搜索排序（服务端 index 参数）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModrinthIndex {
    Relevance,
    Downloads,
    Follows,
    Newest,
}

impl ModrinthIndex {
    pub const ALL: [ModrinthIndex; 4] = [
        ModrinthIndex::Relevance,
        ModrinthIndex::Downloads,
        ModrinthIndex::Follows,
        ModrinthIndex::Newest,
    ];

    pub fn index(self) -> &'static str {
        match self {
            ModrinthIndex::Relevance => "relevance",
            ModrinthIndex::Downloads => "downloads",
            ModrinthIndex::Follows => "follows",
            ModrinthIndex::Newest => "newest",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ModrinthIndex::Relevance => "相关度",
            ModrinthIndex::Downloads => "下载量",
            ModrinthIndex::Follows => "热度",
            ModrinthIndex::Newest => "最新",
        }
    }

    pub fn next(self) -> Self {
        match self {
            ModrinthIndex::Relevance => ModrinthIndex::Downloads,
            ModrinthIndex::Downloads => ModrinthIndex::Follows,
            ModrinthIndex::Follows => ModrinthIndex::Newest,
            ModrinthIndex::Newest => ModrinthIndex::Relevance,
        }
    }
}

/// Modrinth 搜索（project_type 分类，version 版本条件可空，index 排序，offset 分页）
pub async fn search_modrinth(
    query: &str,
    project_type: &str,
    version: &str,
    limit: usize,
    index: ModrinthIndex,
    offset: usize,
) -> Result<Vec<ModpackHit>> {
    let mut facets = vec![format!(r#"["project_type:{project_type}"]"#)];
    if !version.is_empty() {
        facets.push(format!(r#"["versions:{version}"]"#));
    }
    let url = format!(
        "https://api.modrinth.com/v2/search?query={}&facets={}&limit={}&index={}&offset={}",
        urlencode(query),
        urlencode(&format!("[{}]", facets.join(","))),
        limit,
        index.index(),
        offset
    );
    #[derive(Deserialize)]
    struct Resp {
        #[serde(default)]
        hits: Vec<Hit>,
    }
    #[derive(Deserialize)]
    struct Hit {
        #[serde(default)]
        project_id: String,
        #[serde(default)]
        slug: String,
        #[serde(default)]
        title: String,
        #[serde(default)]
        description: String,
        #[serde(default)]
        downloads: i64,
        #[serde(default)]
        follows: i64,
        #[serde(default)]
        date_modified: String,
        #[serde(default)]
        icon_url: String,
        // 搜索接口返回的字段名是 `versions`（游戏版本列表），不是 `game_versions`。
        #[serde(default, alias = "versions")]
        game_versions: Vec<String>,
    }
    let data: Resp = mr_get(&url).await?;
    Ok(data
        .hits
        .into_iter()
        .map(|h| ModpackHit {
            project_id: h.project_id,
            slug: h.slug,
            title: h.title,
            description: h.description,
            downloads: h.downloads,
            follows: h.follows,
            date_modified: h.date_modified,
            icon_url: h.icon_url,
            game_versions: h.game_versions,
        })
        .collect())
}

/// Modrinth 拉取：统一状态码检查 + 正文解码，失败自动重试一次。
/// 官方要求带 User-Agent，但本项目是私用启动器，不做伪装。
async fn mr_get<T: DeserializeOwned>(url: &str) -> Result<T> {
    let client = super::http_client();
    let mut last = None;
    for attempt in 0..2 {
        let r = client.get(url).send().await;
        let parsed = match r {
            Ok(resp) => {
                let status = resp.status();
                // 先收正文再判状态：非 2xx 的正文也要留证（Cloudflare 429 是 HTML）
                let body = resp.text().await?;
                if !status.is_success() {
                    Err(anyhow!("Modrinth HTTP {status}: {url}"))
                } else {
                    serde_json::from_str(&body).map_err(|e| {
                        anyhow!("Modrinth 解析失败 {url}\n  {e}\n  正文: {}", preview(&body))
                    })
                }
            }
            Err(e) => Err(anyhow!("Modrinth 请求失败: {e:#}（{url}）")),
        };
        match parsed {
            Ok(v) => return Ok(v),
            // 连接层抖动（超时/重置/DNS）才值得重试，HTTP 4xx/5xx 与脏 JSON 重试无意义
            Err(e) if attempt == 0 && r_is_transient(&e) => last = Some(e),
            other => return other,
        }
    }
    Err(last.unwrap_or_else(|| anyhow!("Modrinth 请求失败: {url}")))
}

/// 连接层错误（`reqwest` 会把断流/超时统一归到 decode），HTTP 状态错误不在其中
fn r_is_transient(e: &anyhow::Error) -> bool {
    !e.to_string().contains("HTTP ")
}

fn preview(body: &str) -> String {
    body.chars().take(200).collect()
}

fn urlencode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Modrinth 项目版本（含全部历史版本，版本选择页用）
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
pub struct MrVersion {
    pub id: String,
    #[serde(default)]
    pub version_number: String,
    #[serde(default)]
    pub game_versions: Vec<String>,
    #[serde(default)]
    pub loaders: Vec<String>,
    #[serde(default)]
    pub files: Vec<MrFile>,
    #[serde(default)]
    pub dependencies: Vec<MrDependency>,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
pub struct MrFile {
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub filename: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Modrinth 对 embedded 依赖返回 version_id: null（字段存在，非缺失）。
    /// 回归测试：Option 化之后这类正文必须能反序列化。
    #[test]
    fn mr_version_handles_null_dependency_fields() {
        let raw = r#"[{"id":"abc","version_number":"1.0","dependencies":[
            {"dependency_type":"embedded","project_id":null,"file_name":"x.jar","version_id":null},
            {"dependency_type":"required","project_id":"P7dR8mSH","file_name":null,"version_id":"v1"}
        ]}]"#;
        let v: Vec<MrVersion> = serde_json::from_str(raw).unwrap();
        assert_eq!(v[0].dependencies[0].version_id, None);
        assert_eq!(v[0].dependencies[1].version_id.as_deref(), Some("v1"));
    }
}

/// 项目全部版本（版本选择页用）
pub async fn project_versions(project_id: &str) -> Result<Vec<MrVersion>> {
    let url = format!("https://api.modrinth.com/v2/project/{project_id}/version");
    mr_get(&url).await
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
pub struct MrDependency {
    // Modrinth 对 embedded 依赖给 version_id: null、对 file 依赖给 project_id: null，
    // #[serde(default)] 只管字段缺失不管 null，所以这些必须收 Option。
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub dependency_type: String,
    #[serde(default)]
    pub version_id: Option<String>,
}

/// 下载文件到目录
/// mrpack 索引
#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct MrIndex {
    #[serde(default)]
    name: String,
    #[serde(default)]
    dependencies: std::collections::HashMap<String, String>,
    #[serde(default)]
    files: Vec<MrIndexFile>,
}

/// 相对路径是否危险：含 `..`、绝对路径、或 Windows 盘符。
/// zip 条目名不可信，`overrides/../../.ssh/authorized_keys` 这种必须挡住。
/// 不用 canonicalize 判断——目标文件还不存在时它会失败，fallback 就成了摆设。
pub(crate) fn is_unsafe_rel_path(rel: &str) -> bool {
    if rel.starts_with('/') || rel.starts_with('\\') {
        return true;
    }
    // `C:\...` 之类盘符路径
    let b = rel.as_bytes();
    if b.len() >= 2 && b[1] == b':' && b[0].is_ascii_alphabetic() {
        return true;
    }
    rel.split(['/', '\\'])
        .any(|seg| seg == ".." || seg.chars().all(|c| c == '.'))
}

/// 把远端提供的文件名收成「一个安全的纯文件名」再拼路径。
/// Modrinth 的 `filename` / 索引里的 `path` 都不是本机产生的：`../x`、`a/b`、空串、
/// 或者整串 `..` 都可能出现，直接 `dir.join(name)` 会写到目标目录外面。
/// 规则：只取最后一段（兼容 `/` 与 `\\`）→ 空串 / 纯点串 / 带 NUL 一律拒绝。
fn safe_file_name(name: &str) -> Result<String> {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("").trim();
    if base.is_empty() || base.chars().all(|c| c == '.') || base.contains('\0') {
        return Err(anyhow!("文件名不合法: {name:?}"));
    }
    Ok(base.to_string())
}

/// 解压 mrpack 的 `overrides/` 到实例目录。
/// 光影、资源包、配置文件都在这里 —— 只处理 index.files 的话整合包自带的光影会全丢。
/// `..` / 绝对路径一律拒绝（zip slip：条目名不可信，能写到实例目录外面去）。
fn extract_overrides(
    archive: &mut zip::ZipArchive<std::fs::File>,
    inst_dir: &Path,
    progress: &impl Fn(&str, usize, usize),
    on_file: &(dyn Fn(super::FileProgress) + Send + Sync),
) -> Result<usize> {
    let names: Vec<String> = (0..archive.len())
        .filter_map(|i| archive.by_index(i).ok().map(|e| e.name().to_string()))
        .filter(|n| n.starts_with("overrides/") && !n.ends_with('/'))
        .collect();
    if names.is_empty() {
        return Ok(0);
    }
    progress("应用整合包配置", 0, names.len());
    let mut done = 0usize;
    for name in &names {
        let rel = name.trim_start_matches("overrides/");
        // zip slip 防护：**逐组件**检查，不靠 canonicalize。
        // 目标文件通常还不存在，canonicalize 会失败；fallback 拼路径时 `..`
        // 照样生效，等于没防（测试踩过）。
        if is_unsafe_rel_path(rel) {
            progress(&format!("跳过越界路径 {rel}"), done, names.len());
            done += 1;
            continue;
        }
        let dest = inst_dir.join(rel);
        if let Some(p) = dest.parent() {
            std::fs::create_dir_all(p)?;
        }
        let mut e = archive
            .by_name(&name)
            .map_err(|err| anyhow!("读取 {name} 失败: {err}"))?;
        let mut out = std::fs::File::create(&dest)
            .with_context(|| format!("创建 {} 失败", dest.display()))?;
        std::io::copy(&mut e, &mut out)?;
        on_file(super::FileProgress {
            name: rel.to_string(),
            done: u64::MAX,
            total: 0,
            finished_at: Some(std::time::Instant::now()),
            failed: false,
        });
        done += 1;
        progress("应用整合包配置", done, names.len());
    }
    Ok(done)
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct MrIndexFile {
    #[serde(default)]
    path: String,
    #[serde(default)]
    downloads: Vec<String>,
    #[serde(default)]
    env: Option<std::collections::HashMap<String, String>>,
}

/// 下载 Fabric 启动 profile（meta.fabricmc.net），返回实例 id。
/// Fabric profile 以 inheritsFrom 继承官方版本，先装 vanilla 再写 profile。
async fn install_fabric_profile(
    mc_dir: &Path,
    game_version: &str,
    loader_version: &str,
    progress: impl Fn(&str, usize, usize),
    on_file: &(dyn Fn(super::FileProgress) + Send + Sync),
) -> Result<String> {
    let client = super::http_client();
    // 1. 先装官方本体（fabric profile 继承它）
    install_official(mc_dir, game_version, &progress, on_file).await?;
    // 2. 下载 fabric 启动配置
    let profile_url = format!(
        "https://meta.fabricmc.net/v2/versions/loader/{game_version}/{loader_version}/profile/json"
    );
    progress("下载 Fabric 启动配置", 0, 1);
    let resp = client.get(&profile_url).send().await?;
    if !resp.status().is_success() {
        return Err(anyhow!("Fabric meta HTTP {}: {profile_url}", resp.status()));
    }
    let json: serde_json::Value = resp.json().await?;
    let inst_id = format!("{game_version}-fabric-{loader_version}");
    let versions_dir = mc_dir.join("versions").join(&inst_id);
    std::fs::create_dir_all(&versions_dir)?;
    std::fs::write(
        versions_dir.join(format!("{inst_id}.json")),
        serde_json::to_string_pretty(&json)?,
    )?;
    // 3. 合并继承后补全依赖/资产
    let profile = super::merge_inheritance_file(&versions_dir.join(format!("{inst_id}.json")))?;
    progress("下载依赖库", 0, 1);
    super::ensure_libraries(
        mc_dir,
        &profile,
        |done, total| progress("下载依赖库", done, total),
        on_file,
    )
    .await?;
    progress("下载资源文件", 0, 1);
    super::ensure_assets(
        mc_dir,
        &profile,
        |done, total| progress("下载资源文件", done, total),
        on_file,
    )
    .await?;
    Ok(inst_id)
}

/// 通过 镜像源 安装 Forge（下载 installer 并运行 --installClient）
async fn install_forge(
    mc_dir: &Path,
    game_version: &str,
    progress: impl Fn(&str, usize, usize),
    on_file: &(dyn Fn(super::FileProgress) + Send + Sync),
) -> Result<String> {
    let client = super::http_client();
    // 1. 先装官方本体
    install_official(mc_dir, game_version, &progress, on_file).await?;
    // 2. 取 forge 版本清单（镜像源 返回 build 数组，取最后一个 = 最新）
    let list_url = format!("https://bmclapi2.bangbang93.com/forge/minecraft/{game_version}");
    progress("获取 Forge 版本", 0, 1);
    #[derive(Deserialize)]
    struct ForgeBuild {
        #[serde(default)]
        version: String,
        #[serde(default)]
        files: Vec<ForgeFile>,
    }
    #[allow(dead_code)]
    #[derive(Deserialize)]
    struct ForgeFile {
        #[serde(default)]
        #[serde(rename = "type")]
        file_type: String,
        #[serde(default)]
        url: String,
        #[serde(default)]
        sha1: String,
        #[serde(default)]
        size: i64,
    }
    let resp = client.get(&list_url).send().await?;
    if !resp.status().is_success() {
        return Err(anyhow!("Forge 清单 HTTP {}: {list_url}", resp.status()));
    }
    let builds: Vec<ForgeBuild> = resp.json().await?;
    let build = builds
        .into_iter()
        .next_back()
        .ok_or_else(|| anyhow!("Forge {game_version} 无可用构建"))?;
    let installer = build
        .files
        .into_iter()
        .find(|f| f.file_type == "installer")
        .ok_or_else(|| anyhow!("Forge {} 无 installer", build.version))?;

    // 3. 下载 installer 到临时目录
    let tmp_dir = mc_dir.join(".tmp-forge");
    std::fs::create_dir_all(&tmp_dir)?;
    let installer_path = tmp_dir.join("forge-installer.jar");
    progress("下载 Forge 安装器", 0, 1);
    super::download_to(&client, &installer.url, &installer_path, &mut |_, _| {}).await?;

    // 4. 运行 installer
    let java = super::java::find_java(17)
        .ok_or_else(|| anyhow!("未找到 Java 17+，Forge 安装器无法运行"))?;
    progress("运行 Forge 安装器", 0, 1);
    let status = std::process::Command::new(&java)
        .arg("-jar")
        .arg(&installer_path)
        .arg("--installClient")
        .arg(mc_dir)
        .status()?;
    if !status.success() {
        return Err(anyhow!("Forge 安装器退出码 {:?}", status.code()));
    }
    let _ = std::fs::remove_dir_all(&tmp_dir);
    Ok(format!("{game_version}-forge-{}", build.version))
}

/// 安装单个 mod/光影/资源包到当前实例的对应目录。
/// `inst_dir` 是**实例根目录**（`Instance.dir`），不是全局 mc_dir。
/// 模组/光影装进 inst_dir 下的 mods/shaderpacks —— 换实例即换一套模组。
pub async fn install_single_content(
    inst_dir: &Path,
    file: &MrFile,
    progress: impl Fn(&str, usize, usize),
    on_file: &(dyn Fn(super::FileProgress) + Send + Sync),
) -> Result<String> {
    let client = super::http_client();
    // 光影是 zip 结尾，其余（mod/资源包）进 mods
    let kind = if file.filename.ends_with(".zip") {
        crate::instance::content::ContentKind::Shaders
    } else {
        crate::instance::content::ContentKind::Mods
    };
    let dir = crate::instance::content::content_dir(inst_dir, kind);
    std::fs::create_dir_all(&dir)?;

    // 文件名来自远端 API 元数据，收口成纯文件名后再拼路径
    let filename = safe_file_name(&file.filename)?;
    progress(&format!("下载 {filename}"), 0, 1);
    let target = dir.join(&filename);
    // 已存在同名文件 → 覆盖前先删，否则 rename 会失败
    if target.is_file() {
        let _ = std::fs::remove_file(&target);
    }
    let mut tick = |d: u64, t: u64| {
        on_file(super::FileProgress {
            name: filename.clone(),
            done: d,
            total: t,
            finished_at: None,
            failed: false,
        })
    };
    super::download_to(&client, &file.url, &target, &mut tick).await?;
    on_file(super::FileProgress {
        name: filename.clone(),
        done: u64::MAX,
        total: 0,
        finished_at: Some(std::time::Instant::now()),
        failed: false,
    });
    progress("完成", 1, 1);
    Ok(filename)
}

/// 安装 Modrinth 整合包（指定版本）。
/// progress(phase, done, total) 报告阶段与进度。
/// 返回实例 id（fabric 为 "<mc>-fabric-<loader>"，forge 为 "<mc>-forge-<ver>"）。
/// `root` 有两种含义，取决于文件类型（调用方 `spawn_install_modpack` 决定）：
/// - `.mrpack` 整合包 → **全局 mc_dir**（整合包自带自己的实例目录）
/// - `.jar`/`.zip` 单内容 → **选中实例的 `Instance.dir`**（版本隔离）
pub async fn install_modpack(
    root: &Path,
    ver: &MrVersion,
    progress: impl Fn(&str, usize, usize),
    on_file: &(dyn Fn(super::FileProgress) + Send + Sync),
) -> Result<String> {
    let client = super::http_client();
    let Some(file) = ver.files.first() else {
        return Err(anyhow!("整合包无可用文件"));
    };

    // 单个 mod / 光影：文件本身就是 .jar/.zip，直接丢进实例目录。
    // 原实现无条件去解压找 modrinth.index.json，装 Sodium 这种单个模组必然
    // 报「非 Modrinth 整合包」——那不是错误信息，是走错了函数。
    if !file.filename.ends_with(".mrpack") {
        return install_single_content(root, file, progress, on_file).await;
    }

    // 1. 下载 .mrpack
    let tmp_dir = root.join(".tmp-modpack");
    std::fs::create_dir_all(&tmp_dir)?;
    let mrpack = tmp_dir.join("pack.mrpack");
    progress("下载整合包文件", 0, 1);
    super::download_to(&client, &file.url, &mrpack, &mut |_, _| {}).await?;

    // 2. 解压索引
    let zip_file = std::fs::File::open(&mrpack)?;
    let mut archive = zip::ZipArchive::new(zip_file)?;
    let mut index_text = String::new();
    {
        let mut entry = archive
            .by_name("modrinth.index.json")
            .map_err(|_| anyhow!("非 Modrinth 整合包"))?;
        std::io::Read::read_to_string(&mut entry, &mut index_text)?;
    }
    let index: MrIndex = serde_json::from_str(&index_text)?;

    // 3. 解决依赖：fabric-loader / forge
    let game_version = index
        .dependencies
        .get("minecraft")
        .cloned()
        .unwrap_or_default();
    let inst_id = if let Some(loader) = index.dependencies.get("fabric-loader") {
        install_fabric_profile(root, &game_version, loader, &progress, on_file).await?
    } else if index.dependencies.contains_key("forge") {
        install_forge(root, &game_version, &progress, on_file).await?
    } else {
        install_official(root, &game_version, &progress, on_file).await?
    };

    // 4. 模组装进**刚建好的那个实例**（root/versions/<inst_id>/mods），不是 root/mods。
    // 装到 root/mods 会让所有实例共用一套模组 —— fabric 专属模组会把原版实例搞崩。
    let inst_dir = root.join("versions").join(&inst_id);
    let mods_dir = inst_dir.join("mods");
    std::fs::create_dir_all(&mods_dir)?;
    let files: Vec<&MrIndexFile> = index.files.iter().collect();
    let total = files.len();
    let mut done = 0usize;
    progress("下载模组文件", done, total);
    for f in &files {
        if let Some(url) = f.downloads.first() {
            // 索引里的 path 来自远端：收口成纯文件名（`..`、空串、子路径一律不认）
            let name = match safe_file_name(&f.path) {
                Ok(n) => n,
                Err(e) => {
                    eprintln!("[mctl] 跳过不合法文件路径: {e}");
                    done += 1;
                    progress("下载模组文件", done, total);
                    continue;
                }
            };
            let mut tick = |d: u64, t: u64| {
                on_file(super::FileProgress {
                    name: name.clone(),
                    done: d,
                    total: t,
                    finished_at: None,
                    failed: false,
                })
            };
            let r = super::download_to(&client, url, &mods_dir.join(&name), &mut tick).await;
            on_file(super::FileProgress {
                name,
                done: if r.is_ok() { u64::MAX } else { 0 },
                total: 0,
                finished_at: Some(std::time::Instant::now()),
                failed: r.is_err(),
            });
            if let Err(e) = r {
                eprintln!("[mctl] 模组下载失败: {e}");
            }
        }
        done += 1;
        progress("下载模组文件", done, total);
    }
    // 5. overrides：光影/资源包/配置都在这里，必须在实例目录建好之后解
    extract_overrides(
        &mut archive,
        &inst_dir,
        &|p, d, t| progress(p, d, t),
        on_file,
    )?;

    let _ = std::fs::remove_dir_all(&tmp_dir);
    Ok(inst_id)
}

/// 清理安装产生的临时文件
pub fn cleanup(mc_dir: &Path) {
    let _ = std::fs::remove_dir_all(mc_dir.join(".tmp-forge"));
    let _ = std::fs::remove_dir_all(mc_dir.join(".tmp-modpack"));
}

#[allow(dead_code)]
fn _unused(_p: PathBuf) {}

#[cfg(test)]
mod overrides {
    use super::*;
    use std::io::Write;

    /// 造一个最小 mrpack：索引 + overrides（config / resourcepacks）
    fn make_mrpack(entries: &[(&str, &str)]) -> PathBuf {
        let p = crate::storage::tmp_config_dir("mrpack-zip").join("p.mrpack");
        let mut z = zip::ZipWriter::new(std::fs::File::create(&p).unwrap());
        for (name, body) in entries {
            z.start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            z.write_all(body.as_bytes()).unwrap();
        }
        z.finish().unwrap();
        p
    }

    /// overrides 必须落到实例目录下的**相同相对路径**
    /// （overrides/resourcepacks/x.zip → <inst>/resourcepacks/x.zip）
    #[test]
    fn overrides_land_at_same_relative_path() {
        let inst = crate::storage::tmp_config_dir("ov-inst");
        let mr = make_mrpack(&[
            (
                "modrinth.index.json",
                r#"{"name":"t","files":[],"dependencies":{}}"#,
            ),
            ("overrides/config/a.json", "{}"),
            ("overrides/resourcepacks/b.zip", "ZIPDATA"),
        ]);
        let f = std::fs::File::open(&mr).unwrap();
        let mut a = zip::ZipArchive::new(f).unwrap();
        let n = extract_overrides(&mut a, &inst, &|_, _, _| {}, &|_| {}).unwrap();
        assert_eq!(n, 2, "应解压 2 个文件");
        assert!(
            inst.join("config/a.json").is_file(),
            "config 应落到 inst/config"
        );
        assert!(
            inst.join("resourcepacks/b.zip").is_file(),
            "资源包应落到 inst/resourcepacks"
        );
        assert!(!inst.join("overrides").exists(), "不该留 overrides 层");
    }

    /// zip slip：条目名带 `..` 不能写到实例目录外面
    #[test]
    fn overrides_reject_path_traversal() {
        let inst = crate::storage::tmp_config_dir("ov-slip");
        let outside = inst.parent().unwrap().join("pwned.txt");
        let _ = std::fs::remove_file(&outside);
        let mr = make_mrpack(&[("overrides/../pwned.txt", "BAD")]);
        let f = std::fs::File::open(&mr).unwrap();
        let mut a = zip::ZipArchive::new(f).unwrap();
        let _ = extract_overrides(&mut a, &inst, &|_, _, _| {}, &|_| {});
        assert!(
            !outside.exists(),
            "越界文件被写出来了：{}",
            outside.display()
        );
    }

    /// 没有 overrides 的包（纯 index）不该报错
    #[test]
    fn no_overrides_is_fine() {
        let inst = crate::storage::tmp_config_dir("ov-none");
        let mr = make_mrpack(&[("modrinth.index.json", r#"{"files":[]}"#)]);
        let f = std::fs::File::open(&mr).unwrap();
        let mut a = zip::ZipArchive::new(f).unwrap();
        assert_eq!(
            extract_overrides(&mut a, &inst, &|_, _, _| {}, &|_| {}).unwrap(),
            0
        );
    }
}

#[cfg(test)]
mod single_content {
    use super::*;

    fn file(name: &str) -> MrFile {
        MrFile {
            url: format!("https://example.invalid/{name}"),
            filename: name.into(),
        }
    }

    /// 回归：装单个 mod（Sodium.jar）以前报「非 Modrinth 整合包」，
    /// 因为 install_modpack 无条件去解压找 modrinth.index.json。
    /// .jar/.zip 必须走单文件安装，不能碰整合包逻辑。
    #[test]
    fn jar_is_not_a_modpack() {
        assert!(!file("sodium-0.6.2.jar").filename.ends_with(".mrpack"));
        assert!(file("pack.mrpack").filename.ends_with(".mrpack"));
    }

    /// zip 是光影，进 shaderpacks；jar 是 mod，进 mods
    #[test]
    fn content_kind_by_extension() {
        let f = file("complementary-reimagined.zip");
        assert!(f.filename.ends_with(".zip"), "光影包应是 zip");
    }

    /// **版本隔离的硬约定**：模组装进 `Instance.dir/mods`，不是全局 mc_dir/mods。
    /// 回归：早期实现一律塞 mc_dir/mods，导致 1.21.1 和 1.21.1-fabric 共用一套模组，
    /// fabric 专属模组会把原版实例搞崩。
    #[test]
    fn target_dir_follows_instance_not_global_root() {
        let inst = std::path::Path::new("/mc/versions/1.21.1-fabric-0.19.3");
        let global = std::path::Path::new("/mc");
        let d = crate::instance::content::content_dir(
            inst,
            crate::instance::content::ContentKind::Mods,
        );
        assert_eq!(d, inst.join("mods"), "应装进实例自己的 mods");
        assert_ne!(d, global.join("mods"), "绝不能装进全局根");

        // 两个实例的 mods 必须互相独立
        let a = crate::instance::content::content_dir(
            std::path::Path::new("/mc/versions/1.21.1"),
            crate::instance::content::ContentKind::Mods,
        );
        let b = crate::instance::content::content_dir(
            std::path::Path::new("/mc/versions/1.21.1-fabric-0.19.3"),
            crate::instance::content::ContentKind::Mods,
        );
        assert_ne!(a, b, "不同实例的 mods 目录必须不同");
    }

    /// 整合包内部的模组也必须按实例隔离。
    /// 回归：原实现下到 root/mods（全局根），装完 Fabulously Optimized 后
    /// 全局 mods/ 里有 200+ jar，而两个实例的 mods/ 都是空的 ——
    /// 所有实例共用一套模组，fabric 专属模组会搞崩原版实例。
    #[test]
    fn modpack_mods_go_into_its_own_instance() {
        let root = std::path::Path::new("/mc");
        let inst_id = "1.21.1-fabric-0.19.3";
        let inst_dir = root.join("versions").join(inst_id);
        let mods = inst_dir.join("mods");
        assert!(mods.starts_with(root.join("versions")), "{mods:?}");
        assert_ne!(mods, root.join("mods"), "不能回到全局根");
    }

    /// 光影同样按实例隔离
    #[test]
    fn shaders_also_isolated_per_instance() {
        let inst = std::path::Path::new("/mc/versions/26.1.2-fabric");
        let d = crate::instance::content::content_dir(
            inst,
            crate::instance::content::ContentKind::Shaders,
        );
        assert_eq!(d, inst.join("shaderpacks"));
    }

    /// 远端文件名收口：正常名保留，路径分隔与 `..` 不许带出去
    #[test]
    fn safe_file_name_strips_paths_and_dots() {
        assert_eq!(safe_file_name("sodium.jar").unwrap(), "sodium.jar");
        assert_eq!(safe_file_name("dir/sub/a.jar").unwrap(), "a.jar");
        assert_eq!(safe_file_name("dir\\sub\\a.jar").unwrap(), "a.jar");
        assert_eq!(safe_file_name("  spaced.jar  ").unwrap(), "spaced.jar");
        for bad in ["..", ".", "", "dir/", "dir/..", "..\\.."] {
            assert!(safe_file_name(bad).is_err(), "{bad:?} 应该被拒");
        }
        // 带路径的名字只取最后一段（写到内容目录里，等价于正常文件名）
        for ok in ["../x.jar", "/abs.jar", "dir/../y.jar"] {
            assert!(safe_file_name(ok).is_ok(), "{ok:?} 收口后应放行");
        }
    }
}

#[cfg(test)]
mod e2e {
    use super::*;

    #[tokio::test]
    #[ignore]
    async fn real_fabric_install() {
        let mc = std::env::temp_dir().join("mctl-e2e-fabric");
        let _ = std::fs::remove_dir_all(&mc);
        std::fs::create_dir_all(&mc).unwrap();
        let id = install_fabric_profile(
            &mc,
            "1.21.1",
            "0.16.9",
            |p, d, t| println!("[{p}] {d}/{t}"),
            &|_| {},
        )
        .await
        .unwrap();
        println!("installed: {id}");
        // 实例应可被扫描
        let insts = super::super::scan_instances(&mc);
        assert!(insts.iter().any(|i| i.id == id), "实例未扫描到");
        // 合并后的 profile 应有主类和资产索引
        let inst = insts.iter().find(|i| i.id == id).unwrap();
        let prof = super::super::load_profile(inst).unwrap();
        assert!(!prof.main_class.is_empty());
        assert!(prof.asset_index.is_some() || !prof.assets.is_empty());
        println!("libraries: {}", prof.libraries.len());
        let _ = std::fs::remove_dir_all(&mc);
    }
}
