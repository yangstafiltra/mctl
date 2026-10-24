//! 语义色板：所有界面颜色都从这里取。
//!
//! 具体颜色由「主题」决定——设置页可切换（`opaline` 的 39 个内置主题 +
//! mctl 经典配色）。主题 id 存在 `settings.json` 的 `theme` 字段里，
//! 运行时只有一份全局 [`Palette`]，换主题 → 整体替换 → 下一帧全界面变色。
//!
//! **默认不铺背景**：底色一律留给终端自己（用户的终端底色/透明效果优先），
//! 所以正文/普通文字都不带底色。**选中项例外**：列表整行、标签 chip、
//! 来源条等"选中的字"一律用 [`selected_block`]——底色块跟随主题
//! （经典主题 = 原来的蓝底黑字），需要过渡的地方还会按进度"刷"过去
//! （侧栏视图切换、资源中心来源条）。
//!
//! 历史：换主题功能之前是一堆 `pub const`，颜色写死。选主题时为了不改
//! 上百处调用点，const 全部改成了同名函数（`theme::FG` → `theme::fg`）。

use ratatui::style::{Color, Modifier, Style};
use std::sync::{OnceLock, RwLock};

/// mctl 经典配色对应的主题 id（默认主题：青色强调）
pub const LEGACY_ID: &str = "mctl";

/// 一个主题展开成的完整颜色集合（语义化，界面只认这些名字）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    pub fg: Color,
    pub fg_muted: Color,
    pub accent: Color,
    pub primary: Color,
    pub ok: Color,
    pub warn: Color,
    pub error: Color,
    pub border: Color,
    pub border_sel: Color,
    /// 选中块的底色（列表整行高亮用；只给选中项用，不是全屏背景）
    pub selection_bg: Color,
    /// 选中块上的字色
    pub selection_fg: Color,
    /// 世界选中框流光头部（比 accent 亮一档）
    pub head: Color,
}

impl Palette {
    /// mctl 经典配色：换主题之前的样子（青 + 灰；选中 = 蓝底黑字）
    pub const fn legacy() -> Self {
        Self {
            fg: Color::Gray,
            fg_muted: Color::DarkGray,
            accent: Color::Cyan,
            primary: Color::Yellow,
            ok: Color::Green,
            warn: Color::Yellow,
            error: Color::Red,
            border: Color::DarkGray,
            border_sel: Color::Cyan,
            selection_bg: Color::Blue,
            selection_fg: Color::Black,
            head: Color::LightCyan,
        }
    }
}

/// 可选主题 id 列表：第一项 mctl 经典，其余为 opaline 内置主题（按 id 排序）。
/// 首次调用会解析 39 个内置主题，之后走缓存。
pub fn theme_ids() -> &'static [String] {
    static IDS: OnceLock<Vec<String>> = OnceLock::new();
    IDS.get_or_init(|| {
        let mut ids = vec![LEGACY_ID.to_string()];
        let mut names: Vec<String> = opaline::list_available_themes()
            .into_iter()
            .map(|t| t.name)
            .collect();
        names.sort();
        ids.extend(names);
        ids
    })
}

/// 主题显示名（列表里给人看的名字）
pub fn theme_label(id: &str) -> String {
    if id == LEGACY_ID {
        return "mctl 经典".into();
    }
    static LABELS: OnceLock<std::collections::HashMap<String, String>> = OnceLock::new();
    let labels = LABELS.get_or_init(|| {
        opaline::list_available_themes()
            .into_iter()
            .map(|t| (t.name, t.display_name))
            .collect()
    });
    labels.get(id).cloned().unwrap_or_else(|| id.to_string())
}

/// 按 id 取色板；mctl 经典 / 空 / 未知 id 一律回经典配色（配置坏了也能开）
pub fn palette_of(id: &str) -> Palette {
    if id.is_empty() || id == LEGACY_ID {
        return Palette::legacy();
    }
    match opaline::load_by_name(id) {
        Some(t) => from_opaline(&t),
        None => Palette::legacy(),
    }
}

