//! 插件系统：`~/.config/mctl/plugin/` 下的插件仓库管理（包管理器哲学）。
//!
//! 插件托管在 GitHub 等远端，本工具负责装/更/卸：
//! - `mctl plugin install <url[#tag]|路径>`：浅克隆（或复制）+ cargo 构建；
//! 重复 install 同名插件 = 更新。`url#v1.2.0` 钉住 tag/分支。
//! - `mctl plugin update [名称]`：从 origin 重克隆最新并重建。
//! - 来源即 origin url（读 .git/config），无需额外清单文件。
//!
//! ## 运行时钩子约定
//! 插件仓库根放 `provides/<能力名>` 空标记文件即声明能力，核心按需调用其主二进制：
//! - `auth-fallback`：`<bin> auth-fallback`，stdin 喂
//! `{"profile": <GameProfile>, "server_id": "<id>"}`，exit 0 = 认证成功。
//! （见 authlib：本地 Yggdrasil join 失败时触发）
//! - `versions`：资源大厅「插件」来源的数据源。核心只做通用渲染，插件适配核心：
//! * `<bin> versions` → stdout JSON `[{"id","name","installed","size_mb"}]`
//! * `<bin> install <id>` / `launch <id>` / `remove <id>`（进度 → stderr）
//!
//! 需要配置的插件（例如认证兜底要填服务地址）把配置写在**插件目录的 `env` 文件**里：
//! 每行 `KEY=VALUE`，`#` 开头是注释；核心调用插件时注入这些变量，
//! 进程环境里已有的同名变量优先（临时覆盖用 export 就够了）。
//!
//! ## 怎么上架一个插件（作者视角）
//!
//! 硬性要求只有一条：**得是一个能 `cargo build --release` 的 Rust 工程**
//! （core 无标准库/无 Python 运行时，别的语言写不了）。最小骨架：
//!
//! ```text
//! my-plugin/ # 目录名 → 安装后的目录名，也是 url 末段
//! ├── Cargo.toml # [package] name = "my-plugin"
//! │ # version / description 可选（缺失时回退 0.0.0 / DME 首行）
//! ├── provides/versions # 空标记文件 = 声明「我能列版本」
//! └── src/main.rs # 二进制；名字最好 = 目录名
//! ```
//!
//! 上架 = 推成一个 git 仓库，然后：
//!
//! ```sh
//! mctl plugin install https://github.com/<你>/my-plugin.git # 装最新
//! mctl plugin install https://github.com/<你>/my-plugin.git#v1.2.0 # 钉版本
//! mctl plugin list # 确认被识别
//! ```
//!
//! 安装流程：`repo_name(url)` 取 url 末段作目录名 → `git clone --depth 1`
//! → 读 `deps/pacman.txt`（可选，Arch 系统依赖）→ `cargo build --release`
//! → `inspect` 读元数据 + 定位二进制。
//!
//! ### 会被怎样识别
//!
//! | 项 | 来源 | 缺失时 |
//! |---|---|---|
//! | 展示名 | 单 crate=`[package].name`；workspace=目录名 | — |
//! | 版本 | `[package].version` | `"0.0.0"` |
//! | 描述 | `[package].description` | DME 首行 | `（无描述）` |
//! | 主二进制 | 见下 | `（无二进制）`，插件不会被调用 |
//! | 来源 | `.git/config` 的 origin url | 显示「本地」 |
//!
//! ### 主二进制怎么定位（`find_binary`）
//!
//! 1. `cargo metadata` 报出的 bin target 里，**与展示名同名**的（`<name>`/`-cli`/`_cli`）
//! 2. 否则 cargo 报的**其余 bin，按 manifest 声明顺序**（= 作者意图）
//! 3. 都没有 → 扫 `target/release` 兜底
//!
//! 所以 **workspace 有多个 bin 时，把想要的那个放在 manifest 成员列表最前面**
//! （或让它与目录名同名）。别指望字母序。
//!
//! ### 注意
//!
//! - 目录名 ≠ 展示名是常态（仓库叫 `foo`、包名叫 `foo-cli`）。
//! 更新/卸载一律按**目录名**寻址，核心内部已处理；`plugin update <名称>`
//! 传目录名。
//! - `deps/pacman.txt` 一行一个包，仅 Arch 生效；非 Arch 直接跳过。

