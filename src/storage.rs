use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::domain::Credentials;

/// 启动记忆：退出时保存，下次启动恢复
/// 容器级 `#[serde(default)]`：字段缺失时逐个回落到默认值，而不是整份解析失败
/// 静默清空记忆。增删字段时老 session.json 照常可用（多余字段 serde 默认忽略）。
/// 没有这个属性时，往下加一个字段就会让所有存量 session.json 一次性作废。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct SessionState {
    /// 上次视图 index（对应 `HomeView::ALL` 的顺序）
    pub home_view: usize,
    /// 上次选中的实例索引
    pub selected_instance: usize,
    /// 上次选中的世界索引
    pub selected_world: usize,
    /// 上次选中的账户索引
    pub selected_account: usize,
    /// 上次资源中心来源 index
    pub resource_source: usize,
    /// 上次资源分类索引
    pub category: usize,
    /// 上次角色名
    pub role_name: String,
    /// 上次聚焦面板 index（HomeFocus 枚举）
    pub home_focus: usize,
}

pub fn config_dir() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
        .join(".config/mctl")
}

/// 写入 config_dir 下的文件（自动建目录）
/// 先写同目录 `.tmp` 再 rename：崩溃/断电时最多丢掉本次写入，不会留下半截文件
/// 把上次的启动记忆一起弄坏（同目录内 rename 是原子的）。
/// 权限：文件 0600、配置目录 0700（里面有凭证/会话 token）。
fn write_cfg(path: PathBuf, data: &[u8]) -> Result<()> {
    let dir = path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(config_dir);
    std::fs::create_dir_all(&dir)?;
    restrict_dir_perms(&dir);
    let mut tmp = path.clone();
    tmp.set_extension("tmp");
    std::fs::write(&tmp, data)?;
    restrict_file_perms(&tmp);
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// 写一个 `config_dir` 下的具名配置文件（0600、原子替换）。
/// 给插件/服务器列表这类纯文本配置用。
pub fn write_config_file(name: &str, text: &str) -> Result<()> {
    write_cfg(config_dir().join(name), text.as_bytes())
}

/// 文件仅本用户可读写（凭证、会话里含 token/cookie）
#[cfg(unix)]
fn restrict_file_perms(p: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(meta) = std::fs::metadata(p) {
        let mut perm = meta.permissions();
        if perm.mode() & 0o077 != 0 {
            perm.set_mode(0o600);
            let _ = std::fs::set_permissions(p, perm);
        }
    }
}
#[cfg(not(unix))]
fn restrict_file_perms(_p: &std::path::Path) {}

/// 配置目录仅本用户可进入（尽力而为：失败不影响功能）
#[cfg(unix)]
fn restrict_dir_perms(dir: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(meta) = std::fs::metadata(dir) {
        let mut perm = meta.permissions();
        if perm.mode() & 0o077 != 0 {
            perm.set_mode(0o700);
            let _ = std::fs::set_permissions(dir, perm);
        }
    }
}
#[cfg(not(unix))]
fn restrict_dir_perms(_dir: &std::path::Path) {}

pub fn cred_file() -> PathBuf {
    config_dir().join("credentials.json")
}

/// 兼容旧版 x19dl 的凭证文件（迁移期回退读取）
fn legacy_cred_file() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
        .join(".config/x19dl/credentials.json")
}

pub fn save(cred: &Credentials) -> Result<()> {
    write_cfg(cred_file(), serde_json::to_string_pretty(cred)?.as_bytes())
}

pub fn load() -> Result<Credentials> {
    let path = if cred_file().exists() {
        cred_file()
    } else {
        legacy_cred_file()
    };
    let text = std::fs::read_to_string(path)?;
    Ok(serde_json::from_str(&text)?)
}

/// 从**指定配置目录**读凭据（不带旧版回退）。
/// `App::new_from(cfg)` 走这条：测试给临时目录就只读临时目录，
/// 不会像以前那样读到本机真实账号（也避免测试误发真实请求）。
pub fn load_from(root: &std::path::Path) -> Result<Credentials> {
    let text = std::fs::read_to_string(root.join("credentials.json"))?;
    Ok(serde_json::from_str(&text)?)
}

fn ls_file() -> PathBuf {
    config_dir().join("littleskin.json")
}

pub fn save_ls(cfg: &crate::api::littleskin::LsConfig) -> Result<()> {
    write_cfg(ls_file(), serde_json::to_string_pretty(cfg)?.as_bytes())
}

pub fn load_ls() -> Result<crate::api::littleskin::LsConfig> {
    let text = std::fs::read_to_string(ls_file())?;
    Ok(serde_json::from_str(&text)?)
}

fn role_file() -> PathBuf {
    config_dir().join("role_name.txt")
}

