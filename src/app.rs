use anyhow::{anyhow, bail, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ratatui_image::protocol::StatefulProtocol;
use tokio::sync::mpsc;

use crate::api;
use crate::domain::{Credentials, DownloadSub, ItemDetail, ShopItem};
use crate::launcher::{self, Instance};
use crate::models::{Account, WorldInfo};
use crate::storage;

/// 每页请求数量（服务端支持一次拉取 100 条）
const PAGE_SIZE: i64 = 100;

/// Modrinth 单次拉取条数：官方上限 100。一次拉满，翻页次数和网易网格同量级，
/// 不用每 20 条就跨国往返一次。
const MODRINTH_PAGE: usize = 100;

/// 异步任务完成消息
pub enum TaskMsg {
    /// 分类列表加载完成
    CategoryLoaded {
        offset: i64,
        items: Vec<ShopItem>,
        error: Option<String>,
        end: bool,
    },
    /// 详情加载完成
    DetailLoaded(Option<ItemDetail>),
    /// 下载信息加载完成
    DownloadInfoLoaded(Vec<DownloadSub>),
    /// 封面图下载完成
    CoverLoaded {
        index: usize,
        img: Option<image::DynamicImage>,
    },
    /// 联机服务器网格封面加载完成
    SrvCoverLoaded {
        index: usize,
        gen: u64,
        img: Option<image::DynamicImage>,
    },
    /// 网易资源 BE（基岩版）检查完成（None=请求失败）
    BeChecked { index: usize, be: Option<bool> },
    /// Modrinth 封面图下载完成
    MrCoverLoaded {
        index: usize,
        img: Option<image::DynamicImage>,
    },
    /// Modrinth 版本列表加载完成
    MrVersionsLoaded {
        title: String,
        result: Result<Vec<launcher::modpack::MrVersion>>,
    },
    /// 下载任务终态（资源/模组/地图，任务 id 定位到下载中心那一条）
    TaskDone {
        id: u64,
        ok: bool,
        message: String,
    },
    /// 登录完成
    LoginDone(Result<Credentials>),
    /// 4399 登录第一步完成：验证码图已下载，等用户识别（App 存起来供第二步用）
    LoginCaptcha(Result<crate::api::auth::LoginChallenge>),
    /// 启动阶段进度
    LaunchStage(String),
    /// 启动完成（返回游戏进程 PID）
    LaunchDone(Result<u32>),
    /// 横幅游戏截图加载完成（.minecraft/screenshots/ 最新 F2 截图）
    BannerShotLoaded(Option<StatefulProtocol>),
    /// 世界缩略图加载完成（key = "<实例id>/<世界目录名>"；proto=None 表示解码失败）
    WorldIconLoaded {
        key: String,
        proto: Option<StatefulProtocol>,
    },
    /// 账户面板头像加载完成
    AvatarLoaded(Option<StatefulProtocol>),
    /// 游戏输出日志行
    GameLog(String),
    /// 插件版本列表加载完成（provides/versions 契约）
    VersionsLoaded(Result<Vec<VersionItem>>),
    /// 插件版本操作进度/状态
    VersionsStatus(String),
    /// 插件版本操作完成（动作, 版本id, 结果）
    VersionsDone((String, String, Result<String>)),
    /// 游戏进程退出（退出码，None=信号终止）
    GameExit(Option<i32>),
    /// 官方版本清单加载完成
    ManifestLoaded(Result<Vec<launcher::meta::ManifestVersion>>),
    /// 整合包搜索结果
    /// 整合包搜索完成（offset=0 替换结果；>0 追加下一页）
    ModpacksLoaded {
        result: Result<Vec<launcher::modpack::ModpackHit>>,
        offset: usize,
    },
    /// GitHub 搜索结果到达（配额稀缺，只在用户按 Enter 时发）
    GhLoaded {
        result: Result<api::github::SearchResult>,
        /// 首屏加载（允许在注册表为空时回退到宽泛搜索）
        initial: bool,
    },
    /// GitHub 仓库安装完成（clone + cargo build + 识别）
    GhInstalled {
        name: String,
        result: Result<crate::plugin::PluginInfo>,
    },
    /// 安装阶段进度（`id` = InstallTask::id）
    InstallStage {
        id: u64,
        phase: String,
        done: usize,
        total: usize,
    },
    /// 单文件下载进度（驱动 细线；`id` = InstallTask::id）
    FileProgress { id: u64, f: launcher::FileProgress },
    /// 安装完成
    InstallDone { id: u64, result: Result<String> },
    /// 服务器页：Java 国际服列表 + 状态 ping 完成
    ServerListLoaded(Vec<crate::api::mcserver::JavaServer>),
    /// 服务器页：当前实例的 servers.dat 读取 + ping 完成
    UserServersLoaded {
        path: PathBuf,
        result: Result<Vec<crate::api::mcserver::JavaServer>>,
    },
    /// 服务器页：一键进服完成（进程 PID）
    ServerDone(Result<u32>),
    /// 服务器页：横幅宣传图加载完成
    /// 插件安装完成
    PluginInstallDone(Result<crate::plugin::PluginInfo>),
    /// LittleSkin：设备码对已获取
    LsDeviceCode {
        pair: crate::api::littleskin::DeviceCodePair,
        client_id: String,
    },
    /// LittleSkin：授权完成，拿到 access_token
    LsTokenGot(String),
    /// LittleSkin：角色档案列表
    LsProfiles {
        access: String,
        profiles: Vec<crate::api::littleskin::YgProfile>,
    },
    /// LittleSkin：登录完成（选定角色）
    LsDone {
        name: String,
        result: Result<crate::api::littleskin::McToken>,
    },
    /// LittleSkin：流程失败
    LsFailed(String),
}

/// 主视图（Alt+数字 / 视图侧栏切换）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HomeView {
    /// 当前实例的内容（世界等）。启动后的默认视图。
    Instance,
    Server,
    Resource,
    /// 下载中心（Alt+6）。整合包/官方版本/单 mod/光影/插件的下载都汇到这里。
    Download,
    Plugins,
    Settings,
}

/// tui-wm 窗口 ID 常量
pub mod win_id {
    /// 实例视图（启动默认）。id 取 7 而不是从 1 开始：旧 wm.toml 里按数字存了窗口，
    /// 重排会让所有窗口错位（比 Alt 快捷键后移更难排查）。
    pub const INSTANCE: u32 = 7;
    pub const SERVER: u32 = 2;
    pub const RESOURCE: u32 = 4;
    pub const PLUGINS: u32 = 5;
    pub const SETTINGS: u32 = 6;
    /// 下载中心（HomeView::Download）
    pub const DOWNLOADS: u32 = 13;
    pub const ACCOUNTS: u32 = 10;
    pub const VIEWS: u32 = 11;
    pub const STATUS: u32 = 12;
}

impl HomeView {
    pub const ALL: [HomeView; 6] = [
        HomeView::Instance,
        HomeView::Server,
        HomeView::Resource,
        HomeView::Download,
        HomeView::Plugins,
        HomeView::Settings,
    ];

    pub fn label(self) -> &'static str {
        match self {
            HomeView::Instance => "实例",
            HomeView::Server => "服务器",
            HomeView::Resource => "资源下载",
            HomeView::Download => "下载中心",
            HomeView::Plugins => "插件",
            HomeView::Settings => "设置",
        }
    }

    /// 视图栏图标（几何字符，等宽终端安全，避免 emoji 宽度问题）
    pub fn icon(self) -> &'static str {
        match self {
            HomeView::Instance => "▥",
            HomeView::Server => "▤",
            HomeView::Resource => "⇩",
            HomeView::Download => "⟳",
            HomeView::Plugins => "⬡",
            HomeView::Settings => "⚙",
        }
    }

    /// Alt+数字 快捷位（1 起）
    pub fn slot(self) -> u8 {
        Self::ALL.iter().position(|v| *v == self).unwrap_or(0) as u8 + 1
    }
}

/// 界面内聚焦面板（Tab 轮换；F 全屏当前聚焦面板）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HomeFocus {
    Accounts,
    Banner,
    List,
    Downloads,
    Status,
}

impl HomeFocus {
    /// 当前视图可聚焦面板（5 个不同面板，无重复，j/k 真正遍历所有）
    pub fn all_for(view: HomeView) -> [HomeFocus; 5] {
        match view {
            // 实例视图是 banner + 世界列表，没有下载面板
            HomeView::Instance
            | HomeView::Resource
            | HomeView::Download
            | HomeView::Plugins
            | HomeView::Settings => [
                HomeFocus::Accounts,
                HomeFocus::Banner,
                HomeFocus::List,
                HomeFocus::Downloads,
                HomeFocus::Status,
            ],
            HomeView::Server => [
                HomeFocus::Accounts,
                HomeFocus::Banner,
                HomeFocus::List,
                HomeFocus::Status,
                HomeFocus::Downloads, // 服务器也有下载面板（空或占位），保证 5 个不同
            ],
        }
    }

    /// 会话存取用的稳定序号（与 all_for 面板顺序对齐）
    pub fn as_index(self) -> usize {
        match self {
            HomeFocus::Accounts => 0,
            HomeFocus::Banner => 1,
            HomeFocus::List => 2,
            HomeFocus::Downloads => 3,
            HomeFocus::Status => 4,
        }
    }

    /// 从会话序号恢复（越界回退 List）
    pub fn from_index(i: usize) -> Self {
        Self::all_for(HomeView::Instance)
            .get(i)
            .copied()
            .unwrap_or(HomeFocus::List)
    }

    pub fn next(self, view: HomeView) -> Self {
        let all = Self::all_for(view);
        let i = all.iter().position(|f| *f == self).unwrap_or(0);
        all[(i + 1) % all.len()] // 直接循环，不跳过
    }

    pub fn prev(self, view: HomeView) -> Self {
        let all = Self::all_for(view);
        let i = all.iter().position(|f| *f == self).unwrap_or(0);
        all[(i + all.len() - 1) % all.len()]
    }
}

/// 详情页焦点（Tab 切换）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetailFocus {
    Info,
    Versions,
    Download,
}

/// 下载页焦点
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DlFocus {
    Official,
}

/// 下载页状态（官方包 + 整合包）
pub struct DlPage {
    pub focus: DlFocus,
    pub manifest: Vec<launcher::meta::ManifestVersion>,
    pub manifest_loading: bool,
    pub sel_official: usize,
    pub query: String,
    pub sort: launcher::modpack::ModrinthIndex,
    pub results: Vec<launcher::modpack::ModpackHit>,
    pub sel_result: usize,
    pub searching: bool,
    pub more_end: bool,
    pub install_status: String,
    pub installing: bool,
}

impl Default for DlPage {
    fn default() -> Self {
        Self {
            focus: DlFocus::Official,
            manifest: Vec::new(),
            manifest_loading: false,
            sel_official: 0,
            query: String::new(),
            sort: launcher::modpack::ModrinthIndex::Relevance,
            results: Vec::new(),
            sel_result: 0,
            searching: false,
            more_end: false,
            install_status: String::new(),
            installing: false,
        }
    }
}
/// 资源中心：资源源（来源 → 类型 两级结构的第一级）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceSource {
    NetEase,
    Mojang,
    Modrinth,
    /// 插件提供的资源（provides/versions 契约，如基岩版）
    Plugins,
    Github,
    Local,
}

/// Modrinth 分类（显示名, project_type）
pub const MODRINTH_CATEGORIES: [(&str, &str); 5] = [
    ("模组", "mod"),
    ("光影", "shader"),
    ("资源包", "resourcepack"),
    ("整合包", "modpack"),
    ("数据包", "datapack"),
];

impl ResourceSource {
    pub const ALL: [ResourceSource; 6] = [
        ResourceSource::NetEase,
        ResourceSource::Mojang,
        ResourceSource::Modrinth,
        ResourceSource::Plugins,
        ResourceSource::Github,
        ResourceSource::Local,
    ];
    pub fn label(self) -> &'static str {
        match self {
            ResourceSource::NetEase => "NetEase",
            ResourceSource::Mojang => "Mojang",
            ResourceSource::Modrinth => "Modrinth",
            ResourceSource::Plugins => "插件",
            ResourceSource::Github => "GitHub",
            ResourceSource::Local => "Local",
        }
    }
}

/// 安装任务（官方包/整合包/进服依赖，汇入下载中心统一队列）
#[derive(Debug, Clone)]
pub struct InstallTask {
    /// 任务 id：进度消息按 id 找任务（并发多任务时不会串台）
    pub id: u64,
    pub name: String,
    pub phase: String,
    pub done: usize,
    pub total: usize,
    pub status: InstallStatus,
    /// 任务创建时刻，驱动 spinner 转动
    pub started: std::time::Instant,
    /// 正在下载的文件（细线）。按字节数降序，大文件在头。
    pub files: Vec<FileSlot>,
    /// 该任务累计已下/总字节（`files` 会被裁剪、单个 FileSlot 完成后归位，
    /// 所以总量单独记——不然任务总进度条会跟着文件槽一起漂）。
    pub bytes_done: u64,
    pub bytes_total: u64,
}

impl InstallTask {
    /// 任务总进度 0.0–1.0。总长未知（服务器没给 Content-Length）时按 0 算。
    pub fn pct(&self) -> f64 {
        if self.status == InstallStatus::Done {
            return 1.0;
        }
        if self.bytes_total == 0 {
            return 0.0;
        }
        (self.bytes_done as f64 / self.bytes_total as f64).clamp(0.0, 1.0)
    }
}

/// 下载中心里的一个文件条目（由 `launcher::FileProgress` 驱动更新）
#[derive(Debug, Clone)]
pub struct FileSlot {
    pub name: String,
    pub done: u64,
    pub total: u64,
    /// 首次收到该文件进度的时刻，用于算速率
    pub started: std::time::Instant,
    pub failed: bool,
    pub finished: bool,
}

impl FileSlot {
    pub fn pct(&self) -> f64 {
        if self.finished {
            1.0
        } else if self.total == 0 {
            0.0
        } else {
            (self.done as f64 / self.total as f64).clamp(0.0, 1.0)
        }
    }

    /// 平均速率 B/s。速率取区间平均而非瞬时——瞬时值会满屏跳。
    pub fn speed(&self) -> u64 {
        let secs = self.started.elapsed().as_secs_f64();
        if secs < 0.3 || self.done == 0 {
            0
        } else {
            (self.done as f64 / secs) as u64
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallStatus {
    Active,
    Done,
    Failed,
}

/// 资源页（Tab::Maps）内部子页面
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MapPage {
    Grid,
    Detail,
    Download,
}

/// 网易资源排序：0/1=综合（服务端，下载量主导） 2=最新（服务端）
/// Downloads/Hot 为客户端本地重排（search 接口字段缺失时回落为服务端排序）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NeteaseSort {
    Comprehensive,
    Newest,
    Downloads,
    Hot,
}

/// 从下载任务线程推送单文件进度。
/// 回调是同步的（`&dyn Fn + Send`），不能 `.send.await`，也不能
/// `blocking_send`（那会让 spawn 的 future 失去 `Send`）。
/// ponytail: 进度事件允许丢——每个数据块都会再推一次，丢一帧只是细线跳一下。
/// 终态（完成/失败）丢不得，否则文件永远停在 99%，所以带重试；
/// 重试间隔极短，UI 循环 100ms 一帧，队列必然腾出位置。
fn send_file_progress(tx: &mpsc::Sender<TaskMsg>, id: u64, f: launcher::FileProgress) {
    // TaskMsg 不是 Clone（里面挂着 Result/大对象），按需重建
    let msg = |f: &launcher::FileProgress| TaskMsg::FileProgress { id, f: f.clone() };
    if tx.try_send(msg(&f)).is_ok() {
        return;
    }
    if f.finished_at.is_some() {
        for _ in 0..50 {
            std::thread::sleep(std::time::Duration::from_millis(10));
            if tx.try_send(msg(&f)).is_ok() {
                return;
            }
        }
    }
}

/// 阶段文本去哪个状态行（目前只有本地实例启动）
#[derive(Clone, Copy)]
enum StageSink {
    Launch,
}

/// 阶段上报：状态行 +（有任务时）下载中心任务标题。
/// 都丢给 tokio 任务异步发——下载线程不能被 UI 队列阻塞。
fn report_stage(
    tx: &mpsc::Sender<TaskMsg>,
    sink: StageSink,
    task_id: Option<u64>,
    phase: &str,
    done: usize,
    total: usize,
) {
    let text = if total == 0 {
        phase.to_string()
    } else {
        format!("{phase} {done}/{total}")
    };
    if let Some(id) = task_id {
        let tx = tx.clone();
        let phase = phase.to_string();
        tokio::spawn(async move {
            let _ = tx.try_send(TaskMsg::InstallStage {
                id,
                phase,
                done,
                total,
            });
        });
    }
    let tx = tx.clone();
    tokio::spawn(async move {
        let msg = match sink {
            StageSink::Launch => TaskMsg::LaunchStage(text),
        };
        let _ = tx.send(msg).await;
    });
}

impl NeteaseSort {
    pub const ALL: [NeteaseSort; 4] = [
        NeteaseSort::Comprehensive,
        NeteaseSort::Newest,
        NeteaseSort::Downloads,
        NeteaseSort::Hot,
    ];

    pub fn label(self) -> &'static str {
        match self {
            NeteaseSort::Comprehensive => "综合",
            NeteaseSort::Newest => "最新",
            NeteaseSort::Downloads => "下载量",
            NeteaseSort::Hot => "热度",
        }
    }

    pub fn server_sort(self) -> i64 {
        match self {
            NeteaseSort::Comprehensive => 0,
            NeteaseSort::Newest => 2,
            NeteaseSort::Downloads | NeteaseSort::Hot => 0,
        }
    }

    pub fn is_local(self) -> bool {
        matches!(self, NeteaseSort::Downloads | NeteaseSort::Hot)
    }

    pub fn next(self) -> Self {
        let i = Self::ALL.iter().position(|s| *s == self).unwrap_or(0);
        Self::ALL[(i + 1) % Self::ALL.len()]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Sidebar,
    Grid,
}

/// LittleSkin OAuth 状态（账户面板）：应用登记 + 设备码流登录
pub struct LsPage {
    /// 输入/登录模式（账户面板聚焦时 Enter 进入）
    pub input: bool,
    /// OAuth 应用名（littleskin.cn/user/oauth/manage 创建）
    pub app_name: String,
    /// 回调 URL（白名单要求 https://open.littleskin.cn/oauth/callback）
    pub callback_url: String,
    /// 白名单审核通过后分配的客户端 ID
    pub client_id: String,
    /// 当前输入字段：0=应用名 1=回调URL 2=client_id
    pub step: u8,
    pub status: String,

    // ---- 设备码流登录运行态 ----
    /// None=空闲；Some=等待用户在浏览器完成授权
    pub login: Option<LsLogin>,
}

/// 设备码流进行中的状态
pub struct LsLogin {
    pub user_code: String,
    pub verification_url: String,
}

/// 全屏登录界面：上方 MCTL logo，下方居中选择 [4399] / [LittleSkin]
pub struct LoginScreen {
    /// 0=4399 1=LittleSkin
    pub selected: usize,
    /// 4399 内嵌账号密码输入已展开
    pub input_4399: bool,
}

impl Default for LsPage {
    fn default() -> Self {
        Self {
            input: false,
            app_name: String::new(),
            callback_url: crate::api::littleskin::OAUTH_CALLBACK_DEFAULT.to_string(),
            client_id: String::new(),
            step: 0,
            status: String::new(),
            login: None,
        }
    }
}

/// 插件版本项（provides/versions 契约的 JSON 行）
#[derive(Debug, Clone, serde::Deserialize)]
pub struct VersionItem {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub installed: bool,
    #[serde(default)]
    pub size_mb: u64,
}

/// 「最近」的四类记录，和 `storage::Recents` 的四个字段一一对应。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecentKind {
    /// Java 国际服（host:port）
    JavaServer,
    Download,
}

/// 设置页可编辑字段。
/// 渲染与按键处理共用这一份定义——两处各写一遍必然不同步。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingField {
    GameDir,
    DownloadDir,
    RoleName,
    GridCols,
    GridRows,
    /// 顶部大图区：ASCII 常驻 vs 有图显示图
    AsciiAlways,
    /// 世界选中框流光描边动画：←→ 开关
    WorldAnim,
    /// 界面主题：←→ 循环预览，Enter 打开主题列表浮层
    Theme,
    /// 游戏运行时自动截图（隐私相关）
    AutoShot,
}

/// 设置项的分组——**只影响排版**（设置页的小标题/分节），
/// 字段导航顺序仍然是 [`SettingField::ALL`]。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingGroup {
    /// 路径：游戏目录、下载目录
    Path,
    /// 联机：角色名
    Online,
    /// 资源中心：网格排布
    Resource,
    /// 界面：顶部大图 / 动画 / 主题
    Ui,
    /// 隐私：自动截图
    Privacy,
}

impl SettingGroup {
    pub fn title(self) -> &'static str {
        match self {
            SettingGroup::Path => "路径",
            SettingGroup::Online => "联机",
            SettingGroup::Resource => "资源中心",
            SettingGroup::Ui => "界面",
            SettingGroup::Privacy => "隐私",
        }
    }
}

/// 设置页动作按钮
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingBtn {
    Save,
    Revert,
    Reload,
    Defaults,
}

/// 开关型字段的显示文本
fn onoff(b: bool) -> String {
    if b { "开" } else { "关（默认）" }.into()
}

/// `~` 展开成 HOME（配置里写 `~/.minecraft` 是常态）
fn expand_tilde(v: &str) -> String {
    match v.strip_prefix("~/") {
        Some(rest) => std::env::var("HOME")
            .map(|h| PathBuf::from(h).join(rest).display().to_string())
            .unwrap_or_else(|_| v.to_string()),
        None => v.to_string(),
    }
}

impl SettingField {
    pub const ALL: [SettingField; 9] = [
        // 顺序 = 设置页的排版顺序（按分组排好，见 `group`）
        SettingField::GameDir,
        SettingField::DownloadDir,
        SettingField::RoleName,
        SettingField::GridCols,
        SettingField::GridRows,
        SettingField::AsciiAlways,
        SettingField::WorldAnim,
        SettingField::Theme,
        SettingField::AutoShot,
    ];

    /// 所属分组（设置页拿它画小标题）
    pub fn group(self) -> SettingGroup {
        match self {
            SettingField::GameDir | SettingField::DownloadDir => SettingGroup::Path,
            SettingField::RoleName => SettingGroup::Online,
            SettingField::GridCols | SettingField::GridRows => SettingGroup::Resource,
            SettingField::AsciiAlways | SettingField::WorldAnim | SettingField::Theme => {
                SettingGroup::Ui
            }
            SettingField::AutoShot => SettingGroup::Privacy,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            SettingField::GameDir => "游戏目录",
            SettingField::DownloadDir => "下载目录",
            SettingField::RoleName => "角色名",
            SettingField::GridCols => "网格列数",
            SettingField::GridRows => "网格行数",
            SettingField::AutoShot => "自动截图",
            SettingField::AsciiAlways => "顶部大图",
            SettingField::WorldAnim => "选中框动画",
            SettingField::Theme => "主题",
        }
    }

    /// 输入框下方的一行说明
    pub fn hint(self) -> &'static str {
        match self {
            SettingField::GameDir => "Minecraft 实例根目录（versions/ 与 saves/ 的父级）",
            SettingField::DownloadDir => "资源下载的落盘位置",
            SettingField::RoleName => "联机进服用的角色名（写入 role_name.txt）",
            SettingField::GridCols => "资源网格每行卡片数，←→ 增减",
            SettingField::GridRows => "资源网格每列卡片数，←→ 增减",
            SettingField::AutoShot => "游戏运行时定期抓屏存到本地（隐私相关），←→ 开关",
            SettingField::AsciiAlways => {
                "开 = mctl 立体 ASCII 常驻顶部，关 = 有游戏截图就显示图（默认）"
            }
            SettingField::WorldAnim => "世界列表选中框的光点描边动画，←→ 开关（默认开）",
            SettingField::Theme => "整套界面配色：←→ 循环预览 · Enter 打开列表（Esc 还原）",
        }
    }

    /// 数值字段支持 ←→ 增减，不必逐字输入
    pub fn is_num(self) -> bool {
        matches!(
            self,
            SettingField::GridCols
                | SettingField::GridRows
                | SettingField::AutoShot
                | SettingField::WorldAnim
                | SettingField::Theme
        )
    }

    /// 行尾的操作提示
    pub fn action_hint(self) -> &'static str {
        match self {
            SettingField::Theme => "←→ 切换 · Enter 列表",
            SettingField::WorldAnim => "←→ 开关",
            _ if self.is_num() => "←→ 增减",
            _ => "Enter 编辑",
        }
    }

    pub fn value(self, app: &App) -> String {
        match self {
            SettingField::GameDir => app.mc_dir.display().to_string(),
            SettingField::DownloadDir => app.download_dir.display().to_string(),
            SettingField::RoleName => app.role_name.clone(),
            SettingField::GridCols => app.grid_cols.to_string(),
            SettingField::GridRows => app.grid_rows.to_string(),
            SettingField::AutoShot => onoff(app.auto_shot),
            SettingField::AsciiAlways => match app.ascii_always {
                true => "ASCII 常驻".into(),
                false => "有图显示图（默认）".into(),
            },
            SettingField::WorldAnim => match app.world_anim {
                true => "开（默认）".into(),
                false => "关".into(),
            },
            SettingField::Theme => crate::theme::theme_label(&app.theme),
        }
    }

    /// 校验并应用。Err 时不改动 App，由调用方把原因显示到状态栏。
    pub fn apply(self, app: &mut App, raw: &str) -> Result<()> {
        let v = raw.trim();
        if v.is_empty() {
            bail!("{} 不能为空", self.label());
        }
        match self {
            SettingField::GameDir => {
                let p = PathBuf::from(expand_tilde(v));
                if !p.is_dir() {
                    bail!("目录不存在: {}", p.display());
                }
                app.mc_dir = p;
                // 实例列表跟着变，选中项要夹住
                app.instances = launcher::scan_instances(&app.mc_dir);
                App::append_bedrock_instances(&mut app.instances);
                app.selected_instance = app
                    .selected_instance
                    .min(app.instances.len().saturating_sub(1));
                app.refresh_worlds();
            }
            SettingField::DownloadDir => {
                let p = PathBuf::from(expand_tilde(v));
                std::fs::create_dir_all(&p)
                    .map_err(|e| anyhow!("无法创建 {}: {e}", p.display()))?;
                app.download_dir = p.clone();
                app.download_dir_str = p.display().to_string();
            }
            SettingField::RoleName => {
                storage::save_role(v)?;
                app.role_name = v.to_string();
            }
            SettingField::AutoShot => {
                app.auto_shot = matches!(v, "开" | "on" | "true" | "1" | "启用");
            }
            SettingField::AsciiAlways => {
                app.ascii_always = matches!(v, "开" | "on" | "true" | "1" | "启用" | "ASCII 常驻");
            }
            SettingField::WorldAnim => {
                app.world_anim = matches!(v, "开" | "on" | "true" | "1" | "启用");
            }
            SettingField::Theme => {
                // 允许输入主题 id 或列表里的显示名（两边都对得上）
                let ids = crate::theme::theme_ids();
                let found = ids
                    .iter()
                    .find(|id| id.as_str() == v || crate::theme::theme_label(id) == v);
                match found {
                    Some(id) => {
                        app.theme = id.clone();
                        crate::theme::set_theme(&app.theme);
                    }
                    None => bail!("没有这个主题（←→ 切换，Enter 打开列表）"),
                }
            }
            SettingField::GridCols | SettingField::GridRows => {
                let n: usize = v.parse().map_err(|_| anyhow!("请输入整数"))?;
                if !(1..=12).contains(&n) {
                    bail!("{} 范围 1-12", self.label());
                }
                if self == SettingField::GridCols {
                    app.grid_cols = n;
                } else {
                    app.grid_rows = n;
                }
            }
        }
        Ok(())
    }

    /// ←→ 增减（仅数值字段）
    pub fn bump(self, app: &mut App, delta: i32) {
        match self {
            SettingField::GridCols => {
                app.grid_cols = (app.grid_cols as i32 + delta).clamp(1, 12) as usize
            }
            SettingField::GridRows => {
                app.grid_rows = (app.grid_rows as i32 + delta).clamp(1, 12) as usize
            }
            // 开关没有中间态，±1 都当成切换
            SettingField::AutoShot => app.auto_shot = !app.auto_shot,
            SettingField::AsciiAlways => app.ascii_always = !app.ascii_always,
            SettingField::WorldAnim => app.world_anim = !app.world_anim,
            // 主题：循环切换整个列表，切到哪个立刻变色（预览即所得）
            SettingField::Theme => {
                let ids = crate::theme::theme_ids();
                let cur = ids.iter().position(|i| *i == app.theme).unwrap_or(0);
                let next = (cur as i32 + delta).rem_euclid(ids.len() as i32) as usize;
                app.theme = ids[next].clone();
                crate::theme::set_theme(&app.theme);
            }
            _ => {}
        }
    }
}

impl SettingBtn {
    pub const ALL: [SettingBtn; 4] = [
        SettingBtn::Save,
        SettingBtn::Revert,
        SettingBtn::Reload,
        SettingBtn::Defaults,
    ];
    pub fn label(self) -> &'static str {
        match self {
            SettingBtn::Save => "保存",
            SettingBtn::Revert => "放弃",
            SettingBtn::Reload => "重读",
            SettingBtn::Defaults => "默认",
        }
    }
}

/// 世界选中框"流光描边"动画时长（毫秒）。
/// 选中变化 → 框从零开始被光点顺时针描出来；描完边框常驻。
pub const WORLD_ANIM_MS: u64 = 420;

/// 「视图」侧栏选中块"刷色"动画时长（毫秒）。
/// 切换视图 → 底色块从左往右刷过去；刷完底色常驻。
pub const VIEW_ANIM_MS: u64 = 160;

/// 服务器卡片选中框的"流光描边"动画时长（毫秒）。
/// 网格内换选中卡片 → 外框被光点从左往右描一圈；描完边框常驻。
pub const SEL_ANIM_MS: u64 = 420;

/// 资源中心「来源条」选中块的"刷色"动画时长（毫秒）。
/// 切换来源（[ / ]）→ 底色块从左往右刷过去，刷过的地方字色同步翻色；
/// 刷完底色常驻。节奏与侧栏视图切换同一套。
pub const SRC_ANIM_MS: u64 = 200;

/// 主题选择器浮层状态（设置页「主题」行 Enter 打开）
#[derive(Debug, Clone)]
pub struct ThemePicker {
    /// 光标所在主题下标（`crate::theme::theme_ids`）
    pub cursor: usize,
    /// 打开时的主题 id（Esc 还原用）
    pub saved: String,
}

/// 服务器页的三个标签（浏览器式，Ctrl+1/2 直达）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ServerTab {
    /// 推荐 + mctl 自定义 + 当前实例用户服务器
    #[default]
    All,
    /// 当前实例 `servers.dat` 里的服务器（与 Java 双向同步）
    User,
    /// 内置精选推荐
    Recommended,
}

impl ServerTab {
    pub const ALL: [ServerTab; 3] = [ServerTab::All, ServerTab::User, ServerTab::Recommended];

    pub fn label(self) -> &'static str {
        match self {
            ServerTab::All => "Java 国际服",
            ServerTab::User => "用户服务器",
            ServerTab::Recommended => "推荐服务器",
        }
    }

    /// 标签上的快捷键提示（Ctrl+1/2；All 额外给 Ctrl+3）
    pub fn hint(self) -> &'static str {
        match self {
            ServerTab::All => "^3",
            ServerTab::User => "^1",
            ServerTab::Recommended => "^2",
        }
    }

    pub fn index(self) -> usize {
        match self {
            ServerTab::All => 0,
            ServerTab::User => 1,
            ServerTab::Recommended => 2,
        }
    }
}

/// 在服务器页按 a 打开的添加表单。
#[derive(Debug, Clone, Default)]
pub struct ServerAddForm {
    pub addr: String,
    pub name: String,
    /// 0 = 地址，1 = 名称
    pub focus: usize,
    pub error: String,
}

