//! 主题选择器浮层（设置页「主题」行 Enter 打开）。
//!
//! 借鉴 常见做法 的 /theme：**列表 + 即时预览**——光标移一条，整界面立刻
//! 换色；Enter 落定、Esc 还原。所见即所得，不用保存后再看效果。
//!
//! 版面目标：**一屏把所有主题列全**（不靠滚动翻页藏主题）——终端够高就
//! 单列到底，不够高自动加列（列优先填充：↑↓ 顺着列往下、到底进下一列）。
//! 布局由 [`layout`] 统一算：键盘（←→ 跳一列）和渲染共用同一份，不做
//! 「渲染时记账」那种藏状态。
//!
//! 主题清单和配色全部来自 `crate::theme`（单一真相来源），这里只负责画：
//! 键位/状态在 `app.rs`（和设置页、资源页同一套「状态在 App、渲染在 ui」切法）。

use crate::app::App;
use crate::theme;
use crate::ui::{cell_width_of, fit};
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use ratatui::Frame;

/// 光标标记列宽（`▌ `）
const MARK_W: usize = 2;
/// 已保存标记列宽（` ●`）
const SAVED_W: usize = 2;
/// 格与格之间的间距
const GAP_W: usize = 1;
/// 底边键位提示（也是浮层最小宽度的下限：提示要放得下）
const HINT: &str = " ↑↓ 预览 · ←→ 跳列 · Enter 确定 · Esc 还原 · ● 已保存 ";

/// 网格布局：几列、几行、每格多宽（主题按**列优先**填充）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Grid {
    pub cols: usize,
    pub rows: usize,
    pub cell_w: usize,
}

impl Grid {
    /// 第 `idx` 个主题落在第几行（列优先，和 `draw` 的排法一致）
    pub fn row_of(&self, idx: usize) -> usize {
        idx % self.rows.max(1)
    }
}

/// 单格宽度 = 光标标记 + 最长主题名 + 已保存标记 + 格间距
fn cell_width() -> usize {
    let label_w = theme::theme_ids()
        .iter()
        .map(|id| cell_width_of(&theme::theme_label(id)))
        .max()
        .unwrap_or(8);
    label_w + MARK_W + SAVED_W + GAP_W
}

/// 按可用区域算布局，目标是一屏列全：
/// 从 1 列开始试——列少 = 列表高、从上往下扫最顺；行数放不下就加一列，
/// 宽度顶到边就停在上一个能用的布局（终端实在太小才由 `draw` 滚动兜底）。
pub fn layout(count: usize, max_w: usize, max_h: usize) -> Grid {
    let count = count.max(1);
    let cell_w = cell_width();
    let mut best = Grid {
        cols: 1,
        rows: count,
        cell_w,
    };
    for cols in 1..=count {
        if cols * cell_w + 2 > max_w {
            break; // 再宽就出屏了
        }
        let rows = count.div_ceil(cols);
        best = Grid { cols, rows, cell_w };
        if rows + 2 <= max_h {
            break; // 一屏放全
        }
    }
    best
}