/// 保存客户端登录名称（联机角色名）
pub fn save_role(name: &str) -> Result<()> {
    write_cfg(role_file(), name.trim().as_bytes())
}

/// 读取客户端登录名称（无则返回空）
pub fn load_role() -> String {
    std::fs::read_to_string(role_file())
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

/// 会话状态文件名
const SETTINGS_FILE: &str = "settings.json";
const SESSION_FILE: &str = "session.json";

fn session_file_in(dir: &std::path::Path) -> PathBuf {
    dir.join(SESSION_FILE)
}

/// 保存会话状态（退出时调用）
pub fn save_session(dir: &std::path::Path, state: &SessionState) -> Result<()> {
    write_cfg(
        session_file_in(dir),
        serde_json::to_string_pretty(state)?.as_bytes(),
    )
}

/// 加载会话状态（启动时调用，缺失则返回默认值）
pub fn load_session(dir: &std::path::Path) -> SessionState {
    std::fs::read_to_string(session_file_in(dir))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// 可在设置页编辑并持久化的项（`~/.config/mctl/settings.json`）
/// wm.toml 里的项（dock 宽度/强调色/键位）**不在此列**——tui-wm-core 只提供读
/// （`WmConfig::load`），没有序列化器，写不回去；那些仍需手改 wm.toml。
/// 角色名也不在此列：它有独立的 `role_name.txt`，状态栏已在用。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Settings {
    #[serde(default)]
    pub game_dir: Option<String>,
    #[serde(default)]
    pub download_dir: Option<String>,
    #[serde(default)]
    pub grid_cols: Option<usize>,
    #[serde(default)]
    pub grid_rows: Option<usize>,
    /// 启动后是否周期性自动截图（隐私相关）。
    /// /// **默认关**。自动截图会在游戏运行时把画面存到 `~/.config/mctl/shots/<世界>/`，
    /// 频率为开场 1 分钟 3 张 + 之后每 30 分钟一张——等价于持续记录游玩内容。
    /// 想要封面缩略图可以单独开那个（见 SettingField::WorldShot），两者独立。
    /// /// 老配置（没有这个键）也读成 false：宁可不开，也不替用户决定。
    #[serde(default)]
    pub auto_shot: Option<bool>,
    /// 主页大图：true = MCTL 立体 ASCII 常驻顶部（有截图也不铺满）；
    /// false（**默认**）= 有图显示图，没图才显示 ASCII。
    /// /// 老配置没有这个键 → None → 当 false，不替用户改选择。
    #[serde(default)]
    pub ascii_always: Option<bool>,
    /// 世界选择列表：选中框的流光描边动画（**默认开**）。
    /// /// 老配置没有这个键 → None → 当开（换主题功能之前的默认行为）。
    #[serde(default)]
    pub world_anim: Option<bool>,
    /// 界面主题 id（`crate::theme::theme_ids` 里的取值）。
    /// /// **默认 "mctl"**（经典配色，黑底青调）。老配置没有这个键 → None →
    /// 经典配色，皮肤和以前完全一样。
    #[serde(default)]
    pub theme: Option<String>,
}

/// 读设置；文件缺失或损坏一律回默认（不该因为配置坏了进不了程序）
/// **必须传 `cfg_root`**，不能用全局 `config_dir`——那会让测试写进用户真实的
/// `~/.config/mctl`（曾把 game_dir 写成一个临时目录，用户配置直接报废）。
/// 和 `load_session`/`save_session` 同一个约定。
pub fn load_settings(root: &std::path::Path) -> Settings {
    std::fs::read_to_string(root.join(SETTINGS_FILE))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

pub fn save_settings(root: &std::path::Path, s: &Settings) -> Result<()> {
    write_cfg(
        root.join(SETTINGS_FILE),
        serde_json::to_string_pretty(s)?.as_bytes(),
    )
}

/// 测试专用：每次调用返回一个全新的临时配置根目录。
/// 测试是并行跑的，共用同一个目录会互相污染（上次留下的 session.json 会被下个测试读到）。
/// 「最近」历史：主页用它做聚合展示。
/// 单独一个文件而不是塞进 session.json —— session 是「启动记忆」（光标位置、
/// 选中项），recents 是**跨会话增长的用户历史**。混在一起会让 session.json
/// 每次启动都被改写，而它本来大部分时候是不动的。
/// 每类都只保留最近 N 条（见 `PUSH_LIMIT`），免得无限增长。
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Recents {
    /// 游玩过的存档。`id` = "<实例 id>/<世界目录名>"，**按实例分**：
    /// 1.21.1 和 1.21.1-fabric 各有自己的最近世界。
    #[serde(default)]
    pub worlds: Vec<RecentItem>,
    /// 进过的 Java 国际服（旧版本叫 netease_servers，用 alias 兼容老 recents.json）
    #[serde(default, alias = "netease_servers")]
    pub java_servers: Vec<RecentItem>,
    /// 已下载的资源
    #[serde(default)]
    pub downloads: Vec<RecentItem>,
}

/// 一条历史记录。`id` 是稳定标识（host:port / entity_id / item_id），
/// 用来去重——同一个服务器反复进只更新 `at`，不追加新行。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RecentItem {
    pub id: String,
    pub name: String,
    /// 副信息：版本号 / 人数 / 大小
    #[serde(default)]
    pub detail: String,
    /// Unix 秒
    pub at: i64,
}

/// 每类最多留几条
const PUSH_LIMIT: usize = 5;

impl RecentItem {
    pub fn new(id: impl Into<String>, name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            detail: detail.into(),
            at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0),
        }
    }
}