pub struct App {
    // ---- 顶栏 / 横幅 ----
    pub home_view: HomeView,
    pub quit: bool,
    pub status: String,
    pub instances: Vec<Instance>,
    pub selected_instance: usize,
    pub accounts: Vec<Account>,
    pub selected_account: usize,
    pub worlds: Vec<WorldInfo>,
    pub selected_world: usize,
    /// 服务器页：Java 国际服（内置精选 + 自定义），状态来自原版 Server List Ping
    pub servers: Vec<crate::api::mcserver::JavaServer>,
    pub selected_server: usize,
    pub server_loading: bool,
    pub server_status: String,
    pub server_launching: bool,
    /// 服务器页当前标签 + 每个标签各自记住的选中项
    pub server_tab: ServerTab,
    pub server_tab_sel: [usize; 3],
    /// 当前实例 `servers.dat` 的条目（与 Java 客户端共用）
    pub user_servers: Vec<crate::api::mcserver::JavaServer>,
    pub user_servers_loading: bool,
    pub user_servers_error: Option<String>,
    /// 添加表单（Some = 浮层打开，按键只进表单）
    pub server_add: Option<ServerAddForm>,
    /// 服务器标签的屏幕区域（鼠标点击用；每帧渲染时回填）
    pub server_tab_rects: Vec<(ratatui::layout::Rect, ServerTab)>,
    /// 界面聚焦面板（Tab 轮换）
    pub home_focus: HomeFocus,
    /// 当前聚焦面板全屏（f 切换）
    pub home_panel_full: bool,
    /// quickPlay 直进目标世界（目录名）
    pub launch_world: Option<String>,
    pub user_name: String,

    // ---- 资源大厅（迁移自 x19dl） ----
    pub cred: Option<Credentials>,
    pub map_page: MapPage,
    pub focus: Focus,
    pub category: usize,
    pub grid_query: String,
    pub grid_searching: bool,
    pub grid_sort: NeteaseSort,
    pub grid_offset: i64,
    pub grid_items: Vec<ShopItem>,
    pub grid_selected: usize,
    pub grid_cols: usize,
    pub grid_rows: usize,
    /// 启动后周期性自动截图（隐私相关，**默认关**）。
    /// 自动截图 = 游戏运行时持续把画面存到本地；不想被记录就别开。
    pub auto_shot: bool,
    pub grid_end: bool,
    pub grid_auto_all: bool,
    pub grid_covers: Vec<Option<StatefulProtocol>>,
    pub cover_queue: Vec<usize>,
    /// Java 服务器网格封面（索引与列表一一对应，同资源大厅卡片）
    pub srv_covers: Vec<Option<StatefulProtocol>>,
    pub srv_cover_queue: Vec<usize>,
    /// 封面批次：切标签/重载时 +1，旧批次的结果直接丢弃
    pub srv_cover_gen: u64,
    /// 服务器没有 favicon → 卡片退回纯文字
    pub srv_cover_none: Vec<bool>,
    /// 网易网格 BE（基岩版）标记缓存（None=未检查或检查失败）
    pub grid_be: Vec<Option<bool>>,
    pub be_queue: Vec<usize>,
    /// 检查失败重试计数（上限 3 次）
    pub be_tries: Vec<u8>,
    pub detail: Option<ItemDetail>,
    pub detail_versions: Vec<DownloadSub>,
    pub detail_selected: usize,
    /// 详情页焦点（Tab 切换左图/右版本）
    pub detail_focus: DetailFocus,
    /// 详情页 Info 焦点滚动偏移（详情正文）
    pub detail_scroll: u16,
    /// 下载位置编辑缓冲（约束框）
    pub download_dir_str: String,
    pub detail_cover: Option<StatefulProtocol>,
    pub detail_loading: bool,
    pub grid_loading: bool,
    pub login_user: String,
    pub login_pass: String,
    /// 验证码输入缓冲（login_input_step == 2 时）
    pub login_captcha: String,
    /// 已取到验证码图、等待用户输入的中间态。
    /// 必须留着它——`captcha_id` 与 cookie 会话绑定，隔一次请求就失效。
    pub login_challenge: Option<crate::api::auth::LoginChallenge>,
    pub login_input: bool,
    /// 账户面板登录输入当前字段（0=账号 1=密码）
    pub login_input_step: u8,
    pub login_status: Option<String>,
    /// 客户端登录名称（联机角色名，左下角状态栏编辑；空则用 user_id）
    pub role_name: String,
    /// 角色名编辑态（状态栏聚焦 + Enter 进入）
    pub role_input: bool,
    pub picker: Arc<ratatui_image::picker::Picker>,
    pub tx: mpsc::Sender<TaskMsg>,
    pub rx: mpsc::Receiver<TaskMsg>,
    pub download_dir: PathBuf,
    pub mc_dir: PathBuf,
    pub launch_status: String,
    pub launching: bool,
    pub game_log: Vec<String>,
    pub game_pid: Option<u32>,
    pub dl: DlPage,
    /// Modrinth 封面缓存（网格视图）
    pub mr_covers: Vec<Option<StatefulProtocol>>,
    pub mr_cover_queue: Vec<usize>,
    /// 世界缩略图缓存（世界选择列表用）。
    /// 键 = "<实例id>/<世界目录名>"，重扫世界（refresh_worlds）时清空。
    /// 值 None = 加载中或加载失败（画 ◻ 占位，本轮不重试）。
    pub world_icons: HashMap<String, Option<StatefulProtocol>>,
    /// 世界选中框描边动画的起点。选中变化/重扫世界时置为 now，
    /// 界面按经过时间算描边进度；None = 直接显示完整框。
    pub world_anim_start: Option<std::time::Instant>,
    /// 「视图」侧栏选中块刷色动画的起点（换视图时置 now）。
    /// 界面按经过时间算刷到什么位置；None = 直接刷满。
    pub view_anim_start: Option<std::time::Instant>,
    /// 资源中心「来源条」选中块刷色动画的起点（切换来源时置 now）。
    /// 界面按经过时间算刷到什么位置；None = 直接刷满。
    pub src_anim_start: Option<std::time::Instant>,
    /// 服务器卡片网格选中块刷色动画的起点（网格内换选中项时置 now）。
    /// 界面按经过时间算刷到什么位置；None = 直接刷满。
    pub sel_anim_start: Option<std::time::Instant>,
    /// 服务器卡片网格当前列数（UI 每帧回填，↑↓ 按它跳一行）
    pub server_grid_cols: usize,
    /// 联机服务器卡片网格的可视窗口起点（UI 每帧校正）
    pub srv_scroll: usize,
    /// 设置项：世界选中框描边动画开关（默认开，存 settings.json 的 `world_anim`）
    pub world_anim: bool,
    /// 设置项：主题 id（`theme::theme_ids` 里的取值，默认 "mctl" 经典）
    pub theme: String,
    /// 主题选择器浮层（设置页「主题」行 Enter 打开）：Some = 开着，
    /// ↑↓ 即时预览、Enter 确定、Esc 还原
    pub theme_picker: Option<ThemePicker>,
    /// Modrinth 分类（MODRINTH_CATEGORIES 索引）
    pub mr_category: usize,
    /// Modrinth 搜索输入态（/ 进入）
    pub mr_searching: bool,
    /// Modrinth 是否已自动加载过（避免重复请求）
    pub mr_loaded: bool,
    /// Modrinth 版本条件（空=全部；v 进入编辑）
    pub mr_version: String,
    pub mr_version_editing: bool,
    /// Modrinth 详情页（版本选择）
    pub mr_detail_open: bool,
    pub mr_detail_loading: bool,
    pub mr_detail_title: String,
    pub mr_versions: Vec<launcher::modpack::MrVersion>,
    /// 版本详情页选中的**行**（树状结构下行号 ≠ 版本索引）
    pub mr_row_sel: usize,
    /// 折叠的分组 key（MC 号 或 `MC/loader`）
    pub mr_collapsed: std::collections::HashSet<String>,
    /// 资源中心当前来源
    pub source: ResourceSource,
    /// 全局安装任务队列（下载中心显示）
    pub install_tasks: Vec<InstallTask>,
    /// 下一个下载中心任务 id
    pub next_task_id: u64,
    /// 顶部大图区：true = ASCII 常驻；false = 有图显示图（默认）
    pub ascii_always: bool,
    /// 完整日志视图开关
    pub home_log_full: bool,
    /// LittleSkin OAuth 应用登记状态
    pub ls: LsPage,
    /// 横幅游戏截图（自动截图任务产出，按世界存档分目录，screenshots 用户图不参与）
    pub banner_shot: Option<StatefulProtocol>,
    /// 账户面板头像（~/.local/share/mctl/avatar.png；LittleSkin 登录后自动从皮肤裁剪）
    pub avatar: Option<StatefulProtocol>,
    /// 全屏登录界面（Some=显示）
    /// 插件资源页（provides/versions 契约，核心只做通用渲染）
    pub plugin_bin: Option<PathBuf>,
    pub plugin_items: Vec<VersionItem>,
    pub plugin_selected: usize,
    pub plugin_status: String,
    pub plugin_busy: bool,
    pub login_screen: Option<LoginScreen>,
    /// 自动截图循环任务（游戏运行时：开场 1 分钟 3 张 + 每 30 分钟一张；退出时中止）
    pub shot_task: Option<tokio::task::JoinHandle<()>>,
    /// 命令模式（: 进入）：输入缓冲与激活态
    pub shell_input: bool,
    pub shell_buf: String,
    pub shell_cursor: usize,
    /// TUI 窗口管理器外壳
    pub wm: tui_wm_core::Wm,
    /// 配置根目录（`~/.config/mctl`；测试传临时目录）。session.json / wm.toml /
    /// .minecraft / downloads 全部挂在它下面。
    pub cfg_root: PathBuf,
    /// 最近一次终端尺寸（Resize 事件更新，供鼠标坐标换算）
    pub term_size: (u16, u16),

    // ---- 最近历史 ----
    /// 跨会话增长的用户历史，存 `recents.json`。与 session.json 分开：
    /// session 是"启动记忆"，recents 是"我去过哪"，混在一起会让 session
    /// 每次启动都被改写。
    pub recents: storage::Recents,

    // ---- 设置页（可编辑 + 按钮） ----
    /// 焦点：0..4 = 字段（SettingField::ALL 顺序），4..7 = 按钮（SettingBtn::ALL 顺序）
    pub set_focus: usize,
    /// 正在编辑当前字段
    pub set_editing: bool,
    /// 编辑缓冲
    pub set_buf: String,
    /// 有未保存的改动
    pub set_dirty: bool,
    /// 状态提示（保存结果 / 校验失败原因）
    pub set_msg: String,
    /// 进入设置页时的快照，用于「放弃」
    pub set_snapshot: Option<storage::Settings>,

    // ---- GitHub 插件发现（api::github） ----
    /// 原始搜索结果（缓存，配额稀缺所以不重复请求）
    pub gh_results: Vec<crate::api::github::Repo>,
    /// 本地过滤后的下标（细化搜索不花配额）
    pub gh_filtered: Vec<usize>,
    pub gh_selected: usize,
    /// 请求在途
    pub gh_searching: bool,
    /// 已加载过（进页面不重复请求）
    pub gh_loaded: bool,
    /// 搜索配额剩余提示（未认证 10 次/分钟）
    pub gh_rate: String,
    pub gh_total: u64,
    /// 搜索输入态（/ 进入，Enter 才发请求）
    pub gh_search_input: bool,
}

impl App {
    pub fn new() -> Result<Self> {
        Self::new_from(storage::config_dir())
    }

    /// 插件根目录（`<cfg_root>/plugin`）。寻址统一走这里，别再散落 `plugin_root`。
    pub fn plugin_root(&self) -> PathBuf {
        self.cfg_root.join(crate::plugin::PLUGIN_DIR)
    }

    /// 指定配置根目录构造。
    /// /// 测试必须走这个入口并传临时目录：`new` 会让 13 个测试去读写真实的
    /// `~/.config/mctl`（含扫描实例、建目录、整合包 cleanup、存档迁移）。
    pub fn new_from(cfg: PathBuf) -> Result<Self> {
        let (tx, rx) = mpsc::channel(64);
        // 凭据跟着 cfg 走：`new_from(临时目录)` 不该读到本机真实账号（测试卫生）
        let cred = storage::load_from(&cfg).ok();
        let mut app = Self {
            home_view: HomeView::Instance,
            quit: false,
            status: String::new(),
            instances: Vec::new(),
            selected_instance: 0,
            accounts: vec![
                Account {
                    provider: "[CN] 网易".into(),
                    name: "用户".into(),
                    logged_in: true,
                },
                Account {
                    provider: "[INT] 微软".into(),
                    name: "未登录".into(),
                    logged_in: false,
                },
            ],
            selected_account: 0,
            worlds: Vec::new(),
            selected_world: 0,
            servers: Vec::new(),
            selected_server: 0,
            server_loading: false,
            server_status: String::new(),
            server_launching: false,
            server_tab: ServerTab::All,
            server_tab_sel: [0; 3],
            user_servers: Vec::new(),
            user_servers_loading: false,
            user_servers_error: None,
            server_add: None,
            server_tab_rects: Vec::new(),
            home_focus: HomeFocus::Banner,
            home_panel_full: false,
            launch_world: None,
            user_name: "未登录".into(),

            cred,
            // 资源大厅不再有内嵌登录页：未登录由 UI 显示「请登录」，登录统一走登录专页
            map_page: MapPage::Grid,
            cfg_root: cfg.clone(),
            focus: Focus::Grid,
            category: 1,
            grid_offset: 0,
            grid_query: String::new(),
            grid_searching: false,
            grid_sort: NeteaseSort::Comprehensive,
            grid_items: Vec::new(),
            grid_selected: 0,
            grid_cols: 2,
            grid_rows: 3,
            // 隐私相关：默认关，稍后由 settings.json 覆盖
            auto_shot: false,
            grid_end: false,
            grid_auto_all: false,
            grid_covers: Vec::new(),
            cover_queue: Vec::new(),
            srv_covers: Vec::new(),
            srv_cover_queue: Vec::new(),
            srv_cover_gen: 0,
            srv_cover_none: Vec::new(),
            grid_be: Vec::new(),
            be_queue: Vec::new(),
            be_tries: Vec::new(),
            detail: None,
            detail_versions: Vec::new(),
            detail_selected: 0,
            detail_focus: DetailFocus::Versions,
            detail_scroll: 0,
            detail_cover: None,
            detail_loading: false,
            grid_loading: false,
            login_user: String::new(),
            login_pass: String::new(),
            login_input: false,
            login_input_step: 0,
            recents: storage::Recents::default(),
            login_captcha: String::new(),
            login_challenge: None,
            role_name: storage::load_role(),
            role_input: false,
            login_status: None,
            picker: Arc::new(pick_picker()),
            tx,
            rx,
            download_dir: cfg.join("downloads"),
            download_dir_str: cfg.join("downloads").display().to_string(),
            mc_dir: cfg.join(".minecraft"),
            launch_status: String::new(),
            launching: false,
            game_log: Vec::new(),
            game_pid: None,
            dl: DlPage::default(),
            source: ResourceSource::NetEase,
            mr_covers: Vec::new(),
            mr_cover_queue: Vec::new(),
            world_icons: HashMap::new(),
            world_anim_start: Some(std::time::Instant::now()),
            // 启动时选中块直接是满的：刷色只在**换视图**时播
            view_anim_start: None,
            // 来源条同理：切来源才刷色，进页面直接是满的
            src_anim_start: None,
            // 卡片网格同理：启动时不播，动选择才播
            sel_anim_start: None,
            server_grid_cols: 3,
            srv_scroll: 0,
            world_anim: true,
            theme: crate::theme::LEGACY_ID.to_string(),
            theme_picker: None,
            mr_category: 0,
            mr_searching: false,
            mr_loaded: false,
            mr_version: String::new(),
            mr_version_editing: false,
            mr_detail_open: false,
            mr_detail_loading: false,
            mr_detail_title: String::new(),
            mr_versions: Vec::new(),
            mr_row_sel: 0,
            mr_collapsed: std::collections::HashSet::new(),
            install_tasks: Vec::new(),
            next_task_id: 1,
            ascii_always: false,
            home_log_full: false,
            ls: LsPage::default(),
            banner_shot: None,
            avatar: None,
            plugin_bin: None,
            plugin_items: Vec::new(),
            plugin_selected: 0,
            plugin_status: String::new(),
            plugin_busy: false,
            login_screen: None,
            shot_task: None,
            shell_input: false,
            shell_buf: String::new(),
            shell_cursor: 0,
            wm: Self::build_wm(&cfg),
            term_size: crossterm::terminal::size().unwrap_or((80, 24)),

            set_focus: 0,
            set_editing: false,
            set_buf: String::new(),
            set_dirty: false,
            set_msg: String::new(),
            set_snapshot: None,

            gh_results: Vec::new(),
            gh_filtered: Vec::new(),
            gh_selected: 0,
            gh_searching: false,
            gh_loaded: false,
            gh_rate: String::new(),
            gh_total: 0,
            gh_search_input: false,
        };
        // settings.json 覆盖默认路径/网格（必须在 scan_instances 之前）
        let st = storage::load_settings(&app.cfg_root);
        if let Some(v) = &st.game_dir {
            app.mc_dir = PathBuf::from(v);
        }
        if let Some(v) = &st.download_dir {
            app.download_dir = PathBuf::from(v);
            app.download_dir_str = v.clone();
        }
        if let Some(v) = st.grid_cols {
            app.grid_cols = v.clamp(1, 12);
        }
        if let Some(v) = st.grid_rows {
            app.grid_rows = v.clamp(1, 12);
        }
        // 隐私：自动截图默认关。老配置没有这个键时 `auto_shot` 是 None，
        // unwrap_or(false) 保证「没配过」== 不开，而不是替用户选开。
        app.auto_shot = st.auto_shot.unwrap_or(false);
        // 缺失 → false（有图显示图），不替用户选 ASCII 常驻
        app.ascii_always = st.ascii_always.unwrap_or(false);
        // 描边动画默认开（老配置没有这个键 → None → 开，旧行为不变）
        app.world_anim = st.world_anim.unwrap_or(true);
        // 主题：没配过 → mctl 经典配色（和加主题功能之前完全一样）
        app.theme = st
            .theme
            .clone()
            .unwrap_or_else(|| crate::theme::LEGACY_ID.to_string());
        crate::theme::set_theme(&app.theme);
        app.recents = storage::load_recents(&app.cfg_root);
        app.instances = launcher::scan_instances(&app.mc_dir);
        Self::append_bedrock_instances(&mut app.instances);
        // 启动记忆：恢复上次会话状态。每个还原字段都要夹到当前有效范围内——
        // session.json 可能来自旧版本、手改过、或被截断，越界索引会 panic。
        let session = storage::load_session(&app.cfg_root);
        app.selected_instance = session
            .selected_instance
            .min(app.instances.len().saturating_sub(1));
        app.refresh_worlds();
        app.selected_world = session
            .selected_world
            .min(app.worlds.len().saturating_sub(1));
        app.selected_account = session
            .selected_account
            .min(app.accounts.len().saturating_sub(1));
        if session.home_view < HomeView::ALL.len() {
            app.home_view = HomeView::ALL[session.home_view];
            app.sync_wm_from_view();
        }
        if session.resource_source < ResourceSource::ALL.len() {
            app.source = ResourceSource::ALL[session.resource_source];
        }
        // category 是无符号，直接 ALL_CATEGORIES[self.category] 取值，务必夹住
        app.category = session
            .category
            .min(crate::domain::ALL_CATEGORIES.len().saturating_sub(1));
        if !session.role_name.is_empty() {
            app.role_name = session.role_name;
        }
        // 注意：终端尺寸不记忆。crossterm::terminal::size 在启动时永远是准的，
        // 而上次的尺寸在用户改过终端窗口后就是错的——记它反而会让首帧按旧尺寸布局。
        // 恢复聚焦面板
        app.home_focus = HomeFocus::from_index(session.home_focus);
        app.restore_ls();
        // 清理上次可能残留的整合包/Forge 安装临时目录
        launcher::modpack::cleanup(&app.mc_dir);
        app.spawn_banner_shot();
        app.spawn_load_avatar();
        Ok(app)
    }

    /// 窗口管理器配置文件路径：`~/.config/mctl/wm.toml`（类 hyprland）
    fn wm_config_path(cfg: &Path) -> PathBuf {
        cfg.join("wm.toml")
    }

    /// 加载 wm 配置；文件缺失时写入带注释的默认配置并落盘（用户可修改）
    fn load_wm_config(cfg: &Path) -> tui_wm_core::WmConfig {
        let path = Self::wm_config_path(cfg);
        if !path.exists() {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(&path, tui_wm_core::WmConfig::sample_toml());
        }
        tui_wm_core::WmConfig::load(&path)
    }

    /// 构建窗口管理器外壳：主区 4 个视图窗口 + dock 固定面板（账户/视图切换/状态）
    fn build_wm(cfg: &Path) -> tui_wm_core::Wm {
        use tui_wm_core::Wm;
        let cfg = Self::load_wm_config(cfg);
        let mut wm = Wm::with_config(&cfg);
        // 主区视图窗口（可停靠；视图名侧栏已高亮，主区不重复占一行标题栏）
        for id in [
            win_id::INSTANCE,
            win_id::SERVER,
            win_id::RESOURCE,
            win_id::DOWNLOADS,
            win_id::PLUGINS,
            win_id::SETTINGS,
        ] {
            wm.add_window(id, "").show_title = false;
        }
        // dock 固定面板（自带边框标题，无 wm 标题栏，不可拖出）
        let a = wm.add_window(win_id::ACCOUNTS, "账户");
        a.docked = true;
        a.pinned = true;
        a.show_title = false;
        a.flex = 4; // 头像 + 账户列表 + 登录项 + 提示

        let v = wm.add_window(win_id::VIEWS, "视图");
        v.docked = true;
        v.pinned = true;
        v.show_title = false;
        // 8 个视图 + 上下边框需要 10 行。flex=2 在 34 行终端只分到 9 行，
        // 会把最后一个视图（设置）挤出可视区。
        v.flex = 3;
        let s = wm.add_window(win_id::STATUS, "状态");
        s.docked = true;
        s.pinned = true;
        s.show_title = false;
        s.flex = 1;
        // 配置文件按窗口 id 覆盖默认属性（windows.<id> 表）
        for (k, opts) in &cfg.windows {
            if let Ok(id) = k.parse::<u32>() {
                wm.apply_opts_to(id, opts);
            }
        }
        wm.focus(win_id::INSTANCE);
        wm
    }

    /// wm 主区窗口 → HomeView 同步（Tab 切换窗口 / 停靠后主区换窗时调用）
    pub fn sync_view_from_wm(&mut self) {
        let view = match self.wm.main_win {
            win_id::INSTANCE => HomeView::Instance,
            win_id::SERVER => HomeView::Server,
            win_id::RESOURCE => HomeView::Resource,
            win_id::DOWNLOADS => HomeView::Download,
            win_id::PLUGINS => HomeView::Plugins,
            win_id::SETTINGS => HomeView::Settings,
            _ => HomeView::Instance,
        };
        if self.home_view != view {
            self.set_home_view(view);
            match view {
                HomeView::Resource => self.enter_maps(),
                HomeView::Server => self.spawn_load_servers(),
                _ => {}
            }
        }
    }

    /// 鼠标事件路由：点击 dock「视图」项切换主区窗口，其余交 wm 外壳
    pub fn handle_mouse(&mut self, ev: crossterm::event::MouseEvent) {
        use crossterm::event::{MouseButton, MouseEventKind};
        use ratatui::layout::Rect;
        // 事件循环不持有最新 area，用最近一次终端尺寸近似
        let area = Rect::new(0, 1, self.term_size.0, self.term_size.1.saturating_sub(2));
        // 服务器标签页：点击直接切换（只在自己画过标签时生效）
        if self.home_view == HomeView::Server
            && matches!(ev.kind, MouseEventKind::Down(MouseButton::Left))
        {
            if let Some((_, tab)) = self
                .server_tab_rects
                .iter()
                .find(|(r, _)| r.contains((ev.column, ev.row).into()))
            {
                let tab = *tab;
                self.set_server_tab(tab);
                return;
            }
        }
        // 点击 dock「视图」窗口内的某项 → 切换主区窗口
        if matches!(ev.kind, MouseEventKind::Down(MouseButton::Left)) {
            if let Some(rect) = self.wm.window_rect(area, win_id::VIEWS) {
                let (x, y) = (ev.column, ev.row);
                if rect.contains((x, y).into()) {
                    let idx = y.saturating_sub(rect.y + 1) as usize;
                    if let Some(view) = HomeView::ALL.get(idx) {
                        let view = *view;
                        self.set_home_view(view);
                        match view {
                            HomeView::Resource => self.enter_maps(),
                            HomeView::Server => self.spawn_load_servers(),
                            _ => {}
                        }
                        self.wm.focus(self.view_win_id());
                        return;
                    }
                }
            }
        }
        let consumed = self.wm.handle_mouse(ev, area);
        if consumed {
            self.sync_view_from_wm();
        }
    }

    /// 当前选中世界对应的自动截图目录（mctl/shots/<世界目录名>/）；无世界时用根目录
    fn shots_dir(&self) -> PathBuf {
        let base = self.mc_dir.join("mctl").join("shots");
        match self.worlds.get(self.selected_world) {
            Some(w) => base.join(&w.name),
            None => base,
        }
    }

