//! 世界选择界面：每行一张**游戏自带的世界缩略图**
//! （`saves/<世界目录>/icon.png`，Minecraft 保存世界时自动生成）
//! + 名字 / 最后游玩 / 大小。
//!
//! 布局参考 rmcl/alloy 的模组选择列表：图标列在左、文字在右、行高随图标走。
//! 不支持图像协议的终端由 StatefulImage 自动降级为半块字符拼图。
//!
//! 选中行外面有一圈**描边动画**：被选中时不直接出现整框，而是从左向右
//! "同时包围"——上边线和下边线一起从左往右扫，左端先包过来，
//! 扫到右端后右边线合拢"关门"，然后把框常驻（见 [`draw_sel_border`]）。
//!
//! 不用 mctl 自动截图 / F2 截图：那些是"某一刻的画面"，和世界本身不是一回事。

use crate::app::App;
use crate::theme;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use ratatui_image::protocol::StatefulProtocol;

/// 缩略图高度（行）：6 行；宽度按字体高宽比换算（典型 1:2 字体 = 12 列，接近方形）
pub(crate) const ICON_ROWS: u16 = 6;

/// 每行占的总高度：上框线 1 + 缩略图 6 + 下框线 1。
/// 框线行平时是空的，只有选中行的框线才画出来。
pub(crate) const ROW_PITCH: u16 = ICON_ROWS + 2;

/// 描边节奏（占总时长的比例）
const SWEEP_END: f32 = 0.85; // 上下边线扫到右端的进度（之前左端合拢、之后右边线关门）
const LEFT_GROW: f32 = 0.2; // 左边线从上下两角向中间合拢占的进度
/// 流光头部长度：上下边线按列、竖边按行（亮的一段，后面的已定型）
const HEAD_CELLS: f32 = 10.0;
const HEAD_ROWS: f32 = 2.0;

/// 滚动窗口：返回 (首个可见下标, 可见行数)。
/// 不维护独立滚动状态——窗口完全由选中项推出：选中项永远可见，
/// 往下走时贴着底边滚。底部允许露半行（ceil），不浪费空间。
pub(crate) fn view_window(
    selected: usize,
    count: usize,
    view_h: u16,
    row_h: u16,
) -> (usize, usize) {
    if count == 0 || view_h == 0 {
        return (0, 0);
    }
    let row_h = row_h.max(1);
    let selected = selected.min(count - 1);
    let rows_fit = usize::from(view_h.div_ceil(row_h).max(1)).min(count);
    let start = if selected >= rows_fit {
        selected + 1 - rows_fit
    } else {
        0
    };
    let start = start.min(count - rows_fit);
    (start, rows_fit)
}

/// 缩略图列数：把 rows 行高的正方形按字体高宽比换算成列（字体 1:2 时 rows=6 → 12 列）
fn icon_columns(picker: &ratatui_image::picker::Picker, rows: u16) -> u16 {
    let fs = picker.font_size();
    let w = u32::from(fs.width.max(1));
    let h = u32::from(fs.height.max(1));
    ((u32::from(rows) * h + w / 2) / w).max(1) as u16
}