/// opaline 主题 → mctl 语义色板
fn from_opaline(t: &opaline::Theme) -> Palette {
    use opaline::names::tokens as tk;
    let c = |token: &str| {
        let c = t.color(token);
        Color::Rgb(c.r, c.g, c.b)
    };
    let head = t.color(tk::ACCENT_PRIMARY).lighten(0.35);
    Palette {
        fg: c(tk::TEXT_PRIMARY),
        fg_muted: c(tk::TEXT_MUTED),
        accent: c(tk::ACCENT_PRIMARY),
        primary: c(tk::ACCENT_SECONDARY),
        ok: c(tk::SUCCESS),
        warn: c(tk::WARNING),
        error: c(tk::ERROR),
        border: c(tk::BORDER_UNFOCUSED),
        border_sel: c(tk::BORDER_FOCUSED),
        selection_bg: c(tk::BG_SELECTION),
        selection_fg: c(tk::TEXT_PRIMARY),
        head: Color::Rgb(head.r, head.g, head.b),
    }
}

/// 全局当前色板（换主题时整体替换）
static CURRENT: RwLock<Palette> = RwLock::new(Palette::legacy());

/// 测试专用：改配色的测试与断言配色的测试靠这把锁互斥。
/// 并行跑测试时，换主题的测试会在中途把全局色板换成别的，
/// 正在断言某个颜色（比如选中底色）的测试就会看到别人的调色板 → flaky。
#[cfg(test)]
pub fn test_palette_guard() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// 切换全局主题（界面下一帧整体变色）
pub fn set_theme(id: &str) {
    let p = palette_of(id);
    if let Ok(mut g) = CURRENT.write() {
        *g = p;
    }
}

/// 当前色板（锁中毒时退回经典配色，绝不 panic）
pub fn cur() -> Palette {
    CURRENT
        .read()
        .map(|g| *g)
        .unwrap_or_else(|_| Palette::legacy())
}

// ---- 语义色 ----

pub fn fg() -> Color {
    cur().fg
}
pub fn fg_muted() -> Color {
    cur().fg_muted
}
pub fn accent_color() -> Color {
    cur().accent
}
pub fn primary() -> Color {
    cur().primary
}
pub fn ok() -> Color {
    cur().ok
}
pub fn warn() -> Color {
    cur().warn
}
pub fn error_color() -> Color {
    cur().error
}
pub fn border() -> Color {
    cur().border
}
pub fn border_sel() -> Color {
    cur().border_sel
}
/// 选中框流光头部颜色
pub fn head() -> Color {
    cur().head
}

// ---- 组合样式（都不设背景色：背景交给终端）----

pub fn base() -> Style {
    Style::default().fg(fg())
}

pub fn accent() -> Style {
    Style::default().fg(accent_color())
}

/// 选中块：选中的字一律用这个（字带底色，跟随主题）。
/// 列表整行高亮、标签 chip、来源条都用它——**不是全屏背景**，
/// 只给选中项用（见模块头部）。
pub fn selected_block() -> Style {
    Style::default()
        .fg(cur().selection_fg)
        .bg(cur().selection_bg)
        .add_modifier(Modifier::BOLD)
}

pub fn dim() -> Style {
    Style::default().fg(fg_muted())
}

pub fn error() -> Style {
    Style::default()
        .fg(error_color())
        .add_modifier(Modifier::BOLD)
}

/// 颜色按 `alpha` 往次要字色退（1 = 原色，0 = 完全退成 `fg_muted`）。
/// 我们本来就不铺背景（底色是用户终端的），所以「淡出」= 往 fg_muted 拉，
/// 让列表里旧条目不抢新条目的注意力；主题换色时它会跟着一起变。
pub fn fade_color(c: Color, alpha: f32) -> Color {
    let a = alpha.clamp(0.0, 1.0);
    let (r1, g1, b1) = rgb_of(c);
    let (r2, g2, b2) = rgb_of(fg_muted());
    let mix = |x: u8, y: u8| (x as f32 * a + y as f32 * (1.0 - a)).round() as u8;
    Color::Rgb(mix(r1, r2), mix(g1, g2), mix(b1, b2))
}