    /// 扫描截图目录下最新的自动截图（mctl- 前缀，按修改时间）
    fn latest_screenshot(&self) -> Option<PathBuf> {
        let dir = self.shots_dir();
        std::fs::read_dir(dir)
            .ok()?
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("mctl-") && n.ends_with(".png"))
            })
            .max_by_key(|p| {
                std::fs::metadata(p)
                    .and_then(|m| m.modified())
                    .unwrap_or(std::time::UNIX_EPOCH)
            })
    }

    /// 启动自动截图循环：开场立即 + 30s + 60s（开场 1 分钟），之后每 30 分钟一张。
    /// 截图存 mctl/shots/<世界>/，与用户 F2 截图（screenshots/）完全隔离。
    /// 后续做设置页（隐私选项：开关 + 频率）。
    pub fn start_shots(&mut self) {
        // 隐私开关，默认关：不开就根本不起这个循环。
        // 放在最前面而不是让调用方判断，这样将来任何新增的调用点都不会漏掉。
        if !self.auto_shot {
            return;
        }
        if self.shot_task.is_some() {
            return;
        }
        let Some((cmd, args)) = detect_shoot_cmd() else {
            self.launch_status = "自动截图不可用（缺 grim/import/scrot）".into();
            return;
        };
        let dir = self.shots_dir();
        if let Err(e) = std::fs::create_dir_all(&dir) {
            self.launch_status = format!("截图目录创建失败: {e}");
            return;
        }
        self.shot_task = Some(tokio::spawn(async move {
            let dir = dir;
            async fn shoot(cmd: &str, args: &[String], dir: &Path) {
                let ts = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                let out = dir.join(format!("mctl-{ts}.png"));
                let _ = tokio::process::Command::new(cmd)
                    .args(args)
                    .arg(&out)
                    .status()
                    .await;
                trim_shots(dir);
            }
            shoot(&cmd, &args, &dir).await;
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            shoot(&cmd, &args, &dir).await;
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            shoot(&cmd, &args, &dir).await;
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(30 * 60)).await;
                shoot(&cmd, &args, &dir).await;
            }
        }));
    }

    /// 异步加载横幅截图：当前世界目录下最新一张自动截图（无则显示占位）
    pub fn spawn_banner_shot(&mut self) {
        // 横幅只认自动截图，所以自动截图关着时横幅就回落到 logo，
        // 而不是去翻用户自己的 F2 截图（那些是用户主动截的，不该被挪作他用）。
        if !self.auto_shot {
            return;
        }
        let Some(path) = self.latest_screenshot() else {
            return;
        };
        self.banner_shot = None;
        let tx = self.tx.clone();
        let picker = self.picker.clone();
        tokio::spawn(async move {
            let proto = tokio::task::spawn_blocking(move || {
                let bytes = std::fs::read(&path).ok()?;
                let img = image::load_from_memory(&bytes).ok()?;
                Some(picker.new_resize_protocol(img))
            })
            .await
            .ok()
            .flatten();
            let _ = tx.send(TaskMsg::BannerShotLoaded(proto)).await;
        });
    }

    /// 实例的存档目录：委托 [`crate::launcher::saves_dir`]——
    /// CLI 也用同一份映射，避免两边各写一套后悄悄漂移。
    pub fn instance_saves_dir(&self, inst: &crate::launcher::Instance) -> PathBuf {
        crate::launcher::saves_dir(inst)
    }

    /// 旧版共享存档迁移（一次性）：实例 saves 目录首次创建时，
    /// 把共享根 saves/ 里的旧世界以符号链接接入（旧世界全实例可见，新世界按实例隔离）
    fn ensure_java_saves_migrated(&self, inst: &crate::launcher::Instance) {
        if inst.bedrock {
            return;
        }
        let saves = inst.dir.join("saves");
        let shared = self.mc_dir.join("saves");
        let Ok(rd) = std::fs::read_dir(&shared) else {
            let _ = std::fs::create_dir_all(&saves);
            return;
        };
        let worlds: Vec<PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir() && p.join("level.dat").is_file())
            .collect();
        std::fs::create_dir_all(&saves).ok();
        if worlds.is_empty() {
            return;
        }
        // 幂等：共享根里已有的世界逐个补 symlink（新加的世界也能进列表）
        for w in &worlds {
            if let Some(name) = w.file_name() {
                let target = saves.join(name);
                if !target.symlink_metadata().is_ok() {
                    let _ = std::os::unix::fs::symlink(w, &target);
                }
            }
        }
    }

    /// 扫描当前选中实例的存档（有 level.dat 才算），按修改时间最新在前
    pub fn refresh_worlds(&mut self) {
        if let Some(inst) = self.instances.get(self.selected_instance).cloned() {
            self.ensure_java_saves_migrated(&inst);
            let saves = self.instance_saves_dir(&inst);
            self.worlds = scan_worlds_dir(&saves);
        } else {
            self.worlds.clear();
        }
        // 世界列表重扫了，缩略图缓存一并作废（icon.png 可能已经变了）
        self.world_icons.clear();
        // 列表变了，选中框重新描一遍（换实例/按 r 时也有这个仪式感）
        self.world_anim_start = Some(std::time::Instant::now());
        self.selected_world = self.selected_world.min(self.worlds.len().saturating_sub(1));
    }

    // 循环切换主视图（Tab/Shift+Tab）
    // ===== j/k = 面板焦点轮换（类似 Tab 切视图，循环遍历所有面板）=====

    /// j: 下一个面板（循环）
    pub fn move_focus_down(&mut self) {
        self.home_focus = self.home_focus.next(self.home_view);
        self.sync_wm_focus_from_home_focus();
    }

    /// k: 上一个面板（循环）
    pub fn move_focus_up(&mut self) {
        self.home_focus = self.home_focus.prev(self.home_view);
        self.sync_wm_focus_from_home_focus();
    }

    /// 将 home_focus 同步到 WM 的 active 窗口（用于 dock 面板高亮）
    fn sync_wm_focus_from_home_focus(&mut self) {
        let dock_id = match self.home_focus {
            HomeFocus::Accounts => Some(win_id::ACCOUNTS),
            HomeFocus::Status => Some(win_id::STATUS),
            HomeFocus::Downloads => Some(win_id::VIEWS), // 下载面板对应视图侧栏
            _ => None,
        };
        if let Some(id) = dock_id {
            self.wm.focus(id);
        }
    }

    // ===== 方向键 = 当前聚焦面板内的内容导航 =====

    /// ↑: 面板内内容上移（列表选择上一项、网格向上、状态栏实例切换）
    /// 服务器页：横幅只是 MCTL 大字（不放宣传图），没有可操作内容。
    /// 焦点还停在横幅时按方向键 → 直接落进卡片网格，别让用户觉得"方向键没反应/乱"。
    fn promote_banner_focus_to_grid(&mut self) {
        if matches!(self.home_view, HomeView::Server)
            && self.home_focus == HomeFocus::Banner
        {
            self.home_focus = HomeFocus::List;
        }
    }

    pub fn move_content_up(&mut self) {
        // 与 ↓/←/→ 一致：服务器页焦点停在横幅时先落进卡片网格，
        // 否则进页面后第一次按 ↑ 会「没反应」（横幅没有可上移的内容）
        self.promote_banner_focus_to_grid();
        // 账户面板 dock 常驻，任意视图都可聚焦，导航全视图通用
        if self.home_focus == HomeFocus::Accounts {
            if self.selected_account > 0 {
                self.selected_account -= 1;
            }
            return;
        }
        match self.home_view {
            // 实例视图渲染 worlds_panel，↑↓ 选世界
            HomeView::Instance => match self.home_focus {
                HomeFocus::List => {
                    if self.selected_world > 0 {
                        self.selected_world -= 1;
                        self.world_anim_start = Some(std::time::Instant::now());
                        self.spawn_banner_shot();
                    }
                }
                HomeFocus::Status if self.selected_instance > 0 => {
                    self.selected_instance -= 1;
                    self.refresh_worlds();
                }
                _ => {}
            },
            HomeView::Server => match self.home_focus {
                HomeFocus::List => {
                    // 卡片网格：↑ 跳一行（列数由 UI 每帧回填 server_grid_cols）
                    let step = self.server_grid_cols.max(1);
                    if self.selected_server >= step {
                        self.selected_server -= step;
                        self.bump_sel_anim();
                    }
                }
                HomeFocus::Status if self.selected_instance > 0 => {
                    self.selected_instance -= 1;
                    self.reload_user_servers_for_instance();
                }
                _ => {}
            },
            HomeView::Resource => {
                // 资源大厅：方向键一格一格（平铺列表模式），不按行跳
                if self.focus == Focus::Grid && self.grid_selected > 0 {
                    self.grid_selected -= 1;
                }
            }
            // 插件/设置/下载中心是只读展示，没有可选列表
            HomeView::Download | HomeView::Plugins | HomeView::Settings => {}
        }
    }

    /// ↓: 面板内内容下移
    pub fn move_content_down(&mut self) {
        self.promote_banner_focus_to_grid();
        // 账户面板 dock 常驻，任意视图都可聚焦，导航全视图通用
        if self.home_focus == HomeFocus::Accounts {
            // 账户列表 + [4399]/[LittleSkin] 两个登录项
            if self.selected_account + 1 < self.accounts.len() + 2 {
                self.selected_account += 1;
            }
            return;
        }
        match self.home_view {
            HomeView::Instance => match self.home_focus {
                HomeFocus::List => {
                    if self.selected_world + 1 < self.worlds.len() {
                        self.selected_world += 1;
                        self.world_anim_start = Some(std::time::Instant::now());
                        self.spawn_banner_shot();
                    }
                }
                HomeFocus::Status if self.selected_instance + 1 < self.instances.len() => {
                    self.selected_instance += 1;
                    self.refresh_worlds();
                }
                _ => {}
            },
            HomeView::Server => match self.home_focus {
                HomeFocus::List => {
                    // 卡片网格：↓ 跳一行；末行不满时落到最后一行的同列
                    let step = self.server_grid_cols.max(1);
                    let len = self.current_server_list().len();
                    let mut next = self.selected_server;
                    if self.selected_server + step < len {
                        next = self.selected_server + step;
                    } else if len > 0 {
                        let target =
                            ((len - 1) / step * step + self.selected_server % step).min(len - 1);
                        if target > self.selected_server {
                            next = target;
                        }
                    }
                    if next != self.selected_server {
                        self.selected_server = next;
                        self.bump_sel_anim();
                    }
                }
                HomeFocus::Status if self.selected_instance + 1 < self.instances.len() => {
                    self.selected_instance += 1;
                    self.reload_user_servers_for_instance();
                }
                _ => {}
            },
            HomeView::Resource => {
                if self.focus == Focus::Grid && self.grid_selected + 1 < self.grid_items.len() {
                    self.grid_selected += 1;
                }
            }
            // 插件/设置/下载中心是只读展示，没有可选列表
            HomeView::Download | HomeView::Plugins | HomeView::Settings => {}
        }
    }

    /// ←: 面板内内容左移（网格向左）
    pub fn move_content_left(&mut self) {
        self.promote_banner_focus_to_grid();
        match self.home_view {
            HomeView::Resource
                if self.focus == Focus::Grid
                    && !self.grid_selected.is_multiple_of(self.grid_cols) =>
            {
                self.grid_selected -= 1;
            }
            // 卡片网格：同一行内往左；换行不跨（↑↓ 管行）
            HomeView::Server if self.home_focus == HomeFocus::List => {
                let cols = self.server_grid_cols.max(1);
                if cols > 1 && !self.selected_server.is_multiple_of(cols) {
                    self.selected_server -= 1;
                    self.bump_sel_anim();
                }
            }
            _ => {}
        }
    }

    /// →: 面板内内容右移（网格向右）
    pub fn move_content_right(&mut self) {
        self.promote_banner_focus_to_grid();
        match self.home_view {
            HomeView::Resource if self.focus == Focus::Grid => {
                let row_start = self.grid_selected / self.grid_cols * self.grid_cols;
                let row_end = (row_start + self.grid_cols).min(self.grid_items.len());
                if self.grid_selected + 1 < row_end {
                    self.grid_selected += 1;
                }
            }
            HomeView::Server if self.home_focus == HomeFocus::List => {
                let cols = self.server_grid_cols.max(1);
                let row_end =
                    (self.selected_server / cols * cols + cols).min(self.current_server_list().len());
                if cols > 1 && self.selected_server + 1 < row_end {
                    self.selected_server += 1;
                    self.bump_sel_anim();
                }
            }
            _ => {}
        }
    }

    fn cred(&self) -> Result<Credentials> {
        self.cred.clone().ok_or_else(|| anyhow::anyhow!("未登录"))
    }

    /// 进入资源下载视图时若已登录则触发全量加载
    pub fn enter_maps(&mut self) {
        self.set_home_view(HomeView::Resource);
        self.sync_wm_from_view();
        if self.map_page == MapPage::Grid && !self.grid_loading && self.grid_items.is_empty() {
            self.grid_auto_all = true;
            self.spawn_load_grid(false);
        }
        // Modrinth 首屏也在后台拉：切过去时像网易一样已经有内容，
        // 不用干等一次跨国往返。失败不影响当前页，切过去按 r 重试。
        if !self.mr_loaded && !self.dl.searching {
            self.mr_loaded = true;
            self.spawn_search_modpacks();
        }
    }

    // ---------- 资源大厅异步任务（迁移自 x19dl） ----------

    /// 触发分类列表加载（异步），append=false 重新加载首轮
    pub fn spawn_load_grid(&mut self, append: bool) {
        if self.grid_loading {
            return;
        }
        if !append {
            self.grid_items.clear();
            self.grid_covers.clear();
            self.cover_queue.clear();
            self.grid_be.clear();
            self.be_queue.clear();
            self.be_tries.clear();
            self.grid_offset = 0;
            self.grid_selected = 0;
            self.grid_end = false;
            // 本地排序（下载量/热度）需全量数据；服务端排序滚动加载即可
            self.grid_auto_all = self.grid_sort.is_local();
        }
        self.grid_loading = true;
        let cat = crate::domain::ALL_CATEGORIES[self.category];
        let query = self.grid_query.trim().to_string();
        let sort_type = self.grid_sort.server_sort();
        let offset = self.grid_offset;
        let tx = self.tx.clone();
        let cred = self.cred();
        tokio::spawn(async move {
            let cred = match cred {
                Ok(c) => c,
                Err(e) => {
                    let _ = tx
                        .send(TaskMsg::CategoryLoaded {
                            offset,
                            items: vec![],
                            error: Some(format!("未登录: {e}")),
                            end: true,
                        })
                        .await;
                    return;
                }
            };
            // 一轮并行拉 8 页（每页 PAGE_SIZE），大幅加快全量加载
            let mut handles = Vec::new();
            for p in 0..8 {
                let o = offset + p * PAGE_SIZE;
                let c = cat;
                let cr = cred.clone();
                let q = query.clone();
                handles.push(tokio::spawn(async move {
                    if q.is_empty() {
                        (
                            o,
                            api::list_category(&c, o, PAGE_SIZE, &cr, sort_type).await,
                        )
                    } else {
                        (
                            o,
                            api::search_category(&c, &q, o, PAGE_SIZE, &cr, sort_type).await,
                        )
                    }
                }));
            }
            let mut results: Vec<(i64, Result<Vec<ShopItem>, String>)> = Vec::new();
            for h in handles {
                match h.await {
                    Ok((o, Ok(vals))) => {
                        let items = vals
                            .into_iter()
                            .filter_map(|v| serde_json::from_value(v).ok())
                            .collect::<Vec<ShopItem>>();
                        results.push((o, Ok(items)));
                    }
                    Ok((o, Err(e))) => results.push((o, Err(e.to_string()))),
                    Err(e) => results.push((offset, Err(e.to_string()))),
                }
            }
            if let Some((_, Err(e))) = results.iter().find(|(_, r)| r.is_err()) {
                let _ = tx
                    .send(TaskMsg::CategoryLoaded {
                        offset,
                        items: vec![],
                        error: Some(e.clone()),
                        end: true,
                    })
                    .await;
                return;
            }
            results.sort_by_key(|(o, _)| *o);
            let all: Vec<ShopItem> = results.into_iter().flat_map(|(_, r)| r.unwrap()).collect();
            let end = all.is_empty() || all.len() < 8 * PAGE_SIZE as usize;
            let _ = tx
                .send(TaskMsg::CategoryLoaded {
                    offset,
                    items: all,
                    error: None,
                    end,
                })
                .await;
        });
    }

    /// 滚动到末尾时加载下一页（追加）
    pub fn spawn_load_more(&mut self) {
        self.grid_offset = self.grid_items.len() as i64;
        self.spawn_load_grid(true);
    }

    /// Modrinth 封面图下载（icon_url 直接下载）
    pub fn spawn_mr_cover(&mut self, index: usize) {
        let url = match self.dl.results.get(index).map(|h| h.icon_url.clone()) {
            Some(u) if !u.is_empty() => u,
            _ => return,
        };
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let img = match api::get_bytes(&url).await {
                Ok(bytes) => image::load_from_memory(&bytes).ok(),
                Err(_) => None,
            };
            let _ = tx.send(TaskMsg::MrCoverLoaded { index, img }).await;
        });
    }

    /// 触发封面图异步下载
    pub fn spawn_cover(&mut self, index: usize) {
        let url = match self.grid_items.get(index).map(|i| i.entity_id.clone()) {
            Some(id) if !id.is_empty() => id,
            _ => return,
        };
        let cred = self.cred();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let cred = match cred {
                Ok(c) => c,
                Err(_) => return,
            };
            let img = match api::item_detail(&url, &cred).await {
                Ok(d) => {
                    let u = d.brief_image_urls.first().cloned().unwrap_or_default();
                    if u.is_empty() {
                        None
                    } else {
                        match api::get_bytes(&u).await {
                            Ok(bytes) => image::load_from_memory(&bytes).ok(),
                            Err(_) => None,
                        }
                    }
                }
                Err(_) => None,
            };
            let _ = tx.send(TaskMsg::CoverLoaded { index, img }).await;
        });
    }

    /// 服务器卡片封面：Java 协议的 status 响应自带 favicon（data URL），
    /// 直接解码，不发额外网络请求。没有 favicon 就标记无图，卡片退回纯文字。
    pub fn spawn_srv_cover(&mut self, index: usize) {
        let Some(bytes) = self
            .current_server_list()
            .get(index)
            .and_then(|s| s.favicon_bytes())
        else {
            if index < self.srv_cover_none.len() {
                self.srv_cover_none[index] = true;
            }
            self.srv_cover_queue.retain(|&i| i != index);
            return;
        };
        let tx = self.tx.clone();
        let gen = self.srv_cover_gen;
        tokio::spawn(async move {
            let img = tokio::task::spawn_blocking(move || image::load_from_memory(&bytes).ok())
                .await
                .unwrap_or(None);
            let _ = tx
                .send(TaskMsg::SrvCoverLoaded { index, gen, img })
                .await;
        });
    }

    /// 检查网易资源是否为基岩版（download_info 的 mc_version_name 以 100. 开头）
    pub fn spawn_be_check(&mut self, index: usize) {
        let Some(item) = self.grid_items.get(index) else {
            return;
        };
        let Some(cred) = self.cred.clone() else {
            return;
        };
        let id = item.entity_id.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let be = api::download_info(&id, &cred)
                .await
                .map(|subs| subs.iter().any(|s| s.mc_version_name.starts_with("100.")))
                .ok();
            let _ = tx.send(TaskMsg::BeChecked { index, be }).await;
        });
    }

    /// 加载详情
    pub fn spawn_load_detail(&mut self, entity_id: String) {
        self.detail_loading = true;
        let tx = self.tx.clone();
        let cred = self.cred();
        tokio::spawn(async move {
            let cred = match cred {
                Ok(c) => c,
                Err(_) => {
                    let _ = tx.send(TaskMsg::DetailLoaded(None)).await;
                    return;
                }
            };
            let detail = api::item_detail(&entity_id, &cred).await.ok();
            let versions = api::download_info(&entity_id, &cred)
                .await
                .unwrap_or_default();
            let _ = tx.send(TaskMsg::DetailLoaded(detail)).await;
            let _ = tx.send(TaskMsg::DownloadInfoLoaded(versions)).await;
        });
    }

    /// 开始下载（网易资源/模组/地图）。
    /// 走下载中心统一队列 + 流式进度 → 天然并发：下完一个可以接着点下一个，
    /// 全部进度都在下载中心（Alt+6），不再有"全局单进度"这种只能看一个的说法。
    pub fn spawn_download(&mut self, url: String, dest: String) {
        let name = Path::new(&dest)
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "资源".into());
        let id = self.begin_task(format!("下载 {name}"));
        self.status = format!("已加入下载中心: {name}（Alt+6 看进度）");
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let target = PathBuf::from(&dest);
            let slot_name = name.clone();
            let tick_tx = tx.clone();
            let result = launcher::download_to(
                &launcher::http_client_big(),
                &url,
                &target,
                |done, total| {
                    send_file_progress(
                        &tick_tx,
                        id,
                        launcher::FileProgress {
                            name: slot_name.clone(),
                            done,
                            total,
                            finished_at: None,
                            failed: false,
                        },
                    );
                },
            )
            .await;
            let (ok, message) = match &result {
                Ok(()) => (true, format!("完成: {name}")),
                Err(e) => (false, format!("失败: {e}")),
            };
            // 终态：收尾那个文件槽（细线走到 100% / 标红）
            send_file_progress(
                &tx,
                id,
                launcher::FileProgress {
                    name,
                    done: if ok { u64::MAX } else { 0 },
                    total: 0,
                    finished_at: Some(std::time::Instant::now()),
                    failed: !ok,
                },
            );
            let _ = tx.send(TaskMsg::TaskDone { id, ok, message }).await;
        });
    }

    /// 下载中心汇总：(进行中任务数, 已完成数, 失败数, 已下字节, 总字节, 速率 B/s)
    pub fn download_summary(&self) -> (usize, usize, usize, u64, u64, u64) {
        use InstallStatus::*;
        let (mut active, mut done, mut failed, mut bd, mut bt) = (0, 0, 0, 0u64, 0u64);
        for t in &self.install_tasks {
            match t.status {
                Active => {
                    active += 1;
                    bd += t.bytes_done;
                    bt += t.bytes_total;
                }
                Done => done += 1,
                Failed => failed += 1,
            }
        }
        // 速率取各进行中任务的活跃文件平均速率之和（近似即可，只用来给个数量级）
        let speed = self
            .install_tasks
            .iter()
            .filter(|t| t.status == Active)
            .flat_map(|t| t.files.iter())
            .filter(|f| !f.finished)
            .map(|f| f.speed())
            .sum();
        (active, done, failed, bd, bt, speed)
    }

    /// 剩余时间（秒）。总长未知或速率为 0 时给 None——不编个假数字骗人。
    pub fn download_eta(&self) -> Option<u64> {
        let (_, _, _, bd, bt, speed) = self.download_summary();
        if speed == 0 || bt <= bd {
            return None;
        }
        Some((bt - bd) / speed)
    }

    /// 清掉下载中心里已结束（完成/失败）的任务，返回清掉几条
    pub fn clear_finished_tasks(&mut self) -> usize {
        let before = self.install_tasks.len();
        self.install_tasks
            .retain(|t| t.status == InstallStatus::Active);
        before - self.install_tasks.len()
    }

    /// 启动实例（管线：检查 Java → 补库 → 补资产 → natives → 离线启动）
    /// 执行 shell 命令（: 进入命令模式后回车触发）
    pub fn exec_shell(&mut self, line: &str) {
        let line = line.trim();
        if line.is_empty() {
            return;
        }
        let mut parts = line.splitn(2, char::is_whitespace);
        let cmd = parts.next().unwrap_or("");
        let arg = parts.next().unwrap_or("").trim().to_string();
        match cmd {
            "enter" | "进入" => {
                if arg.is_empty() {
                    self.status = "用法: enter <世界名>".into();
                    return;
                }
                let hit = self
                    .worlds
                    .iter()
                    .find(|w| w.name == arg)
                    .or_else(|| self.worlds.iter().find(|w| w.name.contains(&arg)));
                match hit {
                    Some(w) => {
                        // quickPlaySingleplayer 要的是世界**目录名**，不是显示名
                        self.launch_world = Some(w.dir.clone());
                        self.status = format!("进入世界 {} …", w.name);
                        self.spawn_launch();
                    }
                    None => {
                        let names = self
                            .worlds
                            .iter()
                            .map(|w| w.name.clone())
                            .collect::<Vec<_>>()
                            .join(", ");
                        self.status = if names.is_empty() {
                            "没有可用世界（saves 下暂无存档）".into()
                        } else {
                            format!("未找到 {}，可用: {}", arg, names)
                        };
                    }
                }
            }
            "plugin" => {
                let mut sub = arg.splitn(2, char::is_whitespace);
                let op = sub.next().unwrap_or("list");
                let target = sub.next().unwrap_or("").trim().to_string();
                match op {
                    "list" | "ls" | "" => {
                        let root = self.plugin_root();
                        let list = crate::plugin::scan_plugins(&root);
                        if list.is_empty() {
                            self.status = format!("未安装插件（{}）", root.display());
                        } else {
                            self.status = list
                                .iter()
                                .map(|p| format!("{} v{}", p.name, p.version))
                                .collect::<Vec<_>>()
                                .join(" · ");
                        }
                    }
                    "install" if !target.is_empty() => {
                        self.status = format!("安装插件 {} …", target);
                        let root = self.plugin_root();
                        let src = target.clone();
                        let task_id = self.begin_task(format!("插件 {target}"));
                        let tx = self.tx.clone();
                        tokio::spawn(async move {
                            let _ = tx.try_send(TaskMsg::InstallStage {
                                id: task_id,
                                phase: "克隆 + 构建（首次较慢）…".into(),
                                done: 0,
                                total: 0,
                            });
                            let result = crate::plugin::install(&root, &src);
                            let done = result
                                .as_ref()
                                .map(|info| format!("{} v{}", info.name, info.version))
                                .map_err(|e| anyhow::anyhow!("{e:#}"));
                            let _ = tx
                                .send(TaskMsg::InstallDone {
                                    id: task_id,
                                    result: done,
                                })
                                .await;
                            let _ = tx.send(TaskMsg::PluginInstallDone(result)).await;
                        });
                    }
                    "install" => self.status = "用法: plugin install <git-url|路径>".into(),
                    "remove" | "rm" if !target.is_empty() => {
                        let root = self.plugin_root();
                        match crate::plugin::remove(&root, &target) {
                            Ok(()) => self.status = format!("已卸载插件 {}", target),
                            Err(e) => self.status = format!("卸载失败: {e:#}"),
                        }
                    }
                    "remove" | "rm" => self.status = "用法: plugin remove <名称>".into(),
                    _ => self.status = "plugin 命令: list | install <url> | remove <名称>".into(),
                }
            }
            "refresh" | "reload" => {
                self.refresh_worlds();
                self.status = "已刷新世界列表".into();
            }
            "help" | "?" => {
                self.status = "命令: enter <世界> · plugin … · bedrock [pre] · refresh · quit · help · 快捷键: 实例面板聚焦按 o 存档 / f 全屏".into();
            }
            "bedrock" => {
                self.source = ResourceSource::Plugins;
                self.enter_source();
            }
            "quit" | "exit" | "退出" => self.quit = true,
            _ => self.status = format!("未知命令 {cmd}，输入 help 查看帮助"),
        }
    }

    pub fn spawn_launch(&mut self) {
        if self.launching {
            return;
        }
        let Some(inst) = self.instances.get(self.selected_instance).cloned() else {
            self.launch_status = "无实例可选".into();
            return;
        };
        if inst.bedrock {
            self.spawn_launch_bedrock(&inst);
            return;
        }
        // LittleSkin 外置登录优先（uuid+token 齐全才算已登录）
        let ls = storage::load_ls()
            .ok()
            .filter(|c| !c.access_token.is_empty() && !c.uuid.is_empty());
        let username = match &ls {
            Some(c) if !c.name.is_empty() => c.name.clone(),
            _ if self.user_name.is_empty() || self.user_name == "未登录" => "Player".to_string(),
            _ => self.user_name.clone(),
        };
        let auth = ls.map(|c| (c.uuid, c.access_token));
        let world = self.launch_world.take();
        self.launching = true;
        self.launch_status = "检查中…".into();
        self.status = format!("启动 {} …", inst.id);
        self.game_log.clear();
        let mc_dir = self.mc_dir.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result = if let Some(w) = world {
                launch_instance_world(&mc_dir, &inst, &username, w, auth, tx.clone()).await
            } else {
                launch_instance(&mc_dir, &inst, &username, auth, tx.clone()).await
            };
            let _ = tx.send(TaskMsg::LaunchDone(result)).await;
        });
    }

    /// 加载账户头像（avatar.png；无文件但存有 LittleSkin uuid 则自动重新生成）
    pub fn spawn_load_avatar(&mut self) {
        if avatar_path().exists() {
            self.avatar = None;
            let tx = self.tx.clone();
            let picker = self.picker.clone();
            tokio::spawn(async move {
                let proto = tokio::task::spawn_blocking(move || load_avatar_file(&picker))
                    .await
                    .unwrap_or(None);
                let _ = tx.send(TaskMsg::AvatarLoaded(proto)).await;
            });
            return;
        }
        // 无头像文件：尝试用持久化的 LittleSkin uuid 重新生成
        if let Ok(cfg) = storage::load_ls() {
            if !cfg.uuid.is_empty() {
                let uuid = cfg.uuid;
                let tx = self.tx.clone();
                let picker = self.picker.clone();
                tokio::spawn(async move {
                    if regen_avatar(&uuid).await {
                        let proto = tokio::task::spawn_blocking(move || load_avatar_file(&picker))
                            .await
                            .unwrap_or(None);
                        let _ = tx.send(TaskMsg::AvatarLoaded(proto)).await;
                    }
                });
            }
        }
    }

    /// LittleSkin 登录成功后：从 Yggdrasil 皮肤裁双层头部（base+帽子层）生成头像
    fn spawn_ls_avatar(&mut self, uuid: &str) {
        let uuid = uuid.to_string();
        // 持久化 uuid：重启后自动恢复头像
        if let Ok(mut cfg) = storage::load_ls() {
            if cfg.uuid != uuid {
                cfg.uuid = uuid.clone();
                let _ = storage::save_ls(&cfg);
            }
        }
        let tx = self.tx.clone();
        let picker = self.picker.clone();
        tokio::spawn(async move {
            if regen_avatar(&uuid).await {
                let proto = load_avatar_file(&picker);
                let _ = tx.send(TaskMsg::AvatarLoaded(proto)).await;
            }
        });
    }

    /// 加载官方版本清单
    pub fn spawn_load_manifest(&mut self) {
        if self.dl.manifest_loading {
            return;
        }
        self.dl.manifest_loading = true;
        self.dl.install_status = "加载官方版本清单…".into();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result = launcher::meta::fetch_manifest().await;
            let _ = tx.send(TaskMsg::ManifestLoaded(result)).await;
        });
    }

    /// 搜索整合包（offset=0 替换结果；query 空=当前分类全部）
    pub fn spawn_search_modpacks(&mut self) {
        if self.dl.searching {
            return;
        }
        let q = self.dl.query.trim().to_string();
        let version = self.mr_version.clone();
        self.dl.searching = true;
        self.dl.more_end = false;
        self.dl.install_status = if q.is_empty() {
            format!("加载 {}…", MODRINTH_CATEGORIES[self.mr_category].0)
        } else {
            format!("搜索 “{q}”…")
        };
        let index = self.dl.sort;
        let project_type = MODRINTH_CATEGORIES[self.mr_category].1;
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result = launcher::modpack::search_modrinth(
                &q,
                project_type,
                &version,
                MODRINTH_PAGE,
                index,
                0,
            )
            .await;
            let _ = tx.send(TaskMsg::ModpacksLoaded { result, offset: 0 }).await;
        });
    }

    /// 滚动到底加载下一页结果（追加）
    pub fn spawn_search_more(&mut self) {
        let q = self.dl.query.trim().to_string();
        let version = self.mr_version.clone();
        if self.dl.searching || self.dl.more_end {
            return;
        }
        self.dl.searching = true;
        let offset = self.dl.results.len();
        self.dl.install_status = "加载更多…".to_string();
        let index = self.dl.sort;
        let project_type = MODRINTH_CATEGORIES[self.mr_category].1;
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result = launcher::modpack::search_modrinth(
                &q,
                project_type,
                &version,
                MODRINTH_PAGE,
                index,
                offset,
            )
            .await;
            let _ = tx.send(TaskMsg::ModpacksLoaded { result, offset }).await;
        });
    }

    /// 是否该提前取下一页：距列表尾部不足两行就发，不等选到最后一格。
    /// 和网易网格的 `grid_selected + 2 * grid_cols` 是同一口径。
    fn modrinth_needs_more(&self) -> bool {
        if self.dl.searching || self.dl.more_end || self.dl.results.is_empty() {
            return false;
        }
        let cols = self.grid_cols.max(1);
        self.dl.sel_result + 2 * cols >= self.dl.results.len()
    }

    // ─────────── GitHub 插件发现 ───────────

    /// 搜索 GitHub 仓库。
    /// /// ponytail: 只在用户**主动触发**时请求（进页面加载一次 / 按 Enter 搜索），
    /// 绝不在按键时实时搜——未认证 search API 只有 10 次/分钟，实时搜必爆配额。
    /// 结果缓存进 `gh_results`，后续细化走 `refilter_github`（零请求）。
    pub fn spawn_search_github(&mut self) {
        if self.gh_searching {
            return;
        }
        self.spawn_search_github_as(false);
    }

    fn spawn_search_github_as(&mut self, initial: bool) {
        self.gh_searching = true;
        let q = self.dl.query.trim().to_string();
        self.dl.install_status = if q.is_empty() {
            format!("拉取 topic:{} 注册表…", api::github::PLUGIN_TOPIC)
        } else {
            format!("搜索 GitHub “{q}”…")
        };
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let mut result = api::github::search_repos(&q, 1).await;
            // 注册表还没人用（topic 无人认领）时，空查询会返回 0 条 → 首屏死掉。
            // 回退到宽泛搜索，并说明原因。只在首屏发生一次，之后走缓存。
            if initial && q.is_empty() {
                if let Ok(r) = &result {
                    if r.total == 0 {
                        result = api::github::search_repos(api::github::BROAD_QUERY, 1).await;
                    }
                }
            }
            let _ = tx.send(TaskMsg::GhLoaded { result, initial }).await;
        });
    }

    /// 本地过滤缓存结果（细化搜索不花 GitHub 配额）
    pub fn refilter_github(&mut self) {
        let q = self.dl.query.trim().to_lowercase();
        self.gh_filtered = self
            .gh_results
            .iter()
            .enumerate()
            .filter(|(_, r)| {
                q.is_empty()
                    || r.full_name.to_lowercase().contains(&q)
                    || r.description
                        .as_deref()
                        .is_some_and(|d| d.to_lowercase().contains(&q))
            })
            .map(|(i, _)| i)
            .collect();
        self.gh_selected = self
            .gh_selected
            .min(self.gh_filtered.len().saturating_sub(1));
    }

    /// 安装选中的 GitHub 仓库为插件（clone + cargo build + 识别，走既有 plugin::install）
    pub fn spawn_install_github(&mut self) {
        let Some(&idx) = self.gh_filtered.get(self.gh_selected) else {
            self.dl.install_status = "没有选中的仓库".into();
            return;
        };
        let repo = self.gh_results[idx].clone();
        let name = repo.short_name().to_string();
        let root = self.plugin_root();
        let url = repo.clone_url();
        self.dl.installing = true;
        self.dl.install_status = format!("安装 {name}（克隆 + 构建，首次较慢）…");
        let task_id = self.begin_task(format!("插件仓库 {name}"));
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let _ = tx.try_send(TaskMsg::InstallStage {
                id: task_id,
                phase: "克隆 + 构建（首次较慢）…".into(),
                done: 0,
                total: 0,
            });
            // plugin::install 是同步阻塞（git clone + cargo build），
            // 必须放 blocking 线程池，否则卡住整个 tokio 运行时
            let result = tokio::task::spawn_blocking(move || crate::plugin::install(&root, &url))
                .await
                .unwrap_or_else(|e| Err(anyhow::anyhow!("安装任务异常: {e}")))
                .map_err(|e| anyhow::anyhow!("{e:#}"));
            let done = result
                .as_ref()
                .map(|info| format!("{} v{}", info.name, info.version))
                .map_err(|e| anyhow::anyhow!("{e:#}"));
            let _ = tx
                .send(TaskMsg::InstallDone {
                    id: task_id,
                    result: done,
                })
                .await;
            let _ = tx.send(TaskMsg::GhInstalled { name, result }).await;
        });
    }

    /// 打开 Modrinth 详情页并拉取版本列表
    pub fn spawn_mr_detail(&mut self) {
        if self.dl.searching || self.mr_detail_loading {
            return;
        }
        let Some(hit) = self.dl.results.get(self.dl.sel_result) else {
            return;
        };
        self.mr_detail_open = true;
        self.mr_detail_loading = true;
        self.mr_detail_title = hit.title.clone();
        self.mr_versions.clear();
        self.mr_row_sel = 0;
        self.mr_collapsed.clear();
        let title = hit.title.clone();
        let id = hit.project_id.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result = launcher::modpack::project_versions(&id).await;
            let _ = tx.send(TaskMsg::MrVersionsLoaded { title, result }).await;
        });
    }

    /// 安装官方版本
    pub fn spawn_install_official(&mut self) {
        if self.dl.installing {
            return;
        }
        let Some(v) = self.dl.manifest.get(self.dl.sel_official) else {
            return;
        };
        let id = v.id.clone();
        self.dl.installing = true;
        self.dl.install_status = format!("安装官方版本 {id}…");
        let task_id = self.begin_task(format!("官方版本 {id}"));
        let mc_dir = self.mc_dir.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result = launcher::meta::install_official(
                &mc_dir,
                &id,
                |phase, done, total| {
                    let tx = tx.clone();
                    let phase = phase.to_string();
                    tokio::spawn(async move {
                        let _ = tx
                            .send(TaskMsg::InstallStage {
                                id: task_id,
                                phase,
                                done,
                                total,
                            })
                            .await;
                    });
                },
                &|f: launcher::FileProgress| send_file_progress(&tx, task_id, f),
            )
            .await;
            let _ = tx
                .send(TaskMsg::InstallDone {
                    id: task_id,
                    result,
                })
                .await;
        });
    }

    /// 资源页自己处理方向键（网格 / 版本树），全局导航要让位。
    /// `mr_detail_open` 也要算：版本树开着时方向键归它，
    /// 否则 ← 会被 move_content_left 吃掉，树展不开。
    fn maps_owns_arrows(&self) -> bool {
        self.home_view == HomeView::Resource || self.mr_detail_open
    }

    /// 折叠/展开光标所在的分组。`expand=true` 时只展开不折叠（←→ 语义区分）。
    fn toggle_mr_group(&mut self, tree: &[crate::ui::version_tree::TreeRow], expand: bool) {
        use crate::ui::version_tree::TreeRow;
        let Some(row) = tree.get(self.mr_row_sel) else {
            return;
        };
        let (key, is_group) = match row {
            TreeRow::Group { mc, expanded, .. } => (mc.clone(), !*expanded || expand),
            TreeRow::Loader {
                mc,
                loader,
                expanded,
                ..
            } => (format!("{mc}/{loader}"), !*expanded || expand),
            TreeRow::Leaf { .. } => return,
        };
        if is_group {
            self.mr_collapsed.remove(&key);
        } else {
            self.mr_collapsed.insert(key);
        }
    }

    /// 当前选中行对应的版本（叶子才返回）
    fn mr_selected_version(&self) -> Option<launcher::modpack::MrVersion> {
        let tree = crate::ui::version_tree::build_tree(&self.mr_versions, &self.mr_collapsed);
        match tree.get(self.mr_row_sel) {
            Some(crate::ui::version_tree::TreeRow::Leaf { idx, .. }) => {
                self.mr_versions.get(*idx).cloned()
            }
            _ => None,
        }
    }

    /// 安装整合包（详情页选中版本）
    pub fn spawn_install_modpack(&mut self) {
        if self.dl.installing {
            return;
        }
        let Some(ver) = self.mr_selected_version() else {
            self.dl.install_status = "请先选中一个具体版本（分组行不能安装）".into();
            return;
        };
        let label = if ver.version_number.is_empty() {
            "整合包".to_string()
        } else {
            ver.version_number.clone()
        };
        // 单个 mod/光影要装进**选中实例**的目录；整合包装进全局根。
        // 两者目标不同，传错就装错地方（原来一律用 mc_dir）。
        let is_single = ver
            .files
            .first()
            .map(|f| !f.filename.ends_with(".mrpack"))
            .unwrap_or(false);
        let (target_dir, task_name) = if is_single {
            match self.instances.get(self.selected_instance) {
                Some(inst) => (inst.dir.clone(), format!("{label} → {}", inst.id)),
                None => {
                    self.dl.install_status = "没有可用实例，请先在资源页装一个版本".into();
                    return;
                }
            }
        } else {
            (self.mc_dir.clone(), format!("整合包 {label}"))
        };
        self.dl.installing = true;
        self.dl.install_status = format!("安装 {task_name}…");
        let task_id = self.begin_task(format!("安装 {task_name}"));
        let mc_dir = target_dir;
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result = launcher::modpack::install_modpack(
                &mc_dir,
                &ver,
                |phase, done, total| {
                    let tx = tx.clone();
                    let phase = phase.to_string();
                    tokio::spawn(async move {
                        let _ = tx
                            .send(TaskMsg::InstallStage {
                                id: task_id,
                                phase,
                                done,
                                total,
                            })
                            .await;
                    });
                },
                &|f: launcher::FileProgress| send_file_progress(&tx, task_id, f),
            )
            .await;
            let _ = tx
                .send(TaskMsg::InstallDone {
                    id: task_id,
                    result,
                })
                .await;
        });
    }

    /// 下载中心：开一个任务，返回任务 id（进度消息都带这个 id）
    pub fn begin_task(&mut self, name: impl Into<String>) -> u64 {
        let id = self.next_task_id;
        self.next_task_id += 1;
        self.install_tasks.push(InstallTask {
            id,
            name: name.into(),
            phase: "排队中".into(),
            done: 0,
            total: 0,
            status: InstallStatus::Active,
            started: std::time::Instant::now(),
            files: Vec::new(),
            bytes_done: 0,
            bytes_total: 0,
        });
        // 只留最近 20 条，避免长时间挂着把内存/视觉拖爆
        let extra = self.install_tasks.len().saturating_sub(20);
        if extra > 0 {
            self.install_tasks.drain(..extra);
        }
        id
    }

    /// 当前实例的 `servers.dat` 路径（基岩版/无实例返回 None）
    pub fn user_servers_path(&self) -> Option<PathBuf> {
        let inst = self.instances.get(self.selected_instance)?;
        if inst.bedrock {
            None
        } else {
            Some(inst.dir.join("servers.dat"))
        }
    }

    /// 某个标签下的服务器列表。All 会做 host:port 去重。
    pub fn server_list_for(&self, tab: ServerTab) -> Vec<crate::api::mcserver::JavaServer> {
        match tab {
            ServerTab::User => self.user_servers.clone(),
            ServerTab::Recommended => self
                .servers
                .iter()
                .filter(|s| s.builtin)
                .cloned()
                .collect(),
            ServerTab::All => {
                let mut out = self.servers.clone();
                for s in &self.user_servers {
                    if !out
                        .iter()
                        .any(|x| x.host.eq_ignore_ascii_case(&s.host) && x.port == s.port)
                    {
                        out.push(s.clone());
                    }
                }
                out
            }
        }
    }

    /// 当前标签的列表
    pub fn current_server_list(&self) -> Vec<crate::api::mcserver::JavaServer> {
        self.server_list_for(self.server_tab)
    }

    /// 当前标签选中的服务器
    pub fn current_server_entry(&self) -> Option<crate::api::mcserver::JavaServer> {
        self.current_server_list().get(self.selected_server).cloned()
    }

    fn server_tab_status(&self) -> String {
        let list = self.current_server_list();
        let online = list.iter().filter(|s| s.error.is_none()).count();
        match self.server_tab {
            ServerTab::All => format!(
                "全部 {} 个 · 在线 {online} · ↑↓←→ 选择 · 回车进服 · a 添加 · r 刷新",
                list.len()
            ),
            ServerTab::User => {
                if self.user_servers_path().is_none() {
                    "当前实例不是 Java 版，无法读取 servers.dat".to_string()
                } else if self.user_servers_error.is_some() {
                    "读取 servers.dat 失败（按 r 重试）".to_string()
                } else if self.user_servers_loading {
                    "正在读取 servers.dat…".to_string()
                } else {
                    format!(
                        "{} 个 · 与 Java 共用 servers.dat · a 添加 · x 删除 · r 刷新",
                        list.len()
                    )
                }
            }
            ServerTab::Recommended => format!(
                "{} 个内置推荐 · 在线 {online} · 回车进服 · r 刷新",
                list.len()
            ),
        }
    }

    /// 切换服务器标签，保留各标签自己的选中项。
    pub fn set_server_tab(&mut self, tab: ServerTab) {
        if self.server_tab == tab {
            return;
        }
        self.server_tab_sel[self.server_tab.index()] = self.selected_server;
        self.server_tab = tab;
        self.selected_server = self.server_tab_sel[tab.index()];
        let len = self.current_server_list().len();
        if self.selected_server >= len {
            self.selected_server = len.saturating_sub(1);
        }
        self.srv_scroll = 0;
        self.reset_server_covers();
        if !self.server_launching {
            self.server_status = self.server_tab_status();
        }
        self.bump_sel_anim();
    }

    fn reset_server_covers(&mut self) {
        self.srv_cover_gen = self.srv_cover_gen.wrapping_add(1);
        let len = self.current_server_list().len();
        self.srv_covers = (0..len).map(|_| None).collect();
        self.srv_cover_queue.clear();
        self.srv_cover_none = vec![false; len];
    }

    /// 换了实例：先清掉旧列表，避免新实例加载完成前还显示上一个实例的服务器。
    fn reload_user_servers_for_instance(&mut self) {
        self.user_servers.clear();
        self.user_servers_error = None;
        self.selected_server = 0;
        self.server_tab_sel[ServerTab::User.index()] = 0;
        self.reset_server_covers();
        self.spawn_load_user_servers();
    }

    /// 读取当前实例的 `servers.dat` 并 ping（与 Java 客户端双向同步）。
    pub fn spawn_load_user_servers(&mut self) {
        let Some(path) = self.user_servers_path() else {
            self.user_servers.clear();
            self.user_servers_error = None;
            self.user_servers_loading = false;
            self.reset_server_covers();
            return;
        };
        self.user_servers_loading = true;
        self.user_servers_error = None;
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result = crate::api::mcserver::load_user(&path).await;
            let _ = tx
                .send(TaskMsg::UserServersLoaded { path, result })
                .await;
        });
    }

    fn on_user_servers_loaded(
        &mut self,
        path: PathBuf,
        result: Result<Vec<crate::api::mcserver::JavaServer>>,
    ) {
        // 加载期间切了实例 → 旧响应丢弃
        if self.user_servers_path().as_deref() != Some(path.as_path()) {
            return;
        }
        self.user_servers_loading = false;
        match result {
            Ok(list) => {
                self.user_servers = list;
                self.user_servers_error = None;
            }
            Err(e) => {
                self.user_servers.clear();
                self.user_servers_error = Some(format!("{e:#}"));
            }
        }
        let len = self.current_server_list().len();
        if self.selected_server >= len {
            self.selected_server = len.saturating_sub(1);
        }
        self.reset_server_covers();
        if !self.server_launching {
            self.server_status = self.server_tab_status();
        }
    }

    /// 打开添加服务器表单（写入当前实例的 servers.dat）。
    pub fn open_server_add(&mut self) {
        if self.user_servers_path().is_none() {
            self.server_status = "没有可用的 Java 实例：先安装/选择一个 Java 版本".into();
            return;
        }
        self.server_add = Some(ServerAddForm::default());
    }

    fn handle_server_add_key(&mut self, key: crossterm::event::KeyEvent) {
        use crossterm::event::{KeyCode, KeyModifiers};
        let Some(mut form) = self.server_add.take() else {
            return;
        };
        match key.code {
            KeyCode::Esc => return,
            KeyCode::Tab | KeyCode::Down | KeyCode::Up => {
                form.focus = (form.focus + 1) % 2;
                form.error.clear();
            }
            KeyCode::Enter => {
                self.server_add = Some(form);
                self.submit_server_add();
                return;
            }
            KeyCode::Backspace => {
                let field = if form.focus == 0 {
                    &mut form.addr
                } else {
                    &mut form.name
                };
                field.pop();
                form.error.clear();
            }
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                let field = if form.focus == 0 {
                    &mut form.addr
                } else {
                    &mut form.name
                };
                field.push(c);
                form.error.clear();
            }
            _ => {}
        }
        self.server_add = Some(form);
    }

    fn submit_server_add(&mut self) {
        let Some(form) = self.server_add.clone() else {
            return;
        };
        if form.addr.trim().is_empty() {
            if let Some(f) = &mut self.server_add {
                f.error = "地址不能为空".into();
            }
            return;
        }
        let Some(path) = self.user_servers_path() else {
            if let Some(f) = &mut self.server_add {
                f.error = "没有可用的 Java 实例".into();
            }
            return;
        };
        match crate::servers_dat::add(&path, &form.name, &form.addr) {
            Ok(entry) => {
                self.server_add = None;
                let srv = crate::api::mcserver::JavaServer {
                    name: entry.name.clone(),
                    host: entry.host.clone(),
                    port: entry.port,
                    favicon: entry.icon.clone(),
                    ..Default::default()
                };
                let idx = match self
                    .user_servers
                    .iter()
                    .position(|s| s.host.eq_ignore_ascii_case(&entry.host) && s.port == entry.port)
                {
                    Some(i) => {
                        self.user_servers[i] = srv;
                        i
                    }
                    None => {
                        self.user_servers.push(srv);
                        self.user_servers.len() - 1
                    }
                };
                // 切到用户标签并选中刚加的条目
                self.server_tab_sel[self.server_tab.index()] = self.selected_server;
                self.server_tab = ServerTab::User;
                self.selected_server = idx;
                self.server_tab_sel[ServerTab::User.index()] = idx;
                self.srv_scroll = 0;
                self.reset_server_covers();
                self.status = format!("已添加 {}（写入 servers.dat）", entry.addr());
                self.server_status = format!(
                    "已添加 {} · 与 Java 客户端同步 · 正在 ping…",
                    entry.addr()
                );
                self.spawn_load_user_servers();
            }
            Err(e) => {
                if let Some(f) = &mut self.server_add {
                    f.error = format!("{e:#}");
                }
            }
        }
    }

    /// 删除选中项（仅用户标签；直接从 servers.dat 删除）。
    fn remove_selected_user_server(&mut self) {
        if self.server_tab != ServerTab::User {
            return;
        }
        let Some(entry) = self.user_servers.get(self.selected_server).cloned() else {
            return;
        };
        let Some(path) = self.user_servers_path() else {
            return;
        };
        match crate::servers_dat::remove(&path, &entry.addr()) {
            Ok(true) => {
                self.user_servers.retain(|s| {
                    !(s.host.eq_ignore_ascii_case(&entry.host) && s.port == entry.port)
                });
                let len = self.user_servers.len();
                if self.selected_server >= len {
                    self.selected_server = len.saturating_sub(1);
                }
                self.server_tab_sel[ServerTab::User.index()] = self.selected_server;
                self.reset_server_covers();
                self.status = format!("已删除 {}（servers.dat）", entry.addr());
                self.server_status = self.server_tab_status();
            }
            Ok(false) => {
                self.server_status = format!("servers.dat 里没有 {}", entry.addr());
            }
            Err(e) => {
                self.server_status = format!("删除失败: {e:#}");
            }
        }
    }

    /// 服务器页：加载 Java 国际服列表并 ping 状态（无需登录）
    pub fn spawn_load_servers(&mut self) {
        if self.server_loading {
            return;
        }
        self.server_loading = true;
        self.server_status = "正在连接 Java 服务器…".into();
        self.spawn_load_user_servers();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let list = crate::api::mcserver::load_all().await;
            let _ = tx.send(TaskMsg::ServerListLoaded(list)).await;
        });
    }

    /// 服务器页：用选中的本地实例直连 Java 服。
    /// 走原版 `--server/--port`，认证沿用本地实例那套（LittleSkin 优先，否则离线名）。
    /// 正版验证服需要 Microsoft 登录，alpha 暂不支持——状态栏会说明失败原因。
    pub fn spawn_server_launch(&mut self) {
        if self.server_launching {
            return;
        }
        let Some(srv) = self.current_server_entry() else {
            self.server_status = "无可进入的服务器（按 r 刷新）".into();
            return;
        };
        let Some(inst) = self.instances.get(self.selected_instance).cloned() else {
            self.server_status = "没有本地实例：先去 下载(D) 页安装一个 Java 版本".into();
            return;
        };
        if inst.bedrock {
            self.server_status = "这是基岩版实例，进不了 Java 服务器".into();
            return;
        }
        // 与本地启动一致：LittleSkin 外置登录优先（uuid+token 齐全才算）
        let ls = storage::load_ls()
            .ok()
            .filter(|c| !c.access_token.is_empty() && !c.uuid.is_empty());
        let username = match &ls {
            Some(c) if !c.name.is_empty() => c.name.clone(),
            _ if !self.role_name.trim().is_empty() => self.role_name.trim().to_string(),
            _ if self.user_name.is_empty() || self.user_name == "未登录" => "Player".to_string(),
            _ => self.user_name.clone(),
        };
        let auth = ls.map(|c| (c.uuid, c.access_token));
        self.record_recent(RecentKind::JavaServer, &srv.addr(), &srv.name, &srv.version);
        self.server_launching = true;
        self.server_status = format!("启动 {}，连接 {}…", inst.id, srv.addr());
        self.game_log.clear();
        let tx = self.tx.clone();
        let mc_dir = self.mc_dir.clone();
        let host = srv.host.clone();
        let port = srv.port;
        tokio::spawn(async move {
            let result = launch_instance_inner(
                &mc_dir,
                &inst.dir,
                &inst,
                &username,
                None,
                Some((port, host.as_str())),
                auth,
                None,
                tx.clone(),
            )
            .await;
            let _ = tx.send(TaskMsg::ServerDone(result)).await;
        });
    }

    /// 登录第一步：取验证码图。完成后 `login_input_step` 变 2，等用户输入验证码。
    pub fn spawn_login(&mut self) {
        let user = self.login_user.trim().to_string();
        let pass = self.login_pass.clone();
        if user.is_empty() || pass.is_empty() {
            self.login_status = Some("用户名或密码为空".into());
            return;
        }
        // 重新开始一轮：清掉上一次的中间态，否则会拿着已消耗的 captcha_id 去提交
        self.login_challenge = None;
        self.login_captcha.clear();
        self.login_status = Some("正在取验证码…".into());
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result = api::auth::begin_4399(&user, &pass).await;
            let _ = tx.send(TaskMsg::LoginCaptcha(result)).await;
        });
    }

    /// 登录第二步：带用户识别的验证码提交。
    fn spawn_login_finish(&mut self) {
        let Some(ch) = self.login_challenge.take() else {
            self.login_status = Some("验证码已失效，请重新登录".into());
            return;
        };
        let captcha = self.login_captcha.trim().to_string();
        if captcha.is_empty() {
            self.login_challenge = Some(ch);
            self.login_status = Some("验证码不能为空".into());
            return;
        }
        self.login_status = Some("登录中…".into());
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result = api::auth::finish_4399(&ch, &captcha).await;
            let _ = tx.send(TaskMsg::LoginDone(result)).await;
        });
    }

    /// 验证码图已到手：切到第 3 步（step 2）让用户输入
    fn on_login_captcha(&mut self, r: Result<crate::api::auth::LoginChallenge>) {
        match r {
            Ok(ch) => {
                self.login_status = Some(if ch.opened {
                    format!(
                        "验证码图片已用图片管理器打开（{}），输入图里的字符",
                        ch.captcha_path.display()
                    )
                } else {
                    format!(
                        "未能自动打开图片管理器，请手动打开 {} 然后输入",
                        ch.captcha_path.display()
                    )
                });
                self.login_challenge = Some(ch);
                self.login_captcha.clear();
                self.login_input_step = 2;
            }
            Err(e) => {
                self.login_input_step = 0;
                self.login_status = Some(format!("取验证码失败: {e:#}"));
            }
        }
    }

    /// 启动时恢复 LittleSkin OAuth 应用登记（若有）
    fn restore_ls(&mut self) {
        let Ok(c) = storage::load_ls() else { return };
        self.ls.app_name = c.app_name;
        self.ls.callback_url = c.callback_url;
        self.ls.client_id = c.client_id;
        if self.ls.client_id.is_empty() {
            self.ls.status = "OAuth 应用登记已保存，等待白名单审核".into();
        } else {
            self.ls.status = format!(
                "client_id 已保存（{}…），账户面板 Enter 登录",
                &self.ls.client_id[..4.min(self.ls.client_id.len())]
            );
        }
    }

    /// 世界缩略图路径：Java = `icon.png`（游戏保存世界时自动生成）；
    /// 基岩 = `world_icon.jpeg`。都没有返回 None（界面画 ◻ 占位）。
    /// /// 只认游戏自己写的缩略图，不用 mctl 的自动截图 / F2 截图——
    /// 那些是"某个时刻的画面"，和世界本身不是一回事。
    pub fn world_icon_path(&self, dir: &str) -> Option<PathBuf> {
        let inst = self.instances.get(self.selected_instance)?;
        let saves = self.instance_saves_dir(inst);
        ["icon.png", "world_icon.jpeg"]
            .iter()
            .map(|f| saves.join(dir).join(f))
            .find(|p| p.is_file())
    }

    /// 世界缩略图缓存键：实例 + 目录名（换实例后同名目录不串图）
    pub fn world_icon_key(&self, dir: &str) -> String {
        let inst = self
            .instances
            .get(self.selected_instance)
            .map(|i| i.id.as_str())
            .unwrap_or("");
        format!("{inst}/{dir}")
    }

    /// 世界选中框描边动画是否还在播（主循环据此提高重绘频率）
    pub fn world_anim_playing(&self) -> bool {
        self.world_anim
            && self
                .world_anim_start
                .is_some_and(|t| t.elapsed().as_millis() < u128::from(WORLD_ANIM_MS))
    }

    /// 世界选中框描边进度：0 = 还没描上，1 = 描完（框常驻）。
    /// 设置里关掉动画后恒为 1——框直接完整出现，不播过程。
    pub fn world_anim_progress(&self) -> f32 {
        if !self.world_anim {
            return 1.0;
        }
        match self.world_anim_start {
            Some(t0) => (t0.elapsed().as_millis() as f32 / WORLD_ANIM_MS as f32).min(1.0),
            None => 1.0,
        }
    }

    /// 「视图」选中块刷色动画是否还在播（主循环据此提高重绘频率）
    pub fn view_anim_playing(&self) -> bool {
        self.view_anim_start
            .is_some_and(|t| t.elapsed().as_millis() < u128::from(VIEW_ANIM_MS))
    }
    /// 「视图」选中块刷色进度：0 = 还没刷到，1 = 刷满（底色常驻）
    pub fn view_anim_progress(&self) -> f32 {
        match self.view_anim_start {
            Some(t0) => (t0.elapsed().as_millis() as f32 / VIEW_ANIM_MS as f32).min(1.0),
            None => 1.0,
        }
    }

    /// 资源中心「来源条」刷色动画是否还在播（主循环据此提高重绘频率）
    pub fn src_anim_playing(&self) -> bool {
        self.src_anim_start
            .is_some_and(|t| t.elapsed().as_millis() < u128::from(SRC_ANIM_MS))
    }

    /// 来源条刷色进度：0 = 还没刷到，1 = 刷满（底色常驻）
    pub fn src_anim_progress(&self) -> f32 {
        match self.src_anim_start {
            Some(t0) => (t0.elapsed().as_millis() as f32 / SRC_ANIM_MS as f32).min(1.0),
            None => 1.0,
        }
    }

    /// 来源条选中项变化的**唯一入口**（切来源时调，重放刷色动画）
    pub fn bump_src_anim(&mut self) {
        self.src_anim_start = Some(std::time::Instant::now());
    }

    /// 服务器卡片网格选中块刷色动画是否还在播（主循环据此提高重绘频率）
    pub fn sel_anim_playing(&self) -> bool {
        self.sel_anim_start
            .is_some_and(|t| t.elapsed().as_millis() < u128::from(SEL_ANIM_MS))
    }

    /// 卡片网格选中块刷色进度：0 = 还没刷到，1 = 刷满（底色常驻）
    pub fn sel_anim_progress(&self) -> f32 {
        match self.sel_anim_start {
            Some(t0) => (t0.elapsed().as_millis() as f32 / SEL_ANIM_MS as f32).min(1.0),
            None => 1.0,
        }
    }

    /// 下载页/下载中心有活动任务：主循环提高重绘频率，让长进度条的涌动波跑起来。
    pub fn download_anim_playing(&self) -> bool {
        let showing = self.home_view == HomeView::Download
            || (self.home_view == HomeView::Resource && self.map_page == MapPage::Download);
        showing
            && self
                .install_tasks
                .iter()
                .any(|t| t.status == InstallStatus::Active)
    }

    /// 卡片网格选中项变化的**唯一入口**（换卡片时调，重放刷色动画）
    pub fn bump_sel_anim(&mut self) {
        self.sel_anim_start = Some(std::time::Instant::now());
    }

    /// 换主视图的**唯一入口**：真的换了才重放「视图」选中块的刷色动画。
    /// 只负责切视图本身（主题/侧栏刷新等副作用由调用方接着做）。
    pub fn set_home_view(&mut self, view: HomeView) {
        if self.home_view != view {
            self.home_view = view;
            self.view_anim_start = Some(std::time::Instant::now());
        }
    }

    /// 异步加载世界缩略图（本地小文件；解码是 CPU 活，放 blocking 池）
    pub fn spawn_world_icon(&mut self, key: String, path: PathBuf) {
        let tx = self.tx.clone();
        let picker = self.picker.clone();
        tokio::spawn(async move {
            let proto = tokio::task::spawn_blocking(move || {
                let bytes = std::fs::read(&path).ok()?;
                let img = image::load_from_memory(&bytes).ok()?;
                Some(picker.new_resize_protocol(img))
            })
            .await
            .ok()
            .flatten();
            let _ = tx.send(TaskMsg::WorldIconLoaded { key, proto }).await;
        });
    }

    /// 保存会话状态（退出时调用）
    /// 记一次「最近访问」。写盘立刻做——这些记录的丢失代价很低（顶多少了历史入口），
    /// 但如果只在正常退出时存，崩溃/强杀就会丢。
    pub fn record_recent(&mut self, kind: RecentKind, id: &str, name: &str, detail: &str) {
        let item = storage::RecentItem::new(id, name, detail);
        let list = match kind {
            RecentKind::JavaServer => &mut self.recents.java_servers,
            RecentKind::Download => &mut self.recents.downloads,
        };
        storage::push_recent(list, item);
        if let Err(e) = storage::save_recents(&self.cfg_root, &self.recents) {
            eprintln!("[recents] 写入失败: {e}");
        }
    }

    pub fn save_session(&self) {
        let view_idx = HomeView::ALL
            .iter()
            .position(|v| *v == self.home_view)
            .unwrap_or(0);
        let source_idx = ResourceSource::ALL
            .iter()
            .position(|s| *s == self.source)
            .unwrap_or(0);
        let state = storage::SessionState {
            home_view: view_idx,
            selected_instance: self.selected_instance,
            selected_world: self.selected_world,
            selected_account: self.selected_account,
            resource_source: source_idx,
            category: self.category,
            role_name: self.role_name.clone(),
            home_focus: self.home_focus.as_index(),
        };
        let _ = storage::save_session(&self.cfg_root, &state);
    }

    /// 提交 LittleSkin OAuth 应用登记（同步保存 + 引导申请白名单）
    pub fn submit_ls_config(&mut self) {
        let app_name = self.ls.app_name.trim().to_string();
        let callback_url = self.ls.callback_url.trim().to_string();
        if app_name.is_empty() {
            self.ls.status = "请填写应用名".into();
            return;
        }
        if !callback_url.starts_with("https://") && !callback_url.starts_with("http://") {
            self.ls.status = "回调 URL 需以 http(s):// 开头".into();
            return;
        }
        let client_id = self.ls.client_id.trim().to_string();
        let mut cfg = storage::load_ls().unwrap_or_default();
        cfg.app_name = app_name;
        cfg.callback_url = callback_url;
        cfg.client_id = client_id.clone();
        match storage::save_ls(&cfg) {
            Ok(()) => {
                self.ls.input = false;
                if client_id.is_empty() {
                    // 未填 client_id：引导去 OAuth 管理页复制（已过白名单则无需再发邮件）
                    self.ls.status = format!(
                        "到 littleskin.cn/user/oauth/manage 复制客户端 ID 后再次 Enter；未过白名单则邮件 {} 申请",
                        crate::api::littleskin::SUPPORT_EMAIL
                    );
                    self.status = "LittleSkin：到 OAuth 管理页复制「客户端 ID」".into();
                    let _ = std::process::Command::new("xdg-open")
                        .arg(crate::api::littleskin::OAUTH_MANAGE_URL)
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null())
                        .spawn();
                } else {
                    // 已有 client_id：直接发起登录
                    self.start_ls_login();
                }
            }
            Err(e) => self.ls.status = format!("保存失败: {e}"),
        }
    }

    /// LittleSkin 设备码流登录：请求设备码 → 打开浏览器 → 后台轮询
    pub fn start_ls_login(&mut self) {
        let client_id = self.ls.client_id.trim().to_string();
        if client_id.is_empty() {
            self.ls.status = "请先在登记里填写 client_id（白名单审核通过后分配）".into();
            return;
        }
        if self.ls.login.is_some() {
            return; // 已在进行中
        }
        self.ls.status = "请求设备码…".into();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let http = reqwest::Client::new();
            match crate::api::littleskin::request_device_code(&http, &client_id).await {
                Ok(pair) => {
                    let _ = tx.send(TaskMsg::LsDeviceCode { pair, client_id }).await;
                }
                Err(e) => {
                    let _ = tx.send(TaskMsg::LsFailed(format!("{e:#}"))).await;
                }
            }
        });
    }

    /// 轮询循环：由收到 LsDeviceCode 后启动
    fn spawn_ls_poll(&mut self, device_code: String, interval: u64) {
        let client_id = self.ls.client_id.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let http = reqwest::Client::new();
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(interval.max(1))).await;
                match crate::api::littleskin::poll_token(&http, &client_id, &device_code).await {
                    Ok(Some(access)) => {
                        let _ = tx.send(TaskMsg::LsTokenGot(access)).await;
                        return;
                    }
                    Ok(None) => continue,
                    Err(e) => {
                        let _ = tx.send(TaskMsg::LsFailed(format!("{e:#}"))).await;
                        return;
                    }
                }
            }
        });
    }

    /// 收到 access_token：拉角色档案（单角色直接完成，多角色进选择）
    fn on_ls_token(&mut self, access: String) {
        self.ls.status = "获取角色档案…".into();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let http = reqwest::Client::new();
            match crate::api::littleskin::fetch_profiles(&http, &access).await {
                Ok(profiles) => {
                    let _ = tx.send(TaskMsg::LsProfiles { access, profiles }).await;
                }
                Err(e) => {
                    let _ = tx.send(TaskMsg::LsFailed(format!("{e:#}"))).await;
                }
            }
        });
    }

    /// 选定角色：换取 Minecraft 令牌并写入当前账户显示
    fn on_ls_profile_picked(&mut self, access: String, uuid: String, name: String) {
        self.ls.status = "获取 Minecraft 令牌…".into();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let http = reqwest::Client::new();
            let result = crate::api::littleskin::fetch_mc_token(&http, &access, &uuid).await;
            let _ = tx.send(TaskMsg::LsDone { name, result }).await;
        });
    }

    /// 处理异步消息。
    /// /// 这里只做**分派**：36 个变体各归一个 `on_*` 处理器。原先是 524 行的单函数，
    /// 读任何一条消息的处理都得从头线性扫。现在这个 match 读起来像目录，
    /// 顺着变体名就能跳到对应实现。
    pub fn handle_msg(&mut self, msg: TaskMsg) {
        match msg {
            TaskMsg::CategoryLoaded {
                offset,
                items,
                error,
                end,
            } => self.on_category_loaded(offset, items, error, end),
            TaskMsg::BannerShotLoaded(proto) => {
                self.banner_shot = proto;
            }
            TaskMsg::WorldIconLoaded { key, proto } => {
                self.world_icons.insert(key, proto);
            }
            TaskMsg::AvatarLoaded(proto) => {
                self.avatar = proto;
            }
            TaskMsg::CoverLoaded { index, img } => {
                let protocol = img.map(|i| self.picker.new_resize_protocol(i));
                if index == usize::MAX {
                    self.detail_cover = protocol;
                } else if index < self.grid_covers.len() {
                    self.grid_covers[index] = protocol;
                }
            }
            TaskMsg::SrvCoverLoaded { index, gen, img } => {
                if gen != self.srv_cover_gen {
                    return;
                }
                self.srv_cover_queue.retain(|&i| i != index);
                if img.is_none() && index < self.srv_cover_none.len() {
                    // 没图或解码失败 → 卡片退回纯文字，不留空占位
                    self.srv_cover_none[index] = true;
                }
                let protocol = img.map(|i| self.picker.new_resize_protocol(i));
                if index < self.srv_covers.len() {
                    self.srv_covers[index] = protocol;
                }
            }
            TaskMsg::BeChecked { index, be } => {
                self.be_queue.retain(|&i| i != index);
                if let Some(b) = be {
                    if index < self.grid_be.len() {
                        self.grid_be[index] = Some(b);
                    }
                } else if index < self.be_tries.len() {
                    self.be_tries[index] += 1;
                }
            }
            TaskMsg::DetailLoaded(detail) => self.on_detail_loaded(detail),
            TaskMsg::DownloadInfoLoaded(v) => {
                self.detail_versions = v;
                self.detail_selected = 0;
            }
            TaskMsg::TaskDone { id, ok, message } => self.on_task_done(id, ok, message),
            TaskMsg::LoginDone(result) => self.on_login_done(result),
            TaskMsg::LoginCaptcha(r) => self.on_login_captcha(r),
            TaskMsg::LaunchStage(stage) => {
                self.status = stage.clone();
                self.launch_status = stage;
            }
            TaskMsg::LaunchDone(result) => self.on_launch_done(result),
            TaskMsg::GameLog(line) => self.on_game_log(line),
            TaskMsg::ManifestLoaded(result) => {
                self.dl.manifest_loading = false;
                match result {
                    Ok(list) => {
                        self.dl.manifest = list;
                        self.dl.install_status =
                            format!("共 {} 个官方版本", self.dl.manifest.len());
                    }
                    Err(e) => self.dl.install_status = format!("清单加载失败: {e}"),
                }
            }
            TaskMsg::ModpacksLoaded { result, offset } => self.on_modpacks_loaded(result, offset),
            TaskMsg::GhLoaded { result, initial } => self.on_gh_loaded(result, initial),
            TaskMsg::GhInstalled { name, result } => self.on_gh_installed(name, result),
            TaskMsg::MrCoverLoaded { index, img } => {
                let protocol = img.map(|i| self.picker.new_resize_protocol(i));
                if index < self.mr_covers.len() {
                    self.mr_covers[index] = protocol;
                }
            }
            TaskMsg::InstallStage {
                id,
                phase,
                done,
                total,
            } => self.on_install_stage(id, phase, done, total),
            TaskMsg::FileProgress { id, f } => self.on_file_progress(id, f),
            TaskMsg::MrVersionsLoaded { title, result } => {
                self.mr_detail_loading = false;
                self.mr_detail_title = title;
                match result {
                    Ok(versions) => self.mr_versions = versions,
                    Err(e) => {
                        self.mr_versions.clear();
                        self.dl.install_status = format!("获取版本失败: {e:#}");
                    }
                }
            }
            TaskMsg::InstallDone { id, result } => self.on_install_done(id, result),
            TaskMsg::VersionsLoaded(res) => {
                self.plugin_busy = false;
                match res {
                    Ok(items) => {
                        let n = items.len();
                        self.plugin_selected = self.plugin_selected.min(n.saturating_sub(1));
                        self.plugin_status =
                            format!("{n} 个版本 · [Enter]安装/启动 [d]删除 [r]刷新");
                        self.plugin_items = items;
                    }
                    Err(e) => self.plugin_status = format!("加载失败: {e:#}"),
                }
            }
            TaskMsg::VersionsStatus(st) => {
                self.plugin_status = st;
            }
            TaskMsg::VersionsDone((act, id, res)) => {
                let ok = res.is_ok();
                self.plugin_busy = false;
                self.plugin_status = match res {
                    Ok(m) => m,
                    Err(e) => format!("失败: {e:#}"),
                };
                if ok && (act == "install" || act == "remove") {
                    self.spawn_plugin_versions(); // 刷新 installed 标记
                }
                let _ = id;
            }
            TaskMsg::GameExit(code) => self.on_game_exit(code),
            TaskMsg::ServerListLoaded(list) => self.on_server_list_loaded(list),
            TaskMsg::UserServersLoaded { path, result } => {
                self.on_user_servers_loaded(path, result)
            }
            TaskMsg::ServerDone(result) => self.on_server_done(result),
            TaskMsg::PluginInstallDone(result) => match result {
                Ok(info) => {
                    let bin = info
                        .bin
                        .as_ref()
                        .map(|b| b.display().to_string())
                        .unwrap_or_else(|| "（无二进制）".into());
                    self.status = format!("已安装插件 {} v{} · {}", info.name, info.version, bin);
                }
                Err(e) => self.status = format!("插件安装失败: {e:#}"),
            },
            TaskMsg::LsDeviceCode { pair, client_id } => self.on_ls_device_code(pair, client_id),
            TaskMsg::LsTokenGot(access) => {
                self.on_ls_token(access);
            }
            TaskMsg::LsProfiles { access, profiles } => {
                if profiles.is_empty() {
                    self.ls.login = None;
                    self.ls.status = "账号下没有 Minecraft 角色".into();
                } else {
                    let p0 = profiles[0].clone();
                    self.ls.status = format!("{} 个角色，选择 {}", profiles.len(), p0.name);
                    self.on_ls_profile_picked(access, p0.uuid, p0.name);
                }
            }
            TaskMsg::LsDone { name, result } => self.on_ls_done(name, result),
            TaskMsg::LsFailed(msg) => {
                self.ls.login = None;
                self.ls.status = format!("LittleSkin 登录失败: {msg}");
            }
        }
    }

    // ---------- 消息处理器（handle_msg 分派目标） ----------

    /// 网易资源分类列表到达：本地排序 / 追加 / 自动续拉
    fn on_category_loaded(
        &mut self,
        offset: i64,
        items: Vec<ShopItem>,
        error: Option<String>,
        end: bool,
    ) {
        self.grid_loading = false;
        if let Some(e) = error {
            self.grid_end = true;
            self.status = format!("加载失败: {e}");
            if e.contains("登录") {
                self.cred = None;
                self.grid_auto_all = false;
                self.login_status = Some("登录已过期，请重新登录".into());
            }
            return;
        }
        if offset == self.grid_offset {
            if offset == 0 || self.grid_items.is_empty() {
                let mut items = items;
                // 本地排序（下载量/热度）
                if self.grid_sort == NeteaseSort::Downloads {
                    items.sort_by(|a, b| b.download_num.cmp(&a.download_num));
                } else if self.grid_sort == NeteaseSort::Hot {
                    items.sort_by(|a, b| b.like_num.cmp(&a.like_num));
                }
                self.grid_items = items;
                self.grid_selected = 0;
                self.grid_covers = (0..self.grid_items.len()).map(|_| None).collect();
                self.cover_queue = Vec::new();
                self.grid_be = (0..self.grid_items.len()).map(|_| None).collect();
                self.be_queue = Vec::new();
                self.be_tries = vec![0; self.grid_items.len()];
            } else {
                let new_count = items.len();
                let base = self.grid_items.len();
                self.grid_items.extend(items);
                self.grid_covers.extend((0..new_count).map(|_| None));
                self.grid_be.extend((0..new_count).map(|_| None));
                self.be_tries.extend((0..new_count).map(|_| 0));
                if self.grid_selected >= base {
                    self.grid_selected = base;
                }
            }
            self.grid_end = end;
            self.status = if self.grid_end {
                format!("全部 {} 个资源", self.grid_items.len())
            } else {
                format!("已加载 {} 个，继续滚动加载更多", self.grid_items.len())
            };
        }
        // 全量加载：进入资源页后持续拉取直到全部加载完
        if self.grid_auto_all && !self.grid_end && !self.grid_loading {
            self.spawn_load_more();
        }
    }

    /// 详情到达：顺带把首图抓回来（index=usize::MAX 表示详情封面）
    fn on_detail_loaded(&mut self, detail: Option<ItemDetail>) {
        self.detail_loading = false;
        self.detail = detail;
        if let Some(d) = &self.detail {
            let url = d.brief_image_urls.first().cloned().unwrap_or_default();
            if !url.is_empty() {
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    let bytes = api::get_bytes(&url).await.ok();
                    let img = bytes.and_then(|b| image::load_from_memory(&b).ok());
                    let _ = tx
                        .send(TaskMsg::CoverLoaded {
                            index: usize::MAX,
                            img,
                        })
                        .await;
                });
            }
        }
    }

    /// 4399 登录结果：先落盘再改状态，保存失败不假报成功
    fn on_login_done(&mut self, result: Result<Credentials>) {
        match result {
            Ok(c) => {
                if let Err(e) = storage::save(&c) {
                    self.login_status = Some(format!("保存失败: {e}"));
                    return;
                }
                self.cred = Some(c.clone());
                self.login_status = Some("登录成功 ✓".into());
                self.user_name = c.user_id.clone();
                self.login_screen = None;
                self.map_page = MapPage::Grid;
                self.grid_auto_all = true;
                self.spawn_load_grid(false);
            }
            Err(e) => self.login_status = Some(format!("登录失败: {e}")),
        }
    }

    fn on_launch_done(&mut self, result: Result<u32>) {
        self.launching = false;
        match result {
            Ok(pid) => {
                self.game_pid = Some(pid);
                self.launch_status = format!("运行中 PID {pid}");
                self.status = format!("游戏已启动 PID {pid}");
                // 等游戏日志出现 "Done"（世界加载完成）后由 on_game_log 启动自动截图
            }
            Err(e) => {
                // 提示里可能带换行（安装命令另起一行）。状态栏只放第一行，
                // 完整内容进游戏日志（[L] 打开），别把两行文字挤进状态栏。
                let full = format!("{e:#}");
                self.game_log.push(format!("[启动失败] {full}"));
                let head = full.lines().next().unwrap_or("启动失败");
                self.launch_status = format!("启动失败: {head}");
                self.status = format!("启动失败: {head}");
                // 还有后续内容（多半是安装指引）时提示去哪看
                if full.lines().count() > 1 {
                    self.status.push_str("  [L] 看详情");
                }
            }
        }
    }

    /// 游戏日志行：滚存最近 200 行；检测到进入世界则启动自动截图
    fn on_game_log(&mut self, line: String) {
        self.game_log.push(line.clone());
        if self.game_log.len() > 200 {
            self.game_log.drain(..self.game_log.len() - 200);
        }
        // 世界加载完成、玩家进入世界（可操控）后才开始自动截图，避免截到加载界面。
        // 中英双标志：中文版无 "Done (...)" 行（本地化省略），用 加入了游戏/joined the game
        let entered = line.contains("加入了游戏")
            || line.contains("joined the game")
            || line.contains("Done (");
        if entered {
            // 诊断：匹配到进入世界日志，落盘留痕
            let _ = std::fs::write(
                self.mc_dir.join("mctl/shot-debug.log"),
                self.game_log.join("\n"),
            );
            self.start_shots();
        }
    }

    /// Modrinth 整合包搜索结果：首屏替换 / 翻页追加
    fn on_modpacks_loaded(
        &mut self,
        result: Result<Vec<launcher::modpack::ModpackHit>>,
        offset: usize,
    ) {
        self.dl.searching = false;
        match result {
            Ok(list) => {
                // 不足一页 = 服务端已经没有更多；整页才继续翻，省一次空请求
                let short = list.len() < MODRINTH_PAGE;
                if offset == 0 {
                    self.dl.results = list;
                    self.dl.sel_result = 0;
                    self.mr_covers = (0..self.dl.results.len()).map(|_| None).collect();
                    self.mr_cover_queue = Vec::new();
                } else {
                    let base = self.dl.results.len();
                    self.dl.results.extend(list);
                    self.mr_covers
                        .extend((base..self.dl.results.len()).map(|_| None));
                }
                self.dl.more_end = short;
                self.dl.install_status = format!("找到 {} 个整合包", self.dl.results.len());
            }
            Err(e) => self.dl.install_status = format!("搜索失败: {e}"),
        }
    }

    /// GitHub 搜索结果：注册表为空时的宽泛回退要说明白，否则用户以为搜索坏了
    fn on_gh_loaded(&mut self, result: Result<api::github::SearchResult>, initial: bool) {
        self.gh_searching = false;
        match result {
            Ok(r) => {
                let fell_back = initial
                    && self.dl.query.trim().is_empty()
                    && r.total > 0
                    && !r.repos.iter().any(|x| x.is_plugin());
                self.gh_total = r.total;
                self.gh_rate = match &r.rate_remaining {
                    Some(v) => format!("· 剩余配额 {v}"),
                    None => String::new(),
                };
                self.gh_results = r.repos;
                self.gh_selected = 0;
                self.refilter_github();
                self.dl.install_status = if fell_back {
                    format!(
                        "topic:{} 暂无仓库，先按名字搜；作者打上该 topic 即可被收录（{}）",
                        api::github::PLUGIN_TOPIC,
                        self.gh_rate
                    )
                } else {
                    format!("找到 {} 个仓库{}", self.gh_filtered.len(), self.gh_rate)
                };
            }
            Err(e) => self.dl.install_status = format!("搜索失败: {e}"),
        }
    }

    fn on_gh_installed(&mut self, name: String, result: Result<crate::plugin::PluginInfo>) {
        self.dl.installing = false;
        match result {
            Ok(info) => {
                let bin = info
                    .bin
                    .as_ref()
                    .map(|b| b.display().to_string())
                    .unwrap_or_else(|| "（无二进制）".into());
                self.dl.install_status = format!("已安装 {name} v{}：{bin}", info.version);
            }
            Err(e) => self.dl.install_status = format!("安装 {name} 失败: {e}"),
        }
    }

    /// 安装进度：同时更新状态栏与下载中心的任务条目
    fn on_install_stage(&mut self, id: u64, phase: String, done: usize, total: usize) {
        self.status = format!("{phase} {done}/{total}");
        self.dl.install_status = self.status.clone();
        if let Some(t) = self.install_tasks.iter_mut().find(|t| t.id == id) {
            t.phase = phase;
            t.done = done;
            t.total = total;
        }
    }

    /// 单文件进度：更新下载中心里对应文件的细线。
    /// /// 同名文件复用同一个槽位（重试/多源回退时会重复推同一个 name）；
    /// 排序按总量降序——大文件在头，长尾里的小文件不占视觉焦点。
    fn on_file_progress(&mut self, id: u64, f: launcher::FileProgress) {
        let Some(task) = self.install_tasks.iter_mut().find(|t| t.id == id) else {
            return;
        };
        let now = std::time::Instant::now();
        let idx = match task.files.iter().position(|s| s.name == f.name) {
            Some(i) => i,
            None => {
                task.files.push(FileSlot {
                    name: f.name.clone(),
                    done: 0,
                    total: 0,
                    started: now,
                    failed: false,
                    finished: false,
                });
                task.files.len() - 1
            }
        };
        // 任务总字节：文件槽会被裁剪、终态又会把 done 顶到 u64::MAX，
        // 所以这里按「增量」单独累加，和槽位的显示状态解耦。
        let (prev_done, prev_total) = {
            let s = &task.files[idx];
            let pd = if s.done == u64::MAX { s.total } else { s.done };
            (pd, s.total)
        };
        // 终态消息按约定不带长度（total=0），别把已经量到的长度抹掉——
        // 抹掉的话完成行会显示成 "✓ 0 B"
        let new_total = if f.total > 0 { f.total } else { prev_total };
        let cur_done = match f.finished_at {
            // 成功落地按总长收尾；失败就按已下多少算多少
            Some(_) if !f.failed => new_total.max(prev_done),
            Some(_) => prev_done,
            None => f.done,
        };
        task.bytes_done += cur_done.saturating_sub(prev_done);
        task.bytes_total += new_total.saturating_sub(prev_total);
        let slot = &mut task.files[idx];
        slot.total = new_total;
        if f.finished_at.is_some() {
            slot.finished = true;
            slot.failed = f.failed;
            slot.done = if f.failed { prev_done } else { u64::MAX };
        } else {
            slot.done = f.done;
            slot.failed = false;
        }
        // 长度未知（服务器没给 Content-Length）时，总量跟着已下量涨，
        // 任务总进度条才不会永远停在 0%
        if task.bytes_total < task.bytes_done {
            task.bytes_total = task.bytes_done;
        }
        // 大文件排最前；同量的按名字稳定排序，避免每帧跳动
        task.files
            .sort_by(|a, b| b.total.cmp(&a.total).then_with(|| a.name.cmp(&b.name)));
        // 只留正在下的 + 最近完成的，防止 3911 个条目把 UI 撑爆
        const MAX_SLOTS: usize = 12;
        if task.files.len() > MAX_SLOTS {
            let keep_unfinished: Vec<usize> = (0..task.files.len())
                .filter(|&i| !task.files[i].finished)
                .collect();
            let drop_from = task.files.len() - MAX_SLOTS;
            // 从尾部删已完成的，优先删容量最小的（最小的最不重要）
            let mut victims: Vec<usize> = (0..task.files.len())
                .filter(|&i| task.files[i].finished)
                .collect();
            victims.sort_by_key(|&i| std::cmp::Reverse(task.files[i].total));
            for v in victims.into_iter().take(drop_from) {
                if !keep_unfinished.contains(&v) {
                    task.files.remove(v);
                }
            }
        }
    }

    /// 资源/模组/地图下载任务的终态：只动下载中心那一条，不碰实例列表
    /// （实例列表是"装完一个版本"，和"下了一个资源"不是一回事）。
    fn on_task_done(&mut self, id: u64, ok: bool, message: String) {
        if let Some(t) = self.install_tasks.iter_mut().find(|t| t.id == id) {
            t.status = if ok {
                InstallStatus::Done
            } else {
                InstallStatus::Failed
            };
            t.phase = message.clone();
            if ok {
                t.bytes_done = t.bytes_total;
            }
        }
        self.status = message;
    }

    /// 安装完成：刷新实例列表并夹住选中项
    fn on_install_done(&mut self, id: u64, result: Result<String>) {
        self.dl.installing = false;
        match result {
            Ok(done) => {
                self.dl.install_status = format!("安装完成: {done}");
                if let Some(t) = self.install_tasks.iter_mut().find(|t| t.id == id) {
                    t.status = InstallStatus::Done;
                    t.phase = format!("完成: {done}");
                }
                self.instances = launcher::scan_instances(&self.mc_dir);
                Self::append_bedrock_instances(&mut self.instances);
                if !self.instances.is_empty() && self.selected_instance >= self.instances.len() {
                    self.selected_instance = self.instances.len() - 1;
                }
            }
            Err(e) => {
                self.dl.install_status = format!("安装失败: {e}");
                if let Some(t) = self.install_tasks.iter_mut().find(|t| t.id == id) {
                    t.status = InstallStatus::Failed;
                    t.phase = format!("失败: {e}");
                }
            }
        }
    }

    /// 游戏退出：停自动截图、清旧图、刷横幅
    fn on_game_exit(&mut self, code: Option<i32>) {
        match code {
            Some(0) => {
                self.launch_status = "游戏已退出".into();
                self.status = "游戏已退出".into();
            }
            Some(c) => {
                self.launch_status = format!("游戏崩溃，退出码 {c}（日志见下载页）");
                self.status = format!("游戏崩溃，退出码 {c}");
            }
            None => {
                self.launch_status = "游戏被信号终止".into();
                self.status = "游戏被信号终止".into();
            }
        }
        self.game_pid = None;
        // 停止自动截图，清理旧图并立即刷新横幅（游戏刚退，有新截图）
        let shot = self.shot_task.take();
        let had_shot_task = shot.is_some();
        if let Some(t) = shot {
            t.abort();
        }
        // 诊断：本次会话未触发自动截图 → 落盘收到的全部游戏日志（排查 Done 匹配/管道）
        if !had_shot_task {
            let _ = std::fs::write(
                self.mc_dir.join("mctl/shot-debug.log"),
                self.game_log.join("\n"),
            );
        }
        trim_shots(&self.shots_dir());
        self.spawn_banner_shot();
    }

    fn on_server_list_loaded(&mut self, list: Vec<crate::api::mcserver::JavaServer>) {
        self.server_loading = false;
        self.servers = list;
        self.selected_server = 0;
        self.srv_scroll = 0;
        self.reset_server_covers();
        if !self.server_launching {
            self.server_status = self.server_tab_status();
        }
    }

    fn on_server_done(&mut self, result: Result<u32>) {
        self.server_launching = false;
        match result {
            Ok(pid) => {
                self.game_pid = Some(pid);
                self.server_status = format!("已进入服务器，游戏运行中 PID {pid}");
                self.status = format!("进服成功 PID {pid}");
            }
            Err(e) => {
                let hint = if e.to_string().contains("verify username")
                    || e.to_string().contains("Failed to login")
                    || e.to_string().contains("401")
                {
                    "（正版验证服需要 Microsoft 登录，alpha 暂不支持）"
                } else {
                    ""
                };
                self.server_status = format!("进服失败: {e}{hint}");
                self.status = format!("进服失败: {e}");
            }
        }
    }

    /// LittleSkin 设备码：展示码 + 自动开浏览器 + 起轮询
    fn on_ls_device_code(
        &mut self,
        pair: crate::api::littleskin::DeviceCodePair,
        client_id: String,
    ) {
        self.ls.login = Some(LsLogin {
            user_code: pair.user_code.clone(),
            verification_url: pair.verification_uri_complete.clone(),
        });
        let _ = client_id;
        self.ls.status = format!(
            "请在浏览器打开 {} 并输入 {}（{}s 内有效）",
            pair.verification_uri_complete, pair.user_code, pair.expires_in
        );
        // 自动打开带 user_code 的授权页
        let url = pair.verification_uri_complete;
        std::thread::spawn(move || {
            let _ = std::process::Command::new("xdg-open")
                .arg(url)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        });
        self.spawn_ls_poll(pair.device_code, pair.interval);
    }

    /// LittleSkin 换取令牌成功：存盘、更新账户、起头像抓取
    fn on_ls_done(&mut self, name: String, result: Result<crate::api::littleskin::McToken>) {
        match result {
            Ok(token) => {
                if let Some(profile) = &token.selected_profile {
                    self.spawn_ls_avatar(&profile.uuid);
                }
                // 持久化令牌/角色：启动时用 authlib-injector 外置登录
                if let Ok(mut cfg) = storage::load_ls() {
                    cfg.access_token = token.access_token.clone();
                    cfg.name = name.clone();
                    if let Some(p) = &token.selected_profile {
                        cfg.uuid = p.uuid.clone();
                    }
                    let _ = storage::save_ls(&cfg);
                }
                if let Some(acc) = self
                    .accounts
                    .iter_mut()
                    .find(|a| a.provider.contains("LittleSkin"))
                {
                    acc.name = name.clone();
                    acc.logged_in = true;
                } else {
                    self.accounts.push(Account {
                        provider: "[INT] LittleSkin".into(),
                        name: name.clone(),
                        logged_in: true,
                    });
                }
                self.ls.login = None;
                self.ls.status = format!("LittleSkin 登录成功: {name}");
            }
            Err(e) => self.ls.status = format!("换取令牌失败: {e:#}"),
        }
    }

    // ─────────── 设置页交互 ───────────

    /// 当前焦点指向的字段（None = 焦点在按钮上）
    pub fn set_focused_field(&self) -> Option<SettingField> {
        SettingField::ALL.get(self.set_focus).copied()
    }

    /// 当前焦点指向的按钮（None = 焦点在字段上）
    pub fn set_focused_btn(&self) -> Option<SettingBtn> {
        let n = SettingField::ALL.len();
        self.set_focus
            .checked_sub(n)
            .and_then(|i| SettingBtn::ALL.get(i).copied())
    }

    /// 进入设置页：拍一份快照供「放弃」用
    pub fn enter_settings(&mut self) {
        self.set_focus = 0;
        self.set_editing = false;
        self.set_buf.clear();
        self.set_dirty = false;
        self.set_msg.clear();
        self.set_snapshot = Some(self.snapshot_settings());
    }

    fn snapshot_settings(&self) -> storage::Settings {
        storage::Settings {
            game_dir: Some(self.mc_dir.display().to_string()),
            download_dir: Some(self.download_dir.display().to_string()),
            grid_cols: Some(self.grid_cols),
            grid_rows: Some(self.grid_rows),
            auto_shot: Some(self.auto_shot),
            ascii_always: Some(self.ascii_always),
            world_anim: Some(self.world_anim),
            theme: Some(self.theme.clone()),
        }
    }

    /// 把当前 App 状态写进 App（用于放弃/恢复默认/重读）
    fn restore_settings(&mut self, s: &storage::Settings) {
        if let Some(v) = &s.game_dir {
            self.mc_dir = PathBuf::from(v);
            self.instances = launcher::scan_instances(&self.mc_dir);
            App::append_bedrock_instances(&mut self.instances);
            self.selected_instance = self
                .selected_instance
                .min(self.instances.len().saturating_sub(1));
            self.refresh_worlds();
        }
        if let Some(v) = &s.download_dir {
            self.download_dir = PathBuf::from(v);
            self.download_dir_str = v.clone();
        }
        if let Some(v) = s.grid_cols {
            self.grid_cols = v;
        }
        if let Some(v) = s.grid_rows {
            self.grid_rows = v;
        }
        if let Some(v) = s.auto_shot {
            self.auto_shot = v;
        }
        // 以前漏了 ascii_always：快照里有、还原时不用 →「放弃」撤不掉顶部大图的改动
        self.ascii_always = s.ascii_always.unwrap_or(false);
        // 开关/主题没有 None 语义：缺键按默认（动画开、mctl 经典配色）
        self.world_anim = s.world_anim.unwrap_or(true);
        self.theme = s
            .theme
            .clone()
            .unwrap_or_else(|| crate::theme::LEGACY_ID.to_string());
        crate::theme::set_theme(&self.theme);
    }

    /// 打开主题选择器：光标停在当前主题上，↑↓ 即时预览、Enter 确定、Esc 还原
    pub fn open_theme_picker(&mut self) {
        let ids = crate::theme::theme_ids();
        let cursor = ids.iter().position(|i| *i == self.theme).unwrap_or(0);
        self.theme_picker = Some(ThemePicker {
            cursor,
            saved: self.theme.clone(),
        });
    }

    /// 主题选择器按键（浮层开着时独占按键）
    fn handle_theme_picker_key(&mut self, key: crossterm::event::KeyEvent) {
        use crossterm::event::KeyCode;
        let ids = crate::theme::theme_ids();
        let Some(tp) = self.theme_picker.as_ref() else {
            return;
        };
        let mut cursor = tp.cursor;
        let saved = tp.saved.clone();
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                cursor = if cursor == 0 {
                    ids.len() - 1
                } else {
                    cursor - 1
                };
                crate::theme::set_theme(&ids[cursor]); // 预览：立刻换色
            }
            KeyCode::Down | KeyCode::Char('j') => {
                cursor = (cursor + 1) % ids.len();
                crate::theme::set_theme(&ids[cursor]);
            }
            // ←→ 跳一列（浮层是列优先的网格，列步长 = 行数）。单列时步长
            // 等于总数、取模后为 0，等于原地不动——这种布局下的自然结果。
            KeyCode::Left | KeyCode::Right => {
                let (tw, th) = self.term_size;
                let g = crate::ui::theme_picker::layout(ids.len(), tw as usize, th as usize);
                let step = g.rows % ids.len();
                cursor = if key.code == KeyCode::Left {
                    (cursor + ids.len() - step) % ids.len()
                } else {
                    (cursor + step) % ids.len()
                };
                crate::theme::set_theme(&ids[cursor]);
            }
            KeyCode::Enter => {
                let id = ids[cursor].clone();
                crate::theme::set_theme(&id);
                self.theme = id.clone();
                self.theme_picker = None;
                self.set_dirty = true;
                self.set_msg = format!("主题 = {}", crate::theme::theme_label(&id));
                return;
            }
            KeyCode::Esc | KeyCode::Char('q') => {
                crate::theme::set_theme(&saved);
                self.theme = saved;
                self.theme_picker = None;
                self.set_msg = "已还原主题（未保存）".into();
                return;
            }
            // 其它键忽略：避免误触把浮层关了
            _ => return,
        }
        if let Some(tp) = self.theme_picker.as_mut() {
            tp.cursor = cursor;
        }
    }

    /// 设置页按键。返回 true = 已消费（不该再走全局）。
    /// /// 用「消费」而不是「全局键白名单」：白名单要永远追着全局键位增长，
    /// 漏一个就表现为该键在这个页面失效（曾经漏了 f 和 Shift+Tab）。
    fn handle_settings_key(&mut self, key: crossterm::event::KeyEvent) -> bool {
        use crossterm::event::{KeyCode, KeyModifiers};
        let n_fields = SettingField::ALL.len();
        let n_all = n_fields + SettingBtn::ALL.len();

        // Ctrl/Alt 组合一律放行（Ctrl+W 调整模式、Alt+数字 直达视图）
        if key.modifiers.contains(KeyModifiers::CONTROL)
            || key.modifiers.contains(KeyModifiers::ALT)
        {
            return false;
        }

        // Shift+Tab 交给全局切视图。crossterm 有两种表示（BackTab / Tab+SHIFT），
        // 两种都放行——表单页惯例是 Tab 向内换字段、Shift+Tab 向外离开。
        if key.code == KeyCode::BackTab
            || (key.code == KeyCode::Tab && key.modifiers.contains(KeyModifiers::SHIFT))
        {
            return false;
        }

        // 编辑态：字符直入，Enter 提交，Esc 取消
        if self.set_editing {
            match key.code {
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.set_buf.push(c)
                }
                KeyCode::Backspace => {
                    self.set_buf.pop();
                }
                KeyCode::Enter => {
                    if let Some(f) = self.set_focused_field() {
                        match f.apply(self, &self.set_buf.clone()) {
                            Ok(()) => {
                                self.set_editing = false;
                                self.set_dirty = true;
                                self.set_msg = format!("{} 已修改（未保存）", f.label());
                            }
                            Err(e) => self.set_msg = format!("✗ {e}"),
                        }
                    }
                }
                KeyCode::Esc => {
                    self.set_editing = false;
                    self.set_buf.clear();
                    self.set_msg = "已取消编辑".into();
                }
                _ => {}
            }
            return true;
        }

        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.set_focus = if self.set_focus == 0 {
                    n_all - 1
                } else {
                    self.set_focus - 1
                }
            }
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
                self.set_focus = (self.set_focus + 1) % n_all
            }
            // 数值字段 ←→ 增减
            KeyCode::Left | KeyCode::Right
                if self.set_focused_field().is_some_and(|f| f.is_num()) =>
            {
                let d = if key.code == KeyCode::Right { 1 } else { -1 };
                if let Some(f) = self.set_focused_field() {
                    f.bump(self, d);
                    self.set_dirty = true;
                    self.set_msg = format!("{} = {}", f.label(), f.value(self));
                }
                return true;
            }
            KeyCode::Char('/') => {
                if let Some(f) = self.set_focused_field() {
                    self.set_buf = f.value(self);
                    self.set_editing = true;
                    return true;
                }
                return false;
            }
            KeyCode::Enter => {
                if let Some(f) = self.set_focused_field() {
                    match f {
                        // 「主题」回车 = 打开主题列表浮层（↑↓ 即时预览）
                        SettingField::Theme => self.open_theme_picker(),
                        // 开关键回车直接切换（没有逐字输入的必要）
                        SettingField::WorldAnim => {
                            f.bump(self, 1);
                            self.set_dirty = true;
                            self.set_msg = format!("{} = {}", f.label(), f.value(self));
                        }
                        // 数值字段回车即进入编辑；文本字段同理
                        _ => {
                            self.set_buf = f.value(self);
                            self.set_editing = true;
                        }
                    }
                } else if let Some(b) = self.set_focused_btn() {
                    self.run_setting_btn(b);
                }
                return true;
            }
            // 非编辑态的 Esc 不消费：放行给全局去退出全屏/日志。
            // 编辑态的 Esc 已在上面处理（取消编辑）。
            KeyCode::Esc => return false,
            _ => return false,
        }
        true
    }

    /// 执行设置页按钮
    fn run_setting_btn(&mut self, b: SettingBtn) {
        match b {
            SettingBtn::Save => {
                let s = self.snapshot_settings();
                match storage::save_settings(&self.cfg_root, &s) {
                    Ok(()) => {
                        self.set_dirty = false;
                        self.set_msg = "✓ 已保存到 settings.json".into();
                    }
                    Err(e) => self.set_msg = format!("✗ 保存失败: {e}"),
                }
            }
            SettingBtn::Revert => {
                if let Some(s) = self.set_snapshot.clone() {
                    self.restore_settings(&s);
                    self.set_dirty = false;
                    self.set_msg = "已放弃本次修改".into();
                }
            }
            SettingBtn::Reload => {
                let s = storage::load_settings(&self.cfg_root);
                self.restore_settings(&s);
                self.set_dirty = false;
                self.set_msg = "已从 settings.json 重新读取".into();
            }
            SettingBtn::Defaults => {
                let root = self.cfg_root.clone();
                self.mc_dir = root.join(".minecraft");
                self.download_dir = root.join("downloads");
                self.download_dir_str = self.download_dir.display().to_string();
                self.grid_cols = 2;
                self.grid_rows = 3;
                self.ascii_always = false;
                self.world_anim = true;
                self.theme = crate::theme::LEGACY_ID.to_string();
                crate::theme::set_theme(&self.theme);
                self.instances = launcher::scan_instances(&self.mc_dir);
                App::append_bedrock_instances(&mut self.instances);
                self.selected_instance = 0;
                self.refresh_worlds();
                self.set_dirty = true;
                self.set_msg = "已恢复默认（记得按「保存」写入）".into();
            }
        }
    }

    // ---------- 按键 ----------

    /// 打开当前选中实例的存档目录（xdg-open 用户默认文件管理器）
    pub fn open_saves(&mut self) {
        let Some(inst) = self.instances.get(self.selected_instance) else {
            self.status = "无实例可选".into();
            return;
        };
        let saves = self.instance_saves_dir(inst);
        if !inst.bedrock {
            self.ensure_java_saves_migrated(inst);
        }
        if let Err(e) = std::fs::create_dir_all(&saves) {
            self.status = format!("创建存档目录失败: {e}");
            return;
        }
        let path = saves.display().to_string();
        match std::process::Command::new("xdg-open").arg(&path).spawn() {
            Ok(_) => self.status = format!("已打开存档目录: {path}"),
            Err(e) => self.status = format!("打开失败（需要 xdg-open）: {e}"),
        }
    }

    /// 追加基岩版实例（extracted/<版本>，启动委托插件）
    pub(crate) fn append_bedrock_instances(instances: &mut Vec<crate::launcher::Instance>) {
        for v in bedrock_launcher_core::installed_versions() {
            instances.push(crate::launcher::Instance {
                bedrock: true,
                id: v.clone(),
                version_type: "基岩".into(),
                main_class: String::new(),
                assets_index: String::new(),
                java_major: 0,
                dir: bedrock_launcher_core::data_dir().join("extracted").join(&v),
            });
        }
    }

    /// 基岩版启动（委托插件：引擎/prereqs/GameInput 全自动）
    fn spawn_launch_bedrock(&mut self, inst: &crate::launcher::Instance) {
        let Some(bin) = self.find_bedrock_plugin() else {
            self.launch_status = "未找到基岩版插件".into();
            return;
        };
        self.launching = true;
        self.launch_status = "启动基岩版…".into();
        self.status = format!("启动基岩版 {} …", inst.id);
        self.game_log.clear();
        let id = inst.id.clone();
        let tx = self.tx.clone();
        let plugin_env = crate::plugin::env_for_bin(&self.plugin_root(), &bin);
        tokio::spawn(async move {
            let result = async {
                let mut child = tokio::process::Command::new(&bin)
                    .arg("launch")
                    .envs(plugin_env)
                    .arg(&id)
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .spawn()
                    .map_err(|e| anyhow::anyhow!("启动插件失败: {e}"))?;
                let pid = child.id().unwrap_or(0);
                // 输出流式进游戏日志（[l] 查看）
                if let Some(mut out) = child.stdout.take() {
                    let tx2 = tx.clone();
                    tokio::spawn(async move {
                        use tokio::io::AsyncBufReadExt;
                        let mut lines = tokio::io::BufReader::new(&mut out).lines();
                        while let Ok(Some(l)) = lines.next_line().await {
                            let _ = tx2.send(TaskMsg::GameLog(l)).await;
                        }
                    });
                }
                if let Some(mut err) = child.stderr.take() {
                    let tx2 = tx.clone();
                    tokio::spawn(async move {
                        use tokio::io::AsyncBufReadExt;
                        let mut lines = tokio::io::BufReader::new(&mut err).lines();
                        while let Ok(Some(l)) = lines.next_line().await {
                            let _ = tx2.send(TaskMsg::GameLog(l)).await;
                        }
                    });
                }
                let st = child.wait().await?;
                if st.success() {
                    Ok(pid)
                } else {
                    anyhow::bail!("基岩版退出码 {:?}", st.code())
                }
            }
            .await;
            let _ = tx.send(TaskMsg::LaunchDone(result)).await;
        });
    }

    /// 基岩版插件定位（provides/versions 且名字含 bedrock）
    fn find_bedrock_plugin(&self) -> Option<PathBuf> {
        crate::plugin::scan_plugins(&self.plugin_root())
            .into_iter()
            .filter(|p| {
                p.name.to_lowercase().contains("bedrock")
                    || p.path
                        .file_name()
                        .is_some_and(|n| n.to_string_lossy().to_lowercase().contains("bedrock"))
            })
            .find_map(|p| {
                (p.path.join("provides").join("versions").is_file() && p.bin.is_some())
                    .then(|| p.bin.unwrap())
            })
    }

    fn find_versions_plugin(&self) -> Option<PathBuf> {
        crate::plugin::find_by_ability(&self.plugin_root(), "versions").and_then(|p| p.bin)
    }

    /// 拉取插件版本列表（进入资源大厅「插件」来源时）
    pub fn spawn_plugin_versions(&mut self) {
        let Some(bin) = self.find_versions_plugin() else {
            self.plugin_status = "未找到版本管理插件（provides/versions）".into();
            return;
        };
        self.plugin_bin = Some(bin.clone());
        self.plugin_busy = true;
        self.plugin_status = "加载版本列表…".into();
        let tx = self.tx.clone();
        let plugin_env = crate::plugin::env_for_bin(&self.plugin_root(), &bin);
        tokio::spawn(async move {
            let out = tokio::process::Command::new(&bin)
                .arg("versions")
                .envs(plugin_env)
                .output()
                .await;
            let res = match out {
                Ok(o) if o.status.success() => {
                    serde_json::from_slice::<Vec<VersionItem>>(&o.stdout)
                        .map_err(|e| anyhow::anyhow!("解析 versions 失败: {e}"))
                }
                Ok(o) => Err(anyhow::anyhow!(
                    "{}",
                    String::from_utf8_lossy(&o.stderr)
                        .trim()
                        .chars()
                        .take(120)
                        .collect::<String>()
                )),
                Err(e) => Err(anyhow::anyhow!("执行插件失败: {e}")),
            };
            let _ = tx.send(TaskMsg::VersionsLoaded(res)).await;
        });
    }

    /// 插件版本动作：install / launch / remove
    fn spawn_plugin_action(&mut self, action: &str) {
        if self.plugin_busy {
            return;
        }
        let Some(item) = self.plugin_items.get(self.plugin_selected).cloned() else {
            return;
        };
        let Some(bin) = self.plugin_bin.clone() else {
            return;
        };
        self.plugin_busy = true;
        self.plugin_status = format!("{action} {} …", item.id);
        let id = item.id.clone();
        let act = action.to_string();
        let tx = self.tx.clone();
        let plugin_env = crate::plugin::env_for_bin(&self.plugin_root(), &bin);
        tokio::spawn(async move {
            let mut child = match tokio::process::Command::new(&bin)
                .arg(&act)
                .arg(&id)
                .envs(plugin_env)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::piped())
                .spawn()
            {
                Ok(c) => c,
                Err(e) => {
                    let _ = tx
                        .send(TaskMsg::VersionsDone((
                            act,
                            id,
                            Err(anyhow::anyhow!("{e}")),
                        )))
                        .await;
                    return;
                }
            };
            // stderr 进度行流式上屏（提取 [n/total] 文件名）
            if let Some(err) = child.stderr.take() {
                use tokio::io::AsyncBufReadExt;
                let tx2 = tx.clone();
                let act2 = act.clone();
                tokio::spawn(async move {
                    let mut lines = tokio::io::BufReader::new(err).lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        let _ = tx2
                            .send(TaskMsg::VersionsStatus(format!("[{act2}] {line}")))
                            .await;
                    }
                });
            }
            let st = child.wait().await;
            let msg = match st {
                Ok(s) if s.success() => Ok(match act.as_str() {
                    "install" => format!("已安装 {id}，回车启动"),
                    "launch" => format!("{id} 已退出"),
                    _ => format!("{act} {id} 完成"),
                }),
                Ok(s) => Err(anyhow::anyhow!("退出码 {:?}", s.code())),
                Err(e) => Err(anyhow::anyhow!("{e}")),
            };
            let _ = tx.send(TaskMsg::VersionsDone((act, id, msg))).await;
        });
    }

    fn handle_plugin_versions_key(&mut self, key: crossterm::event::KeyEvent) {
        use crossterm::event::KeyCode;
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                if self.plugin_selected > 0 {
                    self.plugin_selected -= 1;
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.plugin_selected + 1 < self.plugin_items.len() {
                    self.plugin_selected += 1;
                }
            }
            KeyCode::Enter => {
                let installed = self
                    .plugin_items
                    .get(self.plugin_selected)
                    .map(|i| i.installed);
                match installed {
                    Some(false) => self.spawn_plugin_action("install"),
                    Some(true) => self.spawn_plugin_action("launch"),
                    _ => {}
                }
            }
            KeyCode::Char('d') | KeyCode::Char('D') => self.spawn_plugin_action("remove"),
            KeyCode::Char('r') | KeyCode::Char('R') => self.spawn_plugin_versions(),
            _ => {}
        }
    }

    pub fn handle_key(&mut self, key: crossterm::event::KeyEvent) {
        // 窗口管理器优先：Ctrl+W 进入/退出调整模式；调整模式下方向键/Tab/d 由 wm 消费
        if self.wm.handle_key(key) {
            self.sync_view_from_wm();
            return;
        }
        // 主题选择器浮层：开着时独占按键（↑↓ 预览 / Enter 确定 / Esc 还原）
        if self.theme_picker.is_some() {
            self.handle_theme_picker_key(key);
            return;
        }
        // 添加服务器表单：开着时独占按键
        if self.server_add.is_some() {
            self.handle_server_add_key(key);
            return;
        }
        // LittleSkin 登录输入模式：短路其余按键处理，只走输入
        if self.ls.input && self.home_view != HomeView::Resource {
            self.handle_global_key(key);
            return;
        }
        // 服务器页：Ctrl+1/2/3 切标签；a 添加、x 删除（仅用户标签）
        if self.home_view == HomeView::Server && !self.role_input && !self.shell_input {
            use crossterm::event::{KeyCode, KeyModifiers};
            if key.modifiers.contains(KeyModifiers::CONTROL) {
                let tab = match key.code {
                    KeyCode::Char('1') => Some(ServerTab::User),
                    KeyCode::Char('2') => Some(ServerTab::Recommended),
                    KeyCode::Char('3') => Some(ServerTab::All),
                    _ => None,
                };
                if let Some(tab) = tab {
                    self.set_server_tab(tab);
                    return;
                }
            } else if !key.modifiers.contains(KeyModifiers::ALT) {
                if matches!(key.code, KeyCode::Char('a') | KeyCode::Char('A'))
                    && (self.home_focus == HomeFocus::List
                        || self.home_focus == HomeFocus::Banner
                        || self.server_tab == ServerTab::User)
                {
                    self.open_server_add();
                    return;
                }
                if matches!(key.code, KeyCode::Char('x') | KeyCode::Char('X'))
                    && self.server_tab == ServerTab::User
                    && self.home_focus == HomeFocus::List
                {
                    self.remove_selected_user_server();
                    return;
                }
            }
        }
        if self.home_view == HomeView::Resource {
            self.handle_maps_key(key);
        }
        // 设置页：页面内交互优先，未消费的键继续走全局（f 全屏 / a 登录 / q 退出…）
        if self.home_view == HomeView::Settings && self.handle_settings_key(key) {
            return;
        }
        self.handle_global_key(key);
    }

    /// Alt+1..6 直达主视图（终端把 Alt 拆成 Esc+字符时由 main 循环调用）
    pub fn switch_view_alt(&mut self, c: char) {
        if !c.is_ascii_digit() {
            return;
        }
        let idx = (c as u8 - b'1') as usize;
        if let Some(v) = HomeView::ALL.get(idx) {
            let v = *v;
            self.set_home_view(v);
            match v {
                HomeView::Resource => self.enter_maps(),
                HomeView::Server => self.spawn_load_servers(),
                _ => {}
            }
            self.sync_wm_from_view();
        }
    }

    /// home_view → wm 主区窗口同步（视图切换后高亮/聚焦对应窗口）
    pub fn sync_wm_from_view(&mut self) {
        // 进入设置页时拍快照，「放弃」按钮才有基准可回退。
        // sync_wm_from_view 只在视图切换时被调用，设置页内的编辑不会触发，
        // 所以快照不会在编辑过程中被自己覆盖。
        if self.home_view == HomeView::Settings {
            self.enter_settings();
        }
        self.wm.focus(self.view_win_id());
    }

    /// HomeView → 主区窗口 id
    pub fn view_win_id(&self) -> u32 {
        match self.home_view {
            HomeView::Instance => win_id::INSTANCE,
            HomeView::Server => win_id::SERVER,
            HomeView::Resource => win_id::RESOURCE,
            HomeView::Download => win_id::DOWNLOADS,
            HomeView::Plugins => win_id::PLUGINS,
            HomeView::Settings => win_id::SETTINGS,
        }
    }

    /// 账户面板聚焦时回车：登录项选择（实例/服务器两视图共用一份）。
    /// /// 原本这段 39 行在三个 Enter 分支里逐字重复了三遍——改一处忘另外两处，
    /// 是最典型的重复代码陷阱。
    fn key_enter_accounts(&mut self) {
        let n_accounts = self.accounts.len();
        match self.selected_account {
            // [4399] 项 → 全屏登录界面并展开账号密码
            i if i == n_accounts => {
                self.login_screen = Some(LoginScreen {
                    selected: 0,
                    input_4399: true,
                });
                self.login_user.clear();
                self.login_pass.clear();
                self.login_input_step = 0;
            }
            // [LittleSkin] 项 → 登记表单（可改注册信息）或直接设备码流登录
            i if i == n_accounts + 1 => {
                if self.ls.client_id.trim().is_empty() {
                    self.ls.input = true;
                    self.ls.step = if self.ls.app_name.trim().is_empty() {
                        0
                    } else {
                        2
                    };
                } else {
                    self.start_ls_login();
                }
            }
            _ => {
                if self.cred.is_none() {
                    self.login_screen = Some(LoginScreen {
                        selected: 0,
                        input_4399: false,
                    });
                }
            }
        }
    }
    /// 命令模式（: 进入）：字符直入命令缓冲
    fn key_shell_input(&mut self, key: crossterm::event::KeyEvent) {
        use crossterm::event::{KeyCode, KeyModifiers};
        let char_idx = |buf: &str, i: usize| {
            buf.char_indices()
                .nth(i)
                .map(|(b, _)| b)
                .unwrap_or(buf.len())
        };
        match key.code {
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                let at = char_idx(&self.shell_buf, self.shell_cursor);
                self.shell_buf.insert(at, c);
                self.shell_cursor += 1;
            }
            KeyCode::Backspace => {
                if self.shell_cursor > 0 {
                    self.shell_cursor -= 1;
                    let at = char_idx(&self.shell_buf, self.shell_cursor);
                    self.shell_buf.remove(at);
                }
            }
            KeyCode::Left => self.shell_cursor = self.shell_cursor.saturating_sub(1),
            KeyCode::Right => {
                if self.shell_cursor < self.shell_buf.chars().count() {
                    self.shell_cursor += 1;
                }
            }
            KeyCode::Enter => {
                let line = std::mem::take(&mut self.shell_buf);
                self.shell_cursor = 0;
                self.shell_input = false;
                self.exec_shell(&line);
            }
            KeyCode::Esc => {
                self.shell_buf.clear();
                self.shell_cursor = 0;
                self.shell_input = false;
            }
            _ => {}
        }
    }

    /// 角色名编辑态（左下角状态栏，服务器视图）：字符直入输入框
    fn key_role_input(&mut self, key: crossterm::event::KeyEvent) {
        use crossterm::event::{KeyCode, KeyModifiers};
        match key.code {
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.role_name.push(c);
            }
            KeyCode::Backspace => {
                self.role_name.pop();
            }
            KeyCode::Enter => {
                let name = self.role_name.trim().to_string();
                self.role_name = name;
                if !self.role_name.is_empty() {
                    let _ = storage::save_role(&self.role_name);
                }
                self.role_input = false;
            }
            KeyCode::Esc => self.role_input = false,
            _ => {}
        }
    }

    /// 账户面板 4399 登录输入态（非资源页）：字符直入输入框
    /// 往当前聚焦的登录字段追加一个字符。
    /// /// 三个字段（账号/密码/验证码）原本各写一遍 if 分支，加验证码就成了第三份。
    fn login_field_push(&mut self, c: char) {
        match self.login_input_step {
            0 => self.login_user.push(c),
            1 => self.login_pass.push(c),
            _ => self.login_captcha.push(c),
        }
    }

    /// 当前聚焦的登录字段退格。
    fn login_field_pop(&mut self) {
        match self.login_input_step {
            0 => {
                self.login_user.pop();
            }
            1 => {
                self.login_pass.pop();
            }
            _ => {
                self.login_captcha.pop();
            }
        }
    }

    /// 登录表单的回车：前两步去取验证码，第三步提交。
    fn login_submit(&mut self) {
        if self.login_input_step == 2 {
            self.spawn_login_finish();
        } else {
            self.spawn_login();
        }
    }

    fn key_login_input(&mut self, key: crossterm::event::KeyEvent) {
        use crossterm::event::{KeyCode, KeyModifiers};
        match key.code {
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.login_field_push(c)
            }
            KeyCode::Backspace => self.login_field_pop(),
            // step 2（验证码）没有可切换的兄弟字段，Tab 不动它
            KeyCode::Tab if self.login_input_step < 2 => {
                self.login_input_step = 1 - self.login_input_step
            }
            KeyCode::Enter => self.login_submit(),
            KeyCode::Esc => self.login_input = false,
            _ => {}
        }
    }

    /// 全屏登录界面：↑↓/jk 选方式，Enter 确认，Esc 退出
    fn key_login_screen(&mut self, key: crossterm::event::KeyEvent) {
        use crossterm::event::{KeyCode, KeyModifiers};
        // 调用方已确保 login_screen.is_some，这里取可变借用供 match 使用
        let Some(ls) = self.login_screen.as_mut() else {
            return;
        };
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => ls.selected = ls.selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => ls.selected = (ls.selected + 1).min(1),
            KeyCode::Char('1') => ls.selected = 0,
            KeyCode::Char('2') => ls.selected = 1,
            KeyCode::Esc => {
                if ls.input_4399 {
                    ls.input_4399 = false; // 先收起输入，再按一次退出
                } else {
                    self.login_screen = None;
                }
            }
            // 快捷键只在**非输入态**生效：否则密码/用户名里含 f/e/a 的字母打不出来
            // （原先无守卫，按 f 直接关页面，密码永远打不出 f）
            KeyCode::Char('f') | KeyCode::Char('F') | KeyCode::Char('a') | KeyCode::Char('A')
                if !ls.input_4399 =>
            {
                self.login_screen = None;
            }
            KeyCode::Char('e') | KeyCode::Char('E') if !ls.input_4399 => {
                // 编辑当前选中项：4399=展开账号密码；LittleSkin=登记表单（可改注册信息）
                match ls.selected {
                    0 => {
                        self.login_user.clear();
                        self.login_pass.clear();
                        self.login_input_step = 0;
                        ls.input_4399 = true;
                    }
                    _ => {
                        if self.ls.client_id.trim().is_empty() || !self.ls.app_name.is_empty() {
                            self.ls.input = true;
                            self.ls.step = if self.ls.app_name.trim().is_empty() {
                                0
                            } else {
                                2
                            };
                        } else {
                            self.ls.input = true;
                            self.ls.step = 0;
                        }
                        self.login_screen = None;
                    }
                }
            }
            KeyCode::Tab if ls.input_4399 && self.login_input_step < 2 => {
                self.login_input_step = 1 - self.login_input_step;
            }
            KeyCode::Backspace if ls.input_4399 => self.login_field_pop(),
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) && ls.input_4399 => {
                self.login_field_push(c)
            }
            KeyCode::Enter => match (ls.selected, ls.input_4399) {
                (0, false) => {
                    // 展开 4399 内嵌输入
                    self.login_user.clear();
                    self.login_pass.clear();
                    self.login_input_step = 0;
                    ls.input_4399 = true;
                }
                (0, true) => {
                    if self.login_input_step == 2 {
                        self.login_submit();
                    } else if !self.login_user.is_empty() && !self.login_pass.is_empty() {
                        self.spawn_login();
                        self.status = format!("4399 取验证码中 {}…", self.login_user);
                    } else {
                        self.ls.status = String::new();
                        self.status = "账号和密码不能为空".into();
                    }
                }
                (1, _) => {
                    // LittleSkin：有 client_id 直接登录；没有进登记表单
                    if !self.ls.client_id.trim().is_empty() {
                        self.start_ls_login();
                    } else {
                        self.ls.input = true;
                        self.ls.step = if self.ls.app_name.trim().is_empty() {
                            0
                        } else {
                            2
                        };
                        self.login_screen = None;
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }

    /// LittleSkin OAuth 登记输入模式：账户面板聚焦时 Enter 进入，字符直入当前字段
    fn key_ls_form(&mut self, key: crossterm::event::KeyEvent) {
        use crossterm::event::{KeyCode, KeyModifiers};
        let field = |ls: &mut LsPage, ch: char| match ls.step {
            1 => ls.callback_url.push(ch),
            2 => ls.client_id.push(ch),
            _ => ls.app_name.push(ch),
        };
        match key.code {
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                field(&mut self.ls, c);
            }
            KeyCode::Backspace => match self.ls.step {
                1 => {
                    self.ls.callback_url.pop();
                }
                2 => {
                    self.ls.client_id.pop();
                }
                _ => {
                    self.ls.app_name.pop();
                }
            },
            KeyCode::Tab => self.ls.step = (self.ls.step + 1) % 3,
            KeyCode::Enter => self.submit_ls_config(),
            KeyCode::Esc => self.ls.input = false,
            _ => {}
        }
    }
    fn handle_global_key(&mut self, key: crossterm::event::KeyEvent) {
        use crossterm::event::{KeyCode, KeyModifiers};
        // 命令模式（: 进入）：字符直入命令缓冲
        if self.shell_input {
            self.key_shell_input(key);
            return;
        }
        // 角色名编辑态（左下角状态栏，服务器视图）：字符直入输入框
        if self.role_input && self.home_view == HomeView::Server {
            self.key_role_input(key);
            return;
        }
        // 账户面板 4399 登录输入态（非资源页）：字符直入输入框
        if self.login_input && self.home_view != HomeView::Resource {
            self.key_login_input(key);
            return;
        }
        // 全屏登录界面：↑↓/jk 选方式，Enter 确认，Esc 退出
        if self.login_screen.is_some() {
            self.key_login_screen(key);
            return;
        }
        // LittleSkin OAuth 登记输入模式：账户面板聚焦时 Enter 进入，字符直入当前字段
        if self.ls.input && self.home_view != HomeView::Resource {
            self.key_ls_form(key);
            return;
        }
        match (key.code, key.modifiers) {
            (KeyCode::Char(':'), _) if !self.launching => {
                self.shell_input = true;
                self.shell_buf.clear();
                self.shell_cursor = 0;
            }
            // Alt+数字 = 直达视图。
            // 不要在事件循环里用「Esc + 短超时」猜 Alt：实测 crossterm 已经把
            // ESC+数字 合并成单个带 ALT 修饰的 KeyEvent（`6` → Char('6')+ALT），
            // 那个猜测从不触发，而且会吞掉 Esc 后的第二个键。
            (KeyCode::Char(c), m) if m.contains(KeyModifiers::ALT) && c.is_ascii_digit() => {
                self.switch_view_alt(c);
            }
            (KeyCode::Char('q'), _) | (KeyCode::Char('Q'), _) => self.quit = true,
            // Esc：资源视图内由 maps 内层处理；其他视图退出面板全屏/日志全屏
            (KeyCode::Esc, _) => {
                if self.home_panel_full {
                    self.home_panel_full = false;
                } else if self.home_log_full {
                    self.home_log_full = false;
                }
            }
            (KeyCode::Char('m'), _) => self.enter_maps(),
            (KeyCode::Char('w'), _) => {
                self.set_home_view(HomeView::Instance);
                self.sync_wm_from_view();
            }
            // o = 打开当前实例的存档目录
            (KeyCode::Char('o') | KeyCode::Char('O'), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
                self.open_saves();
            }
            (KeyCode::Char('u'), _) => {
                self.status = "头像功能开发中：上传图片到 ~/.local/share/mctl/avatar.png".into()
            }
            (KeyCode::Char('l'), KeyModifiers::SHIFT) | (KeyCode::Char('L'), _) => {
                self.home_log_full = !self.home_log_full;
            }
            // Tab / Shift+Tab **不再切视图**（用户要求）：Tab 归表单自己用——
            // 设置页换字段、登录表单换账号/密码、角色名输入等。原先这里全局劫持
            // Tab，每个嵌套输入框都得各自"抢"这个键，抢输了就彻底用不了 Tab。
            // 主视图切换只走 Alt+数字（switch_view_alt）。
            // 资源大厅专用：j/k 切分类
            (KeyCode::Char('j'), _) if self.home_view == HomeView::Resource => {
                if self.category + 1 < crate::domain::ALL_CATEGORIES.len() {
                    self.category += 1;
                    self.grid_items.clear();
                    self.grid_offset = 0;
                    self.grid_selected = 0;
                    self.spawn_load_grid(false);
                }
            }
            (KeyCode::Char('k'), _) if self.home_view == HomeView::Resource => {
                if self.category > 0 {
                    self.category -= 1;
                    self.grid_items.clear();
                    self.grid_offset = 0;
                    self.grid_selected = 0;
                    self.spawn_load_grid(false);
                }
            }
            // f：全屏当前面板——所有视图语义一致（topbar 宣传的就是 [F]全屏面板）。
            // 原先资源大厅=全屏、其他视图=登录页，导致在别处按 f 想全屏却弹出登录页。
            (KeyCode::Char('f'), _) | (KeyCode::Char('F'), _) => {
                self.home_panel_full = !self.home_panel_full;
            }
            // a：登录/账户。独立键位，不占用 f——登录是低频动作，不该和全屏抢键
            (KeyCode::Char('a'), _) => {
                self.login_screen = match self.login_screen {
                    Some(_) => None,
                    None => Some(LoginScreen {
                        selected: 0,
                        input_4399: false,
                    }),
                };
            }
            // j/k: 面板焦点轮换（资源大厅除外，资源大厅用 j/k 切分类）
            (KeyCode::Char('j'), _) if self.home_view != HomeView::Resource => {
                self.move_focus_down();
            }
            (KeyCode::Char('k'), _) if self.home_view != HomeView::Resource => {
                self.move_focus_up();
            }
            // c: 清掉下载中心里已结束（完成/失败）的任务
            (KeyCode::Char('c'), _) if self.home_view == HomeView::Download => {
                let n = self.clear_finished_tasks();
                self.status = if n == 0 {
                    "没有已结束的下载".into()
                } else {
                    format!("已清理 {n} 条已结束的下载")
                };
            }
            (KeyCode::Char('r'), _) => match self.home_view {
                HomeView::Instance => self.refresh_worlds(),
                HomeView::Server => self.spawn_load_servers(),
                _ => {}
            },
            // 方向键 = 面板内内容导航（资源大厅网格/版本树由 handle_maps_key 处理，
            // 此处必须跳过——否则 mr_detail_open 时 ← 会被 move_content_left 吃掉，
            // 树永远展不开）
            (KeyCode::Up, _) if !self.maps_owns_arrows() => self.move_content_up(),
            (KeyCode::Down, _) if !self.maps_owns_arrows() => self.move_content_down(),
            (KeyCode::Left, _) if !self.maps_owns_arrows() => self.move_content_left(),
            (KeyCode::Right, _) if !self.maps_owns_arrows() => self.move_content_right(),

            // 实例视图：回车直进选中世界（其他聚焦面板回车不启动游戏）
            (KeyCode::Enter, KeyModifiers::NONE)
                if self.home_view == HomeView::Instance =>
            {
                if self.home_focus == HomeFocus::List {
                    if let Some(w) = self.worlds.get(self.selected_world) {
                        // quickPlaySingleplayer 要的是世界**目录名**，不是显示名
                        self.launch_world = Some(w.dir.clone());
                    }
                    self.spawn_launch();
                } else if self.home_focus == HomeFocus::Accounts {
                    self.key_enter_accounts();
                }
            }
            (KeyCode::Enter, KeyModifiers::NONE) if self.home_view == HomeView::Server => {
                if self.home_focus == HomeFocus::List {
                    self.spawn_server_launch();
                } else if self.home_focus == HomeFocus::Status {
                    self.role_input = true;
                } else if self.home_focus == HomeFocus::Accounts {
                    self.key_enter_accounts();
                }
            }
            _ => {}
        }
    }

    fn source_idx(&self) -> usize {
        ResourceSource::ALL
            .iter()
            .position(|s| *s == self.source)
            .unwrap_or(0)
    }

    /// 切入来源时触发的初始化
    fn enter_source(&mut self) {
        // 顶栏选中块刷色：切来源（[ / ]）时重放，是来源条动画的唯一入口
        self.bump_src_anim();
        match self.source {
            ResourceSource::Mojang => {
                if self.dl.manifest.is_empty() && !self.dl.manifest_loading {
                    self.spawn_load_manifest();
                }
                self.dl.focus = DlFocus::Official;
            }
            ResourceSource::Modrinth => {
                // 进入即展示当前分类内容（空 query = 全部热门）
                if !self.dl.searching && !self.mr_loaded {
                    self.mr_loaded = true;
                    self.spawn_search_modpacks();
                }
            }
            ResourceSource::NetEase => self.map_page = MapPage::Grid,
            ResourceSource::Github if !self.gh_loaded => {
                self.gh_loaded = true;
                self.spawn_search_github_as(true);
            }
            ResourceSource::Plugins => {
                if self.plugin_bin.is_none() || self.plugin_items.is_empty() {
                    self.spawn_plugin_versions();
                }
            }
            ResourceSource::Github | ResourceSource::Local => {}
        }
    }

    /// 官方版本（Mojang 来源）
    fn handle_mojang_key(&mut self, key: crossterm::event::KeyEvent) {
        use crossterm::event::KeyCode;
        match key.code {
            KeyCode::Up => {
                if self.dl.sel_official > 0 {
                    self.dl.sel_official -= 1;
                }
            }
            KeyCode::Down => {
                if self.dl.sel_official + 1 < self.dl.manifest.len() {
                    self.dl.sel_official += 1;
                }
            }
            KeyCode::Char('r') => self.spawn_load_manifest(),
            KeyCode::Enter => self.spawn_install_official(),
            _ => {}
        }
    }

    /// Modrinth 资源（分类侧栏 + 网格 + 版本选择详情，网易同款交互）
    /// GitHub 来源键位：`/` 搜索、`↑↓` 选择、`Enter` 安装（像模组一样）
    fn handle_github_key(&mut self, key: crossterm::event::KeyEvent) {
        use crossterm::event::KeyCode;
        // 搜索输入态：只有 Enter 才发请求（配额 10 次/分钟）
        if self.gh_search_input {
            match key.code {
                KeyCode::Char(c) => self.dl.query.push(c),
                KeyCode::Backspace => {
                    self.dl.query.pop();
                }
                KeyCode::Enter => {
                    self.gh_search_input = false;
                    self.spawn_search_github();
                }
                KeyCode::Esc => {
                    self.gh_search_input = false;
                    self.dl.query.clear();
                    self.refilter_github();
                }
                _ => {}
            }
            return;
        }
        match key.code {
            KeyCode::Char('/') => self.gh_search_input = true,
            KeyCode::Char('r') => self.spawn_search_github(),
            KeyCode::Up => self.gh_selected = self.gh_selected.saturating_sub(1),
            KeyCode::Down => {
                if self.gh_selected + 1 < self.gh_filtered.len() {
                    self.gh_selected += 1;
                }
            }
            KeyCode::PageUp => self.gh_selected = self.gh_selected.saturating_sub(10),
            KeyCode::PageDown => {
                if self.gh_selected + 10 < self.gh_filtered.len() {
                    self.gh_selected += 10;
                }
            }
            KeyCode::Home => self.gh_selected = 0,
            KeyCode::End => self.gh_selected = self.gh_filtered.len().saturating_sub(1),
            KeyCode::Enter => self.spawn_install_github(),
            _ => {}
        }
    }

    fn handle_modrinth_key(&mut self, key: crossterm::event::KeyEvent) {
        use crossterm::event::KeyCode;
        // 详情页（版本选择，树状结构）
        if self.mr_detail_open {
            let tree = crate::ui::version_tree::build_tree(&self.mr_versions, &self.mr_collapsed);
            match key.code {
                KeyCode::Esc => {
                    self.mr_detail_open = false;
                    self.mr_versions.clear();
                }
                KeyCode::Up => {
                    self.mr_row_sel = self.mr_row_sel.saturating_sub(1);
                }
                KeyCode::Down => {
                    if self.mr_row_sel + 1 < tree.len() {
                        self.mr_row_sel += 1;
                    }
                }
                // ← 折叠 / → 展开（只对分组头有意义，叶子无操作）
                KeyCode::Left => self.toggle_mr_group(&tree, false),
                KeyCode::Right => self.toggle_mr_group(&tree, true),
                KeyCode::Char('r') | KeyCode::Char('R') => {
                    self.mr_collapsed.clear();
                }
                KeyCode::Enter => self.spawn_install_modpack(),
                _ => {}
            }
            return;
        }
        match key.code {
            // 版本条件编辑态
            KeyCode::Char(c) if self.mr_version_editing && (c.is_ascii_digit() || c == '.') => {
                self.mr_version.push(c)
            }
            KeyCode::Backspace if self.mr_version_editing => {
                self.mr_version.pop();
            }
            KeyCode::Enter if self.mr_version_editing => {
                self.mr_version_editing = false;
                self.spawn_search_modpacks();
            }
            KeyCode::Esc if self.mr_version_editing => {
                self.mr_version_editing = false;
            }
            // 搜索输入态
            KeyCode::Char(c) if self.mr_searching => self.dl.query.push(c),
            KeyCode::Backspace if self.mr_searching => {
                self.dl.query.pop();
            }
            KeyCode::Enter if self.mr_searching => {
                self.mr_searching = false;
                self.spawn_search_modpacks();
            }
            KeyCode::Esc if self.mr_searching => {
                self.mr_searching = false;
                if !self.dl.query.is_empty() {
                    self.dl.query.clear();
                    self.spawn_search_modpacks();
                }
            }
            KeyCode::Up | KeyCode::Down | KeyCode::Left | KeyCode::Right | KeyCode::Tab
                if self.mr_searching || self.mr_version_editing => {}
            KeyCode::Char('/') if !self.mr_version_editing => self.mr_searching = true,
            KeyCode::Char('v') if !self.mr_searching => self.mr_version_editing = true,
            KeyCode::Tab => {
                self.focus = match self.focus {
                    Focus::Sidebar => Focus::Grid,
                    Focus::Grid => Focus::Sidebar,
                };
            }
            // 侧栏：切换分类
            KeyCode::Up | KeyCode::Left if self.focus == Focus::Sidebar => {
                if self.mr_category > 0 {
                    self.mr_category -= 1;
                    self.spawn_search_modpacks();
                }
            }
            KeyCode::Down | KeyCode::Right if self.focus == Focus::Sidebar => {
                if self.mr_category + 1 < MODRINTH_CATEGORIES.len() {
                    self.mr_category += 1;
                    self.spawn_search_modpacks();
                }
            }
            // 网格：移动 / 排序 / 刷新 / 加载更多
            KeyCode::Char('s') => {
                self.dl.sort = self.dl.sort.next();
                self.dl.sel_result = 0;
                self.spawn_search_modpacks();
            }
            KeyCode::Char('r') => self.spawn_search_modpacks(),
            KeyCode::Left => {
                if self.dl.sel_result > 0 {
                    self.dl.sel_result -= 1;
                }
            }
            KeyCode::Right => {
                let cols = self.grid_cols.max(1);
                let row_end = (self.dl.sel_result / cols + 1) * cols;
                if self.dl.sel_result + 1 < row_end
                    && self.dl.sel_result + 1 < self.dl.results.len()
                {
                    self.dl.sel_result += 1;
                }
            }
            KeyCode::Up => {
                let cols = self.grid_cols.max(1);
                if self.dl.sel_result >= cols {
                    self.dl.sel_result -= cols;
                } else if self.dl.sel_result > 0 {
                    self.dl.sel_result = 0;
                }
            }
            KeyCode::Down => {
                let cols = self.grid_cols.max(1);
                let next = self.dl.sel_result + cols;
                if next < self.dl.results.len() {
                    self.dl.sel_result = next;
                    // 提前两行预取：不等选到最后一格才发请求（与网易网格同口径）
                    if self.modrinth_needs_more() {
                        self.spawn_search_more();
                    }
                } else if self.modrinth_needs_more() {
                    self.spawn_search_more();
                }
            }
            KeyCode::Enter => self.spawn_mr_detail(),
            _ => {}
        }
    }

    fn handle_maps_key(&mut self, key: crossterm::event::KeyEvent) {
        use crossterm::event::KeyCode;
        match key.code {
            KeyCode::Char('[') => {
                let idx = self.source_idx();
                self.source = ResourceSource::ALL[idx.saturating_sub(1)];
                self.enter_source();
                return;
            }
            KeyCode::Char(']') => {
                let idx = self.source_idx();
                self.source = ResourceSource::ALL[(idx + 1).min(ResourceSource::ALL.len() - 1)];
                self.enter_source();
                return;
            }
            _ => {}
        }
        // 非网易来源的键处理
        match self.source {
            ResourceSource::Mojang => return self.handle_mojang_key(key),
            ResourceSource::Modrinth => return self.handle_modrinth_key(key),
            ResourceSource::Github => return self.handle_github_key(key),
            ResourceSource::Plugins => return self.handle_plugin_versions_key(key),
            ResourceSource::Local => return,
            ResourceSource::NetEase => {}
        }
        match self.map_page {
            // 未登录：只提示，不强求。回车直达登录专页（不必先切视图/找快捷键）
            MapPage::Grid if self.cred.is_none() => {
                if key.code == KeyCode::Enter {
                    self.login_screen = Some(LoginScreen {
                        selected: 0,
                        input_4399: false,
                    });
                }
            }
            MapPage::Grid => match key.code {
                KeyCode::Char(c) if self.grid_searching => self.grid_query.push(c),
                KeyCode::Backspace if self.grid_searching => {
                    self.grid_query.pop();
                }
                KeyCode::Enter if self.grid_searching => {
                    self.grid_searching = false;
                    self.spawn_load_grid(false);
                }
                KeyCode::Esc if self.grid_searching => {
                    self.grid_searching = false;
                    if !self.grid_query.is_empty() {
                        self.grid_query.clear();
                        self.spawn_load_grid(false);
                    }
                }
                KeyCode::Up | KeyCode::Down | KeyCode::Left | KeyCode::Right | KeyCode::Tab
                    if self.grid_searching =>
                {
                    // 搜索输入态屏蔽方向键，防止误触切换分类
                }
                KeyCode::Char('/') => self.grid_searching = true,
                KeyCode::Char('s') => {
                    self.grid_sort = self.grid_sort.next();
                    self.spawn_load_grid(false);
                }
                KeyCode::Tab => {
                    self.focus = match self.focus {
                        Focus::Sidebar => Focus::Grid,
                        Focus::Grid => Focus::Sidebar,
                    };
                }
                KeyCode::Char('r') => self.spawn_load_grid(false),
                KeyCode::Left => {
                    // 上下左右只在右侧方格内移动，不切分类（分类切换走 j/k）
                    if self.focus == Focus::Grid {
                        let row_head = self.grid_selected / self.grid_cols * self.grid_cols;
                        if self.grid_selected > row_head {
                            self.grid_selected -= 1;
                        }
                    }
                }
                KeyCode::Right => {
                    // 只在右侧方格内移动
                    if self.focus == Focus::Grid {
                        let row_tail = (self.grid_selected / self.grid_cols + 1) * self.grid_cols;
                        if self.grid_selected + 1 < row_tail
                            && self.grid_selected + 1 < self.grid_items.len()
                        {
                            self.grid_selected += 1;
                        }
                    }
                }
                KeyCode::Up => {
                    // 只在右侧方格内移动
                    if self.focus == Focus::Grid {
                        if self.grid_selected >= self.grid_cols {
                            self.grid_selected -= self.grid_cols;
                        } else if self.grid_selected > 0 {
                            self.grid_selected %= self.grid_cols;
                        }
                    }
                }
                KeyCode::Down => {
                    // 只在右侧方格内移动
                    if self.focus == Focus::Grid {
                        let next = self.grid_selected + self.grid_cols;
                        if next < self.grid_items.len() {
                            self.grid_selected = next;
                            if self.grid_selected + 2 * self.grid_cols >= self.grid_items.len()
                                && !self.grid_end
                                && !self.grid_loading
                            {
                                self.spawn_load_more();
                            }
                        } else if !self.grid_end && !self.grid_loading {
                            self.spawn_load_more();
                        }
                    }
                }
                KeyCode::Enter => {
                    if self.focus == Focus::Sidebar {
                        self.focus = Focus::Grid;
                        return;
                    }
                    if let Some(item) = self.grid_items.get(self.grid_selected) {
                        let id = item.entity_id.clone();
                        self.detail = None;
                        self.detail_versions.clear();
                        self.detail_cover = None;
                        self.detail_selected = 0;
                        self.map_page = MapPage::Detail;
                        self.spawn_load_detail(id);
                    }
                }
                _ => {}
            },
            MapPage::Detail => match key.code {
                KeyCode::Esc | KeyCode::Char('q') if self.detail_focus != DetailFocus::Download => {
                    self.map_page = MapPage::Grid
                }
                KeyCode::Tab => {
                    self.detail_focus = match self.detail_focus {
                        DetailFocus::Info => DetailFocus::Versions,
                        DetailFocus::Versions => DetailFocus::Download,
                        DetailFocus::Download => DetailFocus::Info,
                    };
                    if self.detail_focus == DetailFocus::Download {
                        self.download_dir_str = self.download_dir.display().to_string();
                    }
                }
                KeyCode::Esc if self.detail_focus == DetailFocus::Download => {
                    self.detail_focus = DetailFocus::Versions;
                    self.download_dir_str = self.download_dir.display().to_string();
                }
                // 下载位置约束框：编辑路径
                KeyCode::Char(c)
                    if self.detail_focus == DetailFocus::Download && !c.is_control() =>
                {
                    self.download_dir_str.push(c);
                }
                KeyCode::Backspace if self.detail_focus == DetailFocus::Download => {
                    self.download_dir_str.pop();
                }
                KeyCode::Enter if self.detail_focus == DetailFocus::Download => {
                    let p = self.download_dir_str.trim();
                    if !p.is_empty() {
                        self.download_dir = PathBuf::from(p);
                    }
                    self.download_dir_str = self.download_dir.display().to_string();
                }
                KeyCode::Up if self.detail_focus == DetailFocus::Info => {
                    self.detail_scroll = self.detail_scroll.saturating_sub(1);
                }
                KeyCode::Down if self.detail_focus == DetailFocus::Info => {
                    self.detail_scroll = self.detail_scroll.saturating_add(1);
                }
                KeyCode::Up if self.detail_focus == DetailFocus::Versions => {
                    if self.detail_selected > 0 {
                        self.detail_selected -= 1;
                    }
                }
                KeyCode::Down if self.detail_focus == DetailFocus::Versions => {
                    if self.detail_selected + 1 < self.detail_versions.len() {
                        self.detail_selected += 1;
                    }
                }
                KeyCode::Enter if self.detail_focus == DetailFocus::Versions => {
                    if let Some(v) = self.detail_versions.get(self.detail_selected) {
                        let url = v.res_url.clone();
                        let name = if v.res_name.is_empty() {
                            format!("{}.7z", v.mc_version_name)
                        } else {
                            v.res_name.clone()
                        };
                        let dest = self.download_dir.join(&name);
                        // 记「最近下载」。用 item 的 id 而不是文件名——文件名会变
                        // （重名/改名），id 才是稳定标识。
                        let (rid, rname, rver) = self
                            .detail
                            .as_ref()
                            .map(|d| (d.entity_id.clone(), d.name.clone(), d.item_version.clone()))
                            .unwrap_or_else(|| (name.clone(), name.clone(), String::new()));
                        self.record_recent(RecentKind::Download, &rid, &rname, &rver);
                        // 不跳页：下载进下载中心跑，页面留在版本列表，
                        // 可以接着选下一个版本/地图一起下（并发）
                        self.spawn_download(url, dest.to_string_lossy().to_string());
                    }
                }
                _ => {}
            },
            MapPage::Download => match key.code {
                KeyCode::Esc => self.map_page = MapPage::Detail,
                KeyCode::Char('c') => {
                    let n = self.clear_finished_tasks();
                    self.status = if n == 0 {
                        "没有已结束的下载".into()
                    } else {
                        format!("已清理 {n} 条已结束的下载")
                    };
                }
                _ => {}
            },
        }
    }
}