/// 画主题选择器浮层（`app.theme_picker` 为 None 时什么都不画）
pub fn draw(frame: &mut Frame, app: &App, area: Rect) {
    let Some(tp) = &app.theme_picker else {
        return;
    };
    let ids = theme::theme_ids();
    let g = layout(ids.len(), area.width as usize, area.height as usize);
    // 框宽至少放得下键位提示。宽度按 **ratatui 的口径**（Line::width）算——它
    // 画边框标题时用的就是这个，自己另算一套会差几列、把提示尾部裁掉；
    // 边框左右还要各吃 1 列，所以再加 2。
    let hint_w = Line::from(HINT).width() + 2;
    let box_w = (g.cols * g.cell_w + 2).max(hint_w).min(area.width as usize);
    let box_h = (g.rows + 2).min(area.height as usize);
    let rect = Rect {
        x: area.x + (area.width.saturating_sub(box_w as u16)) / 2,
        y: area.y + (area.height.saturating_sub(box_h as u16)) / 2,
        width: box_w as u16,
        height: box_h as u16,
    };
    // 先清底：浮层下面是设置页，不清会透出输入框边框
    frame.render_widget(Clear, rect);
    let block = Block::default()
        .title(Span::styled(" 主题 ", theme::accent().bold()))
        .title_bottom(Span::styled(HINT, theme::dim()))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme::border_sel()));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    if inner.height == 0 || (inner.width as usize) < MARK_W + SAVED_W + 4 {
        return;
    }
    // 正常尺寸下一屏放得下（row_start 恒 0），只有极小终端才滚动
    let view_rows = inner.height as usize;
    let row_start = if g.rows > view_rows {
        let half = view_rows / 2;
        g.row_of(tp.cursor)
            .saturating_sub(half)
            .min(g.rows - view_rows)
    } else {
        0
    };
    let label_w = g.cell_w - MARK_W - SAVED_W - GAP_W;
    let mut lines: Vec<Line<'static>> = Vec::new();
    for r in row_start..(row_start + view_rows).min(g.rows) {
        let mut spans: Vec<Span<'static>> = Vec::new();
        for c in 0..g.cols {
            let idx = c * g.rows + r;
            let Some(id) = ids.get(idx) else {
                break; // 最后一列不满（count 除不尽）
            };
            let on = idx == tp.cursor;
            let saved = *id == tp.saved;
            // 主题名按显示列宽补齐，CJK 名不会把右邻的格子推歪
            let label = fit(&theme::theme_label(id), label_w);
            let pad = " ".repeat(label_w.saturating_sub(cell_width_of(&label)));
            spans.push(Span::styled(
                if on { "▌ " } else { "  " },
                theme::accent().bold(),
            ));
            spans.push(Span::styled(
                format!("{label}{pad}"),
                if on { theme::selected_block() } else { theme::base() },
            ));
            spans.push(Span::styled(
                if saved { " ●" } else { "  " },
                if saved {
                    Style::default().fg(theme::ok())
                } else {
                    theme::dim()
                },
            ));
            spans.push(Span::raw(" ".repeat(GAP_W)));
        }
        lines.push(Line::from(spans));
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

#[cfg(test)]
mod tests {
    use super::*;

    const N: usize = 40;

    /// 终端很高：单列到底，所有主题一屏放下
    #[test]
    fn layout_uses_one_column_when_terminal_is_tall() {
        let g = layout(N, 100, 60);
        assert_eq!((g.cols, g.rows), (1, N));
        assert!(g.rows + 2 <= 60, "单列也必须放得下");
    }

    /// 终端不够高：自动加列，直到一屏放得下
    #[test]
    fn layout_adds_columns_until_everything_fits() {
        let g = layout(N, 100, 40);
        assert!(g.cols > 1, "40 行放不下 40 个主题，应分列");
        assert!(g.rows + 2 <= 40, "分列后要一屏放全");
        assert!(g.cols * g.cell_w + 2 <= 100, "总宽不能出屏");
        // 能少列就少列：比它列数少的方案必然放不下
        assert!(N.div_ceil(g.cols - 1) + 2 > 40);
    }

    /// 终端太窄：列数只能取放得下的最大值，不强行撑破宽度
    #[test]
    fn layout_never_overflows_width() {
        let g = layout(N, 30, 10);
        assert!(g.cols * g.cell_w + 2 <= 30, "总宽不能出屏");
        assert!(g.rows + 2 > 10, "这么小本来就放不下，交给滚动兜底");
    }

    /// 列优先：同一行的相邻两格隔着一个列的步长
    #[test]
    fn grid_is_column_major() {
        let g = layout(40, 100, 40);
        assert_eq!(g.row_of(0), 0);
        assert_eq!(g.row_of(g.rows), 0, "下一列的第 1 个应在同一行");
        assert_eq!(g.row_of(g.rows + 1), 1);
    }
}
