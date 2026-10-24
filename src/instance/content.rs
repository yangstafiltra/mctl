//! 实例内容页：已安装的模组/光影/资源包。
//!
//! 参考 alloy/rmcl 的 `instance/content/` 组织方式，但只读扫描 + 启停，
//! 不做安装（安装走资源下载页）。

use std::path::{Path, PathBuf};

/// 一个内容条目（模组/光影/资源包共用）
#[derive(Debug, Clone)]
pub struct ContentEntry {
    /// jar/zip 文件名去掉扩展名
    pub file_stem: String,
    /// 展示名（fabric.mod.json 的 name，或文件名）
    pub name: String,
    pub description: String,
    /// 是否启用（文件名不以 `.disabled` 结尾）
    pub enabled: bool,
    /// 版本号（从 mod json 解析，没有就空）
    pub version: String,
    pub path: PathBuf,
}

/// 内容类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentKind {
    Mods,
    Shaders,
    ResourcePacks,
}

impl ContentKind {
    pub fn dir(self) -> &'static str {
        match self {
            ContentKind::Mods => "mods",
            ContentKind::Shaders => "shaderpacks",
            ContentKind::ResourcePacks => "resourcepacks",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ContentKind::Mods => "模组",
            ContentKind::Shaders => "光影",
            ContentKind::ResourcePacks => "资源包",
        }
    }
}

/// 实例的 mods/shaderpacks/resourcepacks 目录
/// ponytail: 统一放 `inst.dir/<kind>`。记忆里记着「模组暂装到 mc_dir/mods，
/// 实例隔离待做」——这里走 inst.dir 才是每实例独立的正路。
pub fn content_dir(inst_dir: &Path, kind: ContentKind) -> PathBuf {
    inst_dir.join(kind.dir())
}

/// 扫描某个内容目录。文件按名称排序（稳定，重扫不会跳动）。
pub fn scan(inst_dir: &Path, kind: ContentKind) -> Vec<ContentEntry> {
    let dir = content_dir(inst_dir, kind);
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out: Vec<ContentEntry> = Vec::new();
    for e in rd.flatten() {
        let p = e.path();
        if !p.is_file() {
            continue;
        }
        let fname = e.file_name().to_string_lossy().into_owned();
        if !is_content_file(&fname) {
            continue;
        }
        let enabled = !fname.ends_with(".disabled");
        let file_stem = base_stem(&fname);

        // 只模组读 fabric.mod.json（forge 是 mods.toml，格式不同，先不解析）
        let (name, description, version) = if kind == ContentKind::Mods && enabled {
            read_mod_json(&p)
        } else {
            (file_stem.clone(), String::new(), String::new())
        };
        out.push(ContentEntry {
            name: if name.is_empty() {
                file_stem.clone()
            } else {
                name
            },
            description,
            version,
            enabled,
            file_stem,
            path: p,
        });
    }
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    out
}

/// 是不是内容文件（.jar/.zip，可带 .disabled 后缀）
fn is_content_file(name: &str) -> bool {
    // .disabled 要先剥掉再判扩展名（foo.jar.disabled 的真实扩展名是 disabled）
    let stem = name.trim_end_matches(".disabled");
    stem.ends_with(".jar") || stem.ends_with(".zip")
}

/// 内容文件的基础名：去掉 .jar/.zip 和可选的 .disabled
/// `sodium.jar` → `sodium`；`iris.jar.disabled` → `iris`
fn base_stem(name: &str) -> String {
    name.trim_end_matches(".disabled")
        .trim_end_matches(".jar")
        .trim_end_matches(".zip")
        .to_string()
}

#[derive(serde::Deserialize, Default)]
struct ModJson {
    #[serde(default)]
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    version: String,
}

fn read_mod_json(jar: &Path) -> (String, String, String) {
    let Ok(f) = std::fs::File::open(jar) else {
        return (String::new(), String::new(), String::new());
    };
    let mut zip = match zip::ZipArchive::new(f) {
        Ok(z) => z,
        Err(_) => return (String::new(), String::new(), String::new()),
    };
    for candidate in ["fabric.mod.json", "quilt.mod.json", "META-INF/mods.toml"] {
        let Ok(mut e) = zip.by_name(candidate) else {
            continue;
        };
        // META-INF/mods.toml 不是 json，serde 会失败落到下一轮
        if candidate.ends_with(".toml") {
            continue;
        }
        use std::io::Read;
        let mut s = String::new();
        if e.read_to_string(&mut s).is_err() {
            continue;
        }
        if let Ok(j) = serde_json::from_str::<ModJson>(&s) {
            return (j.name, j.description, j.version);
        }
    }
    (String::new(), String::new(), String::new())
}

