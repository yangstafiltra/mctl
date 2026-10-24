//! 命令行接口：层级化上下文 + 全局运维命令。
//!
//! 设计目标：把「在哪儿」和「做什么」分开。上下文像文件系统路径，
//! `list` / `info` 按当前位置解释；深层动作（开服、关服）只在对应上下文里出现。
//!
//! ```text
//! /                 根：上下文索引
//! /account          账号（脱敏展示）
//! /local            本地实例
//! /local/<实例>      某实例的存档（世界）
//! /server           Java 国际服（内置精选 + 自定义）
//! /server/<host:port>  单个服务器（原版 Server List Ping）
//! /plugin           插件
//! ```
//!
//! 两种用法都支持：
//! - 有状态：`mctl cd local` → `mctl list` → `mctl cd ..`
//! - 一次性：`mctl local list`、`mctl server/<host:port> info`（不改当前上下文）
//!
//! 状态只存路径（`~/.config/mctl/cli/context`），不含任何凭据。

use std::io::{self, BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Result};

use crate::api;
use crate::domain::{
    CATEGORY_MAPS_ADVENTURE, CATEGORY_MAPS_MINIGAME, CATEGORY_MODS,
    CATEGORY_SHADERS, CATEGORY_SKINS,
};
use crate::launcher::modpack::{ModpackHit, ModrinthIndex, MrVersion};
use crate::launcher::{self, Instance};
use crate::plugin;
use crate::storage;

/// 资源列表每页条数（固定 20，翻页按它算 offset）。
const PAGE_SIZE: i64 = 20;

/// 上下文路径。只保留一层子节点：实例 / 服务器 / 下载源。
#[derive(Debug, Clone, PartialEq, Eq)]
enum Ctx {
    Root,
    Account,
    Local(Option<String>),
    Server(Option<String>),
    Resource(Option<String>),
    Plugin,
}

impl Default for Ctx {
    fn default() -> Self {
        Self::Root
    }
}

impl Ctx {
    fn path(&self) -> String {
        match self {
            Self::Root => "/".into(),
            Self::Account => "/account".into(),
            Self::Local(None) => "/local".into(),
            Self::Local(Some(id)) => format!("/local/{id}"),
            Self::Server(None) => "/server".into(),
            Self::Server(Some(id)) => format!("/server/{id}"),
            Self::Resource(None) => "/resource".into(),
            Self::Resource(Some(src)) => format!("/resource/{src}"),
            Self::Plugin => "/plugin".into(),
        }
    }

    fn parent(&self) -> Self {
        match self {
            Self::Root => Self::Root,
            Self::Account => Self::Root,
            Self::Local(Some(_)) => Self::Local(None),
            Self::Local(None) => Self::Root,
            Self::Server(Some(_)) => Self::Server(None),
            Self::Server(None) => Self::Root,
            Self::Resource(Some(_)) => Self::Resource(None),
            Self::Resource(None) => Self::Root,
            Self::Plugin => Self::Root,
        }
    }

    /// 解析绝对路径（`local/1.20`、`/server`、`~`、`/`）。
    fn parse(path: &str) -> Result<Self> {
        let p = path.trim().trim_start_matches('/');
        if p.is_empty() || p == "~" {
            return Ok(Self::Root);
        }
        let mut segs = p.split('/').filter(|s| !s.is_empty() && *s != ".");
        let head = segs.next().unwrap_or_default();
        let tail: Vec<&str> = segs.collect();
        if tail.len() > 1 {
            bail!("上下文最多两级：{path}");
        }
        let child = || tail.first().map(|s| s.to_string());
        match head {
            "root" => {
                if tail.is_empty() {
                    Ok(Self::Root)
                } else {
                    bail!("`/root` 下没有子节点")
                }
            }
            "account" => match child() {
                None => Ok(Self::Account),
                Some(_) => bail!("`/account` 下没有子节点"),
            },
            "local" => Ok(Self::Local(child())),
            "server" => Ok(Self::Server(child())),
            "resource" | "res" | "资源下载" => Ok(Self::Resource(child())),
            "plugin" => match child() {
                None => Ok(Self::Plugin),
                Some(_) => bail!("`/plugin` 下没有子节点"),
            },
            other => bail!(
                "未知上下文 `{other}`；可用：/account /local /server /resource /plugin（pwd 看当前位置）"
            ),
        }
    }
}

/// `cd` 的解析：支持绝对路径、`..`、`-`、以及上下文内的裸子节点名。
fn resolve_cd(cur: &Ctx, prev: &Ctx, arg: &str) -> Result<Ctx> {
    let a = arg.trim();
    if a.is_empty() || a == "~" || a == "/" {
        return Ok(Ctx::Root);
    }
    if a == ".." {
        return Ok(cur.parent());
    }
    if a == "-" {
        return Ok(prev.clone());
    }

    let body = a.trim_start_matches('/');
    // `cd download` 是资源下载的别名；但 `mctl download <序号>` 仍然是命令，
    // 所以别名只放在 cd 里，不放进顶层前缀。
    if body == "download" {
        return Ok(Ctx::Resource(None));
    }
    if let Some(tail) = body.strip_prefix("download/") {
        return Ctx::parse(&format!("resource/{tail}"));
    }

    // 绝对路径（`cd /local/1.21.1`）在哪儿都按绝对路径解析；
    // 顶层名字（`cd local`）也一律回 /local。
    if a.starts_with('/')
        || matches!(
            body.split('/').next(),
            Some(
                "root"
                    | "account"
                    | "local"
                    | "server"
                    | "resource"
                    | "res"
                    | "资源下载"
                    | "plugin"
            )
        )
    {
        return Ctx::parse(body);
    }
    match cur {
        Ctx::Local(_) => Ok(Ctx::Local(Some(a.to_string()))),
        Ctx::Server(_) => Ok(Ctx::Server(Some(a.to_string()))),
        Ctx::Resource(_) => Ok(Ctx::Resource(Some(a.to_string()))),
        Ctx::Root => Ctx::parse(a),
        Ctx::Account => bail!("账号上下文下没有子节点；`cd /local` 或 `cd ..`"),
        Ctx::Plugin => bail!("插件上下文下没有子节点；`cd /` 或 `cd ..`"),
    }
}

// ===== 上下文状态（只存路径） =====

#[derive(Debug, Clone)]
struct CliState {
    cur: Ctx,
    prev: Ctx,
    /// /resource 下的页码（从 1 开始）；其他上下文忽略。
    page: usize,
    /// /resource 下的分类 key；空串表示用下载源默认分类。
    category: String,
}

impl Default for CliState {
    fn default() -> Self {
        Self {
            cur: Ctx::Root,
            prev: Ctx::Root,
            page: 1,
            category: String::new(),
        }
    }
}

fn state_file() -> PathBuf {
    storage::config_dir().join("cli").join("context")
}

fn load_state() -> CliState {
    let text = std::fs::read_to_string(state_file()).unwrap_or_default();
    let mut lines = text.lines();
    CliState {
        cur: lines
            .next()
            .and_then(|l| Ctx::parse(l).ok())
            .unwrap_or_default(),
        prev: lines
            .next()
            .and_then(|l| Ctx::parse(l).ok())
            .unwrap_or_default(),
        page: lines
            .next()
            .and_then(|l| l.trim().parse().ok())
            .filter(|p| *p > 0)
            .unwrap_or(1),
        category: lines.next().unwrap_or_default().trim().to_string(),
    }
}

fn save_state(st: &CliState) -> Result<()> {
    let path = state_file();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut tmp = path.clone();
    tmp.set_extension("tmp");
    std::fs::write(
        &tmp,
        format!(
            "{}\n{}\n{}\n{}\n",
            st.cur.path(),
            st.prev.path(),
            st.page.max(1),
            st.category
        ),
    )?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

// ===== 入口 =====

/// CLI 顶层分发。`args[0]` 是程序名。
///
/// 命令按「路径」逐级下沉：`mctl local/1.21.1 list`、
/// `mctl local 1.21.1 list`（在 /local 下还可以直接 `mctl 1.21.1 list`）
/// 都落到同一个节点；没写动词时默认 `list`。
pub async fn run(args: &[String]) -> Result<()> {
    let mut i = 1;
    let mut ctx = load_state().cur;

    // 显式路径前缀（一次性，不写回状态）：mctl server list / mctl / install ...
    if let Some(first) = args.get(1) {
        if is_context_prefix(first) {
            ctx = normalize_ctx(Ctx::parse(first)?)?;
            i = 2;
        }
    }

    // 层级行走：当前节点是可展开的类别根，而下一个词又不是该层动词/全局命令，
    // 就把它当子节点 id。这样命令本身长得就像路径，而不是平铺的 switch。
    while can_descend(&ctx) {
        let Some(tok) = args.get(i) else { break };
        if is_global_verb(tok) || ctx_verbs(&ctx).contains(&tok.as_str()) {
            break;
        }
        // /resource 下直接输入数字是「下载第 n 条」，不是下载源 id。
        if matches!(ctx, Ctx::Resource(None)) && tok.parse::<usize>().is_ok() {
            break;
        }
        let child = match &ctx {
            Ctx::Local(_) => Ctx::Local(Some(tok.clone())),
            Ctx::Server(_) => Ctx::Server(Some(tok.clone())),
            Ctx::Resource(_) => Ctx::Resource(Some(tok.clone())),
            _ => break,
        };
        ctx = normalize_ctx(child)?;
        i += 1;
    }

    let cmd = args.get(i).map(String::as_str).unwrap_or("list");
    dispatch(ctx, cmd, args.get(i + 1..).unwrap_or(&[])).await
}

/// 只有「顶层上下文名」才算前缀；`list` 这些全局命令不受影响。
/// `/` 与 `~` 是根的一次性写法（例如在 /plugin 下用 `mctl / install` 调全局安装）。
fn is_context_prefix(s: &str) -> bool {
    matches!(s, "/" | "~")
        || matches!(
            s.split('/').next(),
            Some(
                "root"
                    | "account"
                    | "local"
                    | "server"
                    | "resource"
                    | "res"
                    | "资源下载"
                    | "plugin"
            )
        )
}

/// 能再往下走一层的节点（类别根）。
fn can_descend(ctx: &Ctx) -> bool {
    matches!(
        ctx,
        Ctx::Local(None) | Ctx::Server(None) | Ctx::Resource(None)
    )
}

/// 某一层自己认识的动词。用于区分「子节点 id」和「命令」。
fn ctx_verbs(ctx: &Ctx) -> &'static [&'static str] {
    match ctx {
        Ctx::Root => &["list", "ls", "info"],
        Ctx::Account => &["list", "ls", "info"],
        Ctx::Local(_) => &["list", "ls", "info", "worlds"],
        Ctx::Server(_) => &["list", "ls", "info", "detail", "add", "rm", "remove"],
        Ctx::Resource(None) => &["list", "ls", "info", "source", "sources"],
        Ctx::Resource(Some(_)) => &[
            "list", "ls", "info", "download", "get", "next", "n", "prev", "p", "page", "category",
            "cat", "source", "sources",
        ],
        Ctx::Plugin => &["list", "ls", "info", "install", "update", "remove"],
    }
}

