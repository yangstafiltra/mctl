//! 离线启动器管线
//! 扫描实例 → 加载版本 profile → 补库 → 补资产 → 解压 natives → 组装命令启动

pub mod java;
pub mod meta;
pub mod modpack;
mod version;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, Context, Result};
use futures_util::StreamExt;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;
use version::{AssetsIndex, Library, VersionProfile};

/// 一个已安装的游戏版本实例
#[derive(Debug, Clone)]
pub struct Instance {
    pub id: String,
    pub version_type: String,
    pub main_class: String,
    pub assets_index: String,
    pub java_major: u32,
    pub dir: PathBuf,
    /// 基岩版实例（Wine/GDK 运行时，启动走插件而非 Java）
    pub bedrock: bool,
}

impl Instance {
    /// 版本 json 路径
    pub fn profile_path(&self) -> PathBuf {
        self.dir.join(format!("{}.json", self.id))
    }
    /// client jar 路径
    pub fn client_jar(&self) -> PathBuf {
        self.dir.join(format!("{}.jar", self.id))
    }
}

/// 实例的存档目录：Java = versions/<id>/saves（版本隔离）；
/// 基岩 = prefix 内的 minecraftWorlds（每版本独立）。
/// CLI 与 TUI 都用这一份映射，避免两边各写一套后悄悄漂移。
pub fn saves_dir(inst: &Instance) -> PathBuf {
    if inst.bedrock {
        bedrock_launcher_core::data_dir()
            .join("prefixes")
            .join(&inst.id)
            .join(
                "drive_c/users/steamuser/AppData/Roaming/Minecraft Bedrock/Users/Shared/games/com.mojang/minecraftWorlds",
            )
    } else {
        inst.dir.join("saves")
    }
}

/// 扫描 .minecraft/versions/ 下的所有已安装实例
pub fn scan_instances(mc_dir: &Path) -> Vec<Instance> {
    let versions_dir = mc_dir.join("versions");
    let mut instances = Vec::new();
    let Ok(entries) = std::fs::read_dir(&versions_dir) else {
        return instances;
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let Some(id) = dir.file_name().and_then(|s| s.to_str()).map(String::from) else {
            continue;
        };
        let profile_path = dir.join(format!("{id}.json"));
        let Ok(text) = std::fs::read_to_string(&profile_path) else {
            continue;
        };
        let Ok(profile) = serde_json::from_str::<VersionProfile>(&text) else {
            continue;
        };
        instances.push(Instance {
            bedrock: false,
            id,
            version_type: profile.version_type.clone(),
            main_class: profile.main_class.clone(),
            assets_index: profile
                .asset_index
                .as_ref()
                .map(|a| a.id.clone())
                .or_else(|| {
                    let a = profile.assets.clone();
                    if a.is_empty() {
                        None
                    } else {
                        Some(a)
                    }
                })
                .unwrap_or_else(|| "legacy".into()),
            java_major: profile
                .java_version
                .as_ref()
                .map(|j| j.major_version)
                .unwrap_or(8),
            dir,
        });
    }
    instances.sort_by(|a, b| a.id.cmp(&b.id));
    instances
}

/// 加载版本 profile（自动合并 inheritsFrom 继承链，子覆盖父）
pub fn load_profile(inst: &Instance) -> Result<VersionProfile> {
    let text = std::fs::read_to_string(inst.profile_path())
        .with_context(|| format!("读取版本 json 失败: {}", inst.profile_path().display()))?;
    let profile: VersionProfile = serde_json::from_str(&text)?;
    merge_inheritance(inst.dir.parent().unwrap_or(Path::new(".")), profile)
}

/// 递归合并继承链
fn merge_inheritance(versions_dir: &Path, child: VersionProfile) -> Result<VersionProfile> {
    let Some(parent_id) = child.inherits_from.clone() else {
        return Ok(child);
    };
    let parent_path = versions_dir
        .join(&parent_id)
        .join(format!("{parent_id}.json"));
    let text = std::fs::read_to_string(&parent_path)
        .with_context(|| format!("继承版本 {parent_id} 缺失: {}", parent_path.display()))?;
    let parent: VersionProfile = serde_json::from_str(&text)?;
    let parent = merge_inheritance(versions_dir, parent)?;
    Ok(merge_profile(parent, child))
}

/// 合并两个 profile：parent 为基础，child 覆盖
pub fn merge_profile(mut parent: VersionProfile, child: VersionProfile) -> VersionProfile {
    if !child.main_class.is_empty() {
        parent.main_class = child.main_class;
    }
    if !child.assets.is_empty() {
        parent.assets = child.assets;
    }
    if child.asset_index.is_some() {
        parent.asset_index = child.asset_index;
    }
    if child.java_version.is_some() {
        parent.java_version = child.java_version;
    }
    if let Some(child_args) = child.arguments {
        let mut game = child_args.game;
        let mut jvm = child_args.jvm;
        if game.is_empty() {
            if let Some(p) = &parent.arguments {
                game = p.game.clone();
            }
        }
        if jvm.is_empty() {
            if let Some(p) = &parent.arguments {
                jvm = p.jvm.clone();
            }
        }
        parent.arguments = Some(version::Arguments { game, jvm });
    }
    if child.minecraft_arguments.is_some() {
        parent.minecraft_arguments = child.minecraft_arguments;
    }
    if !child.libraries.is_empty() {
        parent.libraries.extend(child.libraries);
    }
    if child.downloads.is_some() {
        parent.downloads = child.downloads;
    }
    parent.inherits_from = None;
    parent
}

/// 该库是否需要下载（本地缺失或 sha1 不符）
fn library_need_download(library_dir: &Path, path: &str, sha1: &str) -> bool {
    let target = library_dir.join(path);
    if target.is_file() {
        !sha1.is_empty() && !file_sha1_ok(&target, sha1)
    } else {
        true
    }
}

fn file_sha1_ok(path: &Path, expect: &str) -> bool {
    use std::io::Read;
    let Ok(mut f) = std::fs::File::open(path) else {
        return false;
    };
    use sha1::{Digest, Sha1};
    let mut ctx = Sha1::new();
    let mut buf = [0u8; 65536];
    loop {
        match f.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => ctx.update(&buf[..n]),
            Err(_) => return false,
        }
    }
    format!("{:x}", ctx.finalize()) == expect.to_ascii_lowercase()
}

/// 单个文件的下载进度快照（细线的数据源）
#[derive(Debug, Clone)]
pub struct FileProgress {
    pub name: String,
    pub done: u64,
    pub total: u64,
    /// 完成时刻（秒），用于算速率；下载中为 None
    pub finished_at: Option<std::time::Instant>,
    pub failed: bool,
}

