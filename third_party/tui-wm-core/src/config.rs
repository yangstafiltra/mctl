//! tui-wm-core 配置：类似 hyprland 的 TOML 配置文件（`~/.config/mctl/wm.toml`）。
//!
//! 提供完整的键位绑定、dock 尺寸、配色、窗口默认属性、鼠标策略等可更改项，
//! 为后续插件与开发者预留扩展接口。所有字段都有默认值，配置可部分覆盖。

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::Color;
use serde::Deserialize;

/// 键位组合：`ctrl+w` / `tab` / `d` 等。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyChord {
    pub code: KeyCode,
    pub modifiers: KeyModifiers,
}

impl KeyChord {
    /// 解析形如 `ctrl+w`、`alt+f1`、`shift+tab`、`esc` 的字符串。
    pub fn parse(s: &str) -> Option<KeyChord> {
        let mut modifiers = KeyModifiers::empty();
        let mut code = None;
        for (i, part) in s.split('+').enumerate() {
            let part = part.trim().to_ascii_lowercase();
            match part.as_str() {
                "ctrl" | "control" => modifiers.insert(KeyModifiers::CONTROL),
                "alt" | "option" => modifiers.insert(KeyModifiers::ALT),
                "shift" => modifiers.insert(KeyModifiers::SHIFT),
                "super" | "meta" | "cmd" => modifiers.insert(KeyModifiers::SUPER),
                _ => {
                    if i == 0 && s.contains('+') {
                        return None; // 修饰键必须在前
                    }
                    code = Some(parse_key(&part)?);
                }
            }
        }
        Some(KeyChord { code: code?, modifiers })
    }

    pub fn matches(&self, key: &KeyEvent) -> bool {
        // 只要求配置中的修饰键被按下，忽略多余修饰键（如 Shift 大写）
        self.code == key.code && self.modifiers.contains(key.modifiers)
    }

    /// 返回人类可读的标签（用于提示显示），如 "Ctrl+W"、"Tab"、"←"
    pub fn label(&self) -> String {
        let mut s = String::new();
        if self.modifiers.contains(KeyModifiers::CONTROL) {
            s.push_str("Ctrl+");
        }
        if self.modifiers.contains(KeyModifiers::ALT) {
            s.push_str("Alt+");
        }
        if self.modifiers.contains(KeyModifiers::SHIFT) {
            s.push_str("Shift+");
        }
        if self.modifiers.contains(KeyModifiers::SUPER) {
            s.push_str("Super+");
        }
        // 方向键用箭头符号
        let key_str = match self.code {
            KeyCode::Left => "←",
            KeyCode::Right => "→",
            KeyCode::Up => "↑",
            KeyCode::Down => "↓",
            KeyCode::Tab => "Tab",
            KeyCode::Enter => "Enter",
            KeyCode::Esc => "Esc",
            KeyCode::Backspace => "Backspace",
            KeyCode::Delete => "Del",
            KeyCode::Home => "Home",
            KeyCode::End => "End",
            KeyCode::PageUp => "PgUp",
            KeyCode::PageDown => "PgDn",
            KeyCode::F(n) => return format!("F{}", n),
            KeyCode::Char(c) => return format!("{}{}", s, c.to_ascii_uppercase()),
            _ => return s.trim_end_matches('+').to_string(),
        };
        format!("{}{}", s, key_str)
    }
}