use anyhow::{anyhow, bail, Context, Result};
use std::path::{Path, PathBuf};

/// 插件根目录名（挂在配置根下）
pub const PLUGIN_DIR: &str = "plugin";

/// 插件仓库元数据（取自 Cargo.toml [package]，缺省回退 DME 首行）
#[derive(Debug, Clone)]
pub struct PluginInfo {
    /// 展示名：单 crate 插件取 [package].name，workspace 插件取目录名
    pub name: String,
    pub version: String,
    pub description: String,
    /// 插件仓库目录（= root 下的真实目录名）
    pub path: PathBuf,
    /// 构建产物可执行文件（target/release 下的主二进制）
    pub bin: Option<PathBuf>,
}

impl PluginInfo {
    /// 目录名——寻址一律用它，不要用 `name`。
    /// /// 两者可以不同：仓库叫 `be-converter` 但 `[package] name = "be-converter-cli"` 时，
    /// `root.join(&info.name)` 指向一个不存在的目录，update/remove 直接报「未安装」。
    pub fn dir_name(&self) -> String {
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| self.name.clone())
    }
}

/// 扫描插件目录，返回按名称排序的插件列表
pub fn scan_plugins(root: &Path) -> Vec<PluginInfo> {
    let Ok(rd) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in rd.flatten() {
        let path = entry.path();
        if !path.is_dir()
            || path
                .file_name()
                .is_none_or(|n| n.to_string_lossy().starts_with('.'))
        {
            continue;
        }
        let Some(info) = inspect(&path) else {
            continue;
        };
        out.push(info);
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// 解析单个插件仓库目录（读 Cargo.toml 与构建产物）
pub fn inspect(path: &Path) -> Option<PluginInfo> {
    let dir_name = path.file_name()?.to_string_lossy().to_string();
    let cargo = path.join("Cargo.toml");
    let text = std::fs::read_to_string(&cargo).ok()?;
    let doc = toml::from_str::<toml::Value>(&text).ok()?;
    let (name, version, description) = if doc.get("package").is_some() {
        parse_cargo_manifest(&text)?
    } else {
        // workspace：找第一个含 [package] 的成员（版本/描述回退到 [workspace.package]）
        let ws_ver = doc
            .get("workspace")
            .and_then(|w| w.get("package"))
            .and_then(|p| p.get("version"))
            .and_then(|v| v.as_str())
            .map(String::from);
        let mut picked = None;
        if let Some(members) = doc
            .get("workspace")
            .and_then(|w| w.get("members"))
            .and_then(|m| m.as_array())
        {
            for member in members {
                let Some(m) = member.as_str() else { continue };
                if m.starts_with('.') {
                    continue;
                }
                let Ok(mtext) = std::fs::read_to_string(path.join(m).join("Cargo.toml")) else {
                    continue;
                };
                let Some(mut info) = parse_cargo_manifest(&mtext) else {
                    continue;
                };
                if info.1.is_none() {
                    info.1 = ws_ver.clone();
                }
                picked = Some(info);
                break;
            }
        }
        picked?
    };
    let display_name = if doc.get("package").is_some() {
        name
    } else {
        dir_name
    };
    let bin_name = display_name.clone();
    Some(PluginInfo {
        name: display_name,
        version: version.unwrap_or_else(|| "0.0.0".into()),
        description: description.unwrap_or_else(|| readme_hint(path)),
        path: path.to_path_buf(),
        bin: find_binary(path, &bin_name),
    })
}

/// 安装插件：git clone（或复制本地目录）到 root/<name> 并 cargo 构建。
/// url 支持 https:// / git@ / git:// / file:// 及本地路径；`#tag` 钉住 tag/分支。
/// 目标已存在时按包管理器语义执行更新（删除重装）。
pub fn install(root: &Path, url_or_path: &str) -> Result<PluginInfo> {
    std::fs::create_dir_all(root).with_context(|| format!("创建插件目录 {}", root.display()))?;
    let (src, tag) = split_tag(url_or_path);
    let name = repo_name(src)?;
    let target = root.join(&name);
    if target.exists() {
        std::fs::remove_dir_all(&target)
            .with_context(|| format!("清理旧版本 {}", target.display()))?;
    }
    // 拉取
    if is_remote(src) {
        let mut cmd = std::process::Command::new("git");
        cmd.args(["clone", "--depth", "1"]);
        if let Some(t) = tag {
            cmd.args(["--branch", t]);
        }
        let status = cmd
            .arg(src)
            .arg(&target)
            .status()
            .context("执行 git clone 失败（需要 git）")?;
        if !status.success() {
            let _ = std::fs::remove_dir_all(&target);
            bail!(
                "git clone 失败：{}{}",
                src,
                tag.map(|t| format!(" (#{t})")).unwrap_or_default()
            );
        }
    } else {
        copy_dir(Path::new(src), &target).with_context(|| format!("复制 {} 失败", src))?;
    }
    // 系统依赖声明（deps/pacman.txt，一行一个包）：缺失则列出并交互安装
    let deps_file = target.join("deps").join("pacman.txt");
    if deps_file.is_file() {
        let pkgs: Vec<String> = std::fs::read_to_string(&deps_file)?
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(String::from)
            .collect();
        if !pkgs.is_empty() {
            let refs: Vec<&str> = pkgs.iter().map(String::as_str).collect();
            let missing = crate::deps::missing_pacman(&refs);
            if !missing.is_empty() {
                println!(
                    "插件 {} 声明系统依赖: {}（缺失 {}）",
                    name,
                    pkgs.join(" "),
                    missing.join(" ")
                );
                crate::deps::install_pacman_interactive(
                    &missing.iter().map(String::as_str).collect::<Vec<_>>(),
                )?;
            }
        }
    }
    // cargo 构建（发布模式）
    let status = std::process::Command::new("cargo")
        .args(["build", "--release"])
        .current_dir(&target)
        .status()
        .context("执行 cargo build 失败（需要 cargo）")?;
    if !status.success() {
        let _ = std::fs::remove_dir_all(&target);
        bail!("cargo build 失败：{}（插件仓库可能不完整）", name);
    }
    inspect(&target).ok_or_else(|| anyhow!("插件 {} 缺少有效的 Cargo.toml", name))
}

/// 更新单个插件：记录 origin url 后删除重克隆并重建。
/// ponytail: 重克隆代替增量 pull——插件仓库都很小，换来对 force-push、
/// 换默认分支、tag 移动的零边界情况。仓库大了再改 fetch/reset。
pub fn update(root: &Path, name: &str) -> Result<PluginInfo> {
    let dir = root.join(name);
    if !dir.is_dir() {
        bail!("插件 {} 未安装", name);
    }
    let Some(url) = origin_url(&dir) else {
        bail!(
            "{} 非 git 安装（无 origin），无法自动更新；可 plugin remove 后用远端地址重装",
            name
        );
    };
    install(root, &url)
}

/// 全量更新所有插件，返回 (名称, 结果) 列表
pub fn update_all(root: &Path) -> Vec<(String, Result<PluginInfo>)> {
    scan_plugins(root)
        .iter()
        // 用目录名寻址，不是展示名
        .map(|p| {
            let dir = p.dir_name();
            let res = update(root, &dir);
            (dir, res)
        })
        .collect()
}

/// 读插件目录 `.git/config` 中 origin 的 url（update 的来源依据）
pub fn origin_url(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path.join(".git").join("config")).ok()?;
    let mut in_origin = false;
    for line in text.lines() {
        let l = line.trim();
        if l.starts_with('[') {
            in_origin = l.replace(' ', "").starts_with("[remote\"origin\"");
            continue;
        }
        if in_origin {
            if let Some((k, v)) = l.split_once('=') {
                if k.trim() == "url" {
                    return Some(v.trim().to_string());
                }
            }
        }
    }
    None
}

/// 是否远程仓库地址
fn is_remote(s: &str) -> bool {
    s.starts_with("https://")
        || s.starts_with("http://")
        || s.starts_with("git@")
        || s.starts_with("git://")
        || s.starts_with("file://")
}

/// 拆出 `#tag` 版本钉子：返回 (源, Option<tag>)
fn split_tag(s: &str) -> (&str, Option<&str>) {
    match s.split_once('#') {
        Some((u, t)) if !t.is_empty() => (u, Some(t)),
        _ => (s, None),
    }
}

/// 卸载插件
pub fn remove(root: &Path, name: &str) -> Result<()> {
    let target = root.join(name);
    if !target.exists() {
        bail!("插件 {} 未安装", name);
    }
    std::fs::remove_dir_all(&target).with_context(|| format!("删除 {} 失败", target.display()))?;
    Ok(())
}

/// 从 url/路径提取仓库名（去 .git 后缀）
/// 必须校验结果是**单个安全路径分量**：调用方会 `root.join(&name)` 再
/// `remove_dir_all`，而 name 来自用户输入。`plugin install /some/path/..`
/// 会让 name 变成 ".."，于是 remove_dir_all 删掉整个插件根的父目录
/// （连带 credentials.json / wm.toml / .minecraft）。
fn repo_name(s: &str) -> Result<String> {
    let trimmed = s.trim_end_matches('/');
    let last = trimmed.rsplit(['/', ':']).next().unwrap_or(trimmed);
    let name = last.strip_suffix(".git").unwrap_or(last);
    if name.is_empty() {
        bail!("无法从 {} 推断插件名", s);
    }
    if name == "." || name == ".." || name.contains(['/', '\\']) || name.contains('\0') {
        bail!("插件名 {:?} 不是合法的目录名（来自 {}）", name, s);
    }
    Ok(name.to_string())
}

/// 解析 Cargo.toml：workspace 时遍历成员目录找第一个 [package]
fn parse_cargo_manifest(text: &str) -> Option<(String, Option<String>, Option<String>)> {
    let doc = toml::from_str::<toml::Value>(text).ok()?;
    if let Some(pkg) = doc.get("package") {
        let name = pkg.get("name")?.as_str()?.to_string();
        let version = pkg
            .get("version")
            .and_then(|v| v.as_str())
            .map(String::from);
        let description = pkg
            .get("description")
            .and_then(|d| d.as_str())
            .map(String::from);
        return Some((name, version, description));
    }
    None
}

/// DME 首行作为描述回退
fn readme_hint(path: &Path) -> String {
    for f in ["README.md", "readme.md"] {
        if let Ok(text) = std::fs::read_to_string(path.join(f)) {
            for line in text.lines() {
                let l = line.trim().trim_start_matches(['#', ' ', '*']);
                if !l.is_empty() {
                    return l.chars().take(60).collect();
                }
            }
        }
    }
    String::new()
}

/// 问 cargo 要真实的 bin target 名（权威来源，不猜）。
/// 不问 cargo 就只能按目录名/字母序猜，而 workspace 插件的 bin 名通常和目录名
/// 无关——曾因此静默选中错误的二进制（插件看着装好了，跑起来是别的程序）。
/// cargo metadata 失败（非 cargo 工程 / cargo 不在 PATH）时返回空列表，
/// 交由调用方退回目录扫描。
fn cargo_bin_names(path: &Path) -> Vec<String> {
    let Ok(out) = std::process::Command::new("cargo")
        .arg("metadata")
        .arg("--no-deps")
        .arg("--format-version")
        .arg("1")
        .current_dir(path)
        .output()
    else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(&out.stdout) else {
        return Vec::new();
    };
    v.get("packages")
        .and_then(|p| p.as_array())
        .map(|pkgs| {
            pkgs.iter()
                .filter_map(|pkg| pkg.get("targets").and_then(|t| t.as_array()))
                .flatten()
                .filter(|t| {
                    t.get("kind")
                        .and_then(|k| k.as_array())
                        .is_some_and(|kinds| kinds.iter().any(|k| k.as_str() == Some("bin")))
                })
                .filter_map(|t| t.get("name").and_then(|n| n.as_str()))
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

/// 在 target/release 下找主二进制。
/// 优先级：① cargo 声明的 bin 中与 `name` 同名者 → ② cargo 声明的其余 bin
/// （manifest 顺序 = 作者意图）→ ③ 目录扫描兜底（非 cargo 工程）。
fn find_binary(path: &Path, name: &str) -> Option<PathBuf> {
    let rel = path.join("target").join("release");
    let preferred = [
        name.to_string(),
        format!("{name}-cli"),
        format!("{name}_cli"),
    ];

    // ① cargo 权威名单：先找与展示名同名的
    let bins = cargo_bin_names(path);
    for want in &preferred {
        if bins.iter().any(|b| b == want) {
            let p = rel.join(want);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    // ② 剩下的 bin 按 cargo manifest 顺序（作者的声明顺序），已构建出来的优先
    for b in &bins {
        let p = rel.join(b);
        if p.is_file() {
            return Some(p);
        }
    }

    // ③ 兜底：扫 target/release
    let rd = std::fs::read_dir(&rel).ok()?;
    let mut others: Vec<String> = Vec::new();
    for e in rd.flatten() {
        let n = e.file_name().to_string_lossy().to_string();
        if n.starts_with('.') {
            continue;
        }
        if e.path().is_file() && n.ends_with(".d") {
            continue;
        }
        if e.path().is_file()
            && !n.ends_with(".rlib")
            && !n.ends_with(".rmeta")
            && !n.ends_with(".so")
            && !n.ends_with(".d")
            // cargo 的宿主工具，不是插件主二进制
            && n != "build-script-build"
        {
            others.push(n);
        }
    }
    // read_dir 顺序任意：排序保证可复现
    others.sort();
    for c in preferred.iter().chain(others.iter()) {
        let p = rel.join(c);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

/// 递归复制目录
fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    if from
        .file_name()
        .is_none_or(|n| n.to_string_lossy() == ".git")
    {
        return Ok(());
    }
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let src = entry.path();
        let dst = to.join(entry.file_name());
        if src.is_dir() {
            copy_dir(&src, &dst)?;
        } else {
            std::fs::copy(&src, &dst)?;
        }
    }
    Ok(())
}

/// 插件环境变量：读插件目录下的 `env` 文件（`KEY=VALUE` 每行一条，`#` 注释）。
/// 进程环境里已有的同名变量优先——env 文件是「留一份默认配置」，
/// 临时覆盖照样可以直接 export。
pub fn env_file(dir: &Path) -> Vec<(String, String)> {
    let Ok(text) = std::fs::read_to_string(dir.join("env")) else {
        return Vec::new();
    };
    parse_env(&text)
        .into_iter()
        .filter(|(k, _)| std::env::var_os(k).is_none())
        .collect()
}

/// `env` 文件文本 → 键值对（抽出来单测：CRLF、空行、注释、值里带 = 都要稳）
fn parse_env(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let k = k.trim();
        if k.is_empty() {
            continue;
        }
        out.push((k.to_string(), v.trim().to_string()));
    }
    out
}

/// 按可执行文件路径找它所属插件的环境（界面侧只拿得到 bin 路径）
pub fn env_for_bin(root: &Path, bin: &Path) -> Vec<(String, String)> {
    scan_plugins(root)
        .into_iter()
        .find(|p| p.bin.as_deref() == Some(bin))
        .map(|p| env_file(&p.path))
        .unwrap_or_default()
}

/// 找到声明了 `provides/<ability>` 且有主二进制的插件。
/// 寻址一律用 `p.path`（真实目录），不用 `p.name`（展示名）——两者可以不同。
pub fn find_by_ability(root: &Path, ability: &str) -> Option<PluginInfo> {
    scan_plugins(root)
        .into_iter()
        .find(|p| p.path.join("provides").join(ability).is_file() && p.bin.is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("mctl_plugin_test_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn repo_name_rejects_path_traversal() {
        // install 会 root.join(name) + remove_dir_all，name=".." 会删掉插件根的父目录
        for bad in [
            "/home/x/plugins/..",
            "..",
            "git@github.com:u/..",
            "a/../..",
            "",
        ] {
            assert!(
                repo_name(bad).is_err(),
                "{bad:?} 必须被拒绝，否则 remove_dir_all 会逃出插件根"
            );
        }
    }

    /// workspace 多 bin 时必须选作者声明的那个，不能按字母序瞎猜。
    /// /// 回归：曾按 target/release 字母序取首个，workspace 成员
    /// `crates/tool`(bin=zzz-tool) + `crates/helper`(bin=aaa-helper) 时
    /// 静默选中 aaa-helper——插件显示已安装，跑起来却是别的程序。
    #[test]
    fn find_binary_asks_cargo_not_alphabet() {
        let d = tmpdir("findbin");
        let root = d.join("ws-plug");
        std::fs::create_dir_all(root.join("crates/tool/src")).unwrap();
        std::fs::create_dir_all(root.join("crates/helper/src")).unwrap();
        std::fs::create_dir_all(root.join("target/release")).unwrap();
        // 成员顺序：tool 在前 → 它的 bin 就是意图
        std::fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/tool\", \"crates/helper\"]\nresolver = \"2\"\n",
        )
        .unwrap();
        for (dir, pkg) in [("tool", "zzz-tool"), ("helper", "aaa-helper")] {
            std::fs::write(
                root.join(format!("crates/{dir}/Cargo.toml")),
                format!("[package]\nname = \"{pkg}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n"),
            )
            .unwrap();
            std::fs::write(
                root.join(format!("crates/{dir}/src/main.rs")),
                "fn main(){}\n",
            )
            .unwrap();
        }
        // 只放构建产物，不真跑 cargo（无 lock 的临时工程 cargo metadata 也能读）
        for b in ["aaa-helper", "zzz-tool"] {
            std::fs::write(root.join(format!("target/release/{b}")), b"#!/bin/sh\n").unwrap();
        }
        let bin = find_binary(&root, "ws-plug").expect("应找到主二进制");
        assert_eq!(
            bin.file_name().unwrap().to_string_lossy(),
            "zzz-tool",
            "应选 manifest 里第一个成员的 bin，而不是字母序最前的 aaa-helper"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// 插件 env 文件：注释/空行/CRLF/值里有 = 都要稳，非法行跳过而不是炸
    #[test]
    fn env_file_parsing_is_forgiving() {
        let got = parse_env(
            "# 注释\n\nMCTL_AUTH_ENDPOINT=https://sign.example/\r\nBAD_LINE\n=novalue\n  \nAPPID=abc=def\n",
        );
        assert_eq!(
            got,
            vec![
                ("MCTL_AUTH_ENDPOINT".to_string(), "https://sign.example/".to_string()),
                ("APPID".to_string(), "abc=def".to_string()),
            ],
            "注释/空行/缺等号/空键都跳过，值里的 = 保留"
        );
    }

    /// 插件目录的 env 注入：能按 bin 反查到插件目录；进程已有同名变量时文件不覆盖
    #[test]
    fn env_for_bin_reads_plugin_env_file() {
        let d = tmpdir("envfile");
        let root = d.join("plugin-root");
        let plug = root.join("authproxy");
        std::fs::create_dir_all(plug.join("provides")).unwrap();
        std::fs::create_dir_all(plug.join("target/release")).unwrap();
        std::fs::write(plug.join("Cargo.toml"), "[package]\nname = \"authproxy\"\nversion = \"0.1.0\"\n").unwrap();
        std::fs::write(plug.join("provides/auth-fallback"), b"").unwrap();
        let bin = plug.join("target/release/authproxy");
        std::fs::write(&bin, b"#!/bin/sh\n").unwrap();
        std::fs::write(
            plug.join("env"),
            "# 认证服务\nMCTL_AUTH_ENDPOINT=https://sign.example\n",
        )
        .unwrap();

        let env = env_for_bin(&root, &bin);
        assert_eq!(
            env,
            vec![(
                "MCTL_AUTH_ENDPOINT".to_string(),
                "https://sign.example".to_string()
            )],
            "按 bin 找到插件目录并读出 env"
        );

        // 临时覆盖：进程环境里已有同名变量时，以环境变量为准
        std::env::set_var("MCTL_AUTH_ENDPOINT_TEST_ONLY", "from-shell");
        let env2 = parse_env("MCTL_AUTH_ENDPOINT_TEST_ONLY=from-file\n");
        assert_eq!(env2[0].1, "from-file", "解析本身照读");
        std::fs::write(
            plug.join("env"),
            "MCTL_AUTH_ENDPOINT_TEST_ONLY=from-file\n",
        )
        .unwrap();
        let env2 = env_file(&plug);
        assert!(
            env2.is_empty(),
            "进程已有同名变量时 env 文件不生效（export 可临时覆盖）"
        );
        std::env::remove_var("MCTL_AUTH_ENDPOINT_TEST_ONLY");

        // 没有 env 文件的插件不报错，只是没有变量
        std::fs::remove_file(plug.join("env")).unwrap();
        assert!(env_for_bin(&root, &bin).is_empty());
        // 找不到对应插件也给空表（不 panic）
        assert!(env_for_bin(&root, Path::new("/nonexistent/bin")).is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn dir_name_is_the_addressable_component_not_display_name() {
        // 仓库目录 be-converter，但 [package] name = "conv-cli"：寻址必须用目录名
        let d = tmpdir("dirname");
        let dir = d.join("be-converter");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("Cargo.toml"),
            "[package]\nname = \"conv-cli\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        let info = inspect(&dir).unwrap();
        assert_eq!(info.name, "conv-cli", "展示名取 [package].name");
        assert_eq!(info.dir_name(), "be-converter", "寻址取目录名");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn repo_name_extracts_from_urls_and_paths() {
        assert_eq!(
            repo_name("https://github.com/user/be-converter.git").unwrap(),
            "be-converter"
        );
        assert_eq!(
            repo_name("git@github.com:user/be-converter.git").unwrap(),
            "be-converter"
        );
        assert_eq!(repo_name("/home/x/plugins/hello").unwrap(), "hello");
        assert_eq!(repo_name("https://github.com/user/x/").unwrap(), "x");
        assert!(repo_name("/").is_err());
    }

    #[test]
    fn inspect_single_crate_plugin() {
        let dir = tmpdir("single");
        std::fs::write(
            dir.join("Cargo.toml"),
            "[package]\nname = \"demo\"\nversion = \"1.2.3\"\ndescription = \"测试插件\"\n",
        )
        .unwrap();
        std::fs::write(dir.join("README.md"), "# Demo\n一个演示插件\n").unwrap();
        let info = inspect(&dir).unwrap();
        assert_eq!(info.name, "demo");
        assert_eq!(info.version, "1.2.3");
        assert_eq!(info.description, "测试插件");
    }

    #[test]
    fn inspect_workspace_plugin() {
        let root = tmpdir("ws");
        let dir = root.join("ws");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("Cargo.toml"),
            "[workspace]\nmembers = [\"demo-cli\"]\n",
        )
        .unwrap();
        std::fs::create_dir_all(dir.join("demo-cli/src")).unwrap();
        std::fs::write(
            dir.join("demo-cli/Cargo.toml"),
            "[package]\nname = \"demo-cli\"\nversion = \"0.4.0\"\n",
        )
        .unwrap();
        std::fs::write(dir.join("demo-cli/src/main.rs"), "fn main() {}\n").unwrap();
        std::fs::create_dir_all(dir.join("target/release")).unwrap();
        std::fs::write(dir.join("target/release/demo-cli"), b"bin").unwrap();
        let info = inspect(&dir).unwrap();
        assert_eq!(info.name, "ws", "workspace 插件名 = 目录名");
        assert_eq!(info.version, "0.4.0", "版本取自第一个成员");
        assert_eq!(
            info.bin.unwrap().file_name().unwrap().to_str().unwrap(),
            "demo-cli"
        );
    }

    #[test]
    fn split_tag_parses_pinned_source() {
        assert_eq!(
            split_tag("https://github.com/user/be-converter.git"),
            ("https://github.com/user/be-converter.git", None)
        );
        assert_eq!(
            split_tag("https://github.com/user/p#v1.2.0"),
            ("https://github.com/user/p", Some("v1.2.0"))
        );
        assert_eq!(
            split_tag("https://github.com/user/p#"),
            ("https://github.com/user/p#", None)
        );
        assert_eq!(split_tag("/local/path"), ("/local/path", None));
    }

    #[test]
    fn origin_url_reads_git_config() {
        let d = tmpdir("origin");
        std::fs::create_dir_all(d.join(".git")).unwrap();
        std::fs::write(
            d.join(".git/config"),
            "[core]\n\tbare = false\n[remote \"origin\"]\n\turl = https://github.com/u/demo.git\n\tfetch = +refs/heads/*:refs/remotes/origin/*\n",
        )
        .unwrap();
        assert_eq!(origin_url(&d).unwrap(), "https://github.com/u/demo.git");
        assert_eq!(origin_url(&tmpdir("no_git")), None);
    }

    #[test]
    fn install_from_local_path_builds_and_scans() {
        let src_root = tmpdir("src_repo");
        let src = src_root.join("localplug");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            src.join("Cargo.toml"),
            "[package]\nname = \"localplug\"\nversion = \"0.1.0\"\ndescription = \"本地插件\"\n",
        )
        .unwrap();
        std::fs::create_dir_all(src.join("src")).unwrap();
        std::fs::write(src.join("src/main.rs"), "fn main() { println!(\"hi\"); }\n").unwrap();
        std::fs::write(src.join("README.md"), "# localplug\n本地安装测试\n").unwrap();
        let root = tmpdir("root");
        let info = install(&root, src.to_str().unwrap()).unwrap();
        assert_eq!(info.name, "localplug");
        assert_eq!(info.version, "0.1.0");
        assert_eq!(info.description, "本地插件");
        let list = scan_plugins(&root);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "localplug");
        // 重复安装 = 更新（包管理器语义）
        install(&root, src.to_str().unwrap()).unwrap();
        assert_eq!(scan_plugins(&root).len(), 1);
        // 卸载后可再装
        remove(&root, "localplug").unwrap();
        assert!(scan_plugins(&root).is_empty());
    }

    /// 建一个带提交的本地 git 仓库，作为插件"远端"（file:// 克隆走真 git 路径）。
    /// 目录名 = 仓库名（插件名取自 URL 末段），固定为 gitplug。
    fn git_remote_repo(tag: &str) -> PathBuf {
        use std::process::Command;
        let dir = tmpdir(tag).join("gitplug");
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join("Cargo.toml"),
            "[package]\nname = \"gitplug\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        std::fs::write(dir.join("src/main.rs"), "fn main() {}\n").unwrap();
        let env = [
            ("GIT_AUTHOR_NAME", "t"),
            ("GIT_AUTHOR_EMAIL", "t@t"),
            ("GIT_COMMITTER_NAME", "t"),
            ("GIT_COMMITTER_EMAIL", "t@t"),
        ];
        let git = |args: &[&str], cwd: &Path| {
            let st = Command::new("git")
                .args(args)
                .current_dir(cwd)
                .envs(env)
                .status()
                .unwrap();
            assert!(st.success(), "git {args:?} 失败");
        };
        git(&["init", "-q", "-b", "main"], &dir);
        git(&["add", "."], &dir);
        git(&["commit", "-qm", "init"], &dir);
        dir
    }

    #[test]
    fn install_from_file_url_then_update_pulls_new_version() {
        // 需要 git 可执行文件
        if std::process::Command::new("git")
            .arg("--version")
            .output()
            .is_err()
        {
            return;
        }
        let remote = git_remote_repo("remote");
        let root = tmpdir("update_root");
        let url = format!("file://{}", remote.display());
        let info = install(&root, &url).unwrap();
        assert_eq!(info.version, "0.1.0");
        // 远端发布新版本
        std::fs::write(
            remote.join("Cargo.toml"),
            "[package]\nname = \"gitplug\"\nversion = \"0.2.0\"\n",
        )
        .unwrap();
        let env = [
            ("GIT_AUTHOR_NAME", "t"),
            ("GIT_AUTHOR_EMAIL", "t@t"),
            ("GIT_COMMITTER_NAME", "t"),
            ("GIT_COMMITTER_EMAIL", "t@t"),
        ];
        for args in [&["add", "."][..], &["commit", "-qm", "v2"][..]] {
            assert!(std::process::Command::new("git")
                .args(args)
                .current_dir(&remote)
                .envs(env)
                .status()
                .unwrap()
                .success());
        }
        // update 拉到新版本
        let updated = update(&root, "gitplug").unwrap();
        assert_eq!(updated.version, "0.2.0");
        // 本地路径安装的（无 .git）无法 update
        let plain = tmpdir("plain_src");
        std::fs::create_dir_all(plain.join("plain/src")).unwrap();
        std::fs::write(
            plain.join("plain/Cargo.toml"),
            "[package]\nname = \"plain\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        std::fs::write(plain.join("plain/src/main.rs"), "fn main() {}\n").unwrap();
        install(&root, plain.join("plain").to_str().unwrap()).unwrap();
        assert!(update(&root, "plain").is_err());
    }
}