/// 下载到指定路径（父目录自动创建），写临时文件再改名。`pub(crate)` 供 modpack 复用。
/// `on_tick` 每收到一个数据块回调一次（已下, 总长）——驱动 UI 上的动态细线。
/// 总长来自 `Content-Length`，缺失时为 0（UI 按不定进度渲染）。
pub(crate) async fn download_to(
    client: &reqwest::Client,
    url: &str,
    target: &Path,
    mut on_tick: impl FnMut(u64, u64),
) -> Result<()> {
    let parent = target.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)?;
    let tmp = target.with_extension("tmp");
    let resp = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("请求失败: {url}"))?;
    if !resp.status().is_success() {
        return Err(anyhow!("HTTP {}: {}", resp.status(), url));
    }
    let total = resp.content_length().unwrap_or(0);
    let mut file = std::fs::File::create(&tmp)?;
    let mut done = 0u64;
    // 边下边写，不再 bytes.await 一次性吞进内存
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        file.write_all(&chunk)?;
        done += chunk.len() as u64;
        on_tick(done, total);
    }
    file.flush()?;
    drop(file);
    // Content-Length 缺失时 stream 结束即完整；以实际落盘为准
    if total > 0 && done != total {
        return Err(anyhow!("长度不符: 收到 {done} / 声明 {total}（{url}）"));
    }
    std::fs::rename(&tmp, target)?;
    Ok(())
}

/// 下载：官方源为准，镜像（若配置）先试；连接层抖动自动重试。
async fn download_fallback(
    client: &reqwest::Client,
    mirror_url: Option<&str>,
    official_url: &str,
    target: &Path,
    on_tick: &mut (dyn FnMut(u64, u64) + Send),
) -> Result<()> {
    // ponytail: 只对连接层错误（超时/重置/DNS）重试，HTTP 4xx/5xx 重试无意义。
    // 官方源为主源放最后——它最可靠，镜像只是加速手段。
    let mut urls: Vec<(&str, &str)> = Vec::new(); // (来源标签, url)
    if let Some(m) = mirror_url {
        urls.push(("镜像", m));
    }
    urls.push(("官方", official_url));

    let mut errs = Vec::new();
    for (tag, url) in urls {
        let mut last: Option<anyhow::Error> = None;
        for attempt in 0..DOWNLOAD_ATTEMPTS {
            match download_to(client, url, target, &mut *on_tick).await {
                Ok(()) => return Ok(()),
                Err(e) => {
                    let transient = !e.to_string().contains("HTTP ");
                    last = Some(e);
                    if !transient {
                        break;
                    }
                    if attempt + 1 < DOWNLOAD_ATTEMPTS {
                        tokio::time::sleep(std::time::Duration::from_millis(
                            400 * (attempt as u64 + 1),
                        ))
                        .await;
                    }
                }
            }
        }
        if let Some(e) = last {
            errs.push(format!("[{tag}] {e:#}"));
        }
    }
    Err(anyhow!("{}", errs.join(" → ")))
}

const DOWNLOAD_ATTEMPTS: usize = 3;

/// fabric profile 里那些「无 artifact」库的默认 maven 源。
/// 实测 net.fabricmc / org.ow2.asm 都在这里，fabric-loader 1.9MB 200 可下。
const FABRIC_MAVEN: &str = "https://maven.fabricmc.net";

/// 同时下载的文件数上限。8 是实测可用值：再高会顶到本机 DNS 解析能力。
const DOWNLOAD_CONCURRENCY: usize = 8;

/// 可续传下载（仅测试覆盖）：同一目标路径上若留着上次中断的 `.tmp`，
/// 带 `Range` 从断点接着下；服务端不认 Range（回 200）就从头来。
#[cfg(test)]
pub(crate) async fn download_to_resumable(
    client: &reqwest::Client,
    url: &str,
    target: &Path,
    mut on_tick: impl FnMut(u64, u64),
) -> Result<()> {
    let parent = target.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)?;
    let tmp = target.with_extension("tmp");
    let have = std::fs::metadata(&tmp).map(|m| m.len()).unwrap_or(0);
    let mut req = client.get(url);
    if have > 0 {
        req = req.header(reqwest::header::RANGE, format!("bytes={have}-"));
    }
    let resp = req
        .send()
        .await
        .with_context(|| format!("请求失败: {url}"))?;
    if !resp.status().is_success() {
        return Err(anyhow!("HTTP {}: {}", resp.status(), url));
    }
    let resumed = have > 0 && resp.status() == reqwest::StatusCode::PARTIAL_CONTENT;
    let body_total = resp.content_length().unwrap_or(0);
    let total = if resumed { have + body_total } else { body_total };
    let (mut file, mut done) = if resumed {
        let f = std::fs::OpenOptions::new().append(true).open(&tmp)?;
        (f, have)
    } else {
        (std::fs::File::create(&tmp)?, 0u64)
    };
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        file.write_all(&chunk)?;
        done += chunk.len() as u64;
        on_tick(done, total);
    }
    file.flush()?;
    drop(file);
    if total > 0 && done != total {
        return Err(anyhow!("长度不符: 收到 {done} / 声明 {total}（{url}）"));
    }
    std::fs::rename(&tmp, target)?;
    Ok(())
}

/// 文件 md5（仅测试覆盖；小写十六进制，流式读）
#[cfg(test)]
pub(crate) fn md5_file(path: &Path) -> Result<String> {
    use md5::{Digest as _, Md5};
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let mut h = Md5::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(format!("{:x}", h.finalize()))
}

/// 镜像源：环境变量 `MCTL_MIRROR` 指定，**默认不启用镜像**。
/// 实测 镜像源（曾经的默认值）在境内已不可用：`/assets/...` 返回 302 跳到
/// `bmclapi2.sakura.sld.tw` → `bmclapi4.sakura.sld.tw`，连接被对方重置。
/// 高校镜像（tuna/ustc/nju）只镜像 `minecraft/web_assets` 的元数据，不提供
/// `assets/objects` 资源对象。要镜像自己设 `MCTL_MIRROR=<base>`。
pub fn mirror() -> Option<String> {
    std::env::var("MCTL_MIRROR")
        .ok()
        .map(|s| s.trim().trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty())
}

/// API 请求客户端：全局单例，连接池复用。
/// 每次重建 Client 都要重新 DNS + TLS 握手，连续分页会白白慢半秒；
/// 网易接口那边就是单例（见 `api::client`），这里同口径。
pub(crate) fn http_client() -> reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .connect_timeout(std::time::Duration::from_secs(15))
                .timeout(std::time::Duration::from_secs(120))
                .pool_max_idle_per_host(8)
                .build()
                .unwrap_or_else(|_| reqwest::Client::new())
        })
        .clone()
}

/// 大文件下载客户端：总超时放宽到 30 分钟（游戏版本包上百 MB，
/// 120s 的默认总超时会在慢速链路上半路掐断）。同样复用连接池。
pub(crate) fn http_client_big() -> reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .connect_timeout(std::time::Duration::from_secs(15))
                .timeout(std::time::Duration::from_secs(1800))
                .pool_max_idle_per_host(8)
                .build()
                .unwrap_or_else(|_| reqwest::Client::new())
        })
        .clone()
}

/// 展示用短名：maven 坐标只留最后两段
/// `org/slf4j/slf4j-api/2.0.9/slf4j-api-2.0.9.jar` → `2.0.9/slf4j-api-2.0.9.jar`
fn short_name(path: &str) -> String {
    let segs: Vec<&str> = path.split('/').collect();
    if segs.len() <= 2 {
        path.to_string()
    } else {
        segs[segs.len() - 2..].join("/")
    }
}