fn avatar_path() -> PathBuf {
    std::env::var_os("HOME")
        .map(|h| PathBuf::from(h).join(".local/share/mctl/avatar.png"))
        .unwrap_or_else(|| PathBuf::from(".local/share/mctl/avatar.png"))
}

fn load_avatar_file(picker: &Arc<ratatui_image::picker::Picker>) -> Option<StatefulProtocol> {
    let bytes = std::fs::read(avatar_path()).ok()?;
    let img = image::load_from_memory(&bytes).ok()?;
    Some(picker.new_resize_protocol(img))
}

/// 拉取皮肤 → 合成双层头部（base + 帽子层）→ 放大 128 → 存 avatar.png
async fn regen_avatar(uuid: &str) -> bool {
    let http = reqwest::Client::new();
    let url = format!(
        "https://littleskin.cn/api/yggdrasil/sessionserver/session/minecraft/profile/{uuid}"
    );
    let Ok(resp) = http.get(&url).send().await else {
        return false;
    };
    let Ok(v) = resp.json::<serde_json::Value>().await else {
        return false;
    };
    let tex = v["properties"].as_array().and_then(|ps| {
        ps.iter()
            .find(|p| p["name"] == "textures")
            .and_then(|p| p["value"].as_str())
            .and_then(|b64| {
                use base64::Engine;
                base64::engine::general_purpose::STANDARD.decode(b64).ok()
            })
    });
    let Some(tex) = tex else { return false };
    let Ok(tv) = serde_json::from_slice::<serde_json::Value>(&tex) else {
        return false;
    };
    let Some(skin_url) = tv["textures"]["SKIN"]["url"].as_str() else {
        return false;
    };
    let Ok(png) = http.get(skin_url).send().await else {
        return false;
    };
    let Ok(bytes) = png.bytes().await else {
        return false;
    };
    tokio::task::spawn_blocking(move || {
        let img = image::load_from_memory(&bytes).ok()?;
        // base 头 (8,8) + 帽子层 (40,8) 非透明像素叠加 → 有头发
        let mut head = image::imageops::crop_imm(&img, 8, 8, 8, 8).to_image();
        let hat = image::imageops::crop_imm(&img, 40, 8, 8, 8).to_image();
        for (x, y, p) in hat.enumerate_pixels() {
            if p.0[3] > 0 {
                head.put_pixel(x, y, *p);
            }
        }
        let big = image::imageops::resize(&head, 128, 128, image::imageops::FilterType::Nearest);
        let dir = avatar_path().parent().map(|p| p.to_path_buf())?;
        std::fs::create_dir_all(&dir).ok()?;
        image::DynamicImage::ImageRgba8(big)
            .save_with_format(avatar_path(), image::ImageFormat::Png)
            .ok()
    })
    .await
    .unwrap_or(None)
    .is_some()
}

