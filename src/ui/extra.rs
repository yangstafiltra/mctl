//! 插件页 / 设置页（视图栏新增的两个整页视图）。
//!
//! 两者都是**只读展示**：实际操作走已有的 `:` 命令行
//! （`:plugin install/update/remove/list`），不在这里另造一套交互。
//! 理由：命令已经全了，再做一份按键交互就是两套真相来源。

use crate::app::{App, HomeView};
use crate::plugin;
use crate::theme;
use crate::ui::{cell_width_of, fit};

/// 本文件的截断统一走 ui::fit（按显示列宽，CJK 安全）
fn truncate(s: &str, n: usize) -> String {
    fit(s, n)
}
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};
use ratatui::Frame;

/// `theme::fg` 是 Color，这里要 Style
fn fg(c: Color) -> Style {
    Style::default().fg(c)
}

/// 圆角边框盒子（标题嵌在边框上）
fn frame_box(title: &str) -> Block<'static> {
    Block::default()
        .title(Span::styled(format!(" {title} "), theme::accent().bold()))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme::border_sel()))
}

/// 空态：居中一段说明 + 一段操作提示
fn empty_state(area_title: &str, msg: &str, hint: &str) -> Paragraph<'static> {
    Paragraph::new(vec![
        Line::raw(""),
        Line::from(Span::styled(msg.to_string(), fg(theme::fg()))).alignment(Alignment::Center),
        Line::raw(""),
        Line::from(Span::styled(hint.to_string(), theme::dim())).alignment(Alignment::Center),
    ])
    .block(frame_box(area_title))
}

// ───────────────────────────── 插件页 ─────────────────────────────

/// 插件页：列出已安装插件（名字/版本/描述/二进制/来源）
pub fn plugins_view(frame: &mut Frame, app: &mut App, area: Rect) {
    let root = app.plugin_root();
    let list = plugin::scan_plugins(&root);

    if list.is_empty() {
        frame.render_widget(
            empty_state(
                " ⬡ 插件 ",
                "尚未安装任何插件",
                "按 : 后输入  plugin install <git-url|路径>   安装第一个插件",
            ),
            area,
        );
        return;
    }

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(vec![
        Span::styled(
            format!("  {} ", list.len()),
            fg(theme::primary()).add_modifier(Modifier::BOLD),
        ),
        Span::styled("个插件已安装", theme::fg_muted()),
        Span::styled(
            format!(
                "   根目录 {}",
                fit(
                    &root.display().to_string(),
                    area.width.saturating_sub(16) as usize
                )
            ),
            theme::dim(),
        ),
    ]));
    lines.push(Line::raw(""));

    for p in &list {
        // 标题行：⬡ 名字 v版本
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(HomeView::Plugins.icon(), theme::accent().bold()),
            Span::raw(" "),
            Span::styled(
                fit(&p.name, area.width.saturating_sub(22) as usize),
                fg(theme::fg()).add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("  v{}", p.version), theme::ok()),
        ]));
        // 描述
        if !p.description.is_empty() {
            lines.push(Line::from(vec![
                Span::raw("    "),
                Span::styled(
                    fit(&p.description, area.width.saturating_sub(6) as usize),
                    theme::fg_muted(),
                ),
            ]));
        }
        // 二进制 + 来源
        let bin = p
            .bin
            .as_ref()
            .map(|b| b.display().to_string())
            .unwrap_or_else(|| "（无二进制）".into());
        lines.push(Line::from(vec![
            Span::styled("    ▸ ", theme::dim()),
            Span::styled("bin  ", theme::dim()),
            Span::styled(
                fit(&bin, area.width.saturating_sub(20) as usize),
                theme::fg_muted(),
            ),
        ]));
        let origin = plugin::origin_url(&p.path).unwrap_or_else(|| "本地安装".into());
        lines.push(Line::from(vec![
            Span::styled("    ▸ ", theme::dim()),
            Span::styled("←    ", theme::dim()),
            Span::styled(
                fit(&origin, area.width.saturating_sub(20) as usize),
                theme::fg_muted(),
            ),
        ]));
        lines.push(Line::raw(""));
    }

    // 底部操作提示
    lines.push(Line::from(Span::styled(
        "  :plugin list  ·  :plugin install <url>  ·  :plugin update [名称]  ·  :plugin remove <名称>",
        theme::dim(),
    )));

    frame.render_widget(Paragraph::new(lines).block(frame_box(" ⬡ 插件 ")), area);
}