fn parse_key(s: &str) -> Option<KeyCode> {
    use KeyCode::*;
    let c = |b: u8| -> Option<KeyCode> {
        let ch = b as char;
        match ch {
            'a'..='z' | '0'..='9' => Some(Char(ch)),
            _ => None,
        }
    };
    if let Some(b) = s.bytes().next() {
        if b.is_ascii_lowercase() {
            if s.len() == 1 {
                return c(b);
            }
        }
    }
    match s {
        "esc" | "escape" => Some(Esc),
        "tab" => Some(Tab),
        "enter" | "return" => Some(Enter),
        "space" => Some(Char(' ')),
        "backspace" => Some(Backspace),
        "delete" => Some(Delete),
        "insert" => Some(Insert),
        "home" => Some(Home),
        "end" => Some(End),
        "pageup" => Some(PageUp),
        "pagedown" => Some(PageDown),
        "left" => Some(Left),
        "right" => Some(Right),
        "up" => Some(Up),
        "down" => Some(Down),
        "f1" => Some(F(1)),
        "f2" => Some(F(2)),
        "f3" => Some(F(3)),
        "f4" => Some(F(4)),
        "f5" => Some(F(5)),
        "f6" => Some(F(6)),
        "f7" => Some(F(7)),
        "f8" => Some(F(8)),
        "f9" => Some(F(9)),
        "f10" => Some(F(10)),
        "f11" => Some(F(11)),
        "f12" => Some(F(12)),
        _ => None,
    }
}

/// 把配色字符串（`cyan`、`#rrggbb`）解析成 ratatui Color。
pub fn parse_color(s: &str) -> Option<Color> {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix('#') {
        if hex.len() == 6 {
            if let Ok(v) = u32::from_str_radix(hex, 16) {
                let r = ((v >> 16) & 0xff) as u8;
                let g = ((v >> 8) & 0xff) as u8;
                let b = (v & 0xff) as u8;
                return Some(Color::Rgb(r, g, b));
            }
        }
        return None;
    }
    use Color::*;
    Some(match s.to_ascii_lowercase().as_str() {
        "black" => Black,
        "red" => Red,
        "green" => Green,
        "yellow" => Yellow,
        "blue" => Blue,
        "magenta" => Magenta,
        "cyan" => Cyan,
        "white" => White,
        "gray" | "grey" => Gray,
        "darkgray" | "darkgrey" => DarkGray,
        "lightred" => LightRed,
        "lightgreen" => LightGreen,
        "lightyellow" => LightYellow,
        "lightblue" => LightBlue,
        "lightmagenta" => LightMagenta,
        "lightcyan" => LightCyan,
        "lightgray" | "lightgrey" => Gray,
        _ => return None,
    })
}

/// 键位绑定组（全部可覆盖）。
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct KeyConfig {
    /// 进入/退出键盘调整模式
    pub adjust_mode: String,
    /// 停靠/收回当前聚焦窗口
    pub dock_toggle: String,
    /// 轮转窗口焦点（含已停靠窗口）
    pub cycle: String,
    /// dock 宽度减少
    pub width_decrease: String,
    /// dock 宽度增加
    pub width_increase: String,
    /// dock 窗口高度增加
    pub flex_increase: String,
    /// dock 窗口高度减少
    pub flex_decrease: String,
    /// 退出调整模式（可多个，如 `["esc", "q"]`）
    pub exit: Vec<String>,
}

impl Default for KeyConfig {
    fn default() -> Self {
        Self {
            adjust_mode: "ctrl+w".into(),
            dock_toggle: "d".into(),
            cycle: "tab".into(),
            width_decrease: "left".into(),
            width_increase: "right".into(),
            flex_increase: "up".into(),
            flex_decrease: "down".into(),
            exit: vec!["esc".into(), "q".into()],
        }
    }
}

/// dock 尺寸配置。
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct DockConfig {
    /// 默认 dock 宽度（列）
    pub width: u16,
    /// 最小宽度
    pub min_width: u16,
    /// 最大宽度
    pub max_width: u16,
    /// 是否显示左栏 dock
    pub show: bool,
}

impl Default for DockConfig {
    fn default() -> Self {
        Self { width: 24, min_width: 12, max_width: 120, show: true }
    }
}

/// 配色配置。
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct StyleConfig {
    /// 强调色（聚焦标题栏、调整模式分隔条）
    pub accent: String,
    /// 弱化色（非聚焦标题栏、普通分隔条）
    pub dim: String,
}

impl Default for StyleConfig {
    fn default() -> Self {
        Self { accent: "cyan".into(), dim: "darkgray".into() }
    }
}

/// 鼠标策略配置。
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct MouseConfig {
    /// 拖拽/调整大小必须先进入调整模式（Ctrl+W），防止误触
    pub require_adjust: bool,
    /// 拖入 dock 区域即停靠；false 时需拖到 dock 标题栏才停靠
    pub drag_to_dock: bool,
}