/// 检测可用的截图命令（wayland 优先 grim，X11 用 import），返回 (命令, 前置参数)。
/// grim 不支持 --version，用真实临时截图探测。
fn detect_shoot_cmd() -> Option<(String, Vec<String>)> {
    let tmp = std::env::temp_dir().join(format!("mctl-shot-probe-{}.png", std::process::id()));
    if std::process::Command::new("grim")
        .arg(&tmp)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
    {
        let _ = std::fs::remove_file(&tmp);
        return Some(("grim".to_string(), vec![]));
    }
    for (cmd, probe, args) in [
        ("scrot", "--version", vec![]),
        ("import", "-version", vec!["-window".into(), "root".into()]),
    ] {
        if std::process::Command::new(cmd)
            .arg(probe)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
        {
            return Some((cmd.to_string(), args));
        }
    }
    None
}

/// 清理自动截图：每个世界目录只保留最近 10 张（mctl- 前缀；用户 F2 截图不在本目录，不受影响）
fn trim_shots(dir: &Path) {
    const KEEP: usize = 10;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut shots: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("mctl-") && n.ends_with(".png"))
        })
        .collect();
    if shots.len() <= KEEP {
        return;
    }
    shots.sort_by_key(|p| {
        std::fs::metadata(p)
            .and_then(|m| m.modified())
            .unwrap_or(std::time::UNIX_EPOCH)
    });
    for old in shots.iter().take(shots.len() - KEEP) {
        let _ = std::fs::remove_file(old);
    }
}