/// 边框格 (x, y) 在进度 `p` 时的状态。
/// 返回 `None` = 还没画到；`Some(head)` = 已画出，`head=true` 表示还在流光头部。
/// 动画"从左向右同时包围"：上边线和下边线一起从左往右扫，
/// 左端先包过来（左边线从上下两角向中间长），扫到右端后右边线合拢关门。
/// 行程里多算了头部长度——亮头会自然溜出两端，不会在收尾时啪一下变暗。
pub(crate) fn border_cell(w: u16, h: u16, x: u16, y: u16, p: f32) -> Option<bool> {
    if w < 2 || h < 2 {
        return None;
    }
    let p = p.clamp(0.0, 1.0);
    if p <= 0.0 {
        return None;
    }
    let wu = usize::from(w);
    let hu = usize::from(h);
    let xu = usize::from(x);
    let yu = usize::from(y);
    if y == 0 || y == h - 1 {
        // 上下边线（含四角）：一起从左往右扫
        let front = (p / SWEEP_END).min(1.0) * ((wu - 1) as f32 + HEAD_CELLS);
        if xu as f32 > front {
            return None;
        }
        return Some(front - (xu as f32) < HEAD_CELLS);
    }
    if x == 0 || x == w - 1 {
        // 竖边中段：左端先长，右端最后合拢；都是从上下两角向中间长
        let grow = if x == 0 {
            (p / LEFT_GROW).min(1.0)
        } else {
            ((p - SWEEP_END) / (1.0 - SWEEP_END)).clamp(0.0, 1.0)
        };
        let front = grow * ((hu - 1) as f32 / 2.0 + HEAD_ROWS);
        let d = yu.min(hu - 1 - yu) as f32; // 距离最近一端的行数
        if d > front {
            return None;
        }
        return Some(front - d < HEAD_ROWS);
    }
    None
}

/// 边框格该画什么字符（圆角）
pub(crate) fn border_char(w: u16, h: u16, x: u16, y: u16) -> char {
    let top = y == 0;
    let bottom = y == h - 1;
    let left = x == 0;
    let right = x == w - 1;
    match (top, bottom, left, right) {
        (true, _, true, _) => '╭',
        (true, _, _, true) => '╮',
        (_, true, _, true) => '╯',
        (_, true, true, _) => '╰',
        (true, _, _, _) | (_, true, _, _) => '─',
        _ => '│',
    }
}

/// 流光头部样式（正在跑的那几格，亮一档）
fn head_style() -> Style {
    Style::default()
        .fg(theme::head())
        .add_modifier(Modifier::BOLD)
}

/// 画选中行的描边框。
/// `progress` = 描边进度（0 还没描上，1 完整）；框高固定 [`ROW_PITCH`]，
/// `rect.height` 小的时候（贴面板底边）只画可见的部分。
fn draw_sel_border(frame: &mut Frame, rect: Rect, progress: f32) {
    let w = rect.width;
    let h = ROW_PITCH;
    if w < 2 || h < 2 || progress <= 0.0 {
        return;
    }
    for ry in 0..rect.height.min(h) {
        // 这一行没有已画到的格就不画（省一次 widget）
        if !(0..w).any(|cx| border_cell(w, h, cx, ry, progress).is_some()) {
            continue;
        }
        // 逐格算字符/样式，连续相同样式的格合并成一段
        let mut runs: Vec<(String, Style)> = Vec::new();
        for cx in 0..w {
            let (ch, style) = match border_cell(w, h, cx, ry, progress) {
                Some(is_head) => {
                    let style = if is_head {
                        head_style()
                    } else {
                        theme::accent()
                    };
                    (border_char(w, h, cx, ry), style)
                }
                None => (' ', Style::default()),
            };
            match runs.last_mut() {
                Some((s, st)) if *st == style => s.push(ch),
                _ => runs.push((ch.to_string(), style)),
            }
        }
        let spans: Vec<Span> = runs
            .into_iter()
            .map(|(s, st)| Span::styled(s, st))
            .collect();
        frame.render_widget(
            Paragraph::new(Line::from(spans)),
            Rect {
                x: rect.x,
                y: rect.y + ry,
                width: w,
                height: 1,
            },
        );
    }
}

