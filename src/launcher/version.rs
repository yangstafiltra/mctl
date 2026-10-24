use serde::Deserialize;

/// 版本清单（.minecraft/versions/<id>/<id>.json）
/// 仅解析启动所需字段，未知字段忽略。
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct VersionProfile {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    #[serde(rename = "type")]
    pub version_type: String,
    #[serde(default)]
    pub main_class: String,
    #[serde(default)]
    pub assets: String,
    #[serde(default)]
    pub asset_index: Option<AssetIndex>,
    #[serde(default)]
    pub java_version: Option<JavaVersion>,
    /// 现代版本（1.13+）：arguments.game / arguments.jvm
    #[serde(default)]
    pub arguments: Option<Arguments>,
    /// 旧版本（1.12-）：单个字符串模板
    #[serde(default)]
    pub minecraft_arguments: Option<String>,
    #[serde(default)]
    pub libraries: Vec<Library>,
    #[serde(default)]
    pub downloads: Option<VersionDownloads>,
    /// 继承自（fabric/forge 实例继承官方版本）
    #[serde(default)]
    #[serde(rename = "inheritsFrom")]
    pub inherits_from: Option<String>,
}

/// 资源索引（sha1/size 待完整性校验接入）
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
pub struct AssetIndex {
    pub id: String,
    #[serde(default)]
    pub sha1: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub size: i64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JavaVersion {
    #[serde(default)]
    pub major_version: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Arguments {
    #[serde(default)]
    pub game: Vec<ArgValue>,
    #[serde(default)]
    pub jvm: Vec<ArgValue>,
}

/// 参数项：字符串或带 rules 的对象
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum ArgValue {
    Plain(String),
    Object {
        value: ArgVal,
        #[serde(default)]
        rules: Vec<serde_json::Value>,
    },
}

/// 规则求值上下文（features 与 OS）
#[derive(Debug, Clone, Default)]
pub struct RuleCtx {
    pub os_name: String,
    pub os_arch: String,
    pub is_demo_user: bool,
    pub has_custom_resolution: bool,
    pub has_quick_plays_support: bool,
    pub is_quick_play_singleplayer: bool,
    pub is_quick_play_multiplayer: bool,
    pub is_quick_play_realms: bool,
}

impl RuleCtx {
    pub fn host() -> Self {
        RuleCtx {
            os_name: std::env::consts::OS.to_string(),
            os_arch: std::env::consts::ARCH.to_string(),
            ..Default::default()
        }
    }

    fn feature(&self, key: &str) -> Option<bool> {
        match key {
            "is_demo_user" => Some(self.is_demo_user),
            "has_custom_resolution" => Some(self.has_custom_resolution),
            "has_quick_plays_support" => Some(self.has_quick_plays_support),
            "is_quick_play_singleplayer" => Some(self.is_quick_play_singleplayer),
            "is_quick_play_multiplayer" => Some(self.is_quick_play_multiplayer),
            "is_quick_play_realms" => Some(self.is_quick_play_realms),
            _ => None,
        }
    }

    /// 规则是否匹配当前环境
    pub fn rule_matches(&self, r: &serde_json::Value) -> bool {
        if let Some(os) = r.get("os") {
            if let Some(name) = os.get("name") {
                let want = name.as_str().unwrap_or("");
                if want != self.os_name {
                    return false;
                }
            }
            if let Some(arch) = os.get("arch") {
                let want = arch.as_str().unwrap_or("");
                if want != self.os_arch {
                    return false;
                }
            }
        }
        if let Some(feats) = r.get("features").and_then(|f| f.as_object()) {
            for (k, v) in feats {
                let want = v.as_bool().unwrap_or(false);
                if self.feature(k) != Some(want) {
                    return false;
                }
            }
        }
        true
    }

    /// 整组规则是否放行（任一 allow 匹配即放行；任一 disallow 匹配即拒绝）
    pub fn rules_allow(&self, rules: &[serde_json::Value]) -> bool {
        for r in rules {
            if r.get("action").and_then(|a| a.as_str()) == Some("disallow") && self.rule_matches(r)
            {
                return false;
            }
        }
        for r in rules {
            if r.get("action").and_then(|a| a.as_str()) == Some("allow") && self.rule_matches(r) {
                return true;
            }
        }
        rules.is_empty()
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum ArgVal {
    Single(String),
    Multi(Vec<String>),
}

impl ArgValue {
    pub fn values(&self, ctx: &RuleCtx) -> Vec<String> {
        match self {
            ArgValue::Plain(s) => vec![s.clone()],
            ArgValue::Object { value, rules } => {
                if !ctx.rules_allow(rules) {
                    return Vec::new();
                }
                match value {
                    ArgVal::Single(s) => vec![s.clone()],
                    ArgVal::Multi(v) => v.clone(),
                }
            }
        }
    }
}

/// 库（natives/rules 待原生库分发与 OS 规则接入）
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, Default)]
pub struct Library {
    pub name: String,
    #[serde(default)]
    pub downloads: Option<LibraryDownloads>,
    /// 旧式库（仅 name + 自定义 maven 源）
    #[serde(default)]
    pub url: String,
    /// 原生库分类器（如 linux -> natives-linux）
    #[serde(default)]
    pub natives: Option<std::collections::HashMap<String, String>>,
    /// 规则（OS 相关，简化忽略）
    #[serde(default)]
    pub rules: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LibraryDownloads {
    #[serde(default)]
    pub artifact: Option<DownloadInfo>,
    /// 分类器下载（如 natives-linux）
    #[serde(default)]
    pub classifiers: Option<std::collections::HashMap<String, DownloadInfo>>,
}

/// 下载条目（size 待进度估算接入）
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
pub struct DownloadInfo {
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub sha1: String,
    #[serde(default)]
    pub size: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct VersionDownloads {
    #[serde(default)]
    pub client: Option<DownloadInfo>,
}

/// 解析库坐标 group:artifact:version → maven 相对路径
pub fn maven_path(name: &str) -> String {
    let parts: Vec<&str> = name.split(':').collect();
    if parts.len() < 3 {
        return name.to_string();
    }
    let (group, artifact, version) = (parts[0], parts[1], parts[2]);
    let suffix = if parts.len() > 3 { parts[3] } else { "" };
    let base = format!(
        "{}/{}/{}/{}-{}",
        group.replace('.', "/"),
        artifact,
        version,
        artifact,
        version
    );
    if suffix.is_empty() {
        format!("{base}.jar")
    } else {
        format!("{base}-{suffix}.jar")
    }
}

/// 资产索引文件（assets/indexes/<id>.json）
#[derive(Debug, Clone, Deserialize, Default)]
pub struct AssetsIndex {
    #[serde(default)]
    pub objects: std::collections::HashMap<String, AssetObject>,
}

/// 资产对象（size 待完整性校验接入）
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
pub struct AssetObject {
    #[serde(default)]
    pub hash: String,
    #[serde(default)]
    pub size: i64,
}