/// 扫描存档目录下的世界：目录含 level.dat 才算世界，按修改时间最新在前。
/// 支持符号链接目录（旧世界迁移链接）；基岩世界名优先读 levelname.txt。
pub(crate) fn scan_worlds_dir(saves: &Path) -> Vec<WorldInfo> {
    let mut entries: Vec<(std::time::SystemTime, String, String, String)> = Vec::new();
    let Ok(rd) = std::fs::read_dir(saves) else {
        return Vec::new();
    };
    for e in rd.flatten() {
        let p = e.path();
        if !p.is_dir() || !p.join("level.dat").is_file() {
            continue;
        }
        let dir = e.file_name().to_string_lossy().into_owned();
        let name = std::fs::read_to_string(p.join("levelname.txt"))
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| dir.clone());
        // 只求「大概多大」：递归求和目录里所有文件的长度。世界目录都是本地小目录树，
        // 这点 stat 开销可忽略；失败就写 "—" 而不是 0（0 会被当成"空世界"误导用户）。
        let size = match dir_tree_size(&p) {
            Some(bytes) => human_size(bytes),
            None => "—".to_string(),
        };
        let mtime = e.metadata().and_then(|m| m.modified()).ok();
        if let Some(mtime) = mtime {
            entries.push((mtime, dir, name, size));
        }
    }
    entries.sort_by(|a, b| b.0.cmp(&a.0));
    entries
        .into_iter()
        .map(|(t, dir, name, size)| WorldInfo {
            name,
            dir,
            last_played: format_mtime(t),
            size,
        })
        .collect()
}