impl Default for MouseConfig {
    fn default() -> Self {
        Self { require_adjust: true, drag_to_dock: true }
    }
}

/// 窗口默认属性覆盖（按 WindowId 匹配，key 为数字字符串）。
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct WindowOpts {
    pub flex: Option<u16>,
    pub docked: Option<bool>,
    pub pinned: Option<bool>,
    pub show_title: Option<bool>,
}

impl Default for WindowOpts {
    fn default() -> Self {
        Self { flex: None, docked: None, pinned: None, show_title: None }
    }
}

/// 顶层配置。
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct WmConfig {
    pub keys: KeyConfig,
    pub dock: DockConfig,
    pub style: StyleConfig,
    pub mouse: MouseConfig,
    /// 按窗口 id 覆盖默认属性（`[windows.10]` 表示窗口 10，key 为字符串）
    pub windows: std::collections::HashMap<String, WindowOpts>,
    /// 预留：插件注册扩展点（当前仅记录，供后续插件读取）
    #[serde(default)]
    pub plugins: Vec<String>,
}

impl Default for WmConfig {
    fn default() -> Self {
        Self {
            keys: KeyConfig::default(),
            dock: DockConfig::default(),
            style: StyleConfig::default(),
            mouse: MouseConfig::default(),
            windows: std::collections::HashMap::new(),
            plugins: Vec::new(),
        }
    }
}

impl WmConfig {
    /// 从 TOML 字符串解析；空串或非法内容回退默认值（非法字段会被忽略）。
    pub fn from_toml_str(s: &str) -> Self {
        toml::from_str(s).unwrap_or_default()
    }

    /// 从文件读取；文件不存在/解析失败回退默认值。
    pub fn load(path: &std::path::Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(s) => Self::from_toml_str(&s),
            Err(_) => Self::default(),
        }
    }

    /// 默认配置的 TOML 文本（带注释，可写入配置文件供用户修改）。
    pub fn sample_toml() -> String {
        r#"# mctl TUI 窗口管理器配置（类 hyprland 风格）
# 路径：~/.config/mctl/wm.toml
# 删掉本文件重启 mctl 即可恢复默认。所有项均可省略。

[keys]
# 进入/退出调整模式（调整模式下鼠标拖拽才生效）
adjust_mode = "ctrl+w"
# 停靠/收回当前聚焦窗口
dock_toggle = "d"
# 轮转窗口焦点（会经过已停靠窗口）
cycle = "tab"
# dock 宽度 / 窗口高度调整
width_decrease = "left"
width_increase = "right"
flex_increase = "up"
flex_decrease = "down"
# 退出调整模式（可多个）
exit = ["esc", "q"]

[dock]
width = 24          # 默认 dock 宽度（列）
min_width = 12
max_width = 120
show = true         # 是否显示左栏

[style]
accent = "cyan"     # 强调色：#rrggbb 或颜色名
dim = "darkgray"

[mouse]
# true = 拖拽调整大小/移动窗口必须先按调整模式快捷键，防止误触
require_adjust = true
# 拖入 dock 区域即停靠
drag_to_dock = true

# 按窗口 id 覆盖默认属性（例：让「状态」面板更高）
# [windows.12]
# flex = 3
# pinned = true
# show_title = false

# 预留插件扩展点
# plugins = ["my-plugin"]
"#
        .into()
    }
}

/// 预解析后的键位表（运行时快速匹配）。
#[derive(Debug, Clone)]
pub struct Bindings {
    pub adjust_mode: KeyChord,
    pub dock_toggle: KeyChord,
    pub cycle: KeyChord,
    pub width_decrease: KeyChord,
    pub width_increase: KeyChord,
    pub flex_increase: KeyChord,
    pub flex_decrease: KeyChord,
    pub exit: Vec<KeyChord>,
}