/// 样式版淡出（只动前景色，其它修饰符保留）
pub fn fade(style: Style, alpha: f32) -> Style {
    let fg = style.fg.unwrap_or_else(fg);
    style.fg(fade_color(fg, alpha))
}

/// 标准 16 色 → RGB（主题里既有命名色也有 Rgb，混色需要统一口径）
fn rgb_of(c: Color) -> (u8, u8, u8) {
    match c {
        Color::Rgb(r, g, b) => (r, g, b),
        Color::Black => (0, 0, 0),
        Color::Red => (128, 0, 0),
        Color::Green => (0, 128, 0),
        Color::Yellow => (128, 128, 0),
        Color::Blue => (0, 0, 128),
        Color::Magenta => (128, 0, 128),
        Color::Cyan => (0, 128, 128),
        Color::Gray => (192, 192, 192),
        Color::DarkGray => (128, 128, 128),
        Color::LightRed => (255, 0, 0),
        Color::LightGreen => (0, 255, 0),
        Color::LightYellow => (255, 255, 0),
        Color::LightBlue => (0, 0, 255),
        Color::LightMagenta => (255, 0, 255),
        Color::LightCyan => (0, 255, 255),
        Color::White => (255, 255, 255),
        // Reset / Indexed：没有可混的颜色，按中性灰处理（fade 只影响观感，不会丢信息）
        _ => (192, 192, 192),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 默认主题必须就是**现在的样子**（青调 + 灰字；选中 = 蓝底黑字）
    #[test]
    fn legacy_palette_is_the_old_look() {
        let p = palette_of(LEGACY_ID);
        assert_eq!(p, Palette::legacy());
        assert_eq!(p.accent, Color::Cyan);
        assert_eq!(p.fg, Color::Gray);
        assert_eq!(p.selection_bg, Color::Blue, "经典主题选中块 = 原来的蓝底");
        assert_eq!(p.selection_fg, Color::Black, "经典主题选中块 = 原来的黑字");
        assert_eq!(p.head, Color::LightCyan);
    }

    /// 空 id / 陌生 id 不许 panic，一律回经典配色（配置坏了也能开）
    #[test]
    fn unknown_theme_falls_back_to_legacy() {
        assert_eq!(palette_of(""), Palette::legacy());
        assert_eq!(palette_of("没有这个主题"), Palette::legacy());
    }

    /// opaline 主题要真的映射出另一套颜色，而不是悄悄回退成经典
    #[test]
    fn opaline_themes_map_to_real_palettes() {
        let legacy = Palette::legacy();
        for id in ["catppuccin-mocha", "dracula", "tokyo-night", "gruvbox-dark"] {
            let p = palette_of(id);
            assert_ne!(p, legacy, "{id} 应该是一套不一样的颜色");
            assert_ne!(p.fg, p.accent, "{id} 前景和强调色撞了");
        }
    }

    /// 列表：mctl 经典排第一（←→ 循环的起点），内置主题一个不少、全都能加载
    #[test]
    fn theme_ids_start_with_legacy_and_all_load() {
        let ids = theme_ids();
        assert_eq!(ids.first().map(String::as_str), Some(LEGACY_ID));
        assert!(
            ids.len() >= 30,
            "opaline 内置主题应该都在，实际 {}",
            ids.len()
        );
        for id in ids {
            let p = palette_of(id);
            assert_ne!(p.fg, p.accent, "{id} 前景和强调色撞了");
            // 选中块上的字必须和底色分得开，不然整行高亮就成了糊墙
            assert_ne!(p.selection_fg, p.selection_bg, "{id} 选中块字色和底色撞了");
        }
    }

    /// 显示名：经典有中文名，opaline 主题用自己 meta 里的名字，未知 id 原样兜底
    #[test]
    fn labels_are_human_readable() {
        assert_eq!(theme_label(LEGACY_ID), "mctl 经典");
        assert_eq!(theme_label("catppuccin-mocha"), "Catppuccin Mocha");
        assert_eq!(theme_label("没有这个"), "没有这个");
    }
}