/// 递归求目录里文件总大小（字节）。任一层读失败返回 None。
fn dir_tree_size(dir: &Path) -> Option<u64> {
    let rd = std::fs::read_dir(dir).ok()?;
    let mut total = 0u64;
    for e in rd.flatten() {
        let meta = e.metadata().ok()?;
        if meta.is_dir() {
            total = total.saturating_add(dir_tree_size(&e.path())?);
        } else {
            total = total.saturating_add(meta.len());
        }
    }
    Some(total)
}

/// 人类可读大小（保留 1 位小数）
pub(crate) fn human_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    let b = bytes as f64;
    if b >= KB * KB * KB {
        format!("{:.1} GB", b / (KB * KB * KB))
    } else if b >= KB * KB {
        format!("{:.1} MB", b / (KB * KB))
    } else if b >= KB {
        format!("{:.0} KB", b / KB)
    } else {
        format!("{bytes} B")
    }
}

/// 人类可读速率（B/s）
pub(crate) fn human_rate(bps: u64) -> String {
    if bps == 0 {
        "—".into()
    } else {
        format!("{}/s", human_size(bps))
    }
}

fn format_mtime(t: std::time::SystemTime) -> String {
    let Ok(d) = t.duration_since(std::time::UNIX_EPOCH) else {
        return "未知".into();
    };
    let secs = d.as_secs() as i64;
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    let (h, mi) = (rem / 3600, (rem % 3600) / 60);

    // 公历日期推算（从 1970-01-01 起）
    let mut y = 1970i64;
    let mut d = days;
    loop {
        let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
        let year_days = if leap { 366 } else { 365 };
        if d < year_days {
            break;
        }
        d -= year_days;
        y += 1;
    }
    let months = [
        31,
        if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 {
            29
        } else {
            28
        },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut mo = 1u32;
    let mut dd = d + 1;
    for m in months {
        if dd <= m {
            break;
        }
        dd -= m;
        mo += 1;
    }
    format!("{mo:02}-{dd:02} {h:02}:{mi:02}")
}

/// 根据终端环境选择图片渲染协议：先探测终端是否支持真图片，支持则用原生协议，否则半块降级。
/// 纯环境变量检测，不做 IO 查询（IO 查询在无响应终端会残留阻塞线程）。
fn pick_picker() -> ratatui_image::picker::Picker {
    use ratatui_image::picker::{Picker, ProtocolType};
    let mut picker = Picker::halfblocks();
    if std::env::var("TMUX").is_ok() {
        return picker;
    }
    let term = std::env::var("TERM").unwrap_or_default();
    let proto = if std::env::var("KITTY_WINDOW_ID")
        .map(|v| !v.is_empty())
        .unwrap_or(false)
        || term.contains("kitty")
        || term.contains("ghostty")
    {
        ProtocolType::Kitty
    } else if term.contains("sixel") || term.contains("foot") || term.contains("contour") {
        ProtocolType::Sixel
    } else if std::env::var("ITERM_SESSION_ID")
        .map(|v| !v.is_empty())
        .unwrap_or(false)
    {
        ProtocolType::Iterm2
    } else {
        ProtocolType::Halfblocks
    };
    picker.set_protocol_type(proto);
    picker
}
/// 启动管线：找 Java → 补库 → 补资产 → natives → 启动
/// auth = Some((角色uuid, access_token)) 时走 LittleSkin 外置登录（authlib-injector）
async fn launch_instance(
    mc_dir: &Path,
    inst: &Instance,
    username: &str,
    auth: Option<(String, String)>,
    tx: mpsc::Sender<TaskMsg>,
) -> Result<u32> {
    launch_instance_inner(
        mc_dir, &inst.dir, inst, username, None, None, auth, None, tx,
    )
    .await
}

/// 启动管线 + quickPlay 直进世界（1.20.1+ 支持 --quickPlaySingleplayer）
async fn launch_instance_world(
    mc_dir: &Path,
    inst: &Instance,
    username: &str,
    world: String,
    auth: Option<(String, String)>,
    tx: mpsc::Sender<TaskMsg>,
) -> Result<u32> {
    launch_instance_inner(
        mc_dir,
        &inst.dir,
        inst,
        username,
        Some(world),
        None,
        auth,
        None,
        tx,
    )
    .await
}

/// 启动管线核心。quick_play_world = Some(世界目录名) 时直进该世界；
/// server = Some((local_port, host)) 时追加联机直连参数；
/// auth = Some((uuid, token)) 时注入 authlib-injector 外置登录。
async fn launch_instance_inner(
    mc_dir: &Path,
    game_dir: &Path,
    inst: &Instance,
    username: &str,
    quick_play_world: Option<String>,
    server: Option<(u16, &str)>,
    auth: Option<(String, String)>,
    task_id: Option<u64>,
    tx: mpsc::Sender<TaskMsg>,
) -> Result<u32> {
    macro_rules! stage {
        ($($arg:tt)*) => {{
            let tx = tx.clone();
            let s = format!($($arg)*);
            tokio::spawn(async move { let _ = tx.send(TaskMsg::LaunchStage(s)).await; });
        }};
    }
    // 走服务器的启动（Java 国际服）：缺失的依赖库/资源要能在下载中心看到
    let file_cb = |f: launcher::FileProgress| {
        if let Some(id) = task_id {
            send_file_progress(&tx, id, f);
        }
    };
    let required = if inst.java_major == 0 {
        8
    } else {
        inst.java_major
    };
    stage!("查找 Java {required}+…");
    let java = launcher::java::find_java(required)
        .ok_or_else(|| anyhow::anyhow!("{}", launcher::java::missing_java_message(required)))?;
    let profile = launcher::load_profile(inst)?;
    stage!("补全依赖库…");
    launcher::ensure_libraries(
        mc_dir,
        &profile,
        |done, total| {
            if done % 8 == 0 || done == total {
                report_stage(&tx, StageSink::Launch, task_id, "补全依赖库", done, total);
            }
        },
        &file_cb,
    )
    .await?;
    stage!("补全资源文件…");
    launcher::ensure_assets(
        mc_dir,
        &profile,
        |done, total| {
            if done % 8 == 0 || done == total {
                report_stage(&tx, StageSink::Launch, task_id, "补全资源文件", done, total);
            }
        },
        &file_cb,
    )
    .await?;
    stage!("解压原生库…");
    launcher::extract_natives(mc_dir, &profile, inst)?;
    // 外置登录：确保 authlib-injector 已就位（首次启动下载）
    let jar_opt = if auth.is_some() {
        stage!("准备 authlib-injector…");
        Some(crate::api::littleskin::ensure_authlib_injector(mc_dir).await?)
    } else {
        None
    };
    let authlib = match (&auth, jar_opt.as_ref()) {
        (Some((uuid, token)), Some(jar)) => Some((
            jar.as_path(),
            crate::api::littleskin::YGGDRASIL_ROOT,
            uuid.as_str(),
            token.as_str(),
        )),
        _ => None,
    };
    stage!("启动游戏…");
    use std::process::Stdio;
    use tokio::io::{AsyncBufReadExt, BufReader};
    let cmd = launcher::build_launch_command(
        mc_dir,
        game_dir,
        inst,
        &profile,
        &java,
        username,
        2048,
        quick_play_world.as_deref(),
        authlib,
    )?;
    let mut tcmd = tokio::process::Command::new(cmd.get_program());
    tcmd.args(cmd.get_args());
    if let Some((local_port, host)) = server {
        tcmd.arg("--server")
            .arg(host)
            .arg("--port")
            .arg(local_port.to_string());
    }
    tcmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = tcmd.spawn()?;
    let pid = child.id();

    // 转发日志到 UI
    let tx_log = tx.clone();
    tokio::spawn(async move {
        if let Some(out) = child.stdout.take() {
            let tx = tx_log.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(out).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let _ = tx.send(TaskMsg::GameLog(line)).await;
                }
            });
        }
        if let Some(err) = child.stderr.take() {
            let tx = tx_log.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(err).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let _ = tx.send(TaskMsg::GameLog(line)).await;
                }
            });
        }
        let status = child.wait().await;
        let code = match status {
            Ok(s) => s.code(),
            Err(_) => None,
        };
        let _ = tx.send(TaskMsg::GameExit(code)).await;
    });

    pid.ok_or_else(|| anyhow::anyhow!("无法获取游戏进程 PID"))
}

// ---- tui-wm 外壳：渲染窗口内容 ----
impl tui_wm_core::WmHost for App {
    fn render_window(&mut self, frame: &mut ratatui::Frame, id: u32, area: ratatui::layout::Rect) {
        match id {
            win_id::INSTANCE | win_id::SERVER => {
                crate::ui::home::draw_view(frame, self, self.home_view, area);
            }
            win_id::RESOURCE => crate::ui::maps::draw(frame, self, area),
            win_id::DOWNLOADS => crate::ui::maps::draw_download_center(frame, self, area),
            win_id::PLUGINS => crate::ui::extra::plugins_view(frame, self, area),
            win_id::SETTINGS => crate::ui::extra::settings_view(frame, self, area),
            win_id::ACCOUNTS => {
                let focused = self.wm.active == win_id::ACCOUNTS;
                crate::ui::home::account_panel(frame, self, area, focused);
            }
            win_id::VIEWS => crate::ui::home::view_sidebar(frame, self, area),
            win_id::STATUS => {
                let focused = self.wm.active == win_id::STATUS;
                crate::ui::home::status_panel(frame, self, area, focused);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{storage, SettingField};

    /// 隐私：自动截图必须**默认关**。
    /// /// 自动截图 = 游戏运行时持续把画面存到本地（开场 1 分钟 3 张 + 之后每 30 分钟
    /// 一张）。这不是「顺手存个封面」，而是持续记录游玩内容，所以没配过 = 不开，
    /// 不能反过来给个 true 让用户自己去关。
    #[test]
    fn auto_shot_defaults_to_off() {
        let s = storage::Settings::default();
        assert_eq!(
            s.auto_shot, None,
            "未配置时必须是 None（而不是 Some(true)）"
        );
        assert!(!s.auto_shot.unwrap_or(false), "未配置时解析出来必须是关");
    }

    /// 老配置（没有 auto_shot 键）读出来必须是关，不能是开
    #[test]
    fn legacy_settings_without_auto_shot_loads_as_off() {
        let legacy = r#"{"game_dir":"/tmp/x","grid_cols":2,"grid_rows":3}"#;
        let s: storage::Settings = serde_json::from_str(legacy).unwrap();
        assert_eq!(s.auto_shot, None);
        assert!(!s.auto_shot.unwrap_or(false));
    }

    /// 关着的时候 start_shots 必须直接返回，不起循环
    #[tokio::test]
    async fn start_shots_is_noop_when_disabled() {
        let mut app = app();
        app.auto_shot = false;
        app.shot_task = None;
        app.start_shots();
        assert!(app.shot_task.is_none(), "自动截图关着时不应启动截图任务");
    }

    use super::*;

    /// 隔离配置目录的 App（不让测试碰真实的 ~/.config/mctl）
    fn app() -> App {
        App::new_from(crate::storage::tmp_config_dir("app")).unwrap()
    }

    fn js(name: &str, host: &str, builtin: bool) -> crate::api::mcserver::JavaServer {
        crate::api::mcserver::JavaServer {
            name: name.into(),
            host: host.into(),
            port: 25565,
            builtin,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn server_tabs_split_lists_and_remember_selection() {
        let mut app = app();
        app.servers = vec![js("推荐", "rec.example.net", true), js("自定义", "custom.example.net", false)];
        app.user_servers = vec![js("用户", "user.example.net", false)];
        assert_eq!(app.server_list_for(ServerTab::All).len(), 3);
        assert_eq!(app.server_list_for(ServerTab::Recommended).len(), 1);
        assert_eq!(app.server_list_for(ServerTab::User).len(), 1);

        app.selected_server = 1; // All 里选中「自定义」
        app.set_server_tab(ServerTab::User);
        assert_eq!(app.server_tab, ServerTab::User);
        assert_eq!(app.selected_server, 0);
        app.set_server_tab(ServerTab::All);
        assert_eq!(app.selected_server, 1, "切回 All 要恢复原选中项");
    }

    #[tokio::test]
    async fn ctrl_tabs_only_work_on_server_page() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let ctrl = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);
        let mut app = app();
        app.home_view = HomeView::Instance;
        app.handle_key(ctrl('1'));
        assert_eq!(app.server_tab, ServerTab::All, "非服务器页 Ctrl+1 不生效");

        app.home_view = HomeView::Server;
        app.handle_key(ctrl('1'));
        assert_eq!(app.server_tab, ServerTab::User);
        app.handle_key(ctrl('2'));
        assert_eq!(app.server_tab, ServerTab::Recommended);
        app.handle_key(ctrl('3'));
        assert_eq!(app.server_tab, ServerTab::All);
    }

    #[tokio::test]
    async fn add_and_remove_server_syncs_servers_dat() {
        let dir = crate::storage::tmp_config_dir("app-server-add");
        let mut app = App::new_from(dir.clone()).unwrap();
        let inst_dir = dir.join("inst");
        std::fs::create_dir_all(&inst_dir).unwrap();
        app.instances = vec![Instance {
            bedrock: false,
            id: "1.21.1".into(),
            version_type: "release".into(),
            main_class: "net.minecraft.client.main.Main".into(),
            assets_index: "17".into(),
            java_major: 21,
            dir: inst_dir.clone(),
        }];
        app.selected_instance = 0;
        app.open_server_add();
        {
            let form = app.server_add.as_mut().unwrap();
            form.addr = "127.0.0.1:1".into();
            form.name = "本地测试服".into();
        }
        app.submit_server_add();
        assert_eq!(app.server_tab, ServerTab::User, "添加后切到用户标签");
        assert!(app.server_add.is_none());
        let path = inst_dir.join("servers.dat");
        let list = crate::servers_dat::load(&path).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "本地测试服");
        assert_eq!(app.user_servers.len(), 1);

        app.selected_server = 0;
        app.remove_selected_user_server();
        assert!(crate::servers_dat::load(&path).unwrap().is_empty());
        assert!(app.user_servers.is_empty());
    }

    /// Modrinth 网格：距尾部不足两行就预取下一页，不等选到最后一格
    /// （和网易网格的 `selected + 2 * cols` 同一口径）。
    #[tokio::test]
    async fn modrinth_prefetches_before_reaching_the_end() {
        let mut app = app();
        app.grid_cols = 5;
        app.dl.results = (0..20)
            .map(|i| crate::launcher::modpack::ModpackHit {
                project_id: i.to_string(),
                slug: String::new(),
                title: String::new(),
                description: String::new(),
                downloads: 0,
                follows: 0,
                date_modified: String::new(),
                icon_url: String::new(),
                game_versions: Vec::new(),
            })
            .collect();
        app.dl.searching = false;
        app.dl.more_end = false;

        app.dl.sel_result = 9; // 9 + 2*5 = 19 < 20：还没到预取线
        assert!(!app.modrinth_needs_more());
        app.dl.sel_result = 10; // 10 + 10 >= 20：提前取下一页
        assert!(app.modrinth_needs_more());

        app.dl.searching = true;
        assert!(!app.modrinth_needs_more(), "正在加载时不重复发");
        app.dl.searching = false;
        app.dl.more_end = true;
        assert!(!app.modrinth_needs_more(), "没有更多时不发");
    }

    /// 不足一页就到底：首屏和追加都一样，别再多发一次空请求。
    #[tokio::test]
    async fn modrinth_short_page_marks_the_end() {
        let mut app = app();
        let hit = |i: usize| crate::launcher::modpack::ModpackHit {
            project_id: i.to_string(),
            slug: String::new(),
            title: String::new(),
            description: String::new(),
            downloads: 0,
            follows: 0,
            date_modified: String::new(),
            icon_url: String::new(),
            game_versions: Vec::new(),
        };
        app.on_modpacks_loaded(Ok(vec![hit(0)]), 0);
        assert!(app.dl.more_end, "首屏不足一页就是最后一页");

        app.on_modpacks_loaded(Ok((0..MODRINTH_PAGE).map(hit).collect()), 0);
        assert!(!app.dl.more_end, "整页要继续翻");

        app.on_modpacks_loaded(Ok((0..7).map(hit).collect()), MODRINTH_PAGE);
        assert_eq!(app.dl.results.len(), MODRINTH_PAGE + 7);
        assert!(app.dl.more_end, "追加页不足一页也到底");
    }

    /// 树状列表：← 折叠分组后，其叶子必须从选中集合里消失，
    /// 且行数真的变少（不是只改了标记）。
    #[tokio::test]
    async fn mr_tree_collapse_hides_children() {
        use crate::launcher::modpack::MrVersion;
        let mut app = app();
        let mk = |num: &str, mc: &str| MrVersion {
            id: num.into(),
            version_number: num.into(),
            game_versions: vec![mc.into()],
            loaders: vec!["fabric".into()],
            files: vec![],
            dependencies: vec![],
        };
        app.mr_versions = vec![
            mk("1.21.1", "1.21.1"),
            mk("1.21.1-beta.1", "1.21.1"),
            mk("1.20.4", "1.20.4"),
        ];
        // 详情页只在「资源页 + Modrinth 来源」内存在：
        // home_view 和 wm 主区窗口都要切过去（否则 wm.handle_key 抢先消费方向键），
        // source 也要是 Modrinth（handle_maps_key 按 source 分发按键）
        app.wm.focus(win_id::RESOURCE);
        app.home_view = HomeView::Resource;
        app.source = ResourceSource::Modrinth;
        app.mr_detail_open = true;
        app.mr_row_sel = 0;
        let before = crate::ui::version_tree::build_tree(&app.mr_versions, &app.mr_collapsed).len();

        use crossterm::event::{KeyCode, KeyEvent};
        // 光标在第 0 行（1.21.1 分组头）→ 左键折叠
        app.handle_key(KeyEvent::new(
            KeyCode::Left,
            crossterm::event::KeyModifiers::NONE,
        ));
        let after = crate::ui::version_tree::build_tree(&app.mr_versions, &app.mr_collapsed).len();
        assert!(after < before, "折叠后行数应变少: {before} → {after}");

        // 再按一次 → 展开回来
        app.handle_key(KeyEvent::new(
            KeyCode::Left,
            crossterm::event::KeyModifiers::NONE,
        ));
        let again = crate::ui::version_tree::build_tree(&app.mr_versions, &app.mr_collapsed).len();
        assert_eq!(again, before, "重复折叠/展开应回到原状");
    }

    /// 选中分组头时按 Enter 不能安装（没有可安装的版本）
    #[tokio::test]
    async fn mr_tree_group_row_is_not_installable() {
        use crate::launcher::modpack::MrVersion;
        let mut app = app();
        app.mr_versions = vec![MrVersion {
            id: "a".into(),
            version_number: "1.0".into(),
            game_versions: vec!["1.21.1".into()],
            loaders: vec!["fabric".into()],
            files: vec![],
            dependencies: vec![],
        }];
        app.mr_detail_open = true;
        app.mr_row_sel = 0; // 分组头
        assert!(
            app.mr_selected_version().is_none(),
            "分组头不该解析出可安装版本"
        );
        app.mr_row_sel = 1; // 叶子
        assert!(app.mr_selected_version().is_some(), "叶子应能解析出版本");
    }

    /// 端到端：缺 Java 时的提示要含版本号 + 本机命令 + 指向日志查看
    #[tokio::test]
    async fn launch_failure_hint_points_to_log() {
        let mut app = app();
        app.on_launch_done(Err(anyhow::anyhow!(
            "未找到 Java 21+，启动 Minecraft 需要它\n  安装：sudo pacman -S jdk21-openjdk"
        )));
        // 状态栏只留首行，不含换行
        assert!(
            !app.status.contains('\n'),
            "状态栏不该有换行: {:?}",
            app.status
        );
        assert!(app.status.contains("启动失败"), "{}", app.status);
        assert!(
            !app.status.contains("安装："),
            "命令不该挤进状态栏: {:?}",
            app.status
        );
        // 完整内容在日志里
        assert!(
            app.game_log.iter().any(|l| l.contains("jdk21-openjdk")),
            "日志里应有完整命令: {:?}",
            app.game_log
        );
    }

    #[tokio::test]
    async fn github_local_filter_costs_no_quota() {
        let mut app = app();
        app.dl.query = "bedrock".into();
        app.gh_results = vec![
            crate::api::github::Repo {
                full_name: "a/mctl-bedrock".into(),
                description: Some("基岩版启动".into()),
                stargazers_count: 12,
                updated_at: Some("2026-09-01T10:00:00Z".into()),
                topics: vec!["mctl-plugin".into()],
                html_url: "https://github.com/a/mctl-bedrock".into(),
            },
            crate::api::github::Repo {
                full_name: "b/unrelated".into(),
                description: Some("别的项目".into()),
                stargazers_count: 1,
                updated_at: None,
                topics: vec![],
                html_url: String::new(),
            },
        ];
        app.refilter_github();
        assert_eq!(app.gh_filtered, vec![0], "只应留下名字/描述命中的");
        assert!(app.gh_results[0].is_plugin(), "topic 应被识别为插件徽章");

        // 选中项越界要夹住（结果集变小后）
        app.gh_selected = 5;
        app.refilter_github();
        assert_eq!(app.gh_selected, 0);
    }

    #[tokio::test]
    async fn github_keys_navigate_and_enter_installs() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut app = app();
        app.home_view = HomeView::Resource;
        app.source = ResourceSource::Github;
        app.gh_loaded = true;
        let mk = |full: &str, desc: &str| crate::api::github::Repo {
            full_name: full.into(),
            description: Some(desc.into()),
            stargazers_count: 5,
            updated_at: None,
            topics: vec![],
            html_url: format!("https://github.com/{full}"),
        };
        app.gh_results = vec![mk("u/aaa", "x"), mk("u/bbb", "y"), mk("u/ccc", "z")];
        app.refilter_github();
        assert_eq!(app.gh_filtered.len(), 3);

        let k = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
        // ↓ 移动
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.gh_selected, 1);
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.gh_selected, 2, "末尾不应越界");
        app.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(app.gh_selected, 1);
        // Home/End
        app.handle_key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
        assert_eq!(app.gh_selected, 2);
        app.handle_key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
        assert_eq!(app.gh_selected, 0);

