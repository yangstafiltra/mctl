//! tui-wm-core：mctl TUI 窗口管理器核心。
//!
//! 提供可停靠窗口外壳：
//! - 左侧 dock（「视图」栏）宽度可拖拽/键盘调整
//! - 窗口标题栏可拖拽：拖入左栏 = 停靠，拖回主区 = 取消停靠
//! - `Ctrl+W` 进入调整模式：`←/→` 宽度、`↑/↓` dock 窗口高度、`Tab` 切换、
//!   `d` 停靠/取消、`Esc`/`q` 退出（所有键位、尺寸、配色、鼠标策略均可配置）
//!
//! 配置：`~/.config/mctl/wm.toml`（类 hyprland），见 [`config`] 模块。

pub mod config;

use std::collections::HashMap;

use crate::config::Bindings;
use crossterm::event::{KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

pub use crate::config::{KeyChord, MouseConfig, WmConfig, WindowOpts};

pub type WindowId = u32;

/// dock 最小宽度（列）
pub const MIN_DOCK_WIDTH: u16 = 12;

/// 窗口内容宿主：由集成方（mctl）实现，负责按窗口 id 渲染内容。
pub trait WmHost {
    fn render_window(&mut self, frame: &mut Frame, id: WindowId, area: Rect);
}

#[derive(Debug, Clone)]
pub struct Window {
    pub id: WindowId,
    pub title: String,
    /// 是否停靠在左侧 dock
    pub docked: bool,
    /// 固定停靠：不能拖出 dock（账户卡、状态卡等固定面板）
    pub pinned: bool,
    /// dock 内高度权重（↑/↓ 调整）
    pub flex: u16,
    /// 是否渲染 wm 标题栏行（主区窗口 true；dock 面板自带边框标题时 false）
    pub show_title: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WmMode {
    /// 正常模式：鼠标只做聚焦/点击，不响应拖拽调整
    Normal,
    /// 调整模式（快捷键进入）：键盘与鼠标拖拽调整大小/停靠
    Adjust,
}

#[derive(Debug, Clone, Copy)]
enum Drag {
    /// 拖分隔条：调整 dock 宽度
    Resize { start_x: u16, start_w: u16 },
    /// 拖窗口标题栏：移动窗口（结束落点决定停靠与否）
    Move { id: WindowId },
}

pub struct Wm {
    pub windows: HashMap<WindowId, Window>,
    /// 注册顺序（Tab 轮换与渲染顺序）
    pub order: Vec<WindowId>,
    /// 聚焦窗口（高亮；可能是 dock 窗口）
    pub active: WindowId,
    /// 主区渲染窗口（总是未停靠的窗口）
    pub main_win: WindowId,
    /// dock 宽度（布局时会 clamp 到配置区间）
    pub dock_width: u16,
    /// 左栏可见性
    pub show_dock: bool,
    pub mode: WmMode,
    pub accent: Color,
    pub dim: Color,
    /// 解析后的键位绑定
    pub keys: Bindings,
    /// 鼠标拖拽调整大小/移动是否必须先进入调整模式（防误触）
    pub mouse_require_adjust: bool,
    /// 拖入 dock 区域即停靠（false 时仅标题栏拖入才停靠）
    pub drag_to_dock: bool,
    /// dock 宽度 clamp 边界
    pub dock_min_width: u16,
    pub dock_max_width: u16,
    drag: Option<Drag>,
}

/// 渲染快照：拥有数据，不借用 Wm（宿主可同时可变借用自身与快照渲染）
#[derive(Debug, Clone)]
pub struct WindowLayout {
    pub dock: Option<DockLayout>,
    pub main: MainLayout,
    pub mode: WmMode,
    pub accent: Color,
    pub dim: Color,
}

#[derive(Debug, Clone)]
pub struct DockLayout {
    pub rect: Rect,
    pub sep: Rect,
    pub windows: Vec<DockWindow>,
}

#[derive(Debug, Clone)]
pub struct DockWindow {
    pub id: WindowId,
    pub rect: Rect,
    pub title: String,
    pub focused: bool,
    pub show_title: bool,
}

#[derive(Debug, Clone)]
pub struct MainLayout {
    pub rect: Rect,
    pub id: WindowId,
    pub title: String,
    pub focused: bool,
    pub show_title: bool,
}

impl WindowLayout {
    /// 渲染整个外壳：分隔条 + dock 窗口 + 主区窗口
    pub fn render<H: WmHost>(&self, frame: &mut Frame, host: &mut H) {
        if let Some(dock) = &self.dock {
            let sep_style = if self.mode == WmMode::Adjust {
                Style::default().fg(self.accent).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(self.dim)
            };
            let glyph = if self.mode == WmMode::Adjust { "║" } else { "│" };
            frame.render_widget(Paragraph::new(Span::styled(glyph, sep_style)), dock.sep);
            for w in &dock.windows {
                render_one(frame, w.rect, w.focused, w.id, &w.title, w.show_title, self.accent, self.dim, host);
            }
        }
        render_one(
            frame,
            self.main.rect,
            self.main.focused,
            self.main.id,
            &self.main.title,
            self.main.show_title,
            self.accent,
            self.dim,
            host,
        );
    }
}

/// 渲染单个窗口：可选标题栏 + 内容
fn render_one<H: WmHost>(
    frame: &mut Frame,
    area: Rect,
    focused: bool,
    id: WindowId,
    title: &str,
    show_title: bool,
    accent: Color,
    dim: Color,
    host: &mut H,
) {
    if area.height == 0 {
        return;
    }
    let title_h = if show_title { 1 } else { 0 };
    let content = Rect::new(area.x, area.y + title_h, area.width, area.height.saturating_sub(title_h));
    if show_title {
        let title_style = if focused {
            Style::default().fg(accent).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(dim)
        };
        let spans = vec![
            Span::styled(if focused { " ▌" } else { "  " }, title_style),
            Span::styled(title, title_style),
        ];
        // 标题栏不再显示提示，提示统一在底栏
        frame.render_widget(Paragraph::new(Line::from(spans)), Rect::new(area.x, area.y, area.width, title_h));
    }
    if content.height > 0 {
        host.render_window(frame, id, content);
    }
}

impl Default for Wm {
    fn default() -> Self {
        Self::new()
    }
}

impl Wm {
    /// 默认配置（等同 `WmConfig::default()`）。
    pub fn new() -> Self {
        Self::with_config(&WmConfig::default())
    }

    /// 用外部配置构建（hyprland 式 `~/.config/mctl/wm.toml`）。
    pub fn with_config(cfg: &WmConfig) -> Self {
        let keys = Bindings::from(&cfg.keys);
        let dock_width = cfg.dock.width.max(cfg.dock.min_width);
        Self {
            windows: HashMap::new(),
            order: Vec::new(),
            active: 0,
            main_win: 0,
            dock_width,
            show_dock: cfg.dock.show,
            mode: WmMode::Normal,
            accent: config::parse_color(&cfg.style.accent).unwrap_or(Color::Cyan),
            dim: config::parse_color(&cfg.style.dim).unwrap_or(Color::DarkGray),
            keys,
            mouse_require_adjust: cfg.mouse.require_adjust,
            drag_to_dock: cfg.mouse.drag_to_dock,
            dock_min_width: cfg.dock.min_width.max(MIN_DOCK_WIDTH),
            dock_max_width: cfg.dock.max_width.max(cfg.dock.min_width),
            drag: None,
        }
    }

    /// 应用某个窗口的配置覆盖（[`WindowOpts`] 字段）。
    pub fn apply_opts_to(&mut self, id: WindowId, opts: &WindowOpts) {
        if let Some(w) = self.windows.get_mut(&id) {
            if let Some(flex) = opts.flex {
                w.flex = flex;
            }
            if let Some(docked) = opts.docked {
                w.docked = docked;
            }
            if let Some(pinned) = opts.pinned {
                w.pinned = pinned;
            }
            if let Some(show_title) = opts.show_title {
                w.show_title = show_title;
            }
        }
    }

    // ---- 窗口管理 API ----

    pub fn add_window(&mut self, id: WindowId, title: impl Into<String>) -> &mut Window {
        if !self.windows.contains_key(&id) {
            self.order.push(id);
        }
        self.windows.insert(
            id,
            Window { id, title: title.into(), docked: false, pinned: false, flex: 1, show_title: true },
        );
        if self.active == 0 && self.main_win == 0 {
            self.active = id;
            self.main_win = id;
        }
        self.windows.get_mut(&id).expect("just inserted")
    }

    pub fn focus(&mut self, id: WindowId) {
        if let Some(w) = self.windows.get(&id) {
            self.active = id;
            if !w.docked {
                self.main_win = id;
            } else if !w.pinned {
                // 聚焦一个已停靠（可拖出）窗口 → 收回主区
                self.undock(id);
                self.main_win = id;
            }
        }
    }

    pub fn dock(&mut self, id: WindowId) {
        if let Some(w) = self.windows.get_mut(&id) {
            w.docked = true;
        }
    }

    pub fn undock(&mut self, id: WindowId) {
        if let Some(w) = self.windows.get_mut(&id) {
            if !w.pinned {
                w.docked = false;
            }
        }
    }

    pub fn toggle_dock(&mut self, id: WindowId) {
        let pinned = self.windows.get(&id).is_some_and(|w| w.pinned);
        if pinned {
            return;
        }
        let docked = self.windows.get(&id).is_some_and(|w| w.docked);
        if docked {
            // 从 dock 收回主区
            self.undock(id);
            if self.windows.get(&id).is_some_and(|w| !w.docked) {
                self.active = id;
                self.main_win = id;
            }
        } else {
            self.dock(id);
            // 停靠了主区窗口：主区换到第一个未停靠的窗口
            if self.main_win == id {
                self.focus_first_undocked(id);
            }
        }
    }

    fn focus_first_undocked(&mut self, exclude: WindowId) {
        for wid in &self.order {
            if *wid == exclude {
                continue;
            }
            if self.windows.get(wid).is_some_and(|w| !w.docked) {
                self.active = *wid;
                self.main_win = *wid;
                return;
            }
        }
    }

    // ---- 布局 ----

    /// 返回 (dock 区域, 主区区域)
    pub fn layout(&self, area: Rect) -> (Option<Rect>, Rect) {
        if !self.show_dock || area.width <= self.dock_min_width + 1 {
            return (None, area);
        }
        let max_w = ((area.width as u32 * 2) / 3) as u16;
        let w = self.dock_width.clamp(self.dock_min_width, max_w.max(self.dock_min_width));
        let cols = Layout::horizontal([Constraint::Length(w), Constraint::Min(1)]).split(area);
        (Some(cols[0]), cols[1])
    }

    /// dock 内窗口分配：按 flex 权重均分，每个窗口含标题栏行
    fn dock_layout(&self, dock: Rect) -> Vec<(WindowId, Rect)> {
        let ids: Vec<WindowId> = self
            .order
            .iter()
            .copied()
            .filter(|id| self.windows.get(id).is_some_and(|w| w.docked))
            .collect();
        if ids.is_empty() {
            return Vec::new();
        }
        let sum: u32 = ids.iter().map(|id| self.windows[id].flex as u32).sum::<u32>().max(1);
        let mut y = dock.y;
        let mut out = Vec::new();
        for (i, id) in ids.iter().enumerate() {
            let flex = self.windows[id].flex as u32;
            // 最后一张卡吃掉整数除法的余数，保证底边与主区贴合
            let h = if i + 1 == ids.len() {
                (dock.y + dock.height).saturating_sub(y)
            } else {
                (dock.height as u32 * flex / sum).max(1) as u16
            };
            if y >= dock.y + dock.height {
                break;
            }
            let h = h.min(dock.y + dock.height - y);
            out.push((*id, Rect::new(dock.x, y, dock.width, h)));
            y += h;
        }
        out
    }

    /// 命中测试：返回坐标落在哪个窗口（dock 窗口或主区窗口）
    pub fn hit_test(&self, area: Rect, x: u16, y: u16) -> Option<WindowId> {
        let (dock, main) = self.layout(area);
        if let Some(dock) = dock {
            for (id, rect) in self.dock_layout(dock) {
                if rect.contains((x, y).into()) {
                    return Some(id);
                }
            }
        }
        if main.contains((x, y).into()) {
            return Some(self.main_win);
        }
        None
    }

    /// 窗口渲染区域（dock 或主区）
    pub fn window_rect(&self, area: Rect, id: WindowId) -> Option<Rect> {
        let (dock, main) = self.layout(area);
        if let Some(dock) = dock {
            for (wid, rect) in self.dock_layout(dock) {
                if wid == id {
                    return Some(rect);
                }
            }
        }
        if id == self.main_win {
            return Some(main);
        }
        None
    }

    // ---- 渲染 ----

    /// 产出渲染快照（拥有数据，不借用 self），交由 WindowLayout::render 渲染
    pub fn snapshot(&self, area: Rect) -> WindowLayout {
        let (dock_area, main_area) = self.layout(area);
        let dock = dock_area.map(|dock| {
            let mut windows = Vec::new();
            for (id, rect) in self.dock_layout(dock) {
                let w = &self.windows[&id];
                windows.push(DockWindow {
                    id,
                    rect,
                    title: w.title.clone(),
                    focused: id == self.active,
                    show_title: w.show_title,
                });
            }
            DockLayout {
                rect: dock,
                sep: Rect::new(dock.x + dock.width, dock.y, 1, dock.height),
                windows,
            }
        });
        let main = match self.windows.get(&self.main_win) {
            Some(w) => MainLayout {
                rect: main_area,
                id: w.id,
                title: w.title.clone(),
                focused: w.id == self.active,
                show_title: w.show_title,
            },
            None => MainLayout {
                rect: main_area,
                id: 0,
                title: String::new(),
                focused: false,
                show_title: true,
            },
        };
        WindowLayout { dock, main, mode: self.mode, accent: self.accent, dim: self.dim }
    }

    // ---- 键盘事件 ----

    /// 返回 true = 已消费（调用方不再处理）
    pub fn handle_key(&mut self, key: KeyEvent) -> bool {
        // 调整模式快捷键（全局）：进入/退出
        if self.keys.adjust_mode.matches(&key) {
            self.mode = if self.mode == WmMode::Adjust { WmMode::Normal } else { WmMode::Adjust };
            return true;
        }
        if self.mode != WmMode::Adjust {
            return false;
        }
        if self.keys.exit.iter().any(|k| k.matches(&key)) {
            self.mode = WmMode::Normal;
            return true;
        }
        if self.keys.dock_toggle.matches(&key) {
            self.toggle_dock(self.active);
            return true;
        }
        if self.keys.cycle.matches(&key) {
            self.cycle_active();
            return true;
        }
        if self.keys.width_decrease.matches(&key) {
            self.dock_width = self.dock_width.saturating_sub(1).max(self.dock_min_width);
            return true;
        }
        if self.keys.width_increase.matches(&key) {
            self.dock_width = self.dock_width.saturating_add(1).min(self.dock_max_width);
            return true;
        }
        if self.keys.flex_increase.matches(&key) {
            self.adjust_active_flex(1);
            return true;
        }
        if self.keys.flex_decrease.matches(&key) {
            self.adjust_active_flex(-1);
            return true;
        }
        false
    }

    fn adjust_active_flex(&mut self, delta: i16) {
        let id = self.active;
        if let Some(w) = self.windows.get_mut(&id) {
            let next = (w.flex as i16 + delta).clamp(1, 10) as u16;
            w.flex = next;
        }
    }

    fn cycle_active(&mut self) {
        if self.order.is_empty() {
            return;
        }
        let idx = self.order.iter().position(|id| *id == self.active).unwrap_or(0);
        for k in 1..=self.order.len() {
            let j = (idx + k) % self.order.len();
            let id = self.order[j];
            if let Some(w) = self.windows.get(&id) {
                // 调整模式下：仅轮转已停靠在 dock 的窗口（有 flex 可调高度），
                // 不切换主视图窗口（本地/服务器/租赁/资源等未停靠窗口）
                if !w.pinned && w.docked {
                    self.active = id;
                    // 停靠中的窗口也可以成为焦点（↑↓ 调 flex、d 收回主区）
                    return;
                }
            }
        }
    }

    // ---- 鼠标事件 ----

    /// 返回 true = 已消费
    pub fn handle_mouse(&mut self, ev: MouseEvent, area: Rect) -> bool {
        let (mx, my) = (ev.column, ev.row);
        match ev.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let (dock_area, _main) = self.layout(area);
                // 拖拽调整（分隔条改宽 / 标题栏移动）需先进入调整模式（防误触）
                let can_drag = !self.mouse_require_adjust || self.mode == WmMode::Adjust;
                if can_drag {
                    // 分隔条 ±1 列 → 调整宽度
                    if let Some(dock) = dock_area {
                        let sep_x = dock.x + dock.width;
                        if (mx as i16 - sep_x as i16).unsigned_abs() <= 1 {
                            self.drag = Some(Drag::Resize { start_x: mx, start_w: self.dock_width });
                            return true;
                        }
                        // dock 窗口标题栏 → 移动
                        for (id, rect) in self.dock_layout(dock) {
                            if my == rect.y && rect.contains((mx, my).into()) {
                                self.active = id;
                                self.drag = Some(Drag::Move { id });
                                return true;
                            }
                        }
                    }
                    // 主区标题栏 → 移动主区窗口
                    let (_dock, main) = self.layout(area);
                    if my == main.y && main.contains((mx, my).into()) {
                        self.active = self.main_win;
                        self.drag = Some(Drag::Move { id: self.main_win });
                        return true;
                    }
                }
                // 点击 dock 内容区：聚焦该 dock 窗口；返回 false 让宿主处理（如视图切换）
                if let Some(dock) = dock_area {
                    if dock.contains((mx, my).into()) {
                        for (id, rect) in self.dock_layout(dock) {
                            if rect.contains((mx, my).into()) {
                                self.active = id;
                                return false;
                            }
                        }
                    }
                }
                false
            }
            MouseEventKind::Drag(MouseButton::Left) => match self.drag {
                Some(Drag::Resize { start_x, start_w }) => {
                    let dx = mx as i32 - start_x as i32;
                    self.dock_width =
                        (start_w as i32 + dx).clamp(self.dock_min_width as i32, self.dock_max_width as i32) as u16;
                    true
                }
                Some(Drag::Move { .. }) => true,
                None => false,
            },
            MouseEventKind::Up(MouseButton::Left) => {
                if let Some(drag) = self.drag.take() {
                    if let Drag::Move { id } = drag {
                        // 落点决定停靠与否（pinned 不动）
                        let (dock_area, _) = self.layout(area);
                        let pinned = self.windows.get(&id).is_some_and(|w| w.pinned);
                        if !pinned {
                            // drag_to_dock=false 时，须落在 dock 窗口标题栏才停靠
                            let in_dock = match dock_area {
                                Some(d) if d.contains((mx, my).into()) => {
                                    if self.drag_to_dock {
                                        true
                                    } else {
                                        self.dock_layout(d)
                                            .iter()
                                            .any(|(_, r)| my == r.y && r.contains((mx, my).into()))
                                    }
                                }
                                _ => false,
                            };
                            if in_dock {
                                self.dock(id);
                            } else {
                                self.undock(id);
                            }
                        }
                        self.active = id;
                        if self.windows.get(&id).is_some_and(|w| w.docked) {
                            self.focus_first_undocked(id);
                        } else {
                            self.main_win = id;
                        }
                    }
                    return true;
                }
                false
            }
            MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
                // dock 内滚动：切换 dock 窗口
                if let Some(dock) = self.layout(area).0 {
                    if dock.contains((mx, my).into()) {
                        let ids: Vec<WindowId> = self
                            .order
                            .iter()
                            .copied()
                            .filter(|id| self.windows.get(id).is_some_and(|w| w.docked))
                            .collect();
                        if !ids.is_empty() {
                            let idx = ids.iter().position(|id| *id == self.active).unwrap_or(0);
                            let delta = if ev.kind == MouseEventKind::ScrollDown { 1usize } else { ids.len() - 1 };
                            let next = ids[(idx + delta) % ids.len()];
                            self.active = next;
                        }
                        return true;
                    }
                }
                false
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyModifiers};

    #[test]
    fn add_and_focus() {
        let mut wm = Wm::new();
        wm.add_window(1, "本地");
        wm.add_window(2, "服务器");
        wm.focus(2);
        assert_eq!(wm.active, 2);
        assert_eq!(wm.order, vec![1, 2]);
    }

    #[test]
    fn dock_toggle_moves_focus() {
        let mut wm = Wm::new();
        wm.add_window(1, "本地");
        wm.add_window(2, "服务器");
        wm.focus(1);
        wm.toggle_dock(1); // 停靠活动窗口 → 主区换到 2
        assert!(wm.windows[&1].docked);
        assert_eq!(wm.active, 2);
        wm.toggle_dock(1); // 收回主区
        assert!(!wm.windows[&1].docked);
        assert_eq!(wm.active, 1);
        assert_eq!(wm.main_win, 1);
    }

    #[test]
    fn tab_cycles_through_docked_windows_and_d_undocks() {
        let mut wm = Wm::new();
        wm.add_window(1, "本地");
        wm.add_window(2, "服务器");
        wm.add_window(3, "租赁服");
        wm.focus(1);
        wm.handle_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
        wm.handle_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE)); // 停靠 1
        assert!(wm.windows[&1].docked);
        assert_eq!(wm.active, 2);
        assert_eq!(wm.main_win, 2);
        // 再停靠 2
        wm.active = 2;
        wm.handle_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));
        assert!(wm.windows[&2].docked);
        // 现在 1、2 都停靠，Tab 轮转它们（跳过未停靠的 3）
        wm.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(wm.active, 1, "Tab 轮转到已停靠的 1");
        wm.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(wm.active, 2, "Tab 轮转到已停靠的 2");
        // d 收回 1
        wm.active = 1;
        wm.handle_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));
        assert!(!wm.windows[&1].docked);
        assert_eq!(wm.main_win, 1);
    }

    #[test]
    fn layout_dock_and_main() {
        let mut wm = Wm::new();
        wm.add_window(1, "本地");
        wm.add_window(2, "服务器");
        wm.dock(2);
        let area = Rect::new(0, 0, 100, 30);
        let (dock, main) = wm.layout(area);
        let dock = dock.expect("dock visible");
        assert_eq!(dock.width, wm.dock_width);
        assert_eq!(main.x, dock.x + dock.width);
        assert!(main.width > 0);
    }

    #[test]
    fn dock_layout_splits_by_flex() {
        let mut wm = Wm::new();
        wm.add_window(1, "账户").docked = true;
        wm.add_window(2, "视图").docked = true;
        wm.windows.get_mut(&1).unwrap().flex = 3;
        let dock = Rect::new(0, 0, 24, 40);
        let rects = wm.dock_layout(dock);
        assert_eq!(rects.len(), 2);
        assert!(rects[0].1.height >= rects[1].1.height, "flex 3 应更高");
        assert_eq!(rects[0].1.y + rects[0].1.height, rects[1].1.y);
    }

    #[test]
    fn ctrl_w_toggles_adjust_mode() {
        let mut wm = Wm::new();
        let key = KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL);
        assert!(wm.handle_key(key));
        assert_eq!(wm.mode, WmMode::Adjust);
        assert!(wm.handle_key(key));
        assert_eq!(wm.mode, WmMode::Normal);
    }

    #[test]
    fn adjust_mode_keys_resize_dock() {
        let mut wm = Wm::new();
        wm.handle_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
        let w0 = wm.dock_width;
        wm.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(wm.dock_width, w0 + 1);
        wm.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        assert_eq!(wm.dock_width, w0);
    }

    #[test]
    fn resize_drag_updates_dock_width() {
        let mut wm = Wm::new();
        // 鼠标拖拽须先进入调整模式（防误触）
        wm.handle_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
        let area = Rect::new(0, 0, 100, 30);
        let (dock, _) = wm.layout(area);
        let sep_x = dock.unwrap().x + wm.dock_width;
        // 按下分隔条
        let down = MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: sep_x, row: 5, modifiers: KeyModifiers::NONE };
        assert!(wm.handle_mouse(down, area));
        // 右拖 10 列
        let drag = MouseEvent { kind: MouseEventKind::Drag(MouseButton::Left), column: sep_x + 10, row: 5, modifiers: KeyModifiers::NONE };
        assert!(wm.handle_mouse(drag, area));
        assert_eq!(wm.dock_width, 24 + 10);
        // 释放
        let up = MouseEvent { kind: MouseEventKind::Up(MouseButton::Left), column: sep_x + 10, row: 5, modifiers: KeyModifiers::NONE };
        assert!(wm.handle_mouse(up, area));
    }

    #[test]
    fn mouse_drag_blocked_outside_adjust_mode() {
        let mut wm = Wm::new();
        let area = Rect::new(0, 0, 100, 30);
        let (dock, _) = wm.layout(area);
        let sep_x = dock.unwrap().x + wm.dock_width;
        // 正常模式：按分隔条不进入拖拽（防误触）
        let down = MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: sep_x, row: 5, modifiers: KeyModifiers::NONE };
        assert!(!wm.handle_mouse(down, area), "正常模式不应响应拖拽");
        assert!(wm.drag.is_none());
        // 主区标题栏同样不响应
        let (_d, main) = wm.layout(area);
        let down2 = MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: main.x + 2, row: main.y, modifiers: KeyModifiers::NONE };
        assert!(!wm.handle_mouse(down2, area));
        // require_adjust=false 时直接可拖
        let mut wm2 = Wm::with_config(&WmConfig {
            mouse: MouseConfig { require_adjust: false, drag_to_dock: true },
            ..WmConfig::default()
        });
        assert!(wm2.handle_mouse(down, area));
    }

    #[test]
    fn move_window_to_dock_docks_it() {
        let mut wm = Wm::new();
        wm.add_window(1, "本地");
        wm.add_window(2, "服务器");
        wm.focus(1);
        // 鼠标拖拽须先进入调整模式（防误触）
        wm.handle_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
        let area = Rect::new(0, 0, 100, 30);
        // 主区标题栏 (y=0，x 在分隔条右侧)
        let (dock, main) = wm.layout(area);
        let main_x = main.x + 2;
        let down = MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: main_x, row: main.y, modifiers: KeyModifiers::NONE };
        assert!(wm.handle_mouse(down, area));
        // 拖进左栏
        let dock_x = dock.unwrap().x + 2;
        let drag = MouseEvent { kind: MouseEventKind::Drag(MouseButton::Left), column: dock_x, row: main.y + 5, modifiers: KeyModifiers::NONE };
        assert!(wm.handle_mouse(drag, area));
        let up = MouseEvent { kind: MouseEventKind::Up(MouseButton::Left), column: dock_x, row: main.y + 5, modifiers: KeyModifiers::NONE };
        assert!(wm.handle_mouse(up, area));
        assert!(wm.windows[&1].docked, "拖入左栏应停靠");
        assert_eq!(wm.active, 2, "停靠后主区换到下一个窗口");
    }
}