/// 记录一次访问：同 id 原地更新并置顶，超出上限丢最旧的。
pub fn push_recent(list: &mut Vec<RecentItem>, item: RecentItem) {
    list.retain(|x| x.id != item.id);
    list.insert(0, item);
    list.truncate(PUSH_LIMIT);
}

fn recents_file(root: &std::path::Path) -> PathBuf {
    root.join("recents.json")
}

pub fn load_recents(root: &std::path::Path) -> Recents {
    std::fs::read_to_string(recents_file(root))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

pub fn save_recents(root: &std::path::Path, r: &Recents) -> Result<()> {
    write_cfg(
        recents_file(root),
        serde_json::to_string_pretty(r)?.as_bytes(),
    )
}

#[cfg(test)]
mod recents_tests {
    use super::{
        load_recents, push_recent, save_recents, write_cfg, RecentItem, Recents, PUSH_LIMIT,
    };

    fn it(id: &str, at: i64) -> RecentItem {
        RecentItem {
            id: id.into(),
            name: format!("n{id}"),
            detail: String::new(),
            at,
        }
    }

    /// 同一 id 重复记录应**原地更新并置顶**，而不是追加成两行
    #[test]
    fn same_id_updates_in_place_and_goes_first() {
        let mut list = Vec::new();
        push_recent(&mut list, it("a", 1));
        push_recent(&mut list, it("b", 2));
        push_recent(&mut list, it("a", 3));
        assert_eq!(list.len(), 2, "重复 id 不应产生新行");
        assert_eq!(list[0].id, "a", "重复记录的应排到最前");
        assert_eq!(list[0].at, 3, "时间应被更新");
    }

    /// 超过上限丢最旧的
    #[test]
    fn truncates_to_limit_keeping_newest() {
        let mut list = Vec::new();
        for i in 0..(PUSH_LIMIT + 3) {
            push_recent(&mut list, it(&format!("id{i}"), i as i64));
        }
        assert_eq!(list.len(), PUSH_LIMIT);
        assert_eq!(
            list[0].id,
            format!("id{}", PUSH_LIMIT + 2),
            "最新的应在最前"
        );
    }

    /// 落盘 → 重新读回：配置在 cfg_root 下，**不能**写进用户真实的 ~/.config/mctl
    #[test]
    fn round_trips_inside_cfg_root() {
        let dir = crate::storage::tmp_config_dir("recents");
        let mut r = Recents::default();
        r.java_servers.push(it("123", 42));
        save_recents(&dir, &r).unwrap();
        assert!(
            dir.join("recents.json").is_file(),
            "recents.json 必须落在 cfg_root 内"
        );
        let back = load_recents(&dir);
        assert_eq!(back.java_servers.len(), 1);
        assert_eq!(back.java_servers[0].id, "123");
    }

    /// 文件缺失/损坏一律回默认，不该因为历史文件坏了进不了程序
    #[test]
    fn missing_or_corrupt_file_loads_default() {
        let dir = crate::storage::tmp_config_dir("recents-bad");
        assert_eq!(load_recents(&dir).java_servers.len(), 0);
        std::fs::write(dir.join("recents.json"), b"{ this is not json").unwrap();
        assert_eq!(load_recents(&dir).java_servers.len(), 0);
    }

    /// 凭证文件落盘权限必须是 0600（同机其它用户不可读）
    #[test]
    fn config_files_are_private() {
        let dir = crate::storage::tmp_config_dir("perms");
        let f = dir.join("credentials.json");
        write_cfg(f.clone(), br#"{"access_token":"secret"}"#).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&f).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "凭证文件应仅本用户可读写（实际 {mode:o}）");
        }
        assert_eq!(std::fs::read(&f).unwrap(), br#"{"access_token":"secret"}"#);
    }
}

#[cfg(test)]
pub fn tmp_config_dir(tag: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "mctl-test-{}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed),
        tag
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("创建临时配置目录");
    dir
}