        // / 进输入态；打字不发请求（配额太贵），Enter 才发
        app.handle_key(k('/'));
        assert!(app.gh_search_input);
        app.handle_key(k('b'));
        app.handle_key(k('e'));
        assert_eq!(app.dl.query, "be", "输入应进查询框");
        assert!(
            !app.gh_searching,
            "打字时不得发请求（未认证配额 10 次/分钟）"
        );
        // Esc 取消输入并清空
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(!app.gh_search_input);
        assert!(app.dl.query.is_empty());
    }

    #[tokio::test]
    async fn resource_view_without_login_hints_instead_of_trapping() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut app = app();
        app.cred = None;
        app.home_view = HomeView::Resource;

        // 不再是内嵌登录表单：视图切换必须仍然可用（旧的 MapPage::Login 守卫会吞掉按键）。
        // 切视图走 Alt+数字——Tab 已按用户要求归还给表单，不再承担切视图。
        // 快捷位从 slot 推导，不写死数字——插入新视图后写死的数字会静默错位
        let alt = |v: HomeView| {
            KeyEvent::new(
                KeyCode::Char(char::from(b'0' + v.slot())),
                KeyModifiers::ALT,
            )
        };
        app.handle_key(alt(HomeView::Plugins));
        assert_eq!(
            app.home_view,
            HomeView::Plugins,
            "未登录时 Alt 应能切主视图"
        );
        app.handle_key(alt(HomeView::Resource));
        assert_eq!(app.home_view, HomeView::Resource, "Alt 应能回到资源视图");

        // 回车直达登录专页（不强求，不自动弹）
        assert!(app.login_screen.is_none(), "不自动弹登录页");
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.login_screen.is_some(), "回车直达登录专页");
    }

    /// Alt+数字 直达视图。
    /// /// 回归：原先在事件循环里用「Esc + poll(10ms)」猜 Alt，但 crossterm 早就把
    /// ESC+数字 合并成单个带 ALT 修饰的 KeyEvent（实测 `\x1b6` → Char('6')+ALT），
    /// 那个猜测从不触发，Alt+数字 一直是死的；而且它还会吞掉 Esc 后的第二个键。
    #[tokio::test]
    async fn alt_digit_switches_view() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut app = app();
        // 带 ALT 修饰的字符键（crossterm 对 Alt+数字 的实际解析结果）
        let alt = |c: char| KeyEvent::new(KeyCode::Char(c), KeyModifiers::ALT | KeyModifiers::NONE);
        for v in HomeView::ALL {
            let slot = v.slot();
            app.handle_key(alt(char::from(b'0' + slot)));
            assert_eq!(app.home_view, v, "Alt+{} 应直达 {}", slot, v.label());
        }
        // 非数字的 Alt 组合不应被误当作切视图
        app.home_view = HomeView::Instance;
        app.handle_key(alt('j'));
        assert_eq!(app.home_view, HomeView::Instance, "Alt+j 不应切视图");
    }

    #[tokio::test]
    async fn session_memory_round_trips_across_restart() {
        let dir = crate::storage::tmp_config_dir("session");
        // 第一次启动：改一堆状态
        let mut a = App::new_from(dir.clone()).unwrap();
        a.home_view = HomeView::Resource;
        a.source = ResourceSource::ALL[1];
        a.category = 2;
        a.role_name = "记忆角色".into();
        a.home_focus = HomeFocus::Downloads;
        a.save_session();

        // 第二次启动：应当全部还原
        let b = App::new_from(dir).unwrap();
        assert_eq!(b.home_view, HomeView::Resource, "视图");
        assert_eq!(b.source, ResourceSource::ALL[1], "资源来源");
        assert_eq!(b.category, 2, "资源分类");
        assert_eq!(b.role_name, "记忆角色", "角色名");
        assert_eq!(b.home_focus, HomeFocus::Downloads, "聚焦面板");
    }

    #[tokio::test]
    async fn session_memory_clamps_out_of_range_and_partial_fields() {
        let dir = crate::storage::tmp_config_dir("session-bad");

        // 越界值（手改过 / 来自字段更多的旧版本）必须夹住，不能 panic
        std::fs::write(
            dir.join("session.json"),
            r#"{"home_view":99,"resource_source":99,"category":9999,"home_focus":99,
                "selected_instance":9999,"selected_account":9999,"selected_world":9999}"#,
        )
        .unwrap();
        let a = App::new_from(dir.clone()).unwrap();
        assert_eq!(a.home_view, HomeView::Instance, "越界视图回落默认");
        assert_eq!(a.source, ResourceSource::NetEase, "越界来源回落默认");
        assert_eq!(
            a.category,
            crate::domain::ALL_CATEGORIES.len() - 1,
            "越界分类夹到末尾"
        );
        assert_eq!(a.home_focus, HomeFocus::List, "越界聚焦回落 List");
        // 下方还会索引 instances/worlds/accounts，空列表时 saturating_sub 兜底
        let mut a = a;
        a.spawn_load_grid(false);
        assert!(
            a.category < crate::domain::ALL_CATEGORIES.len(),
            "category 可安全索引"
        );

        // 缺字段（老版本只有部分字段）必须逐个补默认值，而不是整份作废
        std::fs::write(
            dir.join("session.json"),
            r#"{"category":3,"role_name":"老版本"}"#,
        )
        .unwrap();
        let b = App::new_from(dir).unwrap();
        assert_eq!(b.category, 3, "已存在的字段正常恢复");
        assert_eq!(b.role_name, "老版本");
        assert_eq!(
            b.home_view,
            HomeView::Instance,
            "缺失字段回落默认值而非清空整份"
        );
    }

    #[tokio::test]
    async fn scan_worlds_lists_level_dat_dirs_sorted_newest_first() {
        let mc = std::env::temp_dir().join("mctl-scan-worlds-test");
        let _ = std::fs::remove_dir_all(&mc);
        std::fs::create_dir_all(&mc.join("saves/world_b")).unwrap();
        std::fs::create_dir_all(&mc.join("saves/world_a")).unwrap();
        std::fs::create_dir_all(&mc.join("saves/not_a_world")).unwrap();
        std::fs::write(mc.join("saves/world_a/level.dat"), b"x").unwrap();
        std::thread::sleep(std::time::Duration::from_secs(2));
        // world_b 最新
        std::fs::write(mc.join("saves/world_b/level.dat"), b"y").unwrap();
        let worlds = scan_worlds_dir(&mc.join("saves"));
        let names: Vec<&str> = worlds.iter().map(|w| w.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["world_b", "world_a"],
            "按修改时间最新在前, 无 level.dat 的目录忽略: {names:?}"
        );
        assert!(
            worlds[0].last_played.contains(':'),
            "时间格式 MM-DD HH:mm: {}",
            worlds[0].last_played
        );
        let _ = std::fs::remove_dir_all(&mc);
    }

    #[tokio::test]
    async fn view_icons_and_alt_slots_cover_every_view() {
        // 图标/快捷位不能漏：视图栏靠图标区分，Alt+数字靠 slot 直达
        let mut seen_slots = Vec::new();
        for v in HomeView::ALL {
            assert!(!v.icon().is_empty(), "{} 缺图标", v.label());
            assert!(
                crate::ui::cell_width_of(v.icon()) == 1,
                "{} 图标 {:?} 不是单列宽，会撑歪视图栏",
                v.label(),
                v.icon()
            );
            let slot = v.slot();
            assert!(
                (1..=HomeView::ALL.len() as u8).contains(&slot),
                "{} 的 alt 位 {slot} 越界",
                v.label()
            );
            assert!(!seen_slots.contains(&slot), "alt 位 {slot} 重复");
            seen_slots.push(slot);
        }
        // 反向：每个 alt 位都能跳到对应视图
        for v in HomeView::ALL {
            let mut app = app();
            app.switch_view_alt((b'0' + v.slot()) as char);
            assert_eq!(app.home_view, v, "alt+{} 应跳到 {}", v.slot(), v.label());
        }
    }

    #[tokio::test]
    async fn switch_view_alt_jumps_to_view() {
        let mut app = app();
        // 全部 7 个视图都从 slot 推导——写死数字的话，插入 Instance 后
        // 这里的 '3'/'4' 会静默指向别的视图，测试还可能「碰巧」通过。
        for v in HomeView::ALL {
            app.switch_view_alt(char::from(b'0' + v.slot()));
            assert_eq!(app.home_view, v, "Alt+{} 应直达 {}", v.slot(), v.label());
        }
        app.switch_view_alt('9');
        assert_eq!(
            app.home_view,
            HomeView::ALL[HomeView::ALL.len() - 1],
            "越界数字应忽略"
        );
        app.switch_view_alt('x');
        assert_eq!(
            app.home_view,
            HomeView::ALL[HomeView::ALL.len() - 1],
            "非数字应忽略"
        );
    }

    /// Tab **不再切主视图**（用户要求）：Tab 归表单自己用，视图切换只走 Alt+数字。
    /// /// 回归点：原先 `handle_global_key` 全局劫持 Tab/Shift+Tab 切视图，于是每个
    /// 嵌套输入框（设置页字段、登录表单账号/密码、角色名）都得自己"抢"这个键，
    /// 抢输了就彻底用不了 Tab。
    #[tokio::test]
    async fn tab_does_not_switch_view_but_alt_digit_does() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut app = app();
        for v in HomeView::ALL {
            app.home_view = v;
            app.sync_wm_from_view();
            for (code, m) in [
                (KeyCode::Tab, KeyModifiers::NONE),
                (KeyCode::BackTab, KeyModifiers::SHIFT),
                (KeyCode::Tab, KeyModifiers::SHIFT),
            ] {
                app.handle_key(KeyEvent::new(code, m));
                assert_eq!(
                    app.home_view,
                    v,
                    "{} 视图下按 {:?} 不该切视图",
                    v.label(),
                    code
                );
            }
        }
        for v in HomeView::ALL {
            let c = char::from(b'0' + v.slot());
            app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::ALT));
            assert_eq!(app.home_view, v, "Alt+{} 应直达 {}", v.slot(), v.label());
        }
    }

    /// 登录页输入态：含 f/e/a 的用户名密码必须打得进去。
    /// /// 回归：快捷键原先没有输入态守卫，按 f 会直接关掉登录页——
    /// 结果是密码里含 f 的账号根本没法登录，且无任何报错。
    #[tokio::test]
    async fn login_input_accepts_letters_used_by_shortcuts() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut app = app();
        app.login_screen = Some(LoginScreen {
            selected: 0,
            input_4399: true,
        });
        app.login_user.clear();
        app.login_pass.clear();
        let c = |ch| KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE);

        // 用户名字段：f / e / a 都应落进输入框
        app.login_input_step = 0;
        for ch in "f-ea".chars() {
            app.handle_key(c(ch));
        }
        assert_eq!(app.login_user, "f-ea", "用户名里的快捷键字母应正常输入");
        assert!(app.login_screen.is_some(), "输入字母不该关掉登录页");

        // 密码字段同理
        app.login_input_step = 1;
        for ch in "ef-a".chars() {
            app.handle_key(c(ch));
        }
        assert_eq!(app.login_pass, "ef-a", "密码里的快捷键字母应正常输入");
        assert!(app.login_screen.is_some(), "输入字母不该关掉登录页");

        // 非输入态下 a 仍能关闭登录页
        app.login_screen.as_mut().unwrap().input_4399 = false;
        app.handle_key(c('a'));
        assert!(app.login_screen.is_none(), "非输入态 a 应关闭登录页");
    }

    #[tokio::test]
    async fn focus_cycle_filters_by_view() {
        let mut app = app();
        app.home_view = HomeView::Server;
        assert_eq!(HomeFocus::all_for(HomeView::Server).len(), 5);
        // 走生产路径：j/k 实际调的就是 move_focus_down（旧的 cycle_home_focus
        // 包装器已删——它没有任何调用方，而底层 all_for/next 是活的）
        app.home_focus = HomeFocus::Accounts;
        app.move_focus_down();
        assert_eq!(app.home_focus, HomeFocus::Banner);
        app.move_focus_down();
        assert_eq!(app.home_focus, HomeFocus::List);
        app.move_focus_down();
        assert_eq!(app.home_focus, HomeFocus::Status);
        app.move_focus_down();
        assert_eq!(
            app.home_focus,
            HomeFocus::Downloads,
            "服务器视图现在有 Downloads 面板"
        );
        app.move_focus_down();
        assert_eq!(app.home_focus, HomeFocus::Accounts, "循环回到账户");
        // 焦点同步到 dock 高亮（Accounts → win_id::ACCOUNTS）
        assert_eq!(app.wm.active, win_id::ACCOUNTS);
    }

    #[tokio::test]
    async fn enter_launches_world_only_when_list_focused() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        // 构造一个世界目录（版本隔离：世界在实例目录 versions/testinst/saves 下）
        let mc = std::env::temp_dir().join(format!("mctl_ent_test_{}", std::process::id()));
        let inst_dir = mc.join("versions/testinst");
        std::fs::create_dir_all(inst_dir.join("saves/world_x")).unwrap();
        std::fs::write(inst_dir.join("saves/world_x/level.dat"), b"x").unwrap();
        let mut app = app();
        app.instances.clear(); // 隔离真实环境实例：spawn_launch 提前返回，不 take launch_world
        app.mc_dir = mc.clone();
        app.instances.push(crate::launcher::Instance {
            bedrock: false,
            id: "testinst".into(),
            version_type: "release".into(),
            main_class: String::new(),
            assets_index: String::new(),
            java_major: 17,
            dir: inst_dir,
        });
        app.selected_instance = 0; // session 恢复的索引可能越界（真实 session.json 污染）
        app.refresh_worlds();
        assert!(!app.worlds.is_empty());

        // 聚焦横幅：回车不启动、不进入世界
        app.home_focus = HomeFocus::Banner;
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.launch_world.is_none(), "非列表聚焦回车不进入世界");
        assert!(!app.launching, "非列表聚焦回车不启动");

        // 聚焦列表：回车 = quickPlay 直进选中世界（spawn_launch 同步消费 launch_world 并置 launching）
        app.home_focus = HomeFocus::List;
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.launching, "列表聚焦回车直进世界");
        let _ = std::fs::remove_dir_all(&mc);
    }

    #[tokio::test]
    async fn ls_oauth_form_input_and_submit() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut app = app();
        app.ls.app_name.clear(); // 防并行测试经 littleskin.json 污染
        app.ls.callback_url = crate::api::littleskin::OAUTH_CALLBACK_DEFAULT.to_string();
        app.cred = None; // 强制未登录，Enter 才会打开全屏登录界面
        app.ls.client_id.clear(); // 防真实配置污染：无 client_id 才进登记表单
        app.home_focus = HomeFocus::Accounts;
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        // 未登录 Enter 打开全屏登录界面；选 [2]LittleSkin 再 Enter 进登记
        assert!(app.login_screen.is_some(), "未登录回车打开全屏登录界面");
        app.handle_key(KeyEvent::new(KeyCode::Char('2'), KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.ls.input, "选 LittleSkin 回车进入 OAuth 登记输入");
        app.handle_key(KeyEvent::new(KeyCode::Char('m'), KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE));
        assert_eq!(app.ls.app_name, "mc", "字符输入到应用名");
        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.ls.step, 1, "Tab 切到回调 URL");
        app.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        assert_eq!(
            app.ls.callback_url, "https://open.littleskin.cn/oauth/callbackx",
            "字符输入到回调 URL"
        );
        app.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(
            app.ls.callback_url,
            "https://open.littleskin.cn/oauth/callback"
        );
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(!app.ls.input, "Esc 退出输入模式");
    }

    #[tokio::test]
    async fn ls_submit_validates_and_requires_https_url() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut app = app();
        app.cred = None; // 强制未登录
        app.ls.client_id.clear(); // 防真实配置污染
        app.home_focus = HomeFocus::Accounts;
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Char('2'), KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        // 空应用名 → 校验提示，不保存
        app.ls.input = true;
        app.ls.app_name.clear();
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.ls.input, "校验失败保持输入模式");
        assert_eq!(app.ls.status, "请填写应用名");
        // 非法 URL → 校验提示
        app.ls.app_name = "mctl".into();
        app.ls.callback_url = "ftp://x".into();
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.ls.status, "回调 URL 需以 http(s):// 开头");
        // 合法 → 保存成功退出输入模式（备份原配置，测试后恢复）
        let ls_path = storage::config_dir().join("littleskin.json");
        let backup = std::fs::read(&ls_path).ok();
        app.ls.callback_url = "https://open.littleskin.cn/oauth/callback".into();
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(!app.ls.input, "保存成功退出输入模式");
        assert!(app.ls.status.contains("客户端 ID"), "{}", app.ls.status);
        assert_eq!(
            storage::load_ls().unwrap().app_name,
            "mctl",
            "配置已写入 littleskin.json"
        );
        match backup {
            Some(b) => std::fs::write(&ls_path, b).unwrap(),
            None => {
                let _ = std::fs::remove_file(&ls_path);
            }
        }
    }

    #[tokio::test]
    async fn status_focus_cycles_instance_updown() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut app = app();
        app.instances = vec![
            crate::launcher::Instance {
                bedrock: false,
                id: "a".into(),
                version_type: "release".into(),
                main_class: "net.minecraft.client.main.Main".into(),
                assets_index: "1.21".into(),
                java_major: 21,
                dir: "/tmp/a".into(),
            },
            crate::launcher::Instance {
                bedrock: false,
                id: "b".into(),
                version_type: "release".into(),
                main_class: "net.minecraft.client.main.Main".into(),
                assets_index: "1.20".into(),
                java_major: 17,
                dir: "/tmp/b".into(),
            },
        ];
        app.home_focus = HomeFocus::Status;
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.selected_instance, 1, "状态卡聚焦 ↓ 换实例");
        app.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(app.selected_instance, 0, "状态卡聚焦 ↑ 换回");
        // 列表聚焦 ↓ 不应改实例
        app.home_focus = HomeFocus::List;
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.selected_instance, 0, "列表聚焦不改实例");
    }

    /// 回归：Shift+Tab(BackTab) 曾经切视图。已按用户要求移除，Tab 系列归表单使用。
    #[tokio::test]
    async fn shift_tab_no_longer_switches_view() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut app = app();
        app.handle_key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT));
        assert_eq!(app.home_view, HomeView::Instance, "Shift+Tab 不该再切视图");
    }

    /// 设置项「选中框动画」：关了 = 进度恒 1（框直接完整出现）、不再要求高帧重绘
    #[tokio::test]
    async fn world_anim_setting_off_means_instant_border() {
        let mut a = app();
        a.world_anim = false;
        a.world_anim_start = Some(std::time::Instant::now());
        assert!(!a.world_anim_playing(), "关了就不该再催主循环重绘");
        assert_eq!(a.world_anim_progress(), 1.0, "关了 = 框直接完整出现");
        // 开着才播过程
        a.world_anim = true;
        assert!(a.world_anim_playing());
        assert!(a.world_anim_progress() < 1.0);
    }

    /// 换视图会重放「视图」选中块的刷色动画；停在同一个视图不重放
    #[tokio::test]
    async fn switching_view_replays_sidebar_wipe() {
        use std::time::{Duration, Instant};
        let mut a = app();
        assert!(a.view_anim_start.is_none(), "启动时选中块直接是满的");
        assert_eq!(a.view_anim_progress(), 1.0);
        assert!(!a.view_anim_playing(), "不播动画就别催主循环重绘");

        a.switch_view_alt('3');
        assert!(a.view_anim_start.is_some(), "换视图要重放刷色");
        assert!(a.view_anim_playing());
        assert!(a.view_anim_progress() < 1.0, "刚切换时还没刷满");

        // 播完之后是常驻的满块
        a.view_anim_start = Some(Instant::now() - Duration::from_secs(9));
        assert!(!a.view_anim_playing());
        assert_eq!(a.view_anim_progress(), 1.0);
        // 再点同一个视图：不该重新播（起点保持不动）
        let t0 = a.view_anim_start;
        a.switch_view_alt('3');
        assert_eq!(a.view_anim_start, t0, "停在同一个视图不重放动画");
    }

    /// 设置项「主题」：←→ 循环（含绕回）、按 id/显示名应用、未知主题报错不动状态
    #[tokio::test]
    async fn theme_setting_cycles_and_applies() {
        let mut a = app();
        let ids = crate::theme::theme_ids();
        assert_eq!(a.theme, crate::theme::LEGACY_ID, "没配过 = 经典配色");
        SettingField::Theme.bump(&mut a, 1);
        assert_eq!(a.theme, ids[1]);
        assert_eq!(
            crate::theme::cur(),
            crate::theme::palette_of(&ids[1]),
            "切主题要立刻变色（预览即所得）"
        );
        SettingField::Theme.bump(&mut a, -1);
        assert_eq!(a.theme, crate::theme::LEGACY_ID);
        SettingField::Theme.bump(&mut a, -1);
        assert_eq!(a.theme, *ids.last().unwrap(), "第一项再往左 = 绕到最后一项");
        // / 编辑态也能用：id 或显示名都认
        SettingField::Theme.apply(&mut a, "dracula").unwrap();
        assert_eq!(a.theme, "dracula");
        let label = crate::theme::theme_label("dracula");
        SettingField::Theme.apply(&mut a, &label).unwrap();
        assert!(SettingField::Theme.apply(&mut a, "不存在").is_err());
        assert_eq!(a.theme, "dracula", "报错时状态不能被改");
        // 复原全局配色，别串到别的测试
        crate::theme::set_theme(crate::theme::LEGACY_ID);
    }

    /// 主题进入设置页快照：「放弃」能把预览换的主题撤回去
    #[tokio::test]
    async fn revert_restores_theme() {
        let mut a = app();
        a.enter_settings();
        SettingField::Theme.bump(&mut a, 1);
        a.run_setting_btn(SettingBtn::Revert);
        assert_eq!(a.theme, crate::theme::LEGACY_ID, "放弃应还原主题");
        assert_eq!(
            crate::theme::cur(),
            crate::theme::Palette::legacy(),
            "界面配色也要跟着还原"
        );
    }

    // ---- 下载中心：任务 id 与进度落位 ----

    /// 进度消息按任务 id 落位：并发多个下载时不能互相串台
    /// （旧实现是「最后一个 Active 任务」，两个任务同时跑就错位）
    #[tokio::test]
    async fn install_progress_targets_task_by_id() {
        let mut app = app();
        let a = app.begin_task("任务 A");
        let b = app.begin_task("任务 B");
        assert_ne!(a, b);
        app.on_install_stage(a, "阶段 A".into(), 3, 9);
        app.on_file_progress(
            a,
            crate::launcher::FileProgress {
                name: "x.jar".into(),
                done: 5,
                total: 10,
                finished_at: None,
                failed: false,
            },
        );
        let ta = app.install_tasks.iter().find(|t| t.id == a).unwrap();
        let tb = app.install_tasks.iter().find(|t| t.id == b).unwrap();
        assert_eq!(ta.phase, "阶段 A");
        assert_eq!((ta.done, ta.total), (3, 9));
        assert_eq!(ta.files.len(), 1, "文件细线挂在 A 上");
        assert_eq!(tb.phase, "排队中", "B 不该收到 A 的进度");
        assert!(tb.files.is_empty());
        // 终态只落 A
        app.on_install_done(a, Ok("A".into()));
        assert_eq!(
            app.install_tasks.iter().find(|t| t.id == a).unwrap().status,
            InstallStatus::Done
        );
        assert_eq!(
            app.install_tasks.iter().find(|t| t.id == b).unwrap().status,
            InstallStatus::Active
        );
        // 任务列表封顶：长时间挂着不能无限涨
        for i in 0..30 {
            app.begin_task(format!("T{i}"));
        }
        assert_eq!(app.install_tasks.len(), 20);
    }

    /// 下载中心长条的分子分母：多个下载并发时按任务各记各的，
    /// 文件槽的那点显示状态（完成顶到 u64::MAX、最多留 12 条）不能带偏它。
    #[tokio::test]
    async fn task_byte_totals_accumulate_per_task() {
        let mut app = app();
        let a = app.begin_task("下载 地图A.zip");
        let b = app.begin_task("下载 模组B.jar");
        let fp = |n: &str, d: u64, t: u64| crate::launcher::FileProgress {
            name: n.into(),
            done: d,
            total: t,
            finished_at: None,
            failed: false,
        };
        app.on_file_progress(a, fp("a.part1", 5, 10));
        app.on_file_progress(b, fp("b.jar", 50, 100));
        app.on_file_progress(a, fp("a.part1", 10, 10));
        {
            let ta = app.install_tasks.iter().find(|t| t.id == a).unwrap();
            let tb = app.install_tasks.iter().find(|t| t.id == b).unwrap();
            assert_eq!((ta.bytes_done, ta.bytes_total), (10, 10), "A 的总量");
            assert_eq!((tb.bytes_done, tb.bytes_total), (50, 100), "B 的总量");
            assert_eq!(tb.files[0].total, 100, "终态消息不该清掉文件长度");
        }
        // 汇总：一个已完成一个在跑
        app.on_task_done(a, true, "完成: 地图A.zip".into());
        let (active, done, failed, bd, bt, _) = app.download_summary();
        assert_eq!((active, done, failed), (1, 1, 0));
        assert_eq!((bd, bt), (50, 100), "汇总只算进行中的任务");
    }

    /// c 清理：只清已结束的；进行中的任务必须留着（清掉就永远看不到进度了）
    #[tokio::test]
    async fn clear_finished_tasks_keeps_active() {
        let mut app = app();
        let a = app.begin_task("在跑");
        let b = app.begin_task("完成");
        let c = app.begin_task("失败");
        app.on_task_done(b, true, "ok".into());
        app.on_task_done(c, false, "boom".into());
        assert_eq!(app.clear_finished_tasks(), 2);
        assert_eq!(app.install_tasks.len(), 1);
        assert_eq!(app.install_tasks[0].id, a);
        assert_eq!(app.clear_finished_tasks(), 0, "再清一次没有可清的");
    }

    /// 进服阶段上报：状态行 + 下载中心任务标题都要发（异步发送，等一小会儿）
    #[tokio::test]
    async fn report_stage_sends_status_line_and_task_title() {
        let (tx, mut rx) = mpsc::channel(16);
        report_stage(&tx, StageSink::Launch, Some(7), "下载网易客户端 base", 1, 3);
        let (mut got_stage, mut got_task) = (false, false);
        for _ in 0..20 {
            while let Ok(m) = rx.try_recv() {
                match m {
                    TaskMsg::InstallStage {
                        id,
                        phase,
                        done,
                        total,
                    } => {
                        assert_eq!(id, 7);
                        assert_eq!(phase, "下载网易客户端 base");
                        assert_eq!((done, total), (1, 3));
                        got_task = true;
                    }
                    TaskMsg::LaunchStage(s) => {
                        assert_eq!(s, "下载网易客户端 base 1/3");
                        got_stage = true;
                    }
                    _ => panic!("进服阶段不该发这类消息"),
                }
            }
            if got_stage && got_task {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(got_task, "任务标题应更新");
        assert!(got_stage, "状态行应更新");
        // 没有任务时（本地实例启动）只发状态行
        let (tx2, mut rx2) = mpsc::channel(16);
        report_stage(&tx2, StageSink::Launch, None, "补全依赖库", 8, 16);
        let m = rx2.recv().await.unwrap();
        match m {
            TaskMsg::LaunchStage(s) => assert_eq!(s, "补全依赖库 8/16"),
            _ => panic!("应只发状态行"),
        }
    }
}