impl From<&KeyConfig> for Bindings {
    fn from(k: &KeyConfig) -> Self {
        Self {
            adjust_mode: KeyChord::parse(&k.adjust_mode).unwrap_or_else(|| KeyChord {
                code: KeyCode::Char('w'),
                modifiers: KeyModifiers::CONTROL,
            }),
            dock_toggle: KeyChord::parse(&k.dock_toggle).unwrap_or_else(|| KeyChord {
                code: KeyCode::Char('d'),
                modifiers: KeyModifiers::empty(),
            }),
            cycle: KeyChord::parse(&k.cycle).unwrap_or_else(|| KeyChord {
                code: KeyCode::Tab,
                modifiers: KeyModifiers::empty(),
            }),
            width_decrease: KeyChord::parse(&k.width_decrease).unwrap_or_else(|| KeyChord {
                code: KeyCode::Left,
                modifiers: KeyModifiers::empty(),
            }),
            width_increase: KeyChord::parse(&k.width_increase).unwrap_or_else(|| KeyChord {
                code: KeyCode::Right,
                modifiers: KeyModifiers::empty(),
            }),
            flex_increase: KeyChord::parse(&k.flex_increase).unwrap_or_else(|| KeyChord {
                code: KeyCode::Up,
                modifiers: KeyModifiers::empty(),
            }),
            flex_decrease: KeyChord::parse(&k.flex_decrease).unwrap_or_else(|| KeyChord {
                code: KeyCode::Down,
                modifiers: KeyModifiers::empty(),
            }),
            exit: k
                .exit
                .iter()
                .filter_map(|s| KeyChord::parse(s))
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_chords() {
        let c = KeyChord::parse("ctrl+w").unwrap();
        assert_eq!(c.code, KeyCode::Char('w'));
        assert!(c.modifiers.contains(KeyModifiers::CONTROL));
        let t = KeyChord::parse("shift+tab").unwrap();
        assert_eq!(t.code, KeyCode::Tab);
        assert!(t.modifiers.contains(KeyModifiers::SHIFT));
        assert_eq!(KeyChord::parse("esc").unwrap().code, KeyCode::Esc);
        assert_eq!(KeyChord::parse("alt+f5").unwrap().code, KeyCode::F(5));
        assert!(KeyChord::parse("nope").is_none());
    }

    #[test]
    fn color_parses_names_and_hex() {
        assert_eq!(parse_color("cyan"), Some(Color::Cyan));
        assert_eq!(parse_color("#ff0000"), Some(Color::Rgb(255, 0, 0)));
        assert_eq!(parse_color("  GREEN "), Some(Color::Green));
        assert!(parse_color("nope").is_none());
    }

    #[test]
    fn default_config_is_stable() {
        let c = WmConfig::default();
        assert_eq!(c.dock.width, 24);
        assert_eq!(c.mouse.require_adjust, true);
        assert_eq!(c.keys.adjust_mode, "ctrl+w");
        let b = Bindings::from(&c.keys);
        assert_eq!(b.adjust_mode.code, KeyCode::Char('w'));
        assert_eq!(b.exit.len(), 2);
    }

    #[test]
    fn partial_toml_overrides_only_some_fields() {
        let s = r#"
[dock]
width = 40
[mouse]
require_adjust = false
"#;
        let c = WmConfig::from_toml_str(s);
        assert_eq!(c.dock.width, 40);
        assert_eq!(c.mouse.require_adjust, false);
        assert_eq!(c.keys.adjust_mode, "ctrl+w", "未覆盖字段保持默认");
        assert_eq!(c.dock.min_width, 12);
    }

    #[test]
    fn window_opts_override_by_id() {
        let s = r#"
[windows.12]
flex = 3
pinned = true
"#;
        let c = WmConfig::from_toml_str(s);
        let w = c.windows.get("12").unwrap();
        assert_eq!(w.flex, Some(3));
        assert_eq!(w.pinned, Some(true));
        assert_eq!(w.show_title, None);
    }

    #[test]
    fn invalid_toml_falls_back_to_default() {
        let c = WmConfig::from_toml_str("not [[[ valid");
        assert_eq!(c.dock.width, 24);
        assert_eq!(c.keys.adjust_mode, "ctrl+w");
    }
}