/// 库下载排序权重：客户端 jar 最大，natives 次之，其余按路径倒序。
/// ponytail: 库这边索引不带 size，只能按路径特征近似；资产有真实 size，
/// 那边是精确排序。
fn big_first_key(path: &str) -> (u8, String) {
    let weight = if path.starts_with("versions/") {
        0 // 客户端 jar 最大
    } else if path.contains("natives") {
        1
    } else {
        2
    };
    (weight, path.to_string())
}

/// 补全依赖库 + 客户端 jar。
/// `progress(done, total)` 报告整体计数，`on_file(FileProgress)` 报告单个文件的
/// 字节级进度（驱动 UI 细线）。两者独立：前者画总进度，后者画 per-file 细线。
pub async fn ensure_libraries(
    mc_dir: &Path,
    profile: &VersionProfile,
    progress: impl Fn(usize, usize),
    on_file: &(dyn Fn(FileProgress) + Send + Sync),
) -> Result<()> {
    let libraries_dir = mc_dir.join("libraries");
    let client = http_client();

    // 收集下载任务：每项 = (原始路径, 自定义 maven 源, 目标路径)
    let mut targets: Vec<(String, String, PathBuf)> = Vec::new();
    // 没有 artifact 的库：fabric profile 全是这个形态（downloads 里只有 name/sha1，
    // 没有 artifact），原先两个 if let 都不进 → 库被**静默跳过**，
    // 表现是「装完了但一启动就 ClassNotFoundException」。
    let mut skipped: Vec<String> = Vec::new();
    for lib in &profile.libraries {
        let artifact = lib.downloads.as_ref().and_then(|d| d.artifact.as_ref());
        match artifact {
            Some(a) => {
                let path = &a.path;
                // 版本 json 的 artifact.path 不可信（modpack 作者可控）：`..`/绝对路径一律跳过，
                // 否则一个畸形 path 就能把文件写到实例目录外面
                if modpack::is_unsafe_rel_path(path) {
                    skipped.push(path.clone());
                } else if library_need_download(&libraries_dir, path, &a.sha1) {
                    targets.push((path.clone(), lib.url.clone(), libraries_dir.join(path)));
                }
            }
            // 没有 artifact → 按 maven 坐标自己推路径，源优先 lib.url，
            // 否则用 fabric 官方 maven（net.fabricmc/ow2.asm 都托管在这）
            None => {
                let path = version::maven_path(&lib.name);
                if modpack::is_unsafe_rel_path(&path) {
                    skipped.push(path);
                    continue;
                }
                if libraries_dir.join(&path).is_file() {
                    continue;
                }
                // lib.url 是 maven **根**（https://maven.fabricmc.net/），
                // 必须自己把坐标路径接上去——直接传 base 会下到 maven 的
                // HTML 目录索引页（HTTP 200，743 字节，静默写坏文件）
                let base = if lib.url.is_empty() {
                    FABRIC_MAVEN
                } else {
                    lib.url.as_str()
                };
                let base = base.trim_end_matches('/');
                if base.starts_with("http") {
                    targets.push((
                        path.clone(),
                        format!("{base}/{path}"),
                        libraries_dir.join(path),
                    ));
                } else {
                    skipped.push(lib.name.clone());
                }
            }
        }
    }
    if !skipped.is_empty() {
        eprintln!(
            "[mctl] {} 个库既无 artifact 也无 maven 源，跳过：{}",
            skipped.len(),
            skipped.join(", ")
        );
    }
    // 客户端 jar（优先 json 里的官方直链，镜像兜底）
    let jar_id = profile.id.clone();
    if let Some(info) = profile.downloads.as_ref().and_then(|d| d.client.as_ref()) {
        let target = mc_dir
            .join("versions")
            .join(&jar_id)
            .join(format!("{jar_id}.jar"));
        let suffix = format!("versions/{jar_id}/{jar_id}.jar");
        if library_need_download(&mc_dir.join("versions"), &suffix, &info.sha1) {
            targets.push((suffix, info.url.clone(), target));
        }
    }

    let total = targets.len();
    let done = AtomicUsize::new(0);
    progress(0, total);
    let mirror_root = mirror();
    // ponytail: 限流是必需的，不是优化。FuturesUnordered 不限并发时会把
    // 全部任务一次性 poll 出去 → DNS 解析同时打满 Tokio blocking 池
    // → "dns error: Too many open files"。buffered(N) 才是真正的并发闸门。
    let mut futs: Vec<_> = Vec::new();
    // 大文件优先：长尾里最后那几个小文件决定了总等待时间，先啃大的
    targets.sort_by(|a, b| big_first_key(&b.0).cmp(&big_first_key(&a.0)));
    for (path, custom_url, target) in &targets {
        let path_owned = path.clone();
        let custom_url = custom_url.clone();
        let target = target.clone();
        let mirror_root = &mirror_root;
        let client = &client;
        let on_file = on_file;
        futs.push(async move {
            // 镜像需 libraries/ 前缀（镜像源 格式），官方源不需要；未配镜像则为 None
            let mirror = mirror_root.as_ref().map(|r| {
                if path_owned.starts_with("versions/") {
                    // 客户端 jar 走镜像的 /versions/ 路径
                    format!("{r}/{path_owned}")
                } else {
                    format!("{r}/libraries/{path_owned}")
                }
            });
            // 展示用短名：只留最后两段，去掉 maven 坐标的长前缀
            let display = short_name(&path_owned);
            let mut tick = |d: u64, t: u64| {
                on_file(FileProgress {
                    name: display.clone(),
                    done: d,
                    total: t,
                    finished_at: None,
                    failed: false,
                })
            };
            let err = if custom_url.is_empty() {
                let official = format!("https://libraries.minecraft.net/{path_owned}");
                download_fallback(client, mirror.as_deref(), &official, &target, &mut tick).await
            } else {
                // 自定义 maven 源（maven.fabricmc.net 等）自带正确坐标，
                // 套 mirror 的 libraries/ 前缀只会拿到 404，所以不走镜像。
                download_fallback(client, None, &custom_url, &target, &mut tick).await
            };
            on_file(FileProgress {
                name: display,
                done: err.as_ref().map(|_| u64::MAX).unwrap_or(0),
                total: 0,
                finished_at: Some(std::time::Instant::now()),
                failed: err.is_err(),
            });
            (target, err)
        });
    }
    let mut tasks = futures_util::stream::iter(futs).buffered(DOWNLOAD_CONCURRENCY);
    while let Some((target, err)) = tasks.next().await {
        if let Err(e) = err {
            eprintln!("[mctl] 库下载失败 {}: {e}", target.display());
        }
        let d = done.fetch_add(1, Ordering::Relaxed) + 1;
        progress(d, total);
    }
    Ok(())
}

/// 资产索引 id 合法性：非空、相对、无 `..`（它会直接进文件名）
pub fn asset_index_id_ok(id: &str) -> bool {
    !id.is_empty() && !modpack::is_unsafe_rel_path(id)
}