// ───────────────────────────── 设置页 ─────────────────────────────

/// 设置页：**铺满主区**的可编辑设置 + 动作按钮。
/// 字段按 [`crate::app::SettingGroup`] 分成小节排版（路径 / 联机 / 资源中心 /
/// 界面 / 隐私），每节一个小标题——找设置先找分类，不用从头扫到底。
/// 键位：↑↓/jk 移动焦点，Tab 下一个，←→ 增减或开关，Enter 编辑
/// （主题行 = 打开主题列表、开关键 = 直接切换），/ 进编辑，
/// 编辑中 Enter 提交 / Esc 取消。未消费的键落回全局（q 退出、f 全屏、a 登录…）。
pub fn settings_view(frame: &mut Frame, app: &mut App, area: Rect) {
    use crate::app::{SettingBtn, SettingField, SettingGroup};
    use ratatui::widgets::BorderType;

    let w = area.width.saturating_sub(2) as usize; // 卡片内宽
    if w < 24 {
        return;
    }
    // 字段名对齐：名字长短不一（「主题」4 列 vs「选中框动画」10 列），
    // 补齐后右边的操作提示才是一列，扫起来不乱。
    let name_w = SettingField::ALL
        .iter()
        .map(|f| cell_width_of(f.label()))
        .max()
        .unwrap_or(0)
        + 3;
    // ── 可编辑字段：按分组排 ──
    let mut body: Vec<Line<'static>> = Vec::new();
    // 说明行的行号，紧凑模式下按需砍（组标题和输入框不动）
    let mut hint_rows: Vec<usize> = Vec::new();
    let mut cur_group: Option<SettingGroup> = None;
    for (i, f) in SettingField::ALL.iter().enumerate() {
        if cur_group != Some(f.group()) {
            cur_group = Some(f.group());
            body.push(Line::from(vec![
                Span::raw(" ── "),
                Span::styled(f.group().title(), theme::accent().bold()),
                Span::styled(" ", theme::dim()),
                Span::styled("─".repeat(12), theme::dim()),
            ]));
        }
        let on = app.set_focus == i;
        let editing = on && app.set_editing;
        let val = if editing {
            app.set_buf.clone()
        } else {
            f.value(app)
        };
        let dirty = on && app.set_dirty;

        body.push(Line::from(vec![
            Span::styled(if on { "▌" } else { " " }, theme::accent().bold()),
            Span::raw(" "),
            Span::styled(
                format!(
                    "{}{}",
                    f.label(),
                    " ".repeat(name_w.saturating_sub(cell_width_of(f.label())))
                ),
                // 焦点字段名 = 一整块底色（与其他列表的选中项同一口径）
                if on {
                    theme::selected_block()
                } else {
                    theme::base()
                },
            ),
            Span::styled(f.action_hint(), theme::dim()),
            Span::raw("   "),
            Span::styled(if dirty { "● 未保存" } else { "" }, theme::warn()),
        ]));
        // 输入框：两侧都有边框，宽度铺满
        let bcol = if on {
            theme::border_sel()
        } else {
            theme::border()
        };
        let inner_w = w.saturating_sub(4);
        let mut txt = fit(&val, inner_w);
        if editing {
            txt.push('▏'); // 光标块
        }
        let pad = inner_w.saturating_sub(cell_width_of(&txt));
        body.push(Line::from(vec![
            Span::raw("   "),
            Span::styled("│", Style::default().fg(bcol)),
            Span::raw(" "),
            Span::styled(
                txt,
                if editing {
                    theme::accent().bold()
                } else {
                    theme::base()
                },
            ),
            Span::raw(" ".repeat(pad)),
            Span::styled("│", Style::default().fg(bcol)),
        ]));
        hint_rows.push(body.len());
        body.push(Line::from(vec![
            Span::raw("     "),
            Span::styled(truncate(f.hint(), w.saturating_sub(5)), theme::dim()),
        ]));
    }

    // ── 只读区（tui-wm-core 无序列化器，写不回 wm.toml）：两列两行，省地方 ──
    let wm = &app.wm;
    let readonly_start = body.len();
    body.push(Line::from(Span::styled(
        " ── 只读（改 wm.toml 后重启生效） ──",
        theme::dim(),
    )));
    let pairs = [
        (
            "dock 宽度",
            format!(
                "{}  ({}–{})",
                wm.dock_width, wm.dock_min_width, wm.dock_max_width
            ),
        ),
        ("强调色/弱化色", format!("{} / {}", wm.accent, wm.dim)),
        ("调整模式", wm.keys.adjust_mode.label().to_string()),
        ("焦点循环", wm.keys.cycle.label().to_string()),
    ];
    let half = w.saturating_sub(3) / 2;
    for pair in pairs.chunks(2) {
        let mut spans = vec![Span::raw("  ")];
        for (k, v) in pair {
            // 标签对齐到 15 列（按显示宽度算，CJK 才不会歪）
            let label = format!("{k} ");
            let lp = 15usize.saturating_sub(cell_width_of(&label));
            spans.push(Span::styled(
                format!("{label}{}", " ".repeat(lp)),
                theme::dim(),
            ));
            let vmax = half.saturating_sub(15 + 2);
            let val = fit(v, vmax);
            let vp = " ".repeat(vmax.saturating_sub(cell_width_of(&val)) + 2);
            spans.push(Span::styled(val, theme::fg_muted()));
            spans.push(Span::raw(vp));
        }
        body.push(Line::from(spans));
    }

    // ── 底部：按钮条 + 提示（有富余空间时钉在底部）──
    let mut footer: Vec<Line<'static>> = Vec::new();
    let mut spans: Vec<Span<'static>> = vec![Span::raw("   ")];
    for (k, b) in SettingBtn::ALL.iter().enumerate() {
        if k > 0 {
            spans.push(Span::raw("  "));
        }
        let on = app.set_focused_btn() == Some(*b);
        spans.push(Span::styled(
            if on {
                format!(" ▸{}◂ ", b.label())
            } else {
                format!(" [{}] ", b.label())
            },
            if on { theme::selected_block() } else { theme::dim() },
        ));
    }
    footer.push(Line::from(spans));
    let msg = if app.set_msg.is_empty() {
        "↑↓/jk 移动 · Tab 下一项 · Enter 编辑 · / 快速编辑 · Alt+数字 离开本页 · q 退出".to_string()
    } else {
        app.set_msg.clone()
    };
    footer.push(Line::from(vec![
        Span::raw("   "),
        Span::styled(
            truncate(&msg, w.saturating_sub(3)),
            if app.set_dirty {
                theme::warn()
            } else {
                theme::fg_muted()
            },
        ),
    ]));

    // ── 绘制：卡片铺满整个主区 ──
    frame.render_widget(
        Block::default()
            .borders(ratatui::widgets::Borders::ALL)
            .border_type(BorderType::Double)
            .border_style(Style::default().fg(theme::border_sel())),
        area,
    );
    let inner_h = area.height.saturating_sub(2);
    let avail = inner_h as usize;
    // 行数预算：装不下时①砍非焦点字段的说明行（字段名 + 输入框永远不裁，它们是
    // 页面主体）②整块砍只读区 ③兜底从尾部裁。绝不画出框外。
    let mut rows = body.clone();
    let mut removed = 0usize;
    if rows.len() + footer.len() > avail {
        for i in (0..hint_rows.len()).rev() {
            if rows.len() + footer.len() <= avail {
                break;
            }
            if i != app.set_focus && hint_rows[i] < rows.len() {
                rows.remove(hint_rows[i]);
                removed += 1;
            }
        }
    }
    if rows.len() + footer.len() > avail {
        let cut = readonly_start.saturating_sub(removed);
        rows.truncate(cut.min(rows.len()));
    }
    while rows.len() + footer.len() > avail && rows.len() > 1 {
        rows.pop();
    }
    let pad = avail.saturating_sub(rows.len() + footer.len());
    let mut all = rows;
    all.extend(std::iter::repeat_n(Line::raw(String::new()), pad));
    all.extend(footer);

    for (i, l) in all.iter().enumerate().take(avail) {
        frame.render_widget(
            Paragraph::new(l.clone()),
            Rect {
                x: area.x + 1,
                y: area.y + 1 + i as u16,
                width: area.width - 2,
                height: 1,
            },
        );
    }
}