/// 任何层都能用的命令。
fn is_global_verb(s: &str) -> bool {
    matches!(
        s,
        "cd" | "pwd"
            | "help"
            | "?"
            | "-h"
            | "--help"
            | "tree"
            | "map"
            | "status"
            | "version"
            | "-V"
            | "--version"
            | "login"
            | "install"
            | "bedrock"
            | "token"
            | "dl"
            | "detail"
            | "online"
            | "tui"
            | "cli"
            | "shell"
    )
}

async fn dispatch(ctx: Ctx, cmd: &str, rest: &[String]) -> Result<()> {
    // 上下文自己的命令优先：/plugin 下的 install/update/remove 属于插件管理，
    // 不能被全局的 `mctl install`（装游戏版本）抢走。
    if ctx_verbs(&ctx).contains(&cmd) {
        return match &ctx {
            Ctx::Root => match cmd {
                "list" | "ls" => root_list(),
                _ => status_cmd(),
            },
            Ctx::Account => account_show(),
            Ctx::Local(id) => local_cmd(id.as_deref(), cmd, rest),
            Ctx::Server(id) => server_cmd(id.as_deref(), cmd, rest).await,
            Ctx::Resource(src) => resource_cmd(src.as_deref(), cmd, rest).await,
            Ctx::Plugin => plugin_cmd(cmd, rest),
        };
    }

    // 资源上下文里直接输入数字 = 下载这一行：「3」或「3 2」（第 3 条的第 2 个版本）。
    if let Ctx::Resource(Some(src)) = &ctx {
        if let Ok(n) = cmd.parse::<usize>() {
            let ver = rest.first().and_then(|s| s.parse::<usize>().ok());
            return resource_download_row(src, n, ver).await;
        }
    }

    // 与上下文无关的命令（含旧版平铺命令），保证老脚本还能跑。
    match cmd {
        "cd" => return cd_cmd(rest),
        "pwd" => {
            println!("{}", ctx.path());
            return Ok(());
        }
        "tree" | "map" => return print_tree(&ctx),
        "help" | "?" | "-h" | "--help" => return help(&ctx),
        "status" => return status_cmd(),
        "version" | "-V" | "--version" => {
            println!("mctl {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        "cli" | "shell" => return Box::pin(shell()).await,
        "login" => return login_cmd(rest).await,
        "install" => return install_cmd(rest).await,
        "bedrock" => return bedrock_cmd(rest).await,
        "token" => return token_cmd(rest),
        "dl" => return dl_cmd(rest).await,
        "detail" => {
            // 旧命令：mctl detail <名称|host:port>
            let id = rest
                .first()
                .ok_or_else(|| anyhow!("用法: mctl detail <item_id>"))?;
            return server_info_cmd(id).await;
        }
        "online" => return server_table(&[]).await,
        "tui" => {
            println!("不带参数运行 `mctl` 就会进入 TUI。");
            return Ok(());
        }
        _ => {}
    }

    unknown(cmd, &ctx)
}

fn unknown(cmd: &str, ctx: &Ctx) -> Result<()> {
    bail!(
        "`{}` 里没有命令 `{cmd}`；`mctl help` 看本层命令，`mctl tree` 看完整层级",
        ctx.path()
    )
}

// ===== cd / pwd =====

fn cd_cmd(rest: &[String]) -> Result<()> {
    let mut st = load_state();
    let Some(arg) = rest.first() else {
        println!("{}", st.cur.path());
        return Ok(());
    };
    let next = normalize_ctx(resolve_cd(&st.cur, &st.prev, arg)?)?;
    if next != st.cur {
        st.prev = st.cur.clone();
        st.cur = next.clone();
        if matches!(next, Ctx::Resource(_)) {
            st.page = 1;
        }
        if let Ctx::Resource(Some(src)) = &next {
            if !category_valid(src, &st.category) {
                st.category = default_category(src).to_string();
            }
        }
        save_state(&st)?;
    }
    println!("→ {}", next.path());
    match &next {
        Ctx::Root => {
            println!("  本层 list / status；cd account|local|server|resource|plugin")
        }
        Ctx::Account => println!("  本层 list / info；全局 login"),
        Ctx::Local(None) => {
            println!("  本层 list / info / worlds；子节点 cd <实例id> 或 mctl <实例id> list")
        }
        Ctx::Local(Some(_)) => println!("  本层 list / info [世界] / worlds"),
        Ctx::Server(None) => println!("  本层 list / info；子节点 cd <host:port>"),
        Ctx::Server(Some(_)) => println!("  本层 list / info（同一份详情）"),
        Ctx::Resource(None) => {
            println!("  本层 list 看下载源；cd netease | modrinth 选择下载源")
        }
        Ctx::Resource(Some(src)) => {
            println!(
                "  本层 list（20 条/页）/ info <序号> / download <序号>；翻页 next · prev · page <n>；分类 category"
            );
            println!(
                "  当前分类 {} · 第 {} 页（直接输入序号即可下载）",
                category_label(src, &load_state().category),
                st.page
            );
        }
        Ctx::Plugin => println!("  本层 list / info / install / update / remove"),
    }
    if next != Ctx::Root {
        println!("  ↑ 上级 {}", next.parent().path());
    }
    println!("  `mctl tree` 看完整命令树");
    Ok(())
}

/// 进入实例上下文前先验证并规范化为完整 id：
/// `cd 1.21` 能落到唯一前缀对应的实例，写错时立刻报候选，而不是等下一次 list 才炸。
fn normalize_ctx(ctx: Ctx) -> Result<Ctx> {
    match ctx {
        Ctx::Local(Some(id)) => {
            let (_, instances) = all_instances();
            Ok(Ctx::Local(Some(find_instance(&instances, &id)?.id)))
        }
        Ctx::Resource(Some(src)) => {
            let src = canonical_source(&src).ok_or_else(|| {
                anyhow!("未知下载源 `{src}`；可用：netease（网易资源）、modrinth（Modrinth）")
            })?;
            Ok(Ctx::Resource(Some(src.to_string())))
        }
        other => Ok(other),
    }
}

// ===== 根与账号 =====

fn root_list() -> Result<()> {
    let cred = storage::load().ok();
    let (mc_dir, instances) = all_instances();
    let plugin_root = storage::config_dir().join(plugin::PLUGIN_DIR);
    let plugins = plugin::scan_plugins(&plugin_root);

    println!("/ 根（一级上下文）");
    match &cred {
        Some(c) => println!("├─ account   账号        已登录 userId={}", c.user_id),
        None => println!("├─ account   账号        未登录（mctl login）"),
    }
    println!(
        "├─ local     本地实例    {} 个（{}）",
        instances.len(),
        mc_dir.display()
    );
    println!("├─ server    联机服务器  cd server 后 list（或 mctl server list）");
    println!("├─ resource  资源下载    cd resource 后 list；20 条/页，输入序号下载");
    println!("└─ plugin    插件        {} 个", plugins.len());
    println!();
    println!("cd <路径> 进入；`mctl tree` 看完整命令树；`mctl help` 看本层命令");
    Ok(())
}

fn account_show() -> Result<()> {
    let Ok(cred) = storage::load() else {
        println!("未登录。运行 `mctl login` 登录 4399 账号。");
        return Ok(());
    };
    println!("账号（已登录）");
    println!("  userId   {}", cred.user_id);
    println!("  aid      {}", cred.aid);
    println!("  sdkuid   {}", cred.sdkuid);
    // 凭据只报长度，不打印任何一截原文
    println!(
        "  token    {} 字符（已隐藏）",
        cred.user_token.chars().count()
    );
    match cred.sead.as_deref() {
        Some(s) if !s.is_empty() => {
            println!("  sead     {} 字符（已隐藏）", s.chars().count())
        }
        _ => println!("  sead     （无）"),
    }
    let role = storage::load_role();
    if !role.is_empty() {
        println!("  角色     {role}");
    }
    Ok(())
}

fn status_cmd() -> Result<()> {
    let cfg = storage::config_dir();
    let st = storage::load_settings(&cfg);
    let mc_dir = st
        .game_dir
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(|| cfg.join(".minecraft"));
    let (_, instances) = all_instances();
    let worlds: usize = instances
        .iter()
        .map(|i| crate::app::scan_worlds_dir(&launcher::saves_dir(i)).len())
        .sum();
    let plugins = plugin::scan_plugins(&cfg.join(plugin::PLUGIN_DIR));
    let session = storage::load_session(&cfg);

    println!("mctl {} 状态", env!("CARGO_PKG_VERSION"));
    println!("  配置目录   {}", cfg.display());
    println!("  游戏目录   {}", mc_dir.display());
    println!(
        "  实例       {} 个；本地地图 {} 个",
        instances.len(),
        worlds
    );
    println!("  插件       {} 个", plugins.len());
    println!("  角色       {}", {
        let r = storage::load_role();
        if r.is_empty() {
            "（未选择）".to_string()
        } else {
            r
        }
    });
    match storage::load() {
        Ok(c) => println!("  账号       userId={}（token 已隐藏）", c.user_id),
        Err(_) => println!("  账号       未登录（mctl login）"),
    }
    if let Some(inst) = instances.get(session.selected_instance) {
        println!("  TUI 选中   {}（/local/{}）", inst.id, inst.id);
    }
    println!("  当前位置   {}", load_state().cur.path());
    Ok(())
}

// ===== 本地实例 =====

fn all_instances() -> (PathBuf, Vec<Instance>) {
    let cfg = storage::config_dir();
    let st = storage::load_settings(&cfg);
    let mc_dir = st
        .game_dir
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(|| cfg.join(".minecraft"));
    let mut instances = launcher::scan_instances(&mc_dir);
    crate::app::App::append_bedrock_instances(&mut instances);
    (mc_dir, instances)
}

fn find_instance(instances: &[Instance], needle: &str) -> Result<Instance> {
    if let Some(i) = instances.iter().find(|i| i.id == needle) {
        return Ok(i.clone());
    }
    let matched: Vec<&Instance> = instances
        .iter()
        .filter(|i| i.id.starts_with(needle))
        .collect();
    match matched.as_slice() {
        [one] => Ok((*one).clone()),
        [] => bail!(
            "没有实例 `{needle}`；可用: {}",
            instances
                .iter()
                .map(|i| i.id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
        many => bail!(
            "实例 `{needle}` 不唯一，匹配到: {}",
            many.iter()
                .map(|i| i.id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn instance_worlds(inst: &Instance) -> Vec<crate::models::WorldInfo> {
    crate::app::scan_worlds_dir(&launcher::saves_dir(inst))
}

fn local_cmd(id: Option<&str>, cmd: &str, rest: &[String]) -> Result<()> {
    match cmd {
        "list" | "ls" => match id {
            Some(id) => {
                let (_, instances) = all_instances();
                let inst = find_instance(&instances, id)?;
                worlds_table(&inst)
            }
            None => instances_table(),
        },
        "info" => {
            if let Some(id) = id {
                let (_, instances) = all_instances();
                let inst = find_instance(&instances, id)?;
                match rest.first() {
                    Some(world) => world_info(&inst, world),
                    None => instance_info(&inst),
                }
            } else if let Some(id) = rest.first() {
                let (_, instances) = all_instances();
                let inst = find_instance(&instances, id)?;
                instance_info(&inst)
            } else {
                let (mc_dir, instances) = all_instances();
                println!("本地上下文");
                println!("  游戏目录   {}", mc_dir.display());
                println!(
                    "  实例       {} 个；地图 {} 个",
                    instances.len(),
                    instances
                        .iter()
                        .map(|i| instance_worlds(i).len())
                        .sum::<usize>()
                );
                println!("  提示       list 看实例；cd <实例id> 选中实例");
                Ok(())
            }
        }
        "worlds" => {
            let target = rest.first().map(String::as_str).or(id);
            let Some(target) = target else {
                bail!("用法: mctl local worlds <实例id>（或先 cd local/<实例id>）");
            };
            let (_, instances) = all_instances();
            let inst = find_instance(&instances, target)?;
            worlds_table(&inst)
        }
        other => unknown(other, &Ctx::Local(id.map(str::to_string))),
    }
}

fn instances_table() -> Result<()> {
    let (mc_dir, instances) = all_instances();
    if instances.is_empty() {
        println!("游戏目录下还没有实例: {}", mc_dir.display());
        println!("（在 TUI 里安装一个版本，或 `mctl install <版本id>`）");
        return Ok(());
    }
    let session = storage::load_session(&storage::config_dir());
    let selected = instances
        .get(session.selected_instance)
        .map(|i| i.id.clone());
    println!("本地实例（游戏目录: {}）", mc_dir.display());
    println!("  序号  实例                 类型      Java   地图  目录");
    for (i, inst) in instances.iter().enumerate() {
        let mark = if Some(&inst.id) == selected.as_ref() {
            "*"
        } else {
            " "
        };
        let java = if inst.bedrock {
            "基岩".to_string()
        } else {
            format!("{:>2}", inst.java_major)
        };
        println!(
            "  {mark}{:<4} {:<20} {:<8} {:>4}   {:>4}  {}",
            i + 1,
            inst.id,
            inst.version_type,
            java,
            instance_worlds(inst).len(),
            inst.dir.display()
        );
    }
    println!("（* = TUI 上次选中的实例；cd <实例id> 进入）");
    Ok(())
}

fn instance_info(inst: &Instance) -> Result<()> {
    println!("实例 {}", inst.id);
    println!("  类型       {}", inst.version_type);
    if inst.bedrock {
        println!("  运行时     基岩（插件 / Wine-GDK）");
    } else {
        println!("  Java       要求 >= {}", inst.java_major);
        println!("  主类       {}", inst.main_class);
        println!("  assets     {}", inst.assets_index);
    }
    println!("  版本目录   {}", inst.dir.display());
    println!("  存档目录   {}", launcher::saves_dir(inst).display());
    println!("  本地地图   {} 个", instance_worlds(inst).len());
    Ok(())
}

fn worlds_table(inst: &Instance) -> Result<()> {
    let worlds = instance_worlds(inst);
    if worlds.is_empty() {
        println!("实例 {} 还没有本地地图（saves 下暂无世界）", inst.id);
        println!("  目录: {}", launcher::saves_dir(inst).display());
        return Ok(());
    }
    println!(
        "实例 {} 的本地地图（{} 个，存档目录: {}）",
        inst.id,
        worlds.len(),
        launcher::saves_dir(inst).display()
    );
    println!("  序号  名称                     目录                         最近游玩      大小");
    for (i, w) in worlds.iter().enumerate() {
        println!(
            "  {:<4} {:<24} {:<28} {:<12} {}",
            i + 1,
            w.name,
            w.dir,
            w.last_played,
            w.size
        );
    }
    println!("（info <名称|目录> 看详情）");
    Ok(())
}

fn world_info(inst: &Instance, needle: &str) -> Result<()> {
    let worlds = instance_worlds(inst);
    let world = worlds
        .iter()
        .find(|w| w.dir == needle)
        .or_else(|| worlds.iter().find(|w| w.name == needle))
        .ok_or_else(|| anyhow!("实例 {} 下没有世界 `{needle}`（list 查看）", inst.id))?;
    let path = launcher::saves_dir(inst).join(&world.dir);
    println!("世界 {}", world.name);
    println!("  实例       {}", inst.id);
    println!("  目录       {}", world.dir);
    println!("  路径       {}", path.display());
    println!("  最近游玩   {}", world.last_played);
    println!("  大小       {}", world.size);
    println!(
        "  图标       {}",
        if path.join("icon.png").is_file() {
            "有"
        } else {
            "无"
        }
    );
    Ok(())
}

// ===== 联机服务器 =====

async fn server_cmd(id: Option<&str>, cmd: &str, rest: &[String]) -> Result<()> {
    match cmd {
        "list" | "ls" => match id {
            Some(id) => server_info_cmd(id).await,
            None => server_table(rest).await,
        },
        "info" | "detail" => {
            let target = rest.first().map(String::as_str).or(id);
            let Some(target) = target else {
                bail!("用法: mctl server info <名称|host:port>");
            };
            server_info_cmd(target).await
        }
        "add" => {
            let addr = rest
                .first()
                .ok_or_else(|| anyhow!("用法: mctl server add <host[:port]> [名称]"))?;
            let name = rest.get(1..).unwrap_or(&[]).join(" ");
            let s = api::mcserver::add_custom(addr, &name)?;
            println!("已加入自定义列表: {}（{}:{}）", s.name, s.host, s.port);
            Ok(())
        }
        "rm" | "remove" | "del" => {
            let target = rest.first().map(String::as_str).or(id);
            let Some(target) = target else {
                bail!("用法: mctl server rm <host[:port]>");
            };
            if api::mcserver::remove_custom(target)? {
                println!("已从自定义列表删除: {target}");
            } else {
                println!("自定义列表里没有 {target}（内置条目不可删除）");
            }
            Ok(())
        }
        other => unknown(other, &Ctx::Server(id.map(str::to_string))),
    }
}

async fn server_table(_rest: &[String]) -> Result<()> {
    println!("Java 国际服（内置精选 + 自定义，正在 ping…）");
    let list = api::mcserver::load_all().await;
    if list.is_empty() {
        println!("  （空）");
        return Ok(());
    }
    for s in &list {
        let status = match (&s.error, s.latency_ms) {
            (Some(_), _) => "离线 / 超时".to_string(),
            (None, Some(ms)) => format!("{} · {}ms", s.online_text(), ms),
            (None, None) => s.online_text(),
        };
        let tag = if s.builtin { "内置" } else { "自定义" };
        let version = if s.version.is_empty() {
            "-".to_string()
        } else {
            crate::text::truncate(&s.version, 24)
        };
        println!(
            "  {:<16} {:<28} {:<26} {:<10} {}",
            s.name,
            s.addr(),
            status,
            version,
            tag
        );
    }
    println!("（info <名称|host:port> 看详情；add/rm 管理自定义列表）");
    Ok(())
}

async fn server_info_cmd(query: &str) -> Result<()> {
    let s = api::mcserver::probe(query).await?;
    println!("服务器 {}", s.name);
    println!("  地址       {}", s.addr());
    println!("  来源       {}", if s.builtin { "内置精选" } else { "自定义" });
    match &s.error {
        Some(e) => {
            println!("  状态       离线 / 无法连接（{e}）");
        }
        None => {
            println!("  版本       {}（协议 {}）", s.version, s.protocol);
            println!("  在线       {}/{}", s.online, s.max);
            if let Some(ms) = s.latency_ms {
                println!("  延迟       {ms} ms");
            }
            if !s.motd.is_empty() {
                println!("  MOTD       {}", s.motd);
            }
        }
    }
    Ok(())
}

// ===== 插件 =====

fn plugin_cmd(cmd: &str, rest: &[String]) -> Result<()> {
    let root = storage::config_dir().join(plugin::PLUGIN_DIR);
    match cmd {
        "list" | "ls" => plugin_list(&root),
        "info" => {
            let name = rest
                .first()
                .ok_or_else(|| anyhow!("用法: mctl plugin info <名称>（plugin list 看名称）"))?;
            plugin_info(&root, name)
        }
        "install" => {
            let src = rest
                .first()
                .ok_or_else(|| anyhow!("用法: mctl plugin install <git-url[#tag]|路径>"))?;
            println!("安装 {src} → {} …", root.display());
            let info = plugin::install(&root, src)?;
            println!(
                "已安装 {} v{}：{}",
                info.name,
                info.version,
                info.bin
                    .as_ref()
                    .map(|b| b.display().to_string())
                    .unwrap_or_else(|| "（无二进制）".into())
            );
            Ok(())
        }
        "update" => match rest.first() {
            Some(name) => {
                let info = plugin::update(&root, name)?;
                println!("已更新 {name} → v{}", info.version);
                Ok(())
            }
            None => {
                let results = plugin::update_all(&root);
                if results.is_empty() {
                    println!("未安装任何插件（{}）", root.display());
                }
                for (name, res) in results {
                    match res {
                        Ok(i) => println!("ok   {name} → v{}", i.version),
                        Err(e) => println!("fail {name}: {e}"),
                    }
                }
                Ok(())
            }
        },
        "remove" => {
            let name = rest
                .first()
                .ok_or_else(|| anyhow!("用法: mctl plugin remove <名称>"))?;
            plugin::remove(&root, name)?;
            println!("已卸载 {name}");
            Ok(())
        }
        other => unknown(other, &Ctx::Plugin),
    }
}

fn plugin_list(root: &Path) -> Result<()> {
    let list = plugin::scan_plugins(root);
    if list.is_empty() {
        println!("未安装插件（{}）", root.display());
        return Ok(());
    }
    println!("已安装 {} 个插件:", list.len());
    for p in &list {
        let bin = p
            .bin
            .as_ref()
            .map(|b| b.display().to_string())
            .unwrap_or_else(|| "-".into());
        let origin = plugin::origin_url(&root.join(&p.name)).unwrap_or_else(|| "本地".into());
        println!(
            "  {} v{} | {} | {} | {}",
            p.name,
            p.version,
            if p.description.is_empty() {
                "（无描述）"
            } else {
                &p.description
            },
            bin,
            origin
        );
    }
    Ok(())
}

fn plugin_info(root: &Path, name: &str) -> Result<()> {
    let list = plugin::scan_plugins(root);
    let p = list
        .iter()
        .find(|p| p.name == name)
        .ok_or_else(|| anyhow!("未安装插件 `{name}`（plugin list 查看）"))?;
    let dir = root.join(&p.name);
    println!("插件 {}", p.name);
    println!("  版本       {}", p.version);
    println!(
        "  描述       {}",
        if p.description.is_empty() {
            "（无）"
        } else {
            &p.description
        }
    );
    println!(
        "  二进制     {}",
        p.bin
            .as_ref()
            .map(|b| b.display().to_string())
            .unwrap_or_else(|| "（无）".into())
    );
    println!(
        "  来源       {}",
        plugin::origin_url(&dir).unwrap_or_else(|| "本地".into())
    );
    println!("  目录       {}", dir.display());
    Ok(())
}

// ===== 全局命令（从旧 CLI 平铺搬过来，语义不变） =====

// ===== 资源下载 =====

/// 下载源。`netease` 需要登录；`modrinth` 不需要。
const RESOURCE_SOURCES: [(&str, &str, &str); 2] = [
    (
        "netease",
        "网易资源",
        "网易资源中心（地图/模组/光影/皮肤，需登录）",
    ),
    (
        "modrinth",
        "Modrinth",
        "Modrinth（整合包/模组/资源包/光影，免登录）",
    ),
];

fn canonical_source(s: &str) -> Option<&'static str> {
    match s.trim().to_ascii_lowercase().as_str() {
        "netease" | "网易" | "网易资源" | "wy" => Some("netease"),
        "modrinth" | "mr" => Some("modrinth"),
        _ => None,
    }
}

fn source_title(src: &str) -> &'static str {
    RESOURCE_SOURCES
        .iter()
        .find(|(id, _, _)| *id == src)
        .map(|(_, title, _)| *title)
        .unwrap_or("未知下载源")
}

fn netease_categories() -> Vec<(&'static str, &'static str)> {
    vec![
        ("maps-minigame", CATEGORY_MAPS_MINIGAME.name),
        ("maps-adventure", CATEGORY_MAPS_ADVENTURE.name),
        ("mods", CATEGORY_MODS.name),
        ("shaders", CATEGORY_SHADERS.name),
        ("skins", CATEGORY_SKINS.name),
    ]
}

fn modrinth_categories() -> Vec<(&'static str, &'static str)> {
    vec![
        ("modpack", "整合包"),
        ("mod", "模组"),
        ("resourcepack", "资源包"),
        ("shader", "光影"),
        ("datapack", "数据包"),
    ]
}

fn categories(src: &str) -> Vec<(&'static str, &'static str)> {
    match src {
        "netease" => netease_categories(),
        "modrinth" => modrinth_categories(),
        _ => Vec::new(),
    }
}

fn default_category(src: &str) -> &'static str {
    match src {
        "modrinth" => "modpack",
        _ => "maps-minigame",
    }
}

fn category_valid(src: &str, key: &str) -> bool {
    !key.is_empty() && categories(src).iter().any(|(k, _)| *k == key)
}

fn category_label(src: &str, key: &str) -> String {
    let key = if category_valid(src, key) {
        key
    } else {
        default_category(src)
    };
    categories(src)
        .into_iter()
        .find(|(k, _)| *k == key)
        .map(|(_, label)| label.to_string())
        .unwrap_or_else(|| key.to_string())
}

fn current_category(src: &str, st: &CliState) -> String {
    if category_valid(src, &st.category) {
        st.category.clone()
    } else {
        default_category(src).to_string()
    }
}

fn netease_category(key: &str) -> Option<crate::domain::Category> {
    match key {
        "maps-minigame" => Some(CATEGORY_MAPS_MINIGAME),
        "maps-adventure" => Some(CATEGORY_MAPS_ADVENTURE),
        "mods" => Some(CATEGORY_MODS),
        "shaders" => Some(CATEGORY_SHADERS),
        "skins" => Some(CATEGORY_SKINS),
        _ => None,
    }
}

#[derive(Debug, Clone)]
struct ResourceRow {
    id: String,
    name: String,
    author: String,
    downloads: i64,
    version: String,
    extra: String,
    summary: String,
}

fn resource_source_rows() -> Result<()> {
    println!("表 1  下载源");
    let widths = [4usize, 10, 46];
    println!(
        "{}",
        table_row(&["序号".into(), "源".into(), "说明".into()], &widths)
    );
    println!("{}", "-".repeat(widths.iter().sum::<usize>() + 4));
    for (i, (id, title, desc)) in RESOURCE_SOURCES.iter().enumerate() {
        println!(
            "{}",
            table_row(
                &[
                    (i + 1).to_string(),
                    format!("{id}"),
                    format!("{title}：{desc}"),
                ],
                &widths
            )
        );
    }
    println!();
    println!("进入下载源：`cd netease` / `cd modrinth`（或 `source netease`）");
    println!("进入后 `list` 固定 20 条/页；`next` `prev` `page <n>` 翻页；");
    println!("`info <序号>` 看详情；`download <序号>` 或直接输入序号下载。");
    Ok(())
}

fn resource_current_source(src: &str) -> Result<()> {
    let st = load_state();
    println!("当前下载源: {src}（{}）", source_title(src));
    println!(
        "分类: {} · 第 {} 页 · 每页 {} 条",
        category_label(src, &current_category(src, &st)),
        st.page.max(1),
        PAGE_SIZE
    );
    println!("切换源：`source <netease|modrinth>` 或 `cd /resource/<源>`");
    Ok(())
}

async fn resource_switch(src: &str) -> Result<()> {
    let id = canonical_source(src)
        .ok_or_else(|| anyhow!("未知下载源 `{src}`；可用：netease、modrinth"))?;
    let mut st = load_state();
    let next = Ctx::Resource(Some(id.to_string()));
    if st.cur != next {
        st.prev = st.cur.clone();
        st.cur = next;
        st.page = 1;
        if !category_valid(id, &st.category) {
            st.category = default_category(id).to_string();
        }
        save_state(&st)?;
    }
    println!("→ /resource/{id}");
    resource_list(id, 1, true).await
}

async fn resource_categories_cmd(src: &str, rest: &[String]) -> Result<()> {
    let list = categories(src);
    let st = load_state();
    let current = current_category(src, &st);
    if let Some(arg) = rest.first() {
        let key = if let Ok(n) = arg.parse::<usize>() {
            list.get(n.saturating_sub(1))
                .map(|(k, _)| *k)
                .ok_or_else(|| anyhow!("分类序号 {n} 超出范围（1-{}）", list.len()))?
        } else {
            list.iter()
                .find(|(k, label)| *k == arg.as_str() || *label == arg.as_str())
                .map(|(k, _)| *k)
                .ok_or_else(|| {
                    anyhow!(
                        "未知分类 `{arg}`；可用：{}",
                        list.iter()
                            .map(|(k, label)| format!("{k}/{label}"))
                            .collect::<Vec<_>>()
                            .join("、")
                    )
                })?
        };
        let mut st = load_state();
        st.category = key.to_string();
        st.page = 1;
        save_state(&st)?;
        println!("→ 分类 {}（第 1 页）", category_label(src, key));
        return resource_list(src, 1, false).await;
    }

    println!("表 1  分类（{}）", source_title(src));
    let widths = [4usize, 16, 18];
    println!(
        "{}",
        table_row(&["序号".into(), "分类".into(), "key".into()], &widths)
    );
    println!("{}", "-".repeat(widths.iter().sum::<usize>() + 4));
    for (i, (key, label)) in list.iter().enumerate() {
        let mark = if *key == current { " ← 当前" } else { "" };
        println!(
            "{}",
            table_row(
                &[
                    (i + 1).to_string(),
                    format!("{label}{mark}"),
                    (*key).to_string(),
                ],
                &widths
            )
        );
    }
    println!();
    println!(
        "当前：{} · 切换：`category <序号|key>`",
        category_label(src, &current)
    );
    Ok(())
}

async fn resource_page(src: &str, category: &str, page: usize) -> Result<Vec<ResourceRow>> {
    let page = page.max(1);
    let offset = ((page - 1) as i64) * PAGE_SIZE;
    match src {
        "netease" => {
            let cat = netease_category(category).unwrap_or(CATEGORY_MAPS_MINIGAME);
            let cred =
                storage::load().map_err(|_| anyhow!("网易资源需要登录：先运行 `mctl login`"))?;
            let values = api::list_category(&cat, offset, PAGE_SIZE, &cred, 0)
                .await
                .map_err(|e| {
                    anyhow!(
                        "网易资源列表失败：{e}\n提示：登录态可能过期，先运行 `mctl login` 重新登录"
                    )
                })?;
            let mut rows = Vec::new();
            for v in values {
                if let Ok(item) = serde_json::from_value::<crate::domain::ShopItem>(v) {
                    if item.entity_id.is_empty() {
                        continue;
                    }
                    rows.push(ResourceRow {
                        id: item.entity_id,
                        name: item.name,
                        author: item.developer_name,
                        downloads: item.download_num,
                        version: item.item_version,
                        extra: String::new(),
                        summary: item.brief_summary,
                    });
                }
            }
            Ok(rows)
        }
        "modrinth" => {
            let pt = if category_valid("modrinth", category) {
                category
            } else {
                default_category("modrinth")
            };
            let hits = launcher::modpack::search_modrinth(
                "",
                pt,
                "",
                PAGE_SIZE as usize,
                ModrinthIndex::Downloads,
                offset as usize,
            )
            .await?;
            Ok(hits
                .into_iter()
                .map(|hit: ModpackHit| ResourceRow {
                    id: hit.project_id,
                    name: hit.title,
                    author: String::new(),
                    downloads: hit.downloads,
                    version: String::new(),
                    extra: hit
                        .game_versions
                        .iter()
                        .take(4)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", "),
                    summary: hit.description,
                })
                .collect())
        }
        _ => bail!("未知下载源 `{src}`"),
    }
}

async fn resource_list(src: &str, page: usize, persist: bool) -> Result<()> {
    let page = page.max(1);
    let st = load_state();
    let category = current_category(src, &st);
    let rows = resource_page(src, &category, page).await?;
    if rows.is_empty() && page > 1 {
        bail!("第 {page} 页没有资源（已经到最后一页了）");
    }
    if persist {
        let mut st = load_state();
        st.page = page;
        st.category = category.clone();
        save_state(&st)?;
    }

    println!(
        "表 1  {} · {} · 第 {} 页（本页 {} 条）",
        source_title(src),
        category_label(src, &category),
        page,
        rows.len()
    );
    if rows.is_empty() {
        println!("（没有资源）");
        return Ok(());
    }

    if src == "netease" {
        let widths = [4usize, 34, 16, 9, 10];
        println!(
            "{}",
            table_row(
                &[
                    "序号".into(),
                    "名称".into(),
                    "作者".into(),
                    "下载".into(),
                    "版本".into(),
                ],
                &widths
            )
        );
        println!("{}", "-".repeat(widths.iter().sum::<usize>() + 8));
        for (i, row) in rows.iter().enumerate() {
            println!(
                "{}",
                table_row(
                    &[
                        (i + 1).to_string(),
                        row.name.clone(),
                        row.author.clone(),
                        fmt_count(row.downloads),
                        row.version.clone(),
                    ],
                    &widths
                )
            );
        }
    } else {
        let widths = [4usize, 40, 9, 26];
        println!(
            "{}",
            table_row(
                &[
                    "序号".into(),
                    "名称".into(),
                    "下载".into(),
                    "游戏版本".into(),
                ],
                &widths
            )
        );
        println!("{}", "-".repeat(widths.iter().sum::<usize>() + 6));
        for (i, row) in rows.iter().enumerate() {
            println!(
                "{}",
                table_row(
                    &[
                        (i + 1).to_string(),
                        row.name.clone(),
                        fmt_count(row.downloads),
                        row.extra.clone(),
                    ],
                    &widths
                )
            );
        }
    }

    println!();
    println!(
        "第 {page} 页 · {} 条 · 翻页：`next` 下一页 / `prev` 上一页 / `page <n>` 跳页",
        rows.len()
    );
    println!("详情：`info <序号>`；下载：`download <序号> [版本序号]`，或直接输入序号（如 `3`）。");
    Ok(())
}

async fn resource_info(src: &str, n: usize) -> Result<()> {
    let st = load_state();
    let page = st.page.max(1);
    let category = current_category(src, &st);
    let rows = resource_page(src, &category, page).await?;
    let row = rows
        .get(n.saturating_sub(1))
        .ok_or_else(|| anyhow!("序号 {n} 超出本页范围（1-{}）", rows.len()))?
        .clone();
    match src {
        "netease" => resource_info_netease(&row, n).await,
        "modrinth" => resource_info_modrinth(&row, n).await,
        _ => bail!("未知下载源 `{src}`"),
    }
}

async fn resource_info_netease(row: &ResourceRow, n: usize) -> Result<()> {
    let cred = storage::load().map_err(|_| anyhow!("网易资源需要登录：先运行 `mctl login`"))?;
    let detail = api::item_detail(&row.id, &cred).await.ok();
    let versions = api::download_info(&row.id, &cred).await.unwrap_or_default();

    println!("表 1  资源详情（网易资源）");
    println!(
        "名称      {}",
        detail
            .as_ref()
            .map(|d| d.name.as_str())
            .unwrap_or(&row.name)
    );
    println!(
        "作者      {}",
        detail
            .as_ref()
            .map(|d| d.developer_name.as_str())
            .unwrap_or(&row.author)
    );
    println!(
        "版本      {}",
        detail
            .as_ref()
            .map(|d| d.item_version.as_str())
            .unwrap_or(&row.version)
    );
    println!(
        "下载量    {}    点赞 {}",
        fmt_count(
            detail
                .as_ref()
                .map(|d| d.download_num)
                .unwrap_or(row.downloads)
        ),
        detail.as_ref().map(|d| d.like_num).unwrap_or(0)
    );
    println!("ID        {}", row.id);
    println!(
        "简介      {}",
        detail
            .as_ref()
            .map(|d| d.brief_summary.as_str())
            .unwrap_or(&row.summary)
    );
    if let Some(d) = &detail {
        let desc = plain_text(&d.detail_description, 500);
        if !desc.is_empty() {
            println!("描述      {desc}");
        }
    }

    println!();
    if versions.is_empty() {
        println!("没有可下载版本（可能需要购买，或登录态已过期）。");
    } else {
        println!("表 2  可下载版本（{} 个）", versions.len());
        let widths = [4usize, 12, 10, 40];
        println!(
            "{}",
            table_row(
                &[
                    "序号".into(),
                    "MC 版本".into(),
                    "大小".into(),
                    "文件名".into(),
                ],
                &widths
            )
        );
        println!("{}", "-".repeat(widths.iter().sum::<usize>() + 6));
        for (i, v) in versions.iter().enumerate() {
            println!(
                "{}",
                table_row(
                    &[
                        (i + 1).to_string(),
                        v.mc_version_name.clone(),
                        fmt_size(v.res_size),
                        v.res_name.clone(),
                    ],
                    &widths
                )
            );
        }
    }
    println!();
    println!("下载：`download {n} <版本序号>`（默认第 1 个），或直接输入 `{n}`。");
    Ok(())
}

async fn resource_info_modrinth(row: &ResourceRow, n: usize) -> Result<()> {
    let versions = launcher::modpack::project_versions(&row.id).await?;
    println!("表 1  资源详情（Modrinth）");
    println!("名称      {}", row.name);
    println!("下载量    {}", fmt_count(row.downloads));
    println!("游戏版本  {}", row.extra);
    println!("ID        {}", row.id);
    if !row.summary.is_empty() {
        println!("简介      {}", plain_text(&row.summary, 500));
    }

    println!();
    if versions.is_empty() {
        println!("没有可下载版本。");
    } else {
        println!("表 2  可下载版本（{} 个）", versions.len());
        let widths = [4usize, 18, 22, 14, 36];
        println!(
            "{}",
            table_row(
                &[
                    "序号".into(),
                    "版本".into(),
                    "游戏版本".into(),
                    "加载器".into(),
                    "文件".into(),
                ],
                &widths
            )
        );
        println!("{}", "-".repeat(widths.iter().sum::<usize>() + 8));
        for (i, v) in versions.iter().take(20).enumerate() {
            println!(
                "{}",
                table_row(
                    &[
                        (i + 1).to_string(),
                        v.version_number.clone(),
                        v.game_versions
                            .iter()
                            .take(3)
                            .cloned()
                            .collect::<Vec<_>>()
                            .join(", "),
                        v.loaders
                            .iter()
                            .take(2)
                            .cloned()
                            .collect::<Vec<_>>()
                            .join(", "),
                        v.files
                            .first()
                            .map(|f| f.filename.clone())
                            .unwrap_or_default(),
                    ],
                    &widths
                )
            );
        }
        if versions.len() > 20 {
            println!("… 共 {} 个版本，只显示前 20 个", versions.len());
        }
    }
    println!();
    println!("下载：`download {n} <版本序号>`（默认第 1 个），或直接输入 `{n}`。");
    Ok(())
}

async fn resource_download_row(src: &str, n: usize, version: Option<usize>) -> Result<()> {
    let st = load_state();
    let page = st.page.max(1);
    let category = current_category(src, &st);
    let rows = resource_page(src, &category, page).await?;
    let row = rows
        .get(n.saturating_sub(1))
        .ok_or_else(|| anyhow!("序号 {n} 超出本页范围（1-{}）", rows.len()))?
        .clone();
    match src {
        "netease" => download_netease(&row, n, version).await,
        "modrinth" => download_modrinth(&row, n, version).await,
        _ => bail!("未知下载源 `{src}`"),
    }
}

fn resource_download_dir() -> PathBuf {
    let cfg = storage::config_dir();
    let settings = storage::load_settings(&cfg);
    settings
        .download_dir
        .filter(|d| !d.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| cfg.join("downloads"))
}

async fn download_netease(row: &ResourceRow, n: usize, version: Option<usize>) -> Result<()> {
    let cred = storage::load().map_err(|_| anyhow!("网易资源需要登录：先运行 `mctl login`"))?;
    let versions = api::download_info(&row.id, &cred).await?;
    if versions.is_empty() {
        bail!("这个资源没有可下载版本（可能需要购买）");
    }
    let idx = version.unwrap_or(1);
    if idx == 0 || idx > versions.len() {
        bail!("版本序号 {idx} 超出范围（1-{}）", versions.len());
    }
    let sub = &versions[idx - 1];
    if sub.res_url.is_empty() {
        bail!("该版本没有下载地址");
    }
    let filename = if sub.res_name.trim().is_empty() {
        safe_file(&format!("{}-{}.7z", row.name, sub.mc_version_name))
    } else {
        safe_file(&sub.res_name)
    };
    let dir = resource_download_dir();
    std::fs::create_dir_all(&dir)?;
    let dest = dir.join(&filename);

    println!("表 1  下载详情（网易资源）");
    println!("资源      {}（序号 {n}）", row.name);
    println!("版本      {} / 第 {idx} 个", sub.mc_version_name);
    println!("大小      {}", fmt_size(sub.res_size));
    println!("目标      {}", dest.display());
    println!("下载中…");
    api::download_to_file(&sub.res_url, &dest.to_string_lossy()).await?;

    let size = std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);
    println!("完成      {} → {}", fmt_size(size as i64), dest.display());
    if !sub.res_md5.trim().is_empty() {
        let actual = file_md5(&dest)?;
        if actual.eq_ignore_ascii_case(sub.res_md5.trim()) {
            println!("校验      md5 ✓");
        } else {
            println!("校验      md5 ✗（文件已保留，可能不完整）");
        }
    }
    Ok(())
}

async fn download_modrinth(row: &ResourceRow, n: usize, version: Option<usize>) -> Result<()> {
    let versions = launcher::modpack::project_versions(&row.id).await?;
    if versions.is_empty() {
        bail!("这个项目没有可下载版本");
    }
    let idx = version.unwrap_or(1);
    if idx == 0 || idx > versions.len() {
        bail!("版本序号 {idx} 超出范围（1-{}）", versions.len());
    }
    let ver: &MrVersion = &versions[idx - 1];
    let file = ver.files.first().ok_or_else(|| anyhow!("该版本没有文件"))?;
    if file.url.is_empty() {
        bail!("该版本没有下载地址");
    }
    let raw_name = if file.filename.trim().is_empty() {
        format!("{}-{}.jar", row.name, ver.version_number)
    } else {
        file.filename.clone()
    };
    let filename = safe_file(&raw_name);
    let dir = resource_download_dir();
    std::fs::create_dir_all(&dir)?;
    let dest = dir.join(&filename);

    println!("表 1  下载详情（Modrinth）");
    println!("资源      {}（序号 {n}）", row.name);
    println!("版本      {}", ver.version_number);
    println!("目标      {}", dest.display());
    println!("下载中…");
    api::download_to_file(&file.url, &dest.to_string_lossy()).await?;
    let size = std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);
    println!("完成      {} → {}", fmt_size(size as i64), dest.display());
    Ok(())
}

async fn resource_cmd(src: Option<&str>, cmd: &str, rest: &[String]) -> Result<()> {
    let Some(src) = src else {
        return match cmd {
            "list" | "ls" | "info" | "source" | "sources" => resource_source_rows(),
            "download" | "get" => {
                bail!("先选择下载源：`cd netease` 或 `cd modrinth`（`list` 看全部）")
            }
            other => unknown(other, &Ctx::Resource(None)),
        };
    };

    match cmd {
        "list" | "ls" => {
            let page = match rest.first() {
                Some(s) => parse_page(s)?,
                None => load_state().page.max(1),
            };
            resource_list(src, page, true).await
        }
        "next" | "n" => {
            let st = load_state();
            resource_list(src, st.page.max(1).saturating_add(1), true).await
        }
        "prev" | "p" => {
            let st = load_state();
            resource_list(src, st.page.max(1).saturating_sub(1).max(1), true).await
        }
        "page" => match rest.first() {
            Some(s) => {
                let page = parse_page(s)?;
                resource_list(src, page, true).await
            }
            None => {
                let st = load_state();
                println!("当前第 {} 页（每页 {} 条）", st.page.max(1), PAGE_SIZE);
                println!("跳页：`page <n>`；翻页：`next` / `prev`");
                Ok(())
            }
        },
        "category" | "cat" => resource_categories_cmd(src, rest).await,
        "source" | "sources" => match rest.first() {
            Some(id) => resource_switch(id).await,
            None => resource_current_source(src),
        },
        "info" => {
            let n = parse_row(rest, "用法: info <序号>")?;
            resource_info(src, n).await
        }
        "download" | "get" => {
            let n = parse_row(rest, "用法: download <序号> [版本序号]")?;
            let ver = parse_version(rest)?;
            resource_download_row(src, n, ver).await
        }
        other => unknown(other, &Ctx::Resource(Some(src.to_string()))),
    }
}

fn parse_page(s: &str) -> Result<usize> {
    let n: usize = s.parse().map_err(|_| anyhow!("页码必须是数字：{s}"))?;
    if n == 0 {
        bail!("页码从 1 开始");
    }
    Ok(n)
}

fn parse_row(rest: &[String], usage: &str) -> Result<usize> {
    let s = rest.first().ok_or_else(|| anyhow!("{usage}"))?;
    let n: usize = s.parse().map_err(|_| anyhow!("序号必须是数字：{s}"))?;
    if n == 0 {
        bail!("序号从 1 开始");
    }
    Ok(n)
}

fn parse_version(rest: &[String]) -> Result<Option<usize>> {
    match rest.get(1) {
        None => Ok(None),
        Some(s) => {
            let n: usize = s.parse().map_err(|_| anyhow!("版本序号必须是数字：{s}"))?;
            if n == 0 {
                bail!("版本序号从 1 开始");
            }
            Ok(Some(n))
        }
    }
}

fn table_row(cells: &[String], widths: &[usize]) -> String {
    cells
        .iter()
        .zip(widths.iter())
        .map(|(cell, width)| fit_width(cell, *width))
        .collect::<Vec<_>>()
        .join("  ")
}

fn char_width(ch: char) -> usize {
    match ch as u32 {
        0x1100..=0x115F
        | 0x2E80..=0xA4CF
        | 0xAC00..=0xD7A3
        | 0xF900..=0xFAFF
        | 0xFE30..=0xFE4F
        | 0xFF00..=0xFF60
        | 0xFFE0..=0xFFE6 => 2,
        _ => 1,
    }
}

fn fit_width(s: &str, width: usize) -> String {
    let mut out = String::new();
    let mut used = 0;
    for ch in s.chars() {
        let w = char_width(ch);
        if used + w > width {
            if used < width {
                out.push('…');
                used += 1;
            }
            break;
        }
        out.push(ch);
        used += w;
    }
    while used < width {
        out.push(' ');
        used += 1;
    }
    out
}

fn fmt_count(n: i64) -> String {
    if n >= 100_000_000 {
        format!("{:.1}亿", n as f64 / 100_000_000.0)
    } else if n >= 10_000 {
        format!("{:.1}万", n as f64 / 10_000.0)
    } else {
        n.to_string()
    }
}

fn fmt_size(bytes: i64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let b = bytes.max(0) as f64;
    if b >= GB {
        format!("{:.1} GB", b / GB)
    } else if b >= MB {
        format!("{:.1} MB", b / MB)
    } else if b >= KB {
        format!("{:.1} KB", b / KB)
    } else {
        format!("{bytes} B")
    }
}

fn safe_file(name: &str) -> String {
    let mut out: String = name
        .chars()
        .map(|c| {
            if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
                '_'
            } else {
                c
            }
        })
        .collect();
    out = out.trim().trim_matches('.').to_string();
    if out.is_empty() {
        "resource.bin".into()
    } else {
        out
    }
}

fn file_md5(path: &Path) -> Result<String> {
    use md5::{Digest, Md5};
    use std::io::Read as _;

    let mut file = std::fs::File::open(path)?;
    let mut hasher = Md5::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn plain_text(s: &str, max: usize) -> String {
    let mut stripped = String::new();
    let mut in_tag = false;
    for ch in s.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => stripped.push(ch),
            _ => {}
        }
    }
    let text = stripped
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let mut out = String::new();
    for (i, ch) in text.chars().enumerate() {
        if i >= max {
            out.push('…');
            break;
        }
        out.push(ch);
    }
    out
}