/// 资源对象 hash 形状：≥2 位小写 hex（Mojang/网易都是 40 位 sha1）
fn is_asset_hash(h: &str) -> bool {
    h.len() >= 2
        && h.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// 下载资产索引（镜像优先，官方兜底），返回索引文件路径
pub async fn ensure_asset_index(mc_dir: &Path, index: &version::AssetIndex) -> Result<PathBuf> {
    let client = http_client();
    let indexes_dir = mc_dir.join("assets/indexes");
    std::fs::create_dir_all(&indexes_dir)?;
    if !asset_index_id_ok(&index.id) {
        return Err(anyhow!(
            "资产索引 id 非法（不可为空/不可越界）: {:?}",
            index.id
        ));
    }
    let index_file = indexes_dir.join(format!("{}.json", index.id));
    if !index_file.is_file() {
        let mirror = mirror().map(|r| format!("{r}/assets/indexes/{}.json", index.id));
        let official = if index.url.starts_with("http") {
            index.url.clone()
        } else {
            format!(
                "https://piston-meta.mojang.com/minecraft/assetIndex/{}",
                index.url
            )
        };
        if let Err(e) = download_fallback(
            &client,
            mirror.as_deref(),
            &official,
            &index_file,
            &mut |_, _| {},
        )
        .await
        {
            eprintln!("[mctl] 资产索引下载失败: {e}");
            return Ok(index_file);
        }
    }
    Ok(index_file)
}

/// 补全资源文件。
/// `progress(done, total)` 报告整体计数，`on_file(FileProgress)` 报告单文件字节级进度。
pub async fn ensure_assets(
    mc_dir: &Path,
    profile: &VersionProfile,
    progress: impl Fn(usize, usize),
    on_file: &(dyn Fn(FileProgress) + Send + Sync),
) -> Result<()> {
    let Some(index) = &profile.asset_index else {
        progress(0, 1);
        progress(1, 1);
        return Ok(());
    };
    let client = http_client();
    let indexes_dir = mc_dir.join("assets/indexes");
    let objects_dir = mc_dir.join("assets/objects");
    std::fs::create_dir_all(&indexes_dir)?;
    std::fs::create_dir_all(&objects_dir)?;

    // 先下载索引
    let index_file = ensure_asset_index(mc_dir, index).await?;
    let text = std::fs::read_to_string(&index_file)?;
    let assets: AssetsIndex = serde_json::from_str(&text)?;

    let entries: Vec<(&String, &version::AssetObject)> = assets
        .objects
        .iter()
        // hash 不可信（索引可来自 modpack）：畸形值会让 `hash[..2]` panic，
        // 也可能拼出跳目录的路径。Mojang/网易的 hash 都是小写 hex。
        .filter(|(_, o)| is_asset_hash(&o.hash))
        .collect();
    let total = entries.len();
    // 已存在的文件直接计入进度
    let done = AtomicUsize::new(
        entries
            .iter()
            .filter(|(_, obj)| objects_dir.join(&obj.hash[..2]).join(&obj.hash).is_file())
            .count(),
    );
    progress(done.load(Ordering::Relaxed), total);
    let mirror_root = mirror();
    // ponytail: 限流是必需的，不是优化。FuturesUnordered 不限并发时会把
    // 全部任务一次性 poll 出去 → DNS 解析同时打满 Tokio blocking 池
    // → "dns error: Too many open files"。buffered(N) 才是真正的并发闸门。
    let mut futs: Vec<_> = Vec::new();
    // 索引里带真实 size，大文件先下——长尾里那几百个小文件才是等待时间的大头
    let mut pending: Vec<(&String, &version::AssetObject)> = entries
        .iter()
        .filter(|(_, obj)| !objects_dir.join(&obj.hash[..2]).join(&obj.hash).is_file())
        .copied()
        .collect();
    pending.sort_by(|a, b| b.1.size.cmp(&a.1.size));
    for (name, obj) in pending {
        let rel = format!("{}/{}/{}", &obj.hash[..2], obj.hash, name);
        let target = objects_dir.join(&obj.hash[..2]).join(&obj.hash);
        let hash = obj.hash.clone();
        let mirror_root = &mirror_root;
        let client = &client;
        let on_file = on_file;
        let display = (*name).clone();
        futs.push(async move {
            let mirror = mirror_root.as_ref().map(|r| format!("{r}/assets/{rel}"));
            let official = format!(
                "https://resources.download.minecraft.net/{}/{}",
                &hash[..2],
                hash
            );
            let mut tick = |d: u64, t: u64| {
                on_file(FileProgress {
                    name: display.clone(),
                    done: d,
                    total: t,
                    finished_at: None,
                    failed: false,
                })
            };
            let err =
                download_fallback(client, mirror.as_deref(), &official, &target, &mut tick).await;
            on_file(FileProgress {
                name: display,
                done: if err.is_ok() { u64::MAX } else { 0 },
                total: 0,
                finished_at: Some(std::time::Instant::now()),
                failed: err.is_err(),
            });
            (target, err)
        });
    }
    let mut tasks = futures_util::stream::iter(futs).buffered(DOWNLOAD_CONCURRENCY);
    // 失败明细已经在 UI 里逐条显示（on_file 带 failed），这里只聚合计数。
    // 原来每个失败打一行 eprintln!，3911 个失败就是几千行日志把终端刷爆、
    // 进度 UI 根本看不见。
    let mut failed = 0usize;
    let mut first_err: Option<String> = None;
    while let Some((target, err)) = tasks.next().await {
        if let Err(e) = err {
            failed += 1;
            if first_err.is_none() {
                first_err = Some(format!("{}: {e:#}", target.display()));
            }
        }
        let d = done.fetch_add(1, Ordering::Relaxed) + 1;
        if d % 16 == 0 || d == total {
            progress(d, total);
        }
    }
    progress(total, total);
    if failed > 0 {
        eprintln!(
            "[mctl] {failed}/{} 个资产失败（明细见下载中心）首个：{}",
            total,
            first_err.unwrap_or_default()
        );
    }
    Ok(())
}

/// 解压 natives（classifier 含 natives-<os> 的库）到 versions/<id>/natives
pub fn extract_natives(mc_dir: &Path, profile: &VersionProfile, inst: &Instance) -> Result<()> {
    let natives_dir = inst.dir.join("natives");
    std::fs::create_dir_all(&natives_dir)?;
    let libraries_dir = mc_dir.join("libraries");
    let os = std::env::consts::OS;
    let classifier = match os {
        "linux" => "natives-linux",
        "macos" => "natives-macos",
        "windows" => "natives-windows",
        _ => return Ok(()),
    };
    for lib in &profile.libraries {
        let Some(d) = &lib.downloads else {
            continue;
        };
        let Some(classifiers) = &d.classifiers else {
            continue;
        };
        let Some(info) = classifiers.get(classifier) else {
            continue;
        };
        let jar_path = libraries_dir.join(&info.path);
        if !jar_path.is_file() {
            continue;
        }
        unzip_skip_meta_inf(&jar_path, &natives_dir)?;
    }
    Ok(())
}

fn unzip_skip_meta_inf(zip_path: &Path, out_dir: &Path) -> Result<()> {
    let file = std::fs::File::open(zip_path)?;
    let mut archive = zip::ZipArchive::new(file)?;
    std::fs::create_dir_all(out_dir)?;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        let name = entry.name().to_string();
        if name.contains("META-INF") || name.ends_with('/') {
            continue;
        }
        // 跳过同类库的重复文件（已存在则不覆盖）
        let target = out_dir.join(name.rsplit('/').next().unwrap_or(&name));
        if target.is_file() {
            continue;
        }
        let mut out = std::fs::File::create(&target)?;
        std::io::copy(&mut entry, &mut out)?;
    }
    Ok(())
}