pub fn render(frame: &mut Frame, app: &mut App, area: Rect, focused: bool) {
    let block = super::home::panel_block("▣ 最近世界 · ↑↓ 选择 · 回车直进", focused);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.height == 0 {
        return;
    }

    if app.worlds.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "（saves 下暂无世界，启动一次游戏即可创建）",
                theme::dim(),
            ))),
            inner,
        );
        return;
    }

    let count = app.worlds.len();
    let (start, visible) = view_window(app.selected_world, count, inner.height, ROW_PITCH);
    // 缩略图再大也不能把文字挤没：文字至少留 12 列
    let icon_cols = icon_columns(&app.picker, ICON_ROWS).min(inner.width.saturating_sub(18).max(1));
    // 文字结束要给右侧框线留 1 列
    let text_w = usize::from(inner.width.saturating_sub(icon_cols + 5));

    for vi in 0..visible {
        let idx = start + vi;
        // 先把本行数据拷出来：下面要 &mut app 触发缩略图加载，不能持有 worlds 借用
        let Some((name, dir, last_played, size)) = app.worlds.get(idx).map(|w| {
            (
                w.name.clone(),
                w.dir.clone(),
                w.last_played.clone(),
                w.size.clone(),
            )
        }) else {
            break;
        };
        let selected = idx == app.selected_world;

        let y = inner.y + vi as u16 * ROW_PITCH;
        let row_h = ROW_PITCH.min(inner.bottom().saturating_sub(y));
        if row_h == 0 {
            break;
        }

        // 选中行：先铺框（描边进度决定画到哪），内容后画盖在上面
        if selected {
            draw_sel_border(
                frame,
                Rect {
                    x: inner.x,
                    y,
                    width: inner.width,
                    height: row_h,
                },
                app.world_anim_progress(),
            );
        }

        // 行内容：名字/最后游玩/大小。图标位先铺空格，协议图片稍后盖上去。
        // 内容整体缩进 1 列（让开左边框线），文字再让出 1 格 + 图标 + 2 格。
        let name_style = if selected {
            theme::accent().bold()
        } else {
            theme::base()
        };
        let pad = Span::raw(" ".repeat(1 + usize::from(icon_cols) + 2));
        let lines = vec![
            Line::from(vec![
                pad.clone(),
                Span::styled(crate::ui::fit(&name, text_w), name_style),
            ]),
            Line::from(vec![
                pad.clone(),
                Span::styled(format!("最后游玩  {last_played}"), theme::dim()),
            ]),
            Line::from(vec![
                pad,
                Span::styled(format!("大小      {size}"), theme::dim()),
            ]),
        ];
        let text_h = row_h.saturating_sub(1).min(3);
        if text_h > 0 {
            frame.render_widget(
                Paragraph::new(lines[..text_h as usize].to_vec()),
                Rect {
                    x: inner.x + 1,
                    y: y + 1,
                    width: inner.width.saturating_sub(2),
                    height: text_h,
                },
            );
        }

        // 缩略图：没加载过的先入队（None = 加载中/失败占位，防重复生成），
        // 就绪画真图，没有/未就绪画 ◻
        let key = app.world_icon_key(&dir);
        if !app.world_icons.contains_key(&key) {
            match app.world_icon_path(&dir) {
                Some(path) => {
                    app.world_icons.insert(key.clone(), None);
                    app.spawn_world_icon(key.clone(), path);
                }
                None => {
                    app.world_icons.insert(key.clone(), None);
                }
            }
        }
        let icon_h = ICON_ROWS.min(inner.bottom().saturating_sub(y + 1));
        if icon_h == 0 {
            continue;
        }
        let icon_rect = Rect {
            x: inner.x + 2,
            y: y + 1,
            width: icon_cols,
            height: icon_h,
        };
        match app.world_icons.get_mut(&key) {
            Some(Some(proto)) => {
                // 半行（滚动露边）用 Crop 裁底，别把缩略图压扁
                let resize = if icon_h < ICON_ROWS {
                    ratatui_image::Resize::Crop(Some(ratatui_image::CropOptions {
                        clip_top: false,
                        clip_left: false,
                    }))
                } else {
                    ratatui_image::Resize::Scale(None)
                };
                let widget = ratatui_image::StatefulImage::<StatefulProtocol>::new().resize(resize);
                frame.render_stateful_widget(widget, icon_rect, proto);
            }
            _ => {
                frame.render_widget(
                    Paragraph::new(Line::from(Span::styled("◻", theme::dim())))
                        .alignment(Alignment::Center),
                    Rect {
                        x: icon_rect.x,
                        y: icon_rect.y + icon_h / 2,
                        width: icon_cols,
                        height: 1,
                    },
                );
            }
        }
    }
}