// ===== 交互式 CLI =====

fn prompt_user() -> String {
    let role = storage::load_role().trim().to_string();
    let name = if role.is_empty() {
        std::env::var("USER")
            .or_else(|_| std::env::var("LOGNAME"))
            .unwrap_or_else(|_| "local".into())
    } else {
        role
    };
    name.chars()
        .map(|c| if c.is_control() { '_' } else { c })
        .collect()
}

/// 支持单/双引号和反斜杠转义；世界名带空格也能传。
fn split_command(line: &str) -> Result<Vec<String>> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut escaped = false;
    let mut has_token = false;

    for ch in line.chars() {
        if escaped {
            cur.push(ch);
            escaped = false;
            has_token = true;
            continue;
        }
        match quote {
            Some(q) => {
                if ch == q {
                    quote = None;
                } else if ch == '\\' && q == '"' {
                    escaped = true;
                } else {
                    cur.push(ch);
                    has_token = true;
                }
            }
            None => {
                if ch == '\'' || ch == '"' {
                    quote = Some(ch);
                    has_token = true;
                } else if ch == '\\' {
                    escaped = true;
                } else if ch.is_whitespace() {
                    if has_token {
                        out.push(std::mem::take(&mut cur));
                        has_token = false;
                    }
                } else {
                    cur.push(ch);
                    has_token = true;
                }
            }
        }
    }
    if quote.is_some() {
        bail!("引号没有闭合");
    }
    if escaped {
        cur.push('\\');
    }
    if has_token {
        out.push(cur);
    }
    Ok(out)
}