/// 组装 classpath：库 jar + 客户端 jar，': ' 分隔
pub fn build_classpath(mc_dir: &Path, profile: &VersionProfile, inst: &Instance) -> String {
    let libraries_dir = mc_dir.join("libraries");
    let mut parts: Vec<String> = Vec::new();
    for lib in &profile.libraries {
        if let Some(path) = maven_or_download_path(&libraries_dir, lib) {
            parts.push(path);
        }
    }
    if inst.client_jar().is_file() {
        parts.push(inst.client_jar().display().to_string());
    }
    parts.join(":")
}

/// 库的本地 jar 路径（优先 downloads.path，否则 maven 坐标推导）
fn maven_or_download_path(libraries_dir: &Path, lib: &Library) -> Option<String> {
    let path = lib
        .downloads
        .as_ref()
        .and_then(|d| d.artifact.as_ref())
        .map(|a| a.path.clone())
        .unwrap_or_else(|| version::maven_path(&lib.name));
    let p = libraries_dir.join(&path);
    if p.is_file() {
        Some(p.display().to_string())
    } else {
        None
    }
}

/// 启动参数占位符替换
/// mc_dir = 共享根（${assets_root} 取 mc_dir/assets），game_dir = 版本隔离的实例目录（${game_directory}）
fn replace_tokens(
    arg: &str,
    username: &str,
    mc_dir: &Path,
    game_dir: &Path,
    inst: &Instance,
    uuid: &str,
    token: &str,
    user_type: &str,
) -> String {
    arg.replace("${version_name}", &inst.id)
        .replace("${game_directory}", &game_dir.display().to_string())
        .replace(
            "${assets_root}",
            &mc_dir.join("assets").display().to_string(),
        )
        .replace("${assets_index_name}", &inst.assets_index)
        .replace("${auth_player_name}", username)
        .replace("${auth_uuid}", uuid)
        .replace("${auth_access_token}", token)
        .replace("${clientid}", &rand_uuid_str())
        .replace("${auth_xuid}", "0")
        .replace("${auth_session}", "0")
        .replace("${user_type}", user_type)
        .replace("${launcher_name}", "mctl")
        .replace("${launcher_version}", env!("CARGO_PKG_VERSION"))
        .replace("${version_type}", &inst.version_type)
        .replace("${resolution_width}", "1280")
        .replace("${resolution_height}", "720")
        .replace(
            "${natives_directory}",
            &inst.dir.join("natives").display().to_string(),
        )
        .replace(
            "${classpath}",
            &inst.dir.join("classpath.txt").display().to_string(),
        )
}

/// 离线 UUID（v3，MD5("OfflinePlayer:" + 用户名)，离线 UUID 标准算法）
fn offline_uuid(username: &str) -> String {
    use md5::{Digest, Md5};
    let mut ctx = Md5::new();
    ctx.update(format!("OfflinePlayer:{username}"));
    let mut d: [u8; 16] = ctx.finalize().into();
    d[6] = (d[6] & 0x0f) | 0x30; // version 3
    d[8] = (d[8] & 0x3f) | 0x80; // variant 1
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        d[0], d[1], d[2], d[3], d[4], d[5], d[6], d[7], d[8], d[9], d[10], d[11], d[12], d[13], d[14], d[15]
    )
}

fn rand_uuid_bytes() -> [u8; 16] {
    use rand::RngCore;
    let mut b = [0u8; 16];
    rand::rng().fill_bytes(&mut b);
    b
}

fn rand_uuid_str() -> String {
    let d = rand_uuid_bytes();
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        d[0], d[1], d[2], d[3], d[4], d[5], d[6], d[7], d[8], d[9], d[10], d[11], d[12], d[13], d[14], d[15]
    )
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// 组装启动命令（只替换，不真正启动）
pub fn build_launch_command(
    mc_dir: &Path,
    game_dir: &Path,
    inst: &Instance,
    profile: &VersionProfile,
    java: &Path,
    username: &str,
    memory_mb: u32,
    quick_play_world: Option<&str>,
    netease_uuid: Option<&str>,
    authlib: Option<(&Path, &str, &str, &str)>, // (jar, yggdrasil根, 角色uuid, access_token)
) -> Result<Command> {
    let natives_dir = inst.dir.join("natives");
    let classpath = build_classpath(mc_dir, profile, inst);
    let mut cmd = Command::new(java);
    // 同上：游戏 cwd = 游戏目录，别写进启动器的工作目录
    cmd.current_dir(mc_dir);
    cmd.arg(format!("-Xmx{memory_mb}m"));
    if let Some((jar, root, _, _)) = authlib {
        cmd.arg(format!("-javaagent:{}={root}", jar.display()));
    }
    cmd.arg(format!("-Djava.library.path={}", natives_dir.display()));
    cmd.arg("-cp").arg(&classpath);
    cmd.arg(&inst.main_class);

    // 现代版本 arguments.game，旧版 minecraft_arguments 模板
    // ${game_directory} 用 game_dir（版本隔离），资产/库仍取自 mc_dir 共享根
    // 外置登录：真实角色 uuid + access_token（authlib-injector 会向 Yggdrasil 校验）
    let (uuid, token, user_type): (String, String, &str) = match authlib {
        Some((_, _, au, at)) => (au.to_string(), at.to_string(), "msa"),
        None => match netease_uuid {
            Some(u) => (u.to_string(), "0".to_string(), "legacy"),
            None => (
                offline_uuid(username),
                format!("mctl{}", hex(&rand_uuid_bytes())),
                "legacy",
            ),
        },
    };
    let tokens = |s: &str| {
        replace_tokens(
            s, username, mc_dir, game_dir, inst, &uuid, &token, user_type,
        )
    };
    let mut ctx = version::RuleCtx::host();
    // 注入 --width/--height（replace_tokens 里 1280x720）：默认 854x480 窗口 + guiScale
    // 自动缩放会让设置界面元素异常巨大
    ctx.has_custom_resolution = true;
    if quick_play_world.is_some() {
        ctx.has_quick_plays_support = true;
        ctx.is_quick_play_singleplayer = true;
    }
    if let Some(args) = &profile.arguments {
        // 先攒进 Vec 再入 Command：quickPlayPath 需要连同字面量一起丢弃
        let mut game_args: Vec<String> = Vec::new();
        for arg in &args.game {
            for v in arg.values(&ctx) {
                if v == "${quickPlaySingleplayer}" {
                    // 仅 quickPlay 直进世界时注入世界目录名
                    if let Some(w) = quick_play_world {
                        game_args.push(w.to_string());
                    }
                    continue;
                }
                if v == "${quickPlayPath}" {
                    // 官方语义 = 游玩记录日志文件路径（游戏退出时往里写 JSON），
                    // 传世界路径会被 JSON 记录顶替目录名 → 世界从列表消失。
                    // 连同字面量 "--quickPlayPath" 一起丢弃。
                    if game_args.last().map(String::as_str) == Some("--quickPlayPath") {
                        game_args.pop();
                    }
                    continue;
                }
                if v.contains("${") {
                    game_args.push(tokens(&v));
                } else {
                    game_args.push(v);
                }
            }
        }
        for a in game_args {
            cmd.arg(a);
        }
    } else if let Some(tpl) = &profile.minecraft_arguments {
        for part in tpl.split_whitespace() {
            cmd.arg(tokens(part));
        }
    }
    Ok(cmd)
}

