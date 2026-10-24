pub mod extra;
pub mod home;
pub mod maps;
#[cfg(test)]
mod mod_test;
pub mod pwd_prompt;
pub mod theme_picker;
pub mod topbar;
pub mod version_tree;
pub mod world_select;

/// 立体 ASCII 品牌字（trawl 风格，ANSI Shadow 块状）
pub const MCTL_BANNER: [&str; 6] = [
    "███╗   ███╗  ██████╗ ████████╗ ██╗     ",
    "████╗ ████║ ██╔════╝ ╚══██╔══╝ ██║     ",
    "██╔████╔██║ ██║         ██║    ██║     ",
    "██║╚██╔╝██║ ██║         ██║    ██║     ",
    "██║ ╚═╝ ██║ ╚██████╗    ██║    ███████╗",
    "╚═╝     ╚═╝  ╚═════╝    ╚═╝    ╚══════╝",
];

pub const MCTL_TAGLINE: &str = "launch minecraft · in the terminal";

/// 终端显示宽度（列数）。CJK / 全角 = 2，其余 = 1。
/// 几何符号（U+25xx ▣▤◈、U+2Bxx ⬡、U+2699 ⚙）属 East Asian **Ambiguous**：
/// CJK locale 的终端可能按 2 列渲染。这里按 1 列算——绝大多数终端
/// （非 CJK locale）的默认行为如此。设 `LANG=zh_CN.UTF-8` 时若发现图标错位，
/// 把下面 ambiguous 分支改成 2 即可。
pub fn cell_width(c: char) -> usize {
    let u = c as u32;
    match u {
        // 组合字符 / 零宽
        0x0300..=0x036F | 0x200B..=0x200F | 0xFE00..=0xFE0F => 0,
        // 全角 / 东亚宽
        0x1100..=0x115F
        | 0x2E80..=0x303E
        | 0x3041..=0x33FF
        | 0x3400..=0x4DBF
        | 0x4E00..=0x9FFF
        | 0xA000..=0xA4CF
        | 0xAC00..=0xD7A3
        | 0xF900..=0xFAFF
        | 0xFE30..=0xFE6F
        | 0xFF00..=0xFF60
        | 0xFFE0..=0xFFE6
        | 0x1F300..=0x1F9FF
        | 0x20000..=0x3FFFD => 2,
        _ => 1,
    }
}

/// 按**显示列宽**截断，超出时以 `…` 收尾。
/// 不要用 `chars.take(n)`：中文 1 char = 2 列，会撑破边框。
pub fn fit(s: &str, max_cells: usize) -> String {
    if cell_width_of(s) <= max_cells {
        return s.to_string();
    }
    let budget = max_cells.saturating_sub(1); // 给省略号留 1 列
    let mut out = String::new();
    let mut w = 0;
    for c in s.chars() {
        let cw = cell_width(c);
        if w + cw > budget {
            break;
        }
        out.push(c);
        w += cw;
    }
    out.push('…');
    out
}

/// 字符串总显示列宽
pub fn cell_width_of(s: &str) -> usize {
    s.chars().map(cell_width).sum()
}

/// 按**显示列宽**把字符串切成两半：左边 `head_cells` 列，其余归右边。
/// 切口落在宽字符中间时，整个字符归左边（宁可多覆盖一列，也不劈开汉字）。
pub fn split_cells(s: &str, head_cells: usize) -> (String, String) {
    let mut w = 0;
    for (i, c) in s.char_indices() {
        if w >= head_cells {
            return (s[..i].to_string(), s[i..].to_string());
        }
        w += cell_width(c);
    }
    (s.to_string(), String::new())
}

use crate::app::App;
use ratatui::Frame;

/// 主题选择器浮层要盖在所有界面之上（含全屏视图），所以画在最后：
/// draw_inner 里有多个提前 return，包一层比在每个 return 前补画稳妥。
pub fn draw(frame: &mut Frame, app: &mut App) {
    draw_inner(frame, app);
    if app.theme_picker.is_some() {
        let area = frame.area();
        theme_picker::draw(frame, app, area);
    }
    // 密码浮层最后画：它只会在租赁服页出现，但同样要盖住整屏
    if app.pwd_prompt.is_some() {
        let area = frame.area();
        pwd_prompt::draw(frame, app, area);
    }
}

fn draw_inner(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    topbar::draw(frame, app, area);
    let body = ratatui::layout::Rect {
        x: 0,
        y: 1,
        width: area.width,
        height: area.height.saturating_sub(2),
    };
    let status_area = ratatui::layout::Rect {
        x: 0,
        y: area.height.saturating_sub(1),
        width: area.width,
        height: 1,
    };
    // 全屏登录界面（Trawl 风格：logo + 居中登录选项）
    if app.login_screen.is_some() {
        home::draw_login_full(frame, app, body);
        return;
    }
    // 完整日志 / 单面板全屏：绕过 wm 外壳直接铺满
    if app.home_log_full || app.home_panel_full {
        // 必须和正常主区走同一套渲染器：插件/设置是整页视图，
        // 若落到 home::draw → panel_full 会渲染出 dock 面板（账户/世界）而不是本页
        use crate::app::HomeView;
        match app.home_view {
            HomeView::Resource => maps::draw(frame, app, body),
            HomeView::Download => maps::draw_download_center(frame, app, body),
            HomeView::Plugins => extra::plugins_view(frame, app, body),
            HomeView::Settings => extra::settings_view(frame, app, body),
            // 本地/服务器/租赁服：铺满当前聚焦面板（panel_full）或完整日志
            _ => home::draw(frame, app, body),
        }
        topbar::status_bar(frame, app, status_area);
        return;
    }
    // 窗口管理器外壳：dock（左栏停靠窗口）+ 主区窗口
    let layout = app.wm.snapshot(body);
    layout.render(frame, app);
    // 资源页自带底部提示栏（maps::draw），不再叠加全局状态栏
    if app.home_view != crate::app::HomeView::Resource {
        topbar::status_bar(frame, app, status_area);
    }
}