/// `mctl cli`：交互式界面，prompt 形如 `mctl@用户名:/resource/netease>`。
/// 一直读到 `exit` / `quit` / `:q` 或 EOF。
pub async fn shell() -> Result<()> {
    let tty = io::stdin().is_terminal();
    let user = prompt_user();
    if tty {
        println!("mctl 交互式 CLI · 输入 exit 退出 · help 查看命令");
        println!("用户: {user} · 当前位置: {}", load_state().cur.path());
        println!("资源下载：`cd resource` → `list` → 直接输入序号下载；`next` / `prev` 翻页");
    }

    loop {
        let ctx = load_state().cur;
        if tty {
            print!("mctl@{user}:{}> ", ctx.path());
            io::stdout().flush()?;
        }
        let mut line = String::new();
        if io::stdin().lock().read_line(&mut line)? == 0 {
            break;
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if matches!(line, "exit" | "quit" | ":q" | "logout") {
            break;
        }
        let parts = match split_command(line) {
            Ok(parts) => parts,
            Err(e) => {
                eprintln!("Error: {e}");
                continue;
            }
        };
        if parts.is_empty() {
            continue;
        }
        let mut argv = Vec::with_capacity(parts.len() + 1);
        argv.push("mctl".to_string());
        argv.extend(parts);
        if let Err(e) = run(&argv).await {
            eprintln!("Error: {e:#}");
        }
    }
    if tty {
        println!("bye");
    }
    Ok(())
}

async fn login_cmd(rest: &[String]) -> Result<()> {
    // 4399 的 OAuth 登录强制要验证码：先取图 → 用户识别 → 再带 captcha_id 提交。
    // 少了 captcha_id 会被回 202「请稍后再试~」（对「没带」和「填错」故意同响应）。
    let ask = |prompt: &str| -> Result<String> {
        print!("{prompt}");
        io::stdout().flush()?;
        let mut s = String::new();
        io::stdin().lock().read_line(&mut s)?;
        Ok(s.trim().to_string())
    };

    // 账号/密码都可以从命令行给，但不给更好：密码走命令行会留在 shell history 里
    let username = match rest.first() {
        Some(u) => u.clone(),
        None => ask("4399 账号: ")?,
    };
    let password = match rest.get(1) {
        Some(p) => p.clone(),
        None => {
            print!("密码（不回显）: ");
            io::stdout().flush()?;
            rpassword::read_password()?
        }
    };
    if username.is_empty() || password.is_empty() {
        bail!("账号或密码为空");
    }

    let ch = api::auth::begin_4399(&username, &password).await?;
    if ch.opened {
        println!(
            "\n验证码图片已用图片管理器打开: {}",
            ch.captcha_path.display()
        );
    } else {
        println!(
            "\n未能自动打开图片管理器，请手动打开: {}",
            ch.captcha_path.display()
        );
    }
    println!("把图里的字符输入下面（认错就重新登录换一张）。");
    let captcha = ask("验证码: ")?;
    let cred = api::auth::finish_4399(&ch, &captcha).await?;
    storage::save(&cred)?;
    // 不再打印 userToken——它会留在终端回滚缓冲里，凭据已存盘
    println!("登录成功 userId={}", cred.user_id);
    Ok(())
}

async fn install_cmd(rest: &[String]) -> Result<()> {
    // 实测安装流程（含并发下载）: mctl install <版本id> [mc_dir]
    let id = rest.first().ok_or_else(|| anyhow!("缺少版本 id"))?;
    let mc_dir = rest
        .get(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| storage::config_dir().join(".minecraft"));
    let start = std::time::Instant::now();
    launcher::meta::install_official(
        &mc_dir,
        id,
        |phase, done, total| {
            eprintln!("[{phase}] {done}/{total}");
        },
        &|f| {
            if f.finished_at.is_some() {
                let mark = if f.failed { "✗" } else { "✓" };
                eprintln!("  {mark} {}", f.name);
            }
        },
    )
    .await?;
    println!("完成，耗时 {:.1}s", start.elapsed().as_secs_f64());
    Ok(())
}

async fn bedrock_cmd(rest: &[String]) -> Result<()> {
    let release = rest.first().map(|s| s != "pre").unwrap_or(true);
    crate::deps::ensure_pacman_interactive(&["umu-launcher"])?;
    println!(
        "Bedrock({}) 选版本/下载中…",
        if release { "rel" } else { "pre" }
    );
    bedrock_launcher_core::install_and_launch(release, None).await?;
    Ok(())
}

fn token_cmd(rest: &[String]) -> Result<()> {
    let uid = rest
        .first()
        .ok_or_else(|| anyhow!("用法: mctl token <uid> <userToken> [path] [body]"))?;
    let ut = rest.get(1).ok_or_else(|| anyhow!("缺少 userToken"))?;
    let path = rest
        .get(2)
        .map(String::as_str)
        .unwrap_or("/item/query/available");
    let body = rest
        .get(3)
        .map(String::as_str)
        .unwrap_or(r#"{"channel_id":11}"#);
    println!("{}", crate::token::compute(path, body, uid, ut).1);
    Ok(())
}

async fn dl_cmd(rest: &[String]) -> Result<()> {
    let id = rest.first().ok_or_else(|| anyhow!("缺少 id"))?;
    let out = rest.get(1).map(String::as_str).unwrap_or("res.7z");
    let cred = storage::load()?;
    let subs = api::download_info(id, &cred).await?;
    let sub = subs.first().ok_or_else(|| anyhow!("无下载版本"))?;
    println!("下载 {} ({} KB) → {out}", sub.res_name, sub.res_size / 1024);
    api::download_to_file(&sub.res_url, out).await?;
    println!("完成");
    Ok(())
}

// ===== 帮助 =====

fn help(ctx: &Ctx) -> Result<()> {
    // 根节点的帮助 = 完整命令树，层级一眼可见。
    if matches!(ctx, Ctx::Root) {
        return print_tree(ctx);
    }

    println!("当前位置: {}", ctx.path());
    println!();
    match ctx {
        Ctx::Root => unreachable!(),
        Ctx::Account => {
            println!("/account  账号");
            println!("├─ list | info      账号信息（token/sead 只报长度）");
        }
        Ctx::Local(None) => {
            println!("/local  本地实例");
            println!("├─ list             实例列表（地图数 + TUI 选中标记）");
            println!("├─ info <实例>      实例详情");
            println!("├─ worlds <实例>    该实例的本地地图");
            println!("├─ <实例>           子节点：cd <实例>，或 mctl <实例> list");
            let (_, instances) = all_instances();
            if !instances.is_empty() {
                let names = instances
                    .iter()
                    .take(6)
                    .map(|i| i.id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                let more = if instances.len() > 6 { " …" } else { "" };
                println!("│  └─ 当前: {names}{more}");
            }
        }
        Ctx::Local(Some(id)) => {
            println!("/local/{id}  实例");
            println!("├─ list             该实例的本地地图");
            println!("├─ info [世界]      实例详情 / 单个世界详情");
            println!("└─ worlds           同 list");
        }
        Ctx::Server(None) => {
            println!("/server  Java 国际服");
            println!("├─ list                         内置 + 自定义服务器（并发 ping）");
            println!("├─ info <名称|host:port>        单个服务器状态");
            println!("├─ add <host[:port]> [名称]     加入自定义列表");
            println!("├─ rm <host[:port]>             从自定义列表删除");
            println!("└─ <host:port>                  子节点：cd 后 list 同 info");
        }
        Ctx::Server(Some(id)) => {
            println!("/server/{id}  服务器");
            println!("├─ list             服务器状态");
            println!("└─ info             同 list");
        }
        Ctx::Resource(None) => {
            println!("/resource  资源下载");
            println!("├─ list | sources   下载源（netease / modrinth）");
            println!("├─ source <源>      选择下载源（也可 cd <源>）");
            println!("└─ <源>             子节点：cd netease / cd modrinth");
        }
        Ctx::Resource(Some(src)) => {
            println!("/resource/{src}  资源下载（{}）", source_title(src));
            println!("├─ list [页码]      固定 20 条/页");
            println!("├─ next | prev      下一页 / 上一页");
            println!("├─ page <n>         跳到第 n 页");
            println!(
                "├─ category [n|key] 分类（默认 {}）",
                category_label(src, default_category(src))
            );
            println!("├─ info <序号>      资源详情 + 版本表");
            println!("├─ download <序号> [版本号]");
            println!("└─ <数字>           直接下载该序号（如 `3`）");
        }
        Ctx::Plugin => {
            println!("/plugin  插件");
            println!("├─ list             已安装插件");
            println!("├─ info <名称>      插件详情");
            println!("├─ install <url|路径>");
            println!("├─ update [名称]    省略名称 = 全部");
            println!("└─ remove <名称>");
        }
    }
    println!("├─ cd ..            → {}", ctx.parent().path());
    println!("├─ cd /             → /");
    println!("├─ tree             完整命令树");
    println!("└─ help             本层帮助");
    println!();
    println!("全局命令: status version login install bedrock dl token cli");
    println!("一次性执行: `mctl <路径> <命令>`（不改当前位置）");
    Ok(())
}

/// 完整命令树。`▶` 标出当前位置；路径本身是可执行前缀，
/// 所以树里的每个节点都能直接写成 `mctl <路径> <命令>`。
fn print_tree(cur: &Ctx) -> Result<()> {
    let mark = |node: &Ctx| if cur == node { "▶" } else { "" };
    println!("mctl 命令树（▶ = 当前位置；cd <路径> 进入，mctl <路径> <命令> 一步执行）");
    println!();
    println!(
        "{} /                          根：list 上下文索引 / status 本机概览",
        if matches!(cur, Ctx::Root) { "▶" } else { " " }
    );
    println!("├─{} account                 账号", mark(&Ctx::Account));
    println!("│  └─ list | info             账号信息（token/sead 只报长度）");
    println!(
        "├─{} local                   本地实例",
        mark(&Ctx::Local(None))
    );
    println!("│  ├─ list                    实例列表（地图数 + TUI 选中标记）");
    println!("│  ├─ info <实例>             实例详情");
    println!("│  ├─ worlds <实例>           该实例的本地地图");
    match cur {
        Ctx::Local(Some(id)) => {
            println!("│  └─▶ {id}");
        }
        _ => println!("│  └─ <实例>                  子节点：cd <实例> 或 mctl <实例> list"),
    }
    println!("│     ├─ list                 该实例的本地地图");
    println!("│     ├─ info [世界]          实例详情 / 单个世界详情");
    println!("│     └─ worlds               同 list");
    println!(
        "├─{} server                  Java 国际服",
        mark(&Ctx::Server(None))
    );
    println!("│  ├─ list                    列表 + 状态 ping");
    println!("│  ├─ info <名称|host:port>   单个服务器");
    println!("│  ├─ add <host[:port]> [名称]");
    println!("│  ├─ rm <host[:port]>");
    match cur {
        Ctx::Server(Some(id)) => println!("│  └─▶ {id}"),
        _ => println!("│  └─ <host:port>             子节点：list 同 info"),
    }
    println!(
        "├─{} resource                资源下载",
        mark(&Ctx::Resource(None))
    );
    println!("│  ├─ list                    下载源列表");
    println!("│  ├─ source <netease|modrinth>");
    match cur {
        Ctx::Resource(Some(id)) => println!("│  └─▶ {id}"),
        _ => println!("│  └─ <源>                    子节点：cd netease / cd modrinth"),
    }
    println!("│     ├─ list [页码]          固定 20 条/页");
    println!("│     ├─ next | prev          下一页 / 上一页");
    println!("│     ├─ page <n>             跳页");
    println!("│     ├─ category [n|key]     分类（地图·小游戏 / 模组 / 光影 …）");
    println!("│     ├─ info <序号>          详情 + 版本表");
    println!("│     └─ download <序号> [版本]  或直接输入序号");
    println!("└─{} plugin                  插件", mark(&Ctx::Plugin));
    println!("   ├─ list                    已安装插件");
    println!("   ├─ info <名称>             插件详情");
    println!("   ├─ install <url|路径>      插件安装（/plugin 下优先于全局 install）");
    println!("   ├─ update [名称]           省略名称 = 全部");
    println!("   └─ remove <名称>");
    println!();
    println!("全局命令（任何层可用）:");
    println!("  cd pwd help tree status version login install bedrock dl token cli");
    println!("一步到位: mctl local/1.21.1 list · mctl server/mc.hypixel.net info");
    println!("交互式: mctl cli，prompt 形如 mctl@用户名:/resource/netease>，exit 退出");
    println!("无参数运行 mctl 进入 TUI。");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{resolve_cd, Ctx};

    #[test]
    fn parse_context_paths() {
        assert_eq!(Ctx::parse("/").unwrap(), Ctx::Root);
        assert_eq!(Ctx::parse("~").unwrap(), Ctx::Root);
        assert_eq!(Ctx::parse("local").unwrap(), Ctx::Local(None));
        assert_eq!(
            Ctx::parse("/local/1.20").unwrap(),
            Ctx::Local(Some("1.20".into()))
        );
        assert_eq!(
            Ctx::parse("server/abc/").unwrap(),
            Ctx::Server(Some("abc".into()))
        );
        assert!(Ctx::parse("local/a/b").is_err());
        assert!(Ctx::parse("nope").is_err());
    }

    #[test]
    fn cd_parents_and_previous() {
        let cur = Ctx::Local(Some("1.20".into()));
        assert_eq!(
            resolve_cd(&cur, &Ctx::Root, "..").unwrap(),
            Ctx::Local(None)
        );
        assert_eq!(
            resolve_cd(&Ctx::Local(None), &Ctx::Root, "..").unwrap(),
            Ctx::Root
        );
        assert_eq!(resolve_cd(&Ctx::Root, &Ctx::Root, "..").unwrap(), Ctx::Root);
        assert_eq!(
            resolve_cd(&Ctx::Root, &Ctx::Local(None), "-").unwrap(),
            Ctx::Local(None)
        );
    }

    #[test]
    fn cd_relative_child_within_context() {
        assert_eq!(
            resolve_cd(&Ctx::Server(None), &Ctx::Root, "12345").unwrap(),
            Ctx::Server(Some("12345".into()))
        );
        assert_eq!(
            resolve_cd(&Ctx::Local(Some("a".into())), &Ctx::Root, "b").unwrap(),
            Ctx::Local(Some("b".into()))
        );
        // 顶层上下文名始终按绝对路径：在 local 里 `cd server` 也去 /server
        assert_eq!(
            resolve_cd(&Ctx::Local(Some("a".into())), &Ctx::Root, "server").unwrap(),
            Ctx::Server(None)
        );
    }

    #[test]
    fn cd_rejects_children_of_leaf_contexts() {
        assert!(resolve_cd(&Ctx::Account, &Ctx::Root, "x").is_err());
        assert!(resolve_cd(&Ctx::Plugin, &Ctx::Root, "x").is_err());
    }

    #[test]
    fn context_path_roundtrip() {
        for ctx in [
            Ctx::Root,
            Ctx::Account,
            Ctx::Local(None),
            Ctx::Local(Some("1.20".into())),
            Ctx::Server(Some("abc".into())),
            Ctx::Resource(None),
            Ctx::Resource(Some("netease".into())),
            Ctx::Plugin,
        ] {
            assert_eq!(Ctx::parse(&ctx.path()).unwrap(), ctx);
        }
    }

    #[test]
    fn resource_context_and_sources() {
        use super::{canonical_source, category_valid, default_category, is_context_prefix};

        assert_eq!(Ctx::parse("resource").unwrap(), Ctx::Resource(None));
        assert_eq!(
            Ctx::parse("资源下载/网易").unwrap(),
            Ctx::Resource(Some("网易".into()))
        );
        assert_eq!(canonical_source("网易"), Some("netease"));
        assert_eq!(canonical_source("MR"), Some("modrinth"));
        assert_eq!(canonical_source("unknown"), None);
        assert_eq!(default_category("netease"), "maps-minigame");
        assert_eq!(default_category("modrinth"), "modpack");
        assert!(category_valid("netease", "maps-minigame"));
        assert!(!category_valid("netease", "modpack"));
        assert!(is_context_prefix("resource/netease"));
        // `download` 只做 cd 别名，不能当顶层前缀，否则 `mctl download 3` 会歧义
        assert!(!is_context_prefix("download"));
    }

    #[test]
    fn cd_download_alias_and_absolute_paths() {
        assert_eq!(
            resolve_cd(&Ctx::Root, &Ctx::Root, "download").unwrap(),
            Ctx::Resource(None)
        );
        assert_eq!(
            resolve_cd(&Ctx::Root, &Ctx::Root, "download/netease").unwrap(),
            Ctx::Resource(Some("netease".into()))
        );
        assert_eq!(
            resolve_cd(&Ctx::Root, &Ctx::Root, "/resource/modrinth").unwrap(),
            Ctx::Resource(Some("modrinth".into()))
        );
        assert_eq!(
            resolve_cd(
                &Ctx::Resource(Some("netease".into())),
                &Ctx::Root,
                "modrinth"
            )
            .unwrap(),
            Ctx::Resource(Some("modrinth".into()))
        );
        assert_eq!(
            resolve_cd(&Ctx::Root, &Ctx::Root, "/local/1.20").unwrap(),
            Ctx::Local(Some("1.20".into()))
        );
    }

    #[test]
    fn split_command_handles_quotes() {
        use super::split_command;

        assert_eq!(
            split_command("info 我的 世界").unwrap(),
            vec!["info", "我的", "世界"]
        );
        assert_eq!(
            split_command("info \"我的 世界\"").unwrap(),
            vec!["info", "我的 世界"]
        );
        assert_eq!(split_command("cd 'a b'").unwrap(), vec!["cd", "a b"]);
        assert_eq!(split_command(r#"info a\ b"#).unwrap(), vec!["info", "a b"]);
        assert!(split_command("info \"没闭合").is_err());
    }

    #[test]
    fn table_width_truncates_wide_chars() {
        use super::{char_width, fit_width};

        assert_eq!(char_width('a'), 1);
        assert_eq!(char_width('中'), 2);
        assert_eq!(fit_width("abc", 5), "abc  ");
        assert_eq!(fit_width("中文中文", 4), "中文");
        assert_eq!(fit_width("中文中文", 5), "中文…");
    }

    #[test]
    fn hierarchy_prefixes_and_verbs() {
        use super::{can_descend, ctx_verbs, is_context_prefix, is_global_verb};

        // `/` 是根的一次性前缀：在 /plugin 下用 `mctl / install` 调全局安装
        assert!(is_context_prefix("/"));
        assert!(is_context_prefix("~"));
        assert!(is_context_prefix("local/1.21.1"));
        assert!(!is_context_prefix("list"));

        // 只有类别根能继续往下走；选中子节点后就不再解析子路径
        assert!(can_descend(&Ctx::Local(None)));
        assert!(can_descend(&Ctx::Server(None)));
        assert!(can_descend(&Ctx::Resource(None)));
        assert!(!can_descend(&Ctx::Local(Some("1.21.1".into()))));
        assert!(!can_descend(&Ctx::Resource(Some("netease".into()))));
        assert!(!can_descend(&Ctx::Root));

        // 层级动词与全局动词分开：child id 不能叫 list/tree，否则会被当命令
        assert!(ctx_verbs(&Ctx::Local(None)).contains(&"worlds"));
        assert!(ctx_verbs(&Ctx::Plugin).contains(&"install"));
        assert!(ctx_verbs(&Ctx::Resource(Some("netease".into()))).contains(&"download"));
        assert!(ctx_verbs(&Ctx::Resource(Some("netease".into()))).contains(&"next"));
        assert!(is_global_verb("tree"));
        assert!(is_global_verb("cli"));
        assert!(!is_global_verb("worlds"));
    }
}