/// 辅助：读取 game 参数（供测试/调试）
pub fn _game_args(profile: &VersionProfile) -> Vec<String> {
    let ctx = version::RuleCtx::host();
    let mut out = Vec::new();
    if let Some(args) = &profile.arguments {
        for arg in &args.game {
            out.extend(arg.values(&ctx));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 资源索引里不可信的两个字段：hash 形状 + 索引 id（都会进文件路径）
    #[test]
    fn asset_entries_are_validated() {
        assert!(is_asset_hash("d0f2f0e3f0a0b1c2d3e4f5a6b7c8d9e0f1a2b3c4"));
        assert!(is_asset_hash("ab"));
        assert!(!is_asset_hash(""), "空 hash 会在 hash[..2] 上崩");
        assert!(!is_asset_hash("A"), "大写/短 hash 不接受");
        assert!(!is_asset_hash("../../etc"), "畸形 hash 不接受");

        assert!(asset_index_id_ok("5"));
        assert!(asset_index_id_ok("pre-1.18"));
        assert!(!asset_index_id_ok(""));
        assert!(!asset_index_id_ok("../evil"));
        assert!(!asset_index_id_ok("/abs"));
    }

    fn make_instance(mc: &Path) -> Instance {
        let dir = mc.join("versions/1.21-test");
        std::fs::create_dir_all(&dir).unwrap();
        let json = r#"{
            "id": "1.21-test",
            "type": "release",
            "mainClass": "net.minecraft.client.main.Main",
            "assets": "17",
            "javaVersion": { "majorVersion": 21 },
            "arguments": {
                "game": ["--username", "${auth_player_name}", "--version", "${version_name}"]
            },
            "libraries": [
                { "name": "com.mojang:test-lib:1.0", "downloads": { "artifact": { "path": "com/mojang/test-lib/1.0/test-lib-1.0.jar", "url": "x", "sha1": "", "size": 1 } } }
            ],
            "downloads": { "client": { "sha1": "", "size": 1 } }
        }"#;
        std::fs::write(dir.join("1.21-test.json"), json).unwrap();
        Instance {
            bedrock: false,
            id: "1.21-test".into(),
            version_type: "release".into(),
            main_class: "net.minecraft.client.main.Main".into(),
            assets_index: "17".into(),
            java_major: 21,
            dir,
        }
    }

    #[test]
    fn scan_and_profile() {
        let mc = std::env::temp_dir().join("mctl_test_scan");
        let _ = std::fs::remove_dir_all(&mc);
        std::fs::create_dir_all(&mc).unwrap();
        let inst = make_instance(&mc);
        let scanned = scan_instances(&mc);
        assert_eq!(scanned.len(), 1);
        assert_eq!(scanned[0].id, "1.21-test");
        assert_eq!(scanned[0].java_major, 21);
        let profile = load_profile(&inst).unwrap();
        assert_eq!(profile.main_class, "net.minecraft.client.main.Main");
        let _ = std::fs::remove_dir_all(&mc);
    }

    #[test]
    fn classpath_uses_download_path() {
        let mc = std::env::temp_dir().join("mctl_test_cp");
        let _ = std::fs::remove_dir_all(&mc);
        std::fs::create_dir_all(&mc).unwrap();
        let inst = make_instance(&mc);
        let profile = load_profile(&inst).unwrap();
        // 无本地 jar 时应为空 classpath（不崩溃）
        let cp = build_classpath(&mc, &profile, &inst);
        assert!(!cp.contains("test-lib-1.0.jar"));
        let _ = std::fs::remove_dir_all(&mc);
    }

    #[test]
    fn merge_child_overrides_parent() {
        let parent = version::VersionProfile {
            main_class: "parent.Main".into(),
            assets: "17".into(),
            ..Default::default()
        };
        let mut child = parent.clone();
        child.main_class = "child.Main".into();
        child.inherits_from = Some("1.21.1".into());
        let merged = merge_profile(parent, child);
        assert_eq!(merged.main_class, "child.Main");
        assert_eq!(merged.assets, "17");
        assert!(merged.inherits_from.is_none());
    }

    #[test]
    fn merge_keeps_parent_libs_and_adds_child() {
        let parent = version::VersionProfile {
            libraries: vec![serde_json::from_str(r#"{"name":"a:b:1"}"#).unwrap()],
            ..Default::default()
        };
        let child = version::VersionProfile {
            libraries: vec![serde_json::from_str(r#"{"name":"c:d:2","url":"https://x"}"#).unwrap()],
            ..Default::default()
        };
        let merged = merge_profile(parent, child);
        assert_eq!(merged.libraries.len(), 2);
    }

    #[test]
    fn maven_path_legacy() {
        assert_eq!(
            version::maven_path("org.ow2.asm:asm:9.7.1"),
            "org/ow2/asm/asm/9.7.1/asm-9.7.1.jar"
        );
    }

    #[test]
    fn token_replacement() {
        let mc = Path::new("/fake/mc");
        let inst = Instance {
            bedrock: false,
            id: "1.21".into(),
            version_type: "release".into(),
            main_class: "x".into(),
            assets_index: "17".into(),
            java_major: 21,
            dir: PathBuf::from("/fake/mc/versions/1.21"),
        };
        let out = replace_tokens(
            "--username ${auth_player_name} --version ${version_name}",
            "Steve",
            mc,
            mc,
            &inst,
            &offline_uuid("Steve"),
            "test-token",
            "legacy",
        );
        assert!(out.contains("Steve"));
        assert!(out.contains("1.21"));
    }

    #[test]
    fn assets_root_stays_at_shared_root_under_version_isolation() {
        // 回归：版本隔离时 ${game_directory}=实例目录，${assets_root}=共享根/assets
        let mc = Path::new("/fake/mc");
        let inst = Instance {
            bedrock: false,
            id: "1.21".into(),
            version_type: "release".into(),
            main_class: "x".into(),
            assets_index: "17".into(),
            java_major: 21,
            dir: PathBuf::from("/fake/mc/versions/1.21"),
        };
        let out = replace_tokens(
            "--gameDirectory ${game_directory} --assetsDir ${assets_root}",
            "Steve",
            mc,
            &inst.dir,
            &inst,
            "uuid",
            "t",
            "legacy",
        );
        assert!(
            out.contains("--gameDirectory /fake/mc/versions/1.21"),
            "隔离目录: {out}"
        );
        assert!(
            out.contains("--assetsDir /fake/mc/assets"),
            "assets_root 必须取共享根（错误指向实例目录会导致语言列表空+音效丢失）: {out}"
        );
    }

    #[test]
    fn demo_flag_not_injected_for_offline() {
        let mc = std::env::temp_dir().join("mctl_test_demo");
        let _ = std::fs::remove_dir_all(&mc);
        std::fs::create_dir_all(&mc).unwrap();
        let inst = make_instance(&mc);
        let dir = &inst.dir;
        let json = r#"{
            "id": "1.21-test",
            "mainClass": "net.minecraft.client.main.Main",
            "arguments": {
                "game": [
                    "--username", "${auth_player_name}",
                    {"rules": [{"action": "allow", "features": {"is_demo_user": true}}], "value": "--demo"},
                    {"rules": [{"action": "allow", "features": {"has_custom_resolution": true}}], "value": ["--width", "1"]},
                    "--accessToken", "${auth_access_token}"
                ]
            }
        }"#;
        std::fs::write(dir.join("1.21-test.json"), json).unwrap();
        let profile = load_profile(&inst).unwrap();
        let args = _game_args(&profile);
        assert!(
            !args.iter().any(|a| a == "--demo"),
            "demo 仍被注入: {args:?}"
        );
        assert!(!args.iter().any(|a| a == "--width"));
        assert!(args.iter().any(|a| a == "--accessToken"));
        let _ = std::fs::remove_dir_all(&mc);
    }

    #[test]
    fn quick_play_world_injects_world_args() {
        let mc = std::env::temp_dir().join("mctl_test_quickplay");
        let _ = std::fs::remove_dir_all(&mc);
        std::fs::create_dir_all(&mc).unwrap();
        let inst = make_instance(&mc);
        let dir = &inst.dir;
        let json = r#"{
            "id": "1.21-test",
            "mainClass": "net.minecraft.client.main.Main",
            "arguments": {
                "game": [
                    {"rules": [{"action": "allow", "features": {"has_quick_plays_support": true}}], "value": ["--quickPlayPath", "${quickPlayPath}"]},
                    {"rules": [{"action": "allow", "features": {"is_quick_play_singleplayer": true}}], "value": ["--quickPlaySingleplayer", "${quickPlaySingleplayer}"]}
                ]
            }
        }"#;
        std::fs::write(dir.join("1.21-test.json"), json).unwrap();
        let profile = load_profile(&inst).unwrap();
        std::fs::create_dir_all(mc.join("saves/我的世界")).unwrap();

        // 不带世界：quickPlay 参数完全不注入
        let java = std::path::Path::new("/fake/java");
        let cmd = build_launch_command(
            &mc, &mc, &inst, &profile, java, "Player", 2048, None, None, None,
        )
        .unwrap();
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(
            !args.iter().any(|a| a.contains("quickPlay")),
            "普通启动不应带 quickPlay: {args:?}"
        );

        // 带世界：只注入 --quickPlaySingleplayer <目录名>（quickPlayPath 是日志文件路径，传了会写坏世界目录）
        let cmd = build_launch_command(
            &mc,
            &mc,
            &inst,
            &profile,
            java,
            "Player",
            2048,
            Some("我的世界"),
            None,
            None,
        )
        .unwrap();
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let i = args
            .iter()
            .position(|a| a == "--quickPlaySingleplayer")
            .expect("应注入 --quickPlaySingleplayer");
        assert_eq!(args[i + 1], "我的世界");
        assert!(
            !args.iter().any(|a| a == "--quickPlayPath"),
            "quickPlayPath 不应注入（官方语义是游玩记录文件，会顶替世界目录）"
        );
        assert!(
            !args.iter().any(|a| a.contains("${quickPlay")),
            "不应残留 token: {args:?}"
        );
        let _ = std::fs::remove_dir_all(&mc);
    }
}

/// 从文件加载 profile 并合并继承链（供安装期使用）
pub fn merge_inheritance_file(path: &Path) -> Result<VersionProfile> {
    let text = std::fs::read_to_string(path)?;
    let profile: VersionProfile = serde_json::from_str(&text)?;
    let versions_dir = path
        .parent()
        .and_then(|p| p.parent())
        .unwrap_or(Path::new("."));
    merge_inheritance(versions_dir, profile)
}

#[cfg(test)]
mod urlfix {
    use super::*;

    // 定向验证：client jar 用 json 官方直链，落到 versions/ 目录
    // （网络测试：沙箱网络抖动可能偶发失败，cargo test -- --ignored 单独跑）
    #[tokio::test]
    #[ignore]
    async fn client_jar_downloads_correctly() {
        let mc = std::env::temp_dir().join("mctl-e2e-clientjar");
        let _ = std::fs::remove_dir_all(&mc);
        std::fs::create_dir_all(&mc).unwrap();
        let url = "https://piston-data.mojang.com/v1/objects/30c73b1c5da787909b2f73340419fdf13b9def88/client.jar";
        let profile = serde_json::from_str::<version::VersionProfile>(&format!(r#"{{
            "id": "1.21.1",
            "downloads": {{ "client": {{ "path": "", "url": "{url}", "sha1": "30c73b1c5da787909b2f73340419fdf13b9def88", "size": 1 }} }}
        }}"#)).unwrap();
        ensure_libraries(&mc, &profile, |_, _| {}, &|_| {})
            .await
            .unwrap();
        // 客户端 jar 应被下载到 versions/1.21.1/1.21.1.jar
        let jar = mc.join("versions/1.21.1/1.21.1.jar");
        assert!(jar.is_file(), "client jar 未下载: {}", jar.display());
        let _ = std::fs::remove_dir_all(&mc);
    }

    /// md5_file：流式分块算值，跨 1MB 缓冲边界也要和大文件一次算的结果一致
    #[test]
    fn md5_file_streams_and_matches_single_shot() {
        use md5::{Digest as _, Md5};
        let dir = crate::storage::tmp_config_dir("md5-file");
        let small = dir.join("small.bin");
        std::fs::write(&small, b"hello").unwrap();
        assert_eq!(
            md5_file(&small).unwrap(),
            "5d41402abc4b2a76b9719d911017c592"
        );
        let big = dir.join("big.bin");
        let bytes = vec![0u8; 2 * 1024 * 1024 + 7];
        std::fs::write(&big, &bytes).unwrap();
        let mut h = Md5::new();
        h.update(&bytes);
        assert_eq!(md5_file(&big).unwrap(), format!("{:x}", h.finalize()));
    }

    // 流式进度：on_tick 必须被多次触发，且字节单调递增到落盘大小。
    // 这是 细线的数据源——只触发一次的话细线会一步跳到 100%。
    #[tokio::test]
    #[ignore]
    async fn download_to_reports_streaming_progress() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::{Arc, Mutex};

        let client = http_client();
        let target = std::env::temp_dir().join("mctl-e2e-progress.jar");
        let _ = std::fs::remove_file(&target);

        let ticks = Arc::new(AtomicUsize::new(0));
        let seen = Arc::new(Mutex::new(Vec::<u64>::new()));
        let (t2, s2) = (ticks.clone(), seen.clone());
        let mut on_tick = move |done: u64, _total: u64| {
            t2.fetch_add(1, Ordering::Relaxed);
            s2.lock().unwrap().push(done);
        };

        download_to(
            &client,
            "https://piston-data.mojang.com/v1/objects/30c73b1c5da787909b2f73340419fdf13b9def88/client.jar",
            &target,
            &mut on_tick,
        )
        .await
        .unwrap();

        let size = std::fs::metadata(&target).unwrap().len();
        let seen = seen.lock().unwrap().clone();
        assert!(size > 0, "文件为空");
        assert!(
            ticks.load(Ordering::Relaxed) > 1,
            "进度只回调了 {} 次，细线会一步跳完",
            ticks.load(Ordering::Relaxed)
        );
        // 单调不减
        for w in seen.windows(2) {
            assert!(w[1] >= w[0], "进度回退了: {:?} → {:?}", w[0], w[1]);
        }
        assert_eq!(*seen.last().unwrap(), size, "末次进度应等于落盘大小");
        let _ = std::fs::remove_file(&target);
    }

    /// 回归：fabric profile 的库没有 `downloads.artifact`，只有 name + url。
    /// 原实现两个 `if let` 都不进 → 8 个库被静默跳过，装完一启动就崩。
    /// 现在必须按 maven 坐标自己拼路径。
    #[tokio::test]
    #[ignore]
    async fn fabric_profile_libraries_are_not_skipped() {
        let mc = std::env::temp_dir().join("mctl-e2e-fabriclibs");
        let _ = std::fs::remove_dir_all(&mc);
        std::fs::create_dir_all(&mc).unwrap();
        // 真实 fabric profile 片段：downloads 缺 artifact
        let raw = r#"{
            "id": "fabric-loader-0.19.3-1.21.1",
            "inheritsFrom": "1.21.1",
            "mainClass": "net.fabricmc.loader.impl.launch.knot.KnotClient",
            "libraries": [
                {"name": "net.fabricmc:fabric-loader:0.19.3", "url": "https://maven.fabricmc.net/"},
                {"name": "net.fabricmc:intermediary:1.21.1", "url": "https://maven.fabricmc.net/"}
            ]
        }"#;
        let profile: version::VersionProfile = serde_json::from_str(raw).unwrap();
        let libs = mc.join("libraries");
        ensure_libraries(&mc, &profile, |_, _| {}, &|_| {})
            .await
            .unwrap();

        for (name, jar) in [
            (
                "net/fabricmc/fabric-loader/0.19.3",
                "fabric-loader-0.19.3.jar",
            ),
            (
                "net/fabricmc/intermediary/1.21.1",
                "intermediary-1.21.1.jar",
            ),
        ] {
            let jar = libs.join(name).join(jar);
            assert!(
                jar.is_file() && jar.metadata().unwrap().len() > 1000,
                "库未被下载: {}",
                jar.display()
            );
        }
        // ponytail: 手工验证时注释掉这行，--nocapture 后可以直接 ls 目录
        let _ = std::fs::remove_dir_all(&mc);
    }

    // 并发闸门回归：一次性 push 上千个 future 时，FuturesUnordered 不限并发，
    // 会把 DNS 解析全打爆（"Too many open files"）。buffered(N) 必须真的限住。
    #[tokio::test]
    async fn buffered_limits_actual_concurrency() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        let peak = Arc::new(AtomicUsize::new(0));
        let live = Arc::new(AtomicUsize::new(0));
        let n = 64usize;
        let futs: Vec<_> = (0..n)
            .map(|_| {
                let (peak, live) = (peak.clone(), live.clone());
                async move {
                    let cur = live.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(cur, Ordering::SeqCst);
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                    live.fetch_sub(1, Ordering::SeqCst);
                }
            })
            .collect();
        let mut s = futures_util::stream::iter(futs).buffered(DOWNLOAD_CONCURRENCY);
        while s.next().await.is_some() {}
        assert!(
            peak.load(Ordering::SeqCst) <= DOWNLOAD_CONCURRENCY,
            "并发峰值 {} 超过上限 {}",
            peak.load(Ordering::SeqCst),
            DOWNLOAD_CONCURRENCY
        );
        assert!(peak.load(Ordering::SeqCst) > 1, "buffered 把并行度压成了 1");
    }

    // 定向验证：asset_index.url 为完整地址时不再双重前缀
    #[tokio::test]
    #[ignore]
    async fn asset_index_absolute_url_works() {
        let mc = std::env::temp_dir().join("mctl-e2e-assetindex");
        let _ = std::fs::remove_dir_all(&mc);
        std::fs::create_dir_all(&mc).unwrap();
        let index = version::AssetIndex {
            id: "17".into(),
            sha1: String::new(),
            url: "https://piston-meta.mojang.com/v1/packages/ae4f2779919ee343d3d98445f9a3ae1dc6542bc9/17.json".into(),
            size: 0,
        };
        let idx = ensure_asset_index(&mc, &index).await.unwrap();
        assert!(idx.is_file(), "资产索引未下载: {}", idx.display());
        let _ = std::fs::remove_dir_all(&mc);
    }
}