/// 切换启用/禁用：重命名为 `.jar` ↔ `.jar.disabled`
pub fn toggle(inst_dir: &Path, kind: ContentKind, file_stem: &str) -> std::io::Result<()> {
    let dir = content_dir(inst_dir, kind);
    for name in [
        format!("{file_stem}.jar"),
        format!("{file_stem}.zip"),
        format!("{file_stem}.jar.disabled"),
        format!("{file_stem}.zip.disabled"),
    ] {
        let p = dir.join(&name);
        if !p.is_file() {
            continue;
        }
        return if name.ends_with(".disabled") {
            let on = name.trim_end_matches(".disabled").to_string();
            std::fs::rename(&p, dir.join(on))
        } else {
            std::fs::rename(&p, dir.join(format!("{name}.disabled")))
        };
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        format!("找不到 {file_stem}"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 每个测试一个独立目录 —— 固定路径会被并行测试互相污染
    /// （storage.rs 的 tmp_config_dir 注释记过这个坑）
    fn tmp(tag: &str) -> PathBuf {
        crate::storage::tmp_config_dir(&format!("content-{tag}"))
    }

    /// 只认 jar/zip；别把 .txt/.png 当成模组
    #[test]
    fn only_jar_and_zip_are_content() {
        let d = tmp("jarzip");
        let mods = content_dir(&d, ContentKind::Mods);
        std::fs::create_dir_all(&mods).unwrap();
        for f in ["a.jar", "b.zip", "readme.txt", "icon.png", "c.jar.disabled"] {
            std::fs::write(mods.join(f), b"x").unwrap();
        }
        let got = scan(&d, ContentKind::Mods);
        let names: Vec<&str> = got.iter().map(|g| g.file_stem.as_str()).collect();
        assert!(names.contains(&"a"), "{names:?}");
        assert!(names.contains(&"b"), "{names:?}");
        assert!(names.contains(&"c"), "{names:?}");
        assert!(!names.contains(&"readme"), "{names:?}");
        assert_eq!(got.len(), 3, "{names:?}");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// .disabled 后缀的模组 enabled=false，但 file_stem 要去掉后缀
    #[test]
    fn disabled_suffix_strips_from_stem() {
        let d = tmp("jarzip");
        let mods = content_dir(&d, ContentKind::Mods);
        std::fs::create_dir_all(&mods).unwrap();
        std::fs::write(mods.join("sodium.jar"), b"x").unwrap();
        std::fs::write(mods.join("iris.jar.disabled"), b"x").unwrap();
        let got = scan(&d, ContentKind::Mods);
        let iris = got
            .iter()
            .find(|g| g.file_stem == "iris")
            .expect("iris 应存在");
        assert!(!iris.enabled, "iris 应是禁用状态");
        let sodium = got.iter().find(|g| g.file_stem == "sodium").unwrap();
        assert!(sodium.enabled, "sodium 应是启用状态");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// 切换必须真的改名，再切回来要能还原
    #[test]
    fn toggle_renames_both_ways() {
        let d = tmp("jarzip");
        let mods = content_dir(&d, ContentKind::Mods);
        std::fs::create_dir_all(&mods).unwrap();
        std::fs::write(mods.join("sodium.jar"), b"x").unwrap();

        toggle(&d, ContentKind::Mods, "sodium").unwrap();
        assert!(mods.join("sodium.jar.disabled").is_file(), "应被禁用");

        toggle(&d, ContentKind::Mods, "sodium").unwrap();
        assert!(mods.join("sodium.jar").is_file(), "应被重新启用");

        assert!(toggle(&d, ContentKind::Mods, "nope").is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// 三种内容各走各的目录，不能互相串
    #[test]
    fn kinds_use_separate_dirs() {
        let d = tmp("dirs");
        let dirs: Vec<PathBuf> = [
            ContentKind::Mods,
            ContentKind::Shaders,
            ContentKind::ResourcePacks,
        ]
        .iter()
        .map(|k| content_dir(&d, *k))
        .collect();
        for (i, a) in dirs.iter().enumerate() {
            for b in dirs.iter().skip(i + 1) {
                assert_ne!(a, b, "两种内容目录撞了: {a:?} vs {b:?}");
            }
        }
        assert_eq!(dirs[0].file_name().unwrap(), "mods");
        assert_eq!(dirs[1].file_name().unwrap(), "shaderpacks");
        let _ = std::fs::remove_dir_all(&d);
    }
}