#[cfg(test)]
mod resume_tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// 只服务一次请求的极简 HTTP 端：可选是否支持 Range，返回它收到的 Range 头。
    async fn serve_once(
        body: &'static [u8],
        honor_range: bool,
    ) -> (String, tokio::task::JoinHandle<Option<String>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut req = Vec::new();
            let mut buf = [0u8; 1024];
            while !req.windows(4).any(|w| w == b"\r\n\r\n") {
                let n = sock.read(&mut buf).await.unwrap();
                if n == 0 {
                    break;
                }
                req.extend_from_slice(&buf[..n]);
            }
            let req = String::from_utf8_lossy(&req).to_string();
            let range = req
                .lines()
                .find(|l| l.to_ascii_lowercase().starts_with("range:"))
                .map(|l| l.split(':').nth(1).unwrap_or("").trim().to_string());
            let start: usize = range
                .as_deref()
                .and_then(|r| r.strip_prefix("bytes="))
                .and_then(|r| r.trim_end_matches('-').parse().ok())
                .unwrap_or(0);
            let (status, payload) = if honor_range && start > 0 && start < body.len() {
                ("206 Partial Content", &body[start..])
            } else {
                ("200 OK", body)
            };
            let head = format!(
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                payload.len()
            );
            sock.write_all(head.as_bytes()).await.unwrap();
            sock.write_all(payload).await.unwrap();
            sock.shutdown().await.unwrap();
            range
        });
        (format!("http://{addr}/pkg"), handle)
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("mctl-resume-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 服务端认 Range：从 .tmp 的断点接着下，最终文件是完整内容
    #[tokio::test]
    async fn resumes_from_partial_tmp_when_server_supports_range() {
        let dir = scratch("yes");
        let target = dir.join("pkg.zip");
        std::fs::write(dir.join("pkg.tmp"), b"hello ").unwrap();
        let (url, server) = serve_once(b"hello world", true).await;
        download_to_resumable(&reqwest::Client::new(), &url, &target, |_, _| {})
            .await
            .unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"hello world");
        assert_eq!(server.await.unwrap().as_deref(), Some("bytes=6-"));
        assert!(!dir.join("pkg.tmp").exists(), "落盘后不该留半截文件");
    }

    /// 服务端忽略 Range（回 200 全量）：必须清空重下，不能把旧半截拼在前面
    #[tokio::test]
    async fn restarts_when_server_ignores_range() {
        let dir = scratch("no");
        let target = dir.join("pkg.zip");
        std::fs::write(dir.join("pkg.tmp"), b"garbage-from-last-time").unwrap();
        let (url, _server) = serve_once(b"hello world", false).await;
        download_to_resumable(&reqwest::Client::new(), &url, &target, |_, _| {})
            .await
            .unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"hello world");
    }
}
