use crate::app::{App, Focus, MapPage};
use crate::theme;
use crate::ui::fit;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Wrap};
use ratatui::Frame;

const SIDEBAR_W: u16 = 22;

pub fn draw(frame: &mut Frame, app: &mut App, area: Rect) {
    let [source_bar, body, hint_bar] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Fill(1),
        Constraint::Length(1),
    ])
    .areas(area);
    draw_source_bar(frame, app, source_bar);

    use crate::app::ResourceSource;
    match app.source {
        ResourceSource::NetEase => draw_netease(frame, app, body),
        ResourceSource::Mojang => draw_official(frame, app, body),
        ResourceSource::Modrinth => draw_modpack(frame, app, body),
        ResourceSource::Plugins => draw_plugin_versions(frame, app, body),
        ResourceSource::Github => draw_github(frame, app, body),
        ResourceSource::Local => draw_placeholder(frame, app, body, "本地资源（待接入）"),
    }
    draw_hint_bar(frame, app, hint_bar);
}

/// 统一底部提示栏（状态 + 快捷键，一行；随页面变化）
fn draw_hint_bar(frame: &mut Frame, app: &mut App, area: Rect) {
    use crate::app::{MapPage, ResourceSource};
    let hint: &str = match app.source {
        ResourceSource::NetEase if app.cred.is_none() => {
            " Enter 登录网易账号   [ ] 切换到 Modrinth / Mojang（无需登录） "
        }
        ResourceSource::NetEase => match app.map_page {
            MapPage::Grid if app.grid_searching => " 输入搜索词   Enter 搜索   Esc 取消 ",
            MapPage::Grid => " Tab 焦点   ↑↓←→ 选择   Enter 详情   / 搜索   s 排序   r 刷新 ",
            MapPage::Detail => " Tab 焦点   ↑↓ 选择版本   Enter 下载（不跳页，可连点）   Esc 返回 ",
            MapPage::Download => " c 清除已结束   Esc 返回详情 ",
        },
        ResourceSource::Mojang => " ↑↓ 选择版本   Enter 安装   r 刷新   [ ] 切换来源 ",
        ResourceSource::Plugins => " ↑↓ 选择   Enter 安装/启动   d 删除   r 刷新   [ ] 切换来源 ",
        ResourceSource::Modrinth if app.mr_detail_open => " ↑↓ 选择版本   Enter 安装   Esc 返回 ",
        ResourceSource::Modrinth => {
            " Tab 焦点   ↑↓←→ 选择/分类   Enter 选版本   / 搜索   v 版本条件   s 排序   r 刷新   [ ] 切换来源 "
        }
        ResourceSource::Github if app.gh_search_input => {
            " 输入关键词   Enter 搜索（GitHub 配额 10 次/分钟）   Esc 取消 "
        }
        ResourceSource::Github => " ↑↓ 选择   Enter 安装插件   / 搜索   r 重新拉取   [ ] 切换来源 ",
        ResourceSource::Local => " [ ] 切换来源 ",
    };
    // 状态（截断防溢出）
    let status = match app.source {
        ResourceSource::NetEase => app.status.clone(),
        ResourceSource::Mojang | ResourceSource::Modrinth => app.dl.install_status.clone(),
        _ => String::new(),
    };
    let mut spans = Vec::new();
    if !status.is_empty() {
        let s = truncate(&status, (area.width as usize / 3).max(16));
        spans.push(Span::styled(format!(" {s} "), theme::warn()));
        spans.push(Span::styled("|", theme::fg_muted()));
    }
    spans.push(Span::styled(hint, theme::fg_muted()));
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// 来源条：[NetEase] [Mojang] [Modrinth] [GitHub] [Local]
/// 选中项是**底色块**（`theme::selected_block()`，跟随主题/经典主题 = 蓝底黑字），
/// 切换来源时底色块从左往右刷过去（`App::src_anim_progress`），
/// 刷到哪一列哪一列翻身；前端一列用亮头部色（`theme::head`）。
fn draw_source_bar(frame: &mut Frame, app: &mut App, area: Rect) {
    let mut spans = vec![Span::styled(" 来源 ", theme::dim())];
    let p = app.src_anim_progress();
    for (i, src) in crate::app::ResourceSource::ALL.iter().enumerate() {
        let active = *src == app.source;
        let label = format!(" {} ", src.label());
        if i > 0 {
            spans.push(Span::raw(" "));
        }
        if active {
            spans.extend(active_chip(&label, p));
        } else {
            spans.push(Span::styled(label, Style::default().fg(theme::fg_muted())));
        }
    }
    spans.push(Span::raw("   "));
    spans.push(Span::styled("[ ] 切换来源", theme::dim()));
    let line = Line::from(spans);
    let block = Block::default().borders(Borders::ALL);
    frame.render_widget(Paragraph::new(line).block(block), area);
}

/// 活动 chip 的刷色切片：`p` 之前铺选中底色（字色同步翻成选中字色），
/// 正在刷的那一列叠亮头部色，未刷到的部分保持未选中样式。
/// p >= 1（或还没播过）→ 整块选中底色。
fn active_chip(label: &str, p: f32) -> Vec<Span<'static>> {
    let w = crate::ui::cell_width_of(label);
    if w == 0 {
        return vec![Span::styled(label.to_string(), theme::selected_block())];
    }
    if p >= 1.0 {
        return vec![Span::styled(label.to_string(), theme::selected_block())];
    }
    let block = theme::selected_block();
    let inactive = Style::default().fg(theme::fg_muted());
    let cut = crate::ui::home::wipe_width(w, p).min(w);
    if cut == 0 {
        return vec![Span::styled(label.to_string(), inactive)];
    }
    let head_style = theme::selected_block()
        .fg(theme::head())
        .add_modifier(Modifier::BOLD);
    let mut out: Vec<Span<'static>> = Vec::new();
    // 刷到的部分（除最后一列）→ 选中底色
    let (done_raw, rest) = crate::ui::split_cells(label, cut - 1);
    if !done_raw.is_empty() {
        out.push(Span::styled(done_raw.to_string(), block));
    }
    // 头部那一列 → 亮色
    let (head_raw, tail) = crate::ui::split_cells(&rest, 1);
    if !head_raw.is_empty() {
        out.push(Span::styled(head_raw.to_string(), head_style));
    }
    // 还没刷到的部分 → 保持未选中样式
    if !tail.is_empty() {
        out.push(Span::styled(tail.to_string(), inactive));
    }
    out
}

fn draw_placeholder(frame: &mut Frame, app: &mut App, area: Rect, msg: &str) {
    let _ = app;
    let lines = vec![
        Line::raw(""),
        Line::from(Span::styled(msg, theme::dim())).alignment(Alignment::Center),
        Line::from(Span::styled("（按 ] 或 [ 切换资源源）", theme::dim()))
            .alignment(Alignment::Center),
    ];
    frame.render_widget(
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL)),
        area,
    );
}

/// GitHub 插件发现：搜索框 + 仓库列表，回车即装
fn draw_github(frame: &mut Frame, app: &mut App, area: Rect) {
    use crate::api::github::PLUGIN_TOPIC;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::accent())
        .title(format!(
            " ⌘ GitHub 插件 {}{} ",
            if app.dl.query.is_empty() {
                format!("topic:{PLUGIN_TOPIC}")
            } else {
                format!("“{}”", app.dl.query)
            },
            app.gh_rate
        ));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height < 3 {
        return;
    }
    let [search_area, list_area, foot_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(1),
    ])
    .areas(inner);

    // 搜索框
    let cursor = if app.gh_search_input { "▌" } else { " " };
    let q = if app.dl.query.is_empty() {
        Span::styled(format!(" 搜索（/{cursor}）…"), theme::dim())
    } else {
        Span::styled(format!(" /{cursor}{}", app.dl.query), theme::fg())
    };
    frame.render_widget(Paragraph::new(Line::from(q)), search_area);

    // 列表
    let mut items: Vec<ListItem> = Vec::new();
    if app.gh_searching {
        items.push(ListItem::new(Line::from(Span::styled(
            "  搜索中…",
            theme::dim(),
        ))));
    } else if app.gh_filtered.is_empty() {
        items.push(ListItem::new(Line::from(Span::styled(
            if app.gh_results.is_empty() {
                "  没有结果（r 重试）"
            } else {
                "  没有匹配的仓库"
            },
            theme::dim(),
        ))));
    }
    for (row, &idx) in app.gh_filtered.iter().enumerate() {
        let r = &app.gh_results[idx];
        let sel = row == app.gh_selected;
        let star = if r.is_plugin() { "⬡" } else { " " };
        let desc = r.description.clone().unwrap_or_else(|| "（无描述）".into());
        let desc_w = (inner.width as usize).saturating_sub(46);
        let mut spans = vec![
            Span::styled(if sel { "▌ " } else { "  " }, theme::accent().bold()),
            Span::styled(star, theme::primary()),
            Span::raw(" "),
            Span::styled(
                fit(r.short_name(), 26),
                if sel {
                    theme::accent().bold()
                } else {
                    theme::base()
                },
            ),
            Span::styled(format!(" {:>3}★ ", r.stargazers_count), theme::dim()),
            Span::styled(
                fit(&desc, desc_w),
                if sel { theme::base() } else { theme::dim() },
            ),
        ];
        if sel {
            for s in spans.iter_mut() {
                s.style = s.style.add_modifier(Modifier::BOLD);
            }
        }
        if sel {
            // 选中行铺满内宽 + 整块底色（与其它列表同一口径）
            let used: usize = spans.iter().map(|s| crate::ui::cell_width_of(&s.content)).sum();
            if used < inner.width as usize {
                spans.push(Span::raw(" ".repeat(inner.width as usize - used)));
            }
            spans = crate::ui::home::wipe_spans(spans, usize::MAX, theme::selected_block());
        }
        items.push(ListItem::new(Line::from(spans)));
        // 第二行：full_name + 更新日期（仅选中时显示，避免太吵）
        if sel {
            items.push(ListItem::new(Line::from(vec![
                Span::raw("      "),
                Span::styled(
                    fit(
                        &format!("{} · {}", r.full_name, r.updated_day()),
                        (inner.width as usize).saturating_sub(8),
                    ),
                    theme::fg_muted(),
                ),
            ])));
        }
    }
    frame.render_widget(List::new(items), list_area);

    // 底部状态
    let status = if app.dl.installing {
        Span::styled(
            " ⏳ 构建中（cargo build --release，首次可能几分钟）",
            theme::warn(),
        )
    } else if !app.dl.install_status.is_empty() {
        Span::styled(
            format!(
                " {}",
                truncate(&app.dl.install_status, inner.width as usize - 2)
            ),
            theme::fg_muted(),
        )
    } else {
        Span::styled(" 选仓库回车安装", theme::dim())
    };
    frame.render_widget(Paragraph::new(Line::from(status)), foot_area);
}

/// NetEase：资源大厅（需要网易账号；未登录只提示，不强求）
fn draw_netease(frame: &mut Frame, app: &mut App, area: Rect) {
    // 未登录：整页只放一句提示。用网易资源不是必须的（Modrinth/官方面板都不依赖它），
    // 所以这里不做任何拦截式引导。
    if app.cred.is_none() {
        draw_need_login(
            frame,
            area,
            "网易资源需要登录网易账号",
            "按 Enter 打开登录页 · [ ] 切换到 Modrinth / Mojang 无需登录",
        );
        return;
    }
    let [sidebar_area, main_area] = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(SIDEBAR_W), Constraint::Fill(1)])
        .areas(area);

    if app.map_page != MapPage::Download {
        draw_sidebar(frame, app, sidebar_area);
    } else {
        frame.render_widget(
            Block::default().borders(Borders::ALL).title(" 资源 "),
            sidebar_area,
        );
    }
    match app.map_page {
        MapPage::Grid => draw_grid(frame, app, main_area),
        MapPage::Detail => draw_detail(frame, app, main_area),
        MapPage::Download => draw_download(frame, app, main_area),
    }
}

/// 「请登录」占位页（居中两行：提示 + 下一步）
fn draw_need_login(frame: &mut Frame, area: Rect, title: &str, hint: &str) {
    let lines = vec![
        Line::raw(""),
        Line::from(Span::styled(title, theme::warn())).alignment(Alignment::Center),
        Line::raw(""),
        Line::from(Span::styled(hint, theme::dim())).alignment(Alignment::Center),
    ];
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" 资源下载（测试中） "),
        ),
        area,
    );
}

fn draw_sidebar(frame: &mut Frame, app: &mut App, area: Rect) {
    let names: Vec<&str> = crate::domain::ALL_CATEGORIES
        .iter()
        .map(|c| c.name)
        .collect();
    draw_cat_sidebar(
        frame,
        app,
        area,
        &names,
        app.category,
        app.focus == Focus::Sidebar,
        "资源大厅",
    );
}

/// 通用分类侧栏（网易/Modrinth 共用）
fn draw_cat_sidebar(
    frame: &mut Frame,
    _app: &mut App,
    area: Rect,
    cats: &[&str],
    selected: usize,
    focused: bool,
    title: &str,
) {
    let items: Vec<ListItem> = cats
        .iter()
        .enumerate()
        .map(|(i, name)| {
            // 选中项铺满侧栏内宽（底部整块底色，不只是字上一条）
            let inner_w = area.width.saturating_sub(2) as usize;
            let pad = inner_w.saturating_sub(crate::ui::cell_width_of(name) + 2);
            let label = format!(" {name}{} ", " ".repeat(pad));
            let item = if i == selected {
                ListItem::new(Span::styled(
                    label,
                    if focused {
                        // 选中项 = 底色块（对齐备份 v15 的手感：整行带底色）
                        theme::selected_block()
                    } else {
                        Style::default()
                            .fg(theme::accent_color())
                            .add_modifier(Modifier::BOLD)
                    },
                ))
            } else {
                ListItem::new(Span::styled(label, Style::default().fg(theme::fg())))
            };
            item
        })
        .collect();

    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .title(format!(" {title} "))
            .border_style(if focused {
                Style::default().fg(theme::border_sel())
            } else {
                Style::default().fg(theme::border())
            }),
    );
    frame.render_widget(list, area);
}

fn draw_grid(frame: &mut Frame, app: &mut App, area: Rect) {
    use crate::app::NeteaseSort;
    // 搜索区独立方框（Modrinth 同款），标题 = 分类名 + 数量
    let [search_area, grid_area] =
        Layout::vertical([Constraint::Length(3), Constraint::Fill(1)]).areas(area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(if app.focus == Focus::Grid {
            Style::default().fg(theme::border_sel())
        } else {
            Style::default().fg(theme::border())
        })
        .title(Line::from(vec![Span::styled(
            format!(
                " {}  ({})  ",
                crate::domain::ALL_CATEGORIES[app.category].name,
                app.grid_items.len()
            ),
            theme::fg(),
        )]));
    let inner = block.inner(search_area);
    frame.render_widget(block, search_area);

    // 搜索极左 / 排序极右（分栏，右边框分隔）
    let [left, right] =
        Layout::horizontal([Constraint::Length(22), Constraint::Fill(1)]).areas(inner);
    let mut sline = Line::from(vec![
        Span::styled("搜索: ", theme::dim()),
        Span::raw(app.grid_query.clone()),
    ]);
    if app.grid_searching {
        sline.push_span(Span::styled("▌", theme::accent()));
    } else {
        sline.push_span(Span::styled("  (/ 输入)", theme::fg_muted()));
    }
    frame.render_widget(Paragraph::new(sline), left);
    let mut rline = Line::default();
    for s in NeteaseSort::ALL {
        let cur = s == app.grid_sort;
        rline.push_span(if cur {
            Span::styled(format!(" [{}] ", s.label()), theme::selected_block())
        } else {
            Span::styled(format!(" {} ", s.label()), theme::dim())
        });
    }
    rline.push_span(Span::styled(" (s)", theme::fg_muted()));
    frame.render_widget(Paragraph::new(rline).alignment(Alignment::Right), right);

    if app.grid_items.is_empty() {
        let msg = if app.grid_loading {
            "加载中…".to_string()
        } else if app.status.starts_with("加载失败:") {
            app.status.clone()
        } else {
            "空，按 r 刷新".to_string()
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(msg, theme::fg_muted()))),
            grid_area,
        );
        return;
    }

    draw_card_grid(
        frame,
        app,
        grid_area,
        app.grid_items.len(),
        app.grid_selected,
        |f, a, cell, i| {
            draw_card(f, a, cell, i);
        },
        |a, i| {
            if a.grid_covers.get(i).map(|c| c.is_none()).unwrap_or(false)
                && !a.cover_queue.contains(&i)
            {
                a.cover_queue.push(i);
                a.spawn_cover(i);
            }
        },
    );
}

/// 通用网格渲染：分页计算 + 回调绘制每个可见卡片（网易/Modrinth 共用）
/// prefetch：渲染后预加载当前页+下一页封面（滚动顺滑，无空白闪烁）
fn draw_card_grid(
    frame: &mut Frame,
    app: &mut App,
    area: Rect,
    count: usize,
    selected: usize,
    mut card: impl FnMut(&mut Frame, &mut App, Rect, usize),
    mut prefetch: impl FnMut(&mut App, usize),
) {
    let cols = (area.width / 24).max(2) as usize;
    let rows = (area.height / 14).max(1) as usize;
    app.grid_cols = cols;
    app.grid_rows = rows;
    let page = cols * rows;
    let start = selected / page * page;
    let visible: Vec<usize> = (start..(start + page).min(count)).collect();
    let grid = Layout::default()
        .direction(Direction::Vertical)
        .constraints((0..rows).map(|_| Constraint::Fill(1)).collect::<Vec<_>>())
        .split(area);
    for (r, row_area) in grid.iter().enumerate() {
        let cells = Layout::default()
            .direction(Direction::Horizontal)
            .constraints((0..cols).map(|_| Constraint::Fill(1)).collect::<Vec<_>>())
            .split(*row_area);
        for (c, cell) in cells.iter().enumerate() {
            let idx = r * cols + c;
            if idx >= visible.len() {
                continue;
            }
            card(frame, app, *cell, visible[idx]);
        }
    }
    // 预加载当前页 + 下一页封面
    for i in start..(start + page * 2).min(count) {
        prefetch(app, i);
    }
}

fn draw_card(frame: &mut Frame, app: &mut App, area: Rect, idx: usize) {
    let Some(item) = app.grid_items.get(idx).cloned() else {
        return;
    };
    let selected = app.grid_selected == idx;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(if selected {
            Style::default().fg(theme::border_sel())
        } else {
            Style::default().fg(theme::border())
        });
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let [img_area, text_area] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(55), Constraint::Fill(1)])
        .areas(inner);

    // 封面图
    if idx < app.grid_covers.len() {
        if let Some(protocol) = app.grid_covers[idx].as_mut() {
            frame.render_stateful_widget(
                ratatui_image::StatefulImage::<ratatui_image::protocol::StatefulProtocol>::new(),
                img_area,
                protocol,
            );
        } else {
            if !app.cover_queue.contains(&idx) {
                app.cover_queue.push(idx);
                app.spawn_cover(idx);
            }
            frame.render_widget(
                Paragraph::new("🖼")
                    .style(Style::default().fg(theme::fg_muted()))
                    .alignment(Alignment::Center),
                img_area,
            );
        }
    }

    // BE（基岩版）角标：异步检查（失败重试最多 3 次），左下角显示
    if idx < app.grid_be.len()
        && app.grid_be[idx].is_none()
        && app.be_tries.get(idx).is_none_or(|&t| t < 3)
        && !app.be_queue.contains(&idx)
    {
        app.be_queue.push(idx);
        app.spawn_be_check(idx);
    }
    if app.grid_be.get(idx).and_then(|b| *b).unwrap_or(false) {
        let [_, be_row] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(img_area);
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "BE",
                Style::default()
                    .fg(theme::primary())
                    .add_modifier(Modifier::BOLD),
            ))),
            be_row,
        );
    }

    // 文本
    let name = truncate(&item.name, inner.width as usize);
    let dev = truncate(&item.developer_name, inner.width as usize);
    let dl = format_downloads(item.download_num);
    let date = if item.publish_time > 0 {
        unix_to_date(item.publish_time)
    } else {
        String::new()
    };
    let lines = vec![
        Line::from(Span::styled(
            name,
            if selected {
                Style::default()
                    .fg(theme::primary())
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme::fg())
            },
        )),
        Line::from(Span::styled(dev, theme::fg_muted())),
        Line::from(Span::styled(
            format!("▼{}  ☆{}  {}", dl, item.like_num, date),
            theme::ok(),
        )),
    ];
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), text_area);
}

/// UNIX 秒 → YY-MM-DD（无 chrono 依赖）
fn unix_to_date(ts: i64) -> String {
    let days = ts.div_euclid(86400);
    let (y, m, d) = civil_from_days(days);
    format!("{y:02}-{m:02}-{d:02}")
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y } % 100, m, d)
}

/// 去 HTML 标签 + 转义实体（<p>/<br> 等块级标签 → 换行）
fn strip_html(raw: &str) -> String {
    let s = raw
        .replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&shy;", "")
        .replace("&middot;", "·")
        .replace("\r", "");
    let mut out = String::new();
    let mut tag = String::new();
    let mut in_tag = false;
    let mut prev_nl = false;
    for c in s.chars() {
        if c == '<' {
            in_tag = true;
            tag.clear();
        } else if c == '>' {
            in_tag = false;
            let t = tag.trim_start_matches('/').to_ascii_lowercase();
            let block = matches!(
                t.as_str(),
                "p" | "br"
                    | "div"
                    | "li"
                    | "ul"
                    | "ol"
                    | "h1"
                    | "h2"
                    | "h3"
                    | "h4"
                    | "img"
                    | "table"
                    | "tr"
                    | "hr"
            );
            if block && !prev_nl {
                out.push('\n');
                prev_nl = true;
            }
        } else if in_tag {
            tag.push(c);
        } else {
            out.push(c);
            prev_nl = false;
        }
    }
    while out.starts_with('\n') {
        out.remove(0);
    }
    while out.ends_with('\n') {
        out.pop();
    }
    out
}

fn draw_detail(frame: &mut Frame, app: &mut App, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::border_sel()))
        .title(format!(
            " {} 详情 ",
            app.detail
                .as_ref()
                .map(|d| d.name.as_str())
                .unwrap_or("加载中")
        ));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if app.detail.is_none() {
        frame.render_widget(
            Paragraph::new(Span::styled(
                if app.detail_loading {
                    "加载中…"
                } else {
                    "无数据，按 Esc 返回"
                },
                theme::fg_muted(),
            )),
            inner,
        );
        return;
    }

    let d = app.detail.as_ref().unwrap();
    let [left, right] = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(45), Constraint::Fill(1)])
        .areas(inner);

    // 左：大图 + 简介 + 详情（Info 焦点时边框高亮）
    let [cover_area, brief_area, desc_area] = Layout::vertical([
        Constraint::Percentage(38),
        Constraint::Length(4),
        Constraint::Fill(1),
    ])
    .areas(left);
    let focus_sel = app.detail_focus == crate::app::DetailFocus::Info;
    let info_border = if focus_sel {
        Style::default().fg(theme::border_sel())
    } else {
        Style::default().fg(theme::border())
    };
    let cover_block = Block::default()
        .borders(Borders::ALL)
        .border_style(info_border);
    let cover_inner = cover_block.inner(cover_area);
    frame.render_widget(cover_block, cover_area);
    if let Some(protocol) = app.detail_cover.as_mut() {
        frame.render_stateful_widget(
            ratatui_image::StatefulImage::<ratatui_image::protocol::StatefulProtocol>::new(),
            cover_inner,
            protocol,
        );
    } else {
        frame.render_widget(
            Paragraph::new("🖼").alignment(Alignment::Center),
            cover_inner,
        );
    }

    // 简介（图片下方，brief_summary）
    let brief_block = Block::default()
        .borders(Borders::ALL)
        .title(" 简介 ")
        .border_style(info_border);
    let brief_inner = brief_block.inner(brief_area);
    frame.render_widget(brief_block, brief_area);
    let summary = if d.brief_summary.is_empty() {
        "无简介".to_string()
    } else {
        d.brief_summary.clone()
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(summary, theme::fg()))).wrap(Wrap { trim: true }),
        brief_inner,
    );

    // 详情正文（detail_description 去 HTML 标签，↑↓ 滚动）
    let desc_block = Block::default()
        .borders(Borders::ALL)
        .title(" 详情 (↑↓ 滚动) ")
        .border_style(info_border);
    let desc_inner = desc_block.inner(desc_area);
    frame.render_widget(desc_block, desc_area);
    let desc = if d.detail_description.is_empty() {
        "无详情".to_string()
    } else {
        strip_html(&d.detail_description)
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(desc, theme::fg())))
            .wrap(Wrap { trim: true })
            .scroll((app.detail_scroll, 0)),
        desc_inner,
    );

    // 右：信息 + 版本列表
    let right_chunks: [Rect; 3] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(5),
            Constraint::Fill(1),
            Constraint::Length(3),
        ])
        .areas(right);

    let info = vec![
        Line::from(Span::styled(
            &d.name,
            Style::default()
                .fg(theme::primary())
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            format!("作者: {}", d.developer_name),
            theme::fg_muted(),
        )),
        Line::from(Span::styled(
            format!(
                "下载: {}  点赞: {}",
                format_downloads(d.download_num),
                d.like_num
            ),
            theme::ok(),
        )),
        Line::from(Span::styled(
            format!("标签: {}", d.network_tag),
            theme::fg_muted(),
        )),
    ];
    frame.render_widget(Paragraph::new(info), right_chunks[0]);

    // 版本列表
    let items: Vec<ListItem> = app
        .detail_versions
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let label = format!(
                "  {}  (MC {} | {} KB | {}字节)",
                v.mc_version_name,
                v.java_version,
                v.res_size / 1024,
                v.res_size,
            );
            if i == app.detail_selected {
                ListItem::new(Span::styled(label, theme::selected_block()))
            } else {
                ListItem::new(Span::styled(label, theme::fg()))
            }
        })
        .collect();
    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" 选择版本下载 ")
            .border_style(if app.detail_focus == crate::app::DetailFocus::Versions {
                Style::default().fg(theme::border_sel())
            } else {
                Style::default().fg(theme::border())
            }),
    );
    frame.render_widget(list, right_chunks[1]);

    // 下载位置约束框（Download 焦点时高亮，可编辑路径）
    let dl_focus = app.detail_focus == crate::app::DetailFocus::Download;
    let dl_block = Block::default()
        .borders(Borders::ALL)
        .title(if dl_focus {
            " 下载位置 (输入修改  Enter 确认) "
        } else {
            " 下载位置 "
        })
        .border_style(if dl_focus {
            Style::default().fg(theme::border_sel())
        } else {
            Style::default().fg(theme::border())
        });
    let dl_inner = dl_block.inner(right_chunks[2]);
    frame.render_widget(dl_block, right_chunks[2]);
    let mut dline = Line::from(vec![
        Span::styled("下载到: ", theme::dim()),
        Span::raw(app.download_dir_str.clone()),
    ]);
    if dl_focus {
        dline.push_span(Span::styled("▌", theme::accent()));
    }
    frame.render_widget(Paragraph::new(dline), dl_inner);
}

/// 人类可读字节数（仅测试覆盖）
#[cfg(test)]
fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i + 1 < UNITS.len() {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else if v < 10.0 {
        format!("{v:.1} {}", UNITS[i])
    } else {
        format!("{v:.0} {}", UNITS[i])
    }
}

/// braille 旋转 spinner。帧号由经过时间推导，无需额外 tick。
const SPINNER: [&str; 8] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"];

/// 正在下载的文件才转；已完成的静止，返回空串。
fn spinner_for(started: std::time::Instant, active: bool) -> &'static str {
    if !active {
        return "";
    }
    // 120ms 一帧，和 UI 的 100ms 刷新节奏接近
    let frame = (started.elapsed().as_millis() / 120) as usize % SPINNER.len();
    SPINNER[frame]
}

/// 细线：一行细线 + 一行信息（名字左，字节/百分比/速度右）。
/// 已下载部分是实心 `━`，未完成是极细虚线 `┄`。
fn thin_line(width: usize, pct: f64, style: ratatui::style::Style) -> Line<'static> {
    let w = width.max(1);
    let filled = ((pct.clamp(0.0, 1.0)) * w as f64).round() as usize;
    let filled = filled.min(w);
    let mut s = String::with_capacity(w * 3);
    s.push_str(&"━".repeat(filled));
    s.push_str(&"┄".repeat(w - filled));
    Line::from(Span::styled(s, style))
}

/// 涌动波的时钟：40ms 走一格。所有进度条共用，按任务 id 错开相位。
fn anim_phase() -> usize {
    use std::sync::OnceLock;
    static T0: OnceLock<std::time::Instant> = OnceLock::new();
    let t0 = T0.get_or_init(std::time::Instant::now);
    t0.elapsed().as_millis() as usize / 40
}

/// 进度条一格的颜色：波峰最亮，两侧加粗过渡，其余保持主色。
/// 用环绕距离，波从右边界出去时同时从左边界进来，看起来是连续涌动。
fn wave_style(x: usize, phase: usize, bw: usize, age: f32) -> Style {
    let base = theme::fade(Style::default().fg(theme::accent_color()), age);
    if bw == 0 {
        return base;
    }
    let phase = phase % bw;
    let raw = x.min(bw - 1).abs_diff(phase);
    let d = raw.min(bw - raw);
    if d <= 1 {
        theme::fade(
            Style::default()
                .fg(theme::head())
                .add_modifier(Modifier::BOLD),
            age,
        )
    } else if d <= 3 {
        theme::fade(
            Style::default()
                .fg(theme::accent_color())
                .add_modifier(Modifier::BOLD),
            age,
        )
    } else {
        base
    }
}

/// 大进度条：完成段用 `█`，未完成段用 `░`，完成段里有一道来回涌动的亮波。
/// 同一颜色的相邻格合成一个 Span，宽度不变。
fn big_bar_spans(filled: usize, bw: usize, phase: usize, age: f32) -> Vec<Span<'static>> {
    if bw == 0 {
        return Vec::new();
    }
    let filled = filled.min(bw);
    let track = theme::fade(Style::default().fg(theme::fg_muted()), age);
    let mut out: Vec<Span<'static>> = Vec::new();
    let mut buf = String::new();
    let mut cur: Option<Style> = None;
    for x in 0..bw {
        let st = if x >= filled {
            track
        } else if x + 1 == filled {
            // 笔尖：当前下载到的位置永远亮着
            theme::fade(
                Style::default()
                    .fg(theme::head())
                    .add_modifier(Modifier::BOLD),
                age,
            )
        } else {
            wave_style(x, phase, bw, age)
        };
        if cur != Some(st) {
            if !buf.is_empty() {
                out.push(Span::styled(std::mem::take(&mut buf), cur.unwrap()));
            }
            cur = Some(st);
        }
        buf.push(if x < filled { '█' } else { '░' });
    }
    if !buf.is_empty() {
        out.push(Span::styled(buf, cur.unwrap()));
    }
    out
}

/// 下载中心主页渲染（Alt+6）。复用资源页内部的同一个下载视图，
/// 别写第二份——两处渲染漂移过一次，状态显示就不一致了。
pub fn draw_download_center(frame: &mut Frame, app: &mut App, area: Rect) {
    draw_download(frame, app, area)
}

fn draw_download(frame: &mut Frame, app: &mut App, area: Rect) {
    use crate::app::{human_rate, human_size, InstallStatus};

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" 下载中心 ")
        .border_style(Style::default().fg(theme::border_sel()));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let width = inner.width as usize;
    let mut lines: Vec<Line> = Vec::new();
    let (active_n, done_n, failed_n, bd, bt, speed) = app.download_summary();

    // ---- 汇总条：几个在跑 / 完事 / 挂掉 + 总速度 + 剩余时间 ----
    let mut head: Vec<Span> = vec![Span::styled(" 进行中 ", theme::dim())];
    head.push(Span::styled(
        active_n.to_string(),
        Style::default().fg(if active_n > 0 {
            theme::primary()
        } else {
            theme::fg_muted()
        }),
    ));
    head.push(Span::styled(" · 完成 ", theme::dim()));
    head.push(Span::styled(done_n.to_string(), theme::ok()));
    if failed_n > 0 {
        head.push(Span::styled(" · 失败 ", theme::dim()));
        head.push(Span::styled(failed_n.to_string(), theme::error()));
    }
    if active_n > 0 {
        head.push(Span::styled("   总速度 ", theme::dim()));
        head.push(Span::styled(human_rate(speed), theme::primary()));
        if let Some(eta) = app.download_eta() {
            head.push(Span::styled(
                format!(" · 剩余 {}", human_duration(eta)),
                theme::dim(),
            ));
        }
    }
    lines.push(Line::from(head));

    // 所有进行中任务的合计进度（按字节；长度未知时不画）
    if active_n > 0 && bt > 0 {
        let bw = width.saturating_sub(26).max(10);
        let pct = (bd as f64 / bt as f64).clamp(0.0, 1.0);
        let filled = ((pct * bw as f64) as usize).min(bw);
        let mut spans = vec![Span::styled(" ", theme::dim())];
        spans.extend(big_bar_spans(filled, bw, anim_phase(), 1.0));
        spans.push(Span::styled(
            format!(" {:>3}% ", (pct * 100.0) as u32),
            theme::primary(),
        ));
        spans.push(Span::styled(
            format!("{}/{}", human_size(bd), human_size(bt)),
            theme::dim(),
        ));
        lines.push(Line::from(spans));
    }
    lines.push(Line::from(""));

    // ---- 进行中：每个任务一段（任务总进度 + 大文件细线） ----
    let active: Vec<&crate::app::InstallTask> = app
        .install_tasks
        .iter()
        .filter(|t| t.status == InstallStatus::Active)
        .collect();
    if !active.is_empty() {
        lines.push(section_line("进行中"));
        let n = active.len();
        for (i, t) in active.iter().enumerate() {
            // 旧的下沉：越靠前越淡，最新的（最后一个）最亮
            let age = (i + 1) as f32 / n.max(1) as f32;
            push_task_lines(&mut lines, t, width, age);
        }
        lines.push(Line::from(""));
    }

    // ---- 已完成（最近的在前，最多 8 条） ----
    let done: Vec<&crate::app::InstallTask> = app
        .install_tasks
        .iter()
        .filter(|t| t.status == InstallStatus::Done)
        .collect();
    if !done.is_empty() {
        lines.push(section_line(&format!("已完成 · {}", done.len())));
        for t in done.iter().rev().take(8) {
            lines.push(Line::from(vec![
                Span::styled(" ✓ ", theme::ok()),
                Span::styled(t.name.clone(), theme::base()),
                Span::styled(format!(" · {}", t.phase), theme::dim()),
            ]));
        }
        if done.len() > 8 {
            lines.push(Line::from(Span::styled(
                format!("   … 还有 {} 条（按 c 清理）", done.len() - 8),
                theme::dim(),
            )));
        }
        lines.push(Line::from(""));
    }

    // ---- 失败：给完整原因，别只留个红叉 ----
    let failed: Vec<&crate::app::InstallTask> = app
        .install_tasks
        .iter()
        .filter(|t| t.status == InstallStatus::Failed)
        .collect();
    if !failed.is_empty() {
        lines.push(section_line(&format!("失败 · {}", failed.len())));
        for t in failed.iter().rev().take(6) {
            lines.push(Line::from(Span::styled(
                format!(" ✗ {} · {}", t.name, t.phase),
                theme::error(),
            )));
        }
        lines.push(Line::from(""));
    }

    if app.install_tasks.is_empty() {
        lines.push(Line::from(Span::styled(
            " 暂无下载。资源页选好版本回车，就会出现在这里（可同时下多个）。",
            theme::dim(),
        )));
        lines.push(Line::from(Span::styled(
            " Alt+6 随时回到本页查看。",
            theme::dim(),
        )));
    }

    lines.push(Line::from(Span::styled(
        " c 清除已结束任务 · Esc 返回",
        theme::fg_muted(),
    )));
    frame.render_widget(Paragraph::new(lines), inner);
}

/// 分区标题
fn section_line(title: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled("▎", theme::primary()),
        Span::styled(title.to_string(), theme::primary()),
    ])
}

/// 单个下载：长进度条（整条，按字节）+ 下面的文件细线（保留原来的细线口径）。
/// 节奏参考 cargo-pretty：左侧标签槽固定宽度，条子拉长，右端百分比；
/// 同一时刻可以有多个下载，每条各占一段，旧的下沉（淡出），新的最亮。
fn push_task_lines(
    lines: &mut Vec<Line<'static>>,
    t: &crate::app::InstallTask,
    width: usize,
    age: f32,
) {
    let width = width.saturating_sub(2);
    let spin = spinner_for(t.started, true);
    let elapsed = human_duration(t.started.elapsed().as_secs());
    // 右对齐的计时器（任务跑了多久）
    let left_w = width.saturating_sub(crate::ui::cell_width_of(&elapsed) + 1);
    let mut name = format!("{spin} {}", t.name);
    if !t.phase.is_empty() {
        name.push_str(&format!(" · {}", t.phase));
    }
    name = truncate(&name, left_w.max(8));
    let pad = left_w.saturating_sub(crate::ui::cell_width_of(&name)).max(1);
    lines.push(Line::from(vec![
        Span::styled(
            name,
            theme::fade(Style::default().fg(theme::primary()), age),
        ),
        Span::raw(" ".repeat(pad)),
        Span::styled(elapsed, theme::fade(theme::dim(), age)),
    ]));

    // 长进度条：整条按字节走，完成段里有涌动波
    if t.bytes_total > 0 {
        let pct = t.pct();
        let label = " 进度 ";
        let gutter = crate::ui::cell_width_of(label);
        let tail = format!(
            " {:>3}%  {}/{}",
            (pct * 100.0) as u32,
            crate::app::human_size(t.bytes_done),
            crate::app::human_size(t.bytes_total)
        );
        let tail_w = crate::ui::cell_width_of(&tail);
        let bw = width.saturating_sub(gutter + tail_w + 1).max(8);
        let filled = ((pct * bw as f64) as usize).min(bw);
        // 任务 id 错开相位：多条并发时不会整屏同步闪
        let phase = anim_phase().wrapping_add(t.id as usize * 7);
        let mut spans = vec![Span::styled(label, theme::fade(theme::dim(), age))];
        spans.extend(big_bar_spans(filled, bw, phase, age));
        spans.push(Span::styled(
            tail,
            theme::fade(Style::default().fg(theme::primary()), age),
        ));
        lines.push(Line::from(spans));
    }

    // 文件细线：保留原来的口径（大文件在头，最多 4 条）
    for f in t.files.iter().filter(|f| !f.finished).take(4) {
        let style = if f.failed {
            theme::error()
        } else {
            ratatui::style::Style::default().fg(theme::accent_color())
        };
        lines.push(thin_line(width, f.pct(), style));
        let right = if f.total == 0 {
            format!("{} · 长度未知", crate::app::human_size(f.done))
        } else {
            format!(
                "{}/{} · {}% · {}",
                crate::app::human_size(f.done),
                crate::app::human_size(f.total),
                (f.pct() * 100.0) as u32,
                crate::app::human_rate(f.speed())
            )
        };
        let right_w = crate::ui::cell_width_of(&right);
        let name_w = width.saturating_sub(right_w + 3).max(8);
        let name = truncate(&f.name, name_w);
        let pad = width
            .saturating_sub(crate::ui::cell_width_of(&name) + right_w + 1)
            .max(1);
        lines.push(Line::from(vec![
            Span::styled("  ", theme::dim()),
            Span::styled(name, theme::base()),
            Span::raw(" ".repeat(pad)),
            Span::styled(right, theme::dim()),
        ]));
    }
    let hidden = t.files.iter().filter(|f| !f.finished).count().saturating_sub(4);
    if hidden > 0 {
        lines.push(Line::from(Span::styled(
            format!("  … 还有 {hidden} 个文件在下"),
            theme::dim(),
        )));
    }
    lines.push(Line::from(""));
}

/// 秒 → "1m20s" / "45s"
fn human_duration(secs: u64) -> String {
    if secs >= 3600 {
        format!("{}h{}m", secs / 3600, (secs % 3600) / 60)
    } else if secs >= 60 {
        format!("{}m{}s", secs / 60, secs % 60)
    } else {
        format!("{secs}s")
    }
}

/// 按显示列宽截断（中文 2 列，不能用 chars.count）
fn truncate(s: &str, max: usize) -> String {
    crate::ui::fit(s, max)
}

fn format_downloads(n: i64) -> String {
    if n >= 100_000_000 {
        format!("{}万", n / 10_000)
    } else if n >= 10_000 {
        format!("{:.1}万", n as f64 / 10_000.0)
    } else {
        n.to_string()
    }
}
// ---------- Mojang：官方版本 ----------

fn draw_official(frame: &mut Frame, app: &mut App, area: Rect) {
    let installed: Vec<&str> = app.instances.iter().map(|i| i.id.as_str()).collect();
    let title = format!(
        " 官方版本 {} ",
        if app.dl.manifest_loading {
            "加载中…"
        } else {
            "(r 刷新 · Enter 安装)"
        }
    );
    let items: Vec<ListItem> = app
        .dl
        .manifest
        .iter()
        .map(|v| {
            let installed = installed.contains(&v.id.as_str());
            let mut spans: Vec<Span> = vec![
                Span::styled(
                    if installed { "✓ " } else { "  " },
                    if installed {
                        theme::ok()
                    } else {
                        theme::fg_muted()
                    },
                ),
                Span::raw(format!(" {}", v.id)),
                Span::raw(" ".repeat(4)),
                Span::styled(
                    match v.version_type.as_str() {
                        "release" => "正式版",
                        "snapshot" => "快照",
                        "old_beta" => "Beta",
                        "old_alpha" => "Alpha",
                        other => other,
                    },
                    if v.version_type == "release" {
                        theme::ok()
                    } else {
                        theme::fg_muted()
                    },
                ),
            ];
            if installed {
                spans.push(Span::raw(" ".repeat(4)));
                spans.push(Span::styled("已安装", theme::ok()));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();
    let list = List::new(items)
        .block(Block::default().title(title).borders(Borders::ALL))
        .highlight_style(theme::selected_block())
        .highlight_symbol("> ");
    let mut state = ratatui::widgets::ListState::default();
    state.select(Some(app.dl.sel_official));
    frame.render_stateful_widget(list, area, &mut state);
}

// ---------- Modrinth：整合包 ----------

/// Modrinth 详情页：版本选择（网易详情同款交互）
fn draw_mr_detail(frame: &mut Frame, app: &mut App, area: Rect) {
    let [title_area, list_area] =
        Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(area);

    let status = if app.mr_detail_loading {
        " 加载版本…"
    } else {
        ""
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                format!(" {} ", app.mr_detail_title),
                Style::default().fg(theme::primary()),
            ),
            Span::styled(status, theme::fg_muted()),
        ])),
        title_area,
    );

    let tree = super::version_tree::build_tree(&app.mr_versions, &app.mr_collapsed);

    let rows = list_area.height.saturating_sub(2) as usize;
    // 选中行居中滚动；夹住范围，折叠后行数变少也不能越界
    let sel = app.mr_row_sel.min(tree.len().saturating_sub(1));
    let start = sel.saturating_sub(rows / 2);
    let end = (start + rows).min(tree.len());

    let items: Vec<ListItem> = (start..end)
        .map(|row_i| {
            let selected = row_i == sel;
            let style = if selected {
                theme::selected_block()
            } else {
                Style::default().fg(theme::fg())
            };
            let spans = match &tree[row_i] {
                super::version_tree::TreeRow::Group {
                    mc,
                    count,
                    expanded,
                } => {
                    let mark = if *expanded { "▾" } else { "▸" };
                    vec![
                        Span::styled(format!(" {mark} "), Style::default().fg(theme::primary())),
                        Span::styled(
                            mc.clone(),
                            Style::default()
                                .fg(theme::primary())
                                .add_modifier(ratatui::style::Modifier::BOLD),
                        ),
                        Span::styled(
                            format!("  {count} 个版本"),
                            Style::default().fg(theme::fg_muted()),
                        ),
                    ]
                }
                super::version_tree::TreeRow::Loader {
                    loader,
                    count,
                    expanded,
                    ..
                } => {
                    let mark = if *expanded { "▾" } else { "▸" };
                    vec![
                        Span::raw("   "),
                        Span::styled(format!("{mark} "), Style::default().fg(theme::fg_muted())),
                        Span::styled(loader.clone(), Style::default().fg(theme::fg())),
                        Span::styled(format!("  {count}"), Style::default().fg(theme::fg_muted())),
                    ]
                }
                super::version_tree::TreeRow::Leaf { idx, depth } => {
                    let v = &app.mr_versions[*idx];
                    let pre = super::version_tree::is_prerelease(&v.version_number);
                    let indent = "  ".repeat(*depth + 1);
                    // 稳定版给一个醒目的方块标记（用户明确要求）
                    let badge = if pre {
                        Span::styled("  ", Style::default())
                    } else {
                        Span::styled("▪ ", Style::default().fg(theme::ok()))
                    };
                    let name_style = if pre {
                        Style::default().fg(theme::fg_muted())
                    } else if selected {
                        style
                    } else {
                        Style::default().fg(theme::fg())
                    };
                    let mut spans = vec![
                        Span::raw(indent),
                        Span::styled("└", Style::default().fg(theme::fg_muted())),
                        badge,
                        Span::styled(
                            format!("{:<22}", truncate(&v.version_number, 22)),
                            name_style,
                        ),
                    ];
                    if !v.loaders.is_empty() {
                        spans.push(Span::styled(
                            format!("[{}]", v.loaders.join("/")),
                            Style::default().fg(theme::fg_muted()),
                        ));
                    }
                    if let Some(f) = v.files.first() {
                        spans.push(Span::raw("  "));
                        spans.push(Span::styled(
                            truncate(&f.filename, 44),
                            Style::default().fg(theme::fg_muted()),
                        ));
                    }
                    spans
                }
            };
            ListItem::new(Line::from(spans)).style(style)
        })
        .collect();

    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" 选择版本 (↑↓ 选择  ←→ 折叠/展开  Enter 安装  r 展开全部  Esc 返回) "),
    );
    frame.render_widget(list, list_area);
}

fn draw_modpack(frame: &mut Frame, app: &mut App, area: Rect) {
    // 详情页（版本选择）
    if app.mr_detail_open {
        draw_mr_detail(frame, app, area);
        return;
    }
    let [sidebar_area, main_area] = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(SIDEBAR_W), Constraint::Fill(1)])
        .areas(area);
    let names: Vec<&str> = crate::app::MODRINTH_CATEGORIES
        .iter()
        .map(|c| c.0)
        .collect();
    draw_cat_sidebar(
        frame,
        app,
        sidebar_area,
        &names,
        app.mr_category,
        app.focus == Focus::Sidebar,
        "资源分类",
    );

    let [search_area, grid_area] =
        Layout::vertical([Constraint::Length(3), Constraint::Fill(1)]).areas(main_area);

    // 搜索极左 / 版本条件+排序极右（分栏，右边框分隔）
    let search_block = Block::default()
        .title(" 资源 (Modrinth) ")
        .borders(Borders::ALL);
    let inner = search_block.inner(search_area);
    frame.render_widget(search_block, search_area);
    let [left, right] =
        Layout::horizontal([Constraint::Length(22), Constraint::Fill(1)]).areas(inner);
    let mut query_line = Line::from(vec![
        Span::styled("搜索: ", theme::dim()),
        Span::raw(app.dl.query.clone()),
    ]);
    if app.mr_searching {
        query_line.push_span(Span::styled("▌", theme::accent()));
    } else {
        query_line.push_span(Span::styled("  (/ 输入)", theme::fg_muted()));
    }
    frame.render_widget(Paragraph::new(query_line), left);
    let mut right_line = Line::default();
    if app.mr_version_editing {
        right_line.push_span(Span::styled(
            format!("版本: {}{} ", app.mr_version, "▌"),
            theme::accent(),
        ));
    } else if app.mr_version.is_empty() {
        right_line.push_span(Span::styled("版本: 全部 (v) ", theme::fg_muted()));
    } else {
        right_line.push_span(Span::styled(
            format!("版本: [{}] ", app.mr_version),
            theme::selected_block(),
        ));
    }
    for s in crate::launcher::modpack::ModrinthIndex::ALL {
        if s == app.dl.sort {
            right_line.push_span(Span::styled(
                format!(" [{}] ", s.label()),
                theme::selected_block(),
            ));
        } else {
            right_line.push_span(Span::styled(format!(" {} ", s.label()), theme::dim()));
        }
    }
    right_line.push_span(Span::styled(" (s)", theme::fg_muted()));
    frame.render_widget(
        Paragraph::new(right_line).alignment(Alignment::Right),
        right,
    );

    if app.dl.results.is_empty() {
        let msg = if app.dl.searching {
            "加载中…".to_string()
        } else {
            " / 搜索 · 回车执行 ".to_string()
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(msg, theme::fg_muted()))),
            grid_area,
        );
        return;
    }

    draw_card_grid(
        frame,
        app,
        grid_area,
        app.dl.results.len(),
        app.dl.sel_result,
        |f, a, cell, i| {
            draw_mr_card(f, a, cell, i);
        },
        |a, i| {
            if a.mr_covers.get(i).map(|c| c.is_none()).unwrap_or(false)
                && !a.mr_cover_queue.contains(&i)
            {
                a.mr_cover_queue.push(i);
                a.spawn_mr_cover(i);
            }
        },
    );
}

/// Modrinth 网格卡片（封面 + 标题 + 下载量/热度/日期/版本）
fn draw_mr_card(frame: &mut Frame, app: &mut App, area: Rect, idx: usize) {
    let Some(hit) = app.dl.results.get(idx).cloned() else {
        return;
    };
    let selected = app.dl.sel_result == idx;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(if selected {
            Style::default().fg(theme::border_sel())
        } else {
            Style::default().fg(theme::border())
        });
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let [img_area, text_area] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(55), Constraint::Fill(1)])
        .areas(inner);

    // 封面
    if idx < app.mr_covers.len() {
        if let Some(protocol) = app.mr_covers[idx].as_mut() {
            frame.render_stateful_widget(
                ratatui_image::StatefulImage::<ratatui_image::protocol::StatefulProtocol>::new(),
                img_area,
                protocol,
            );
        } else {
            if !app.mr_cover_queue.contains(&idx) {
                app.mr_cover_queue.push(idx);
                app.spawn_mr_cover(idx);
            }
            frame.render_widget(
                Paragraph::new("🖼")
                    .style(Style::default().fg(theme::fg_muted()))
                    .alignment(Alignment::Center),
                img_area,
            );
        }
    }

    // 文本
    let date = hit.date_modified.get(0..10).unwrap_or("");
    let lines = vec![
        Line::from(Span::styled(
            truncate(&hit.title, inner.width as usize),
            if selected {
                Style::default()
                    .fg(theme::primary())
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme::fg())
            },
        )),
        Line::from(Span::styled(
            format!("▼{}  ☆{}  {}", hit.downloads, hit.follows, date),
            theme::ok(),
        )),
        Line::from(Span::styled(
            hit.game_versions.first().cloned().unwrap_or_default(),
            theme::fg_muted(),
        )),
    ];
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), text_area);
}

/// 插件版本列表（provides/versions 契约：核心只做通用渲染，数据全部来自插件二进制）
fn draw_plugin_versions(frame: &mut Frame, app: &mut App, area: Rect) {
    use ratatui::widgets::{List, ListItem, ListState, Paragraph};
    let block = crate::ui::home::panel_block("插件资源 · 版本管理", true);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let rows = ratatui::layout::Layout::vertical([
        ratatui::layout::Constraint::Min(1),
        ratatui::layout::Constraint::Length(1),
    ])
    .split(inner);

    let busy_mark = if app.plugin_busy { "⏳ " } else { "" };
    let items: Vec<ListItem> = app
        .plugin_items
        .iter()
        .enumerate()
        .map(|(i, it)| {
            let sel = i == app.plugin_selected;
            let mark = if sel { "▌" } else { " " };
            let check = if it.installed { "✓ " } else { "  " };
            let size = if it.installed && it.size_mb > 0 {
                format!("  {:>5} MB", it.size_mb)
            } else {
                String::new()
            };
            let style = if it.installed {
                crate::theme::accent()
            } else {
                crate::theme::base()
            };
            ListItem::new(ratatui::text::Line::from(vec![
                ratatui::text::Span::styled(format!("{mark}{check}"), style),
                ratatui::text::Span::styled(
                    it.name.clone(),
                    if sel { style.bold() } else { style },
                ),
                ratatui::text::Span::styled(size, crate::theme::dim()),
            ]))
        })
        .collect();
    let mut state = ListState::default();
    state.select(Some(app.plugin_selected));
    frame.render_stateful_widget(
        List::new(items).highlight_style(crate::theme::selected_block()),
        rows[0],
        &mut state,
    );

    frame.render_widget(
        Paragraph::new(ratatui::text::Line::from(vec![
            ratatui::text::Span::styled(busy_mark, crate::theme::warn()),
            ratatui::text::Span::styled(app.plugin_status.clone(), crate::theme::dim()),
        ])),
        rows[1],
    );
}

#[cfg(test)]
mod download_ui_tests {
    use super::*;

    /// 细线：已下载部分是实心 ━，未完成是虚线 ┄，总宽恒等于 width
    #[test]
    fn thin_line_width_and_fill() {
        for width in [1usize, 4, 20, 80] {
            for pct in [0.0, 0.01, 0.5, 0.999, 1.0] {
                let l = thin_line(width, pct, ratatui::style::Style::default());
                let s = l.spans[0].content.as_ref();
                assert_eq!(crate::ui::cell_width_of(s), width, "w={width} pct={pct}");
            }
        }
    }

    #[test]
    fn thin_line_is_full_at_100_and_empty_at_0() {
        let at0 = thin_line(10, 0.0, ratatui::style::Style::default());
        assert!(!at0.spans[0].content.contains('━'));
        let at1 = thin_line(10, 1.0, ratatui::style::Style::default());
        assert_eq!(at1.spans[0].content.as_ref(), "━".repeat(10));
        assert!(!at1.spans[0].content.contains('┄'));
    }

    /// spinner 必须在时间里转起来，且完成/失败后静止
    #[test]
    fn spinner_rotates_over_time_and_stops_when_done() {
        let t0 = std::time::Instant::now();
        let a = spinner_for(t0, true);
        std::thread::sleep(std::time::Duration::from_millis(260));
        let b = spinner_for(t0, true);
        assert_ne!(a, b, "260ms 后 spinner 没转");
        assert!(SPINNER.contains(&a) && SPINNER.contains(&b));
        assert_eq!(spinner_for(t0, false), "", "非活动时不该有 spinner");
    }

    /// spinner 每一帧都必须是单列宽，否则右对齐会跳
    #[test]
    fn spinner_frames_are_single_cell() {
        for f in SPINNER {
            assert_eq!(crate::ui::cell_width_of(f), 1, "{f:?} 不是单列");
        }
    }

    /// 涌动波：波峰在相位处是亮头色，远处回落到主色；跨边界时连续
    #[test]
    fn wave_has_a_moving_bright_crest() {
        let head = wave_style(10, 10, 20, 1.0);
        assert_eq!(head.fg, Some(crate::theme::fade_color(crate::theme::head(), 1.0)));
        assert!(head.add_modifier.contains(Modifier::BOLD));

        let shoulder = wave_style(13, 10, 20, 1.0);
        assert_eq!(
            shoulder.fg,
            Some(crate::theme::fade_color(crate::theme::accent_color(), 1.0))
        );
        assert!(
            shoulder.add_modifier.contains(Modifier::BOLD),
            "波峰两侧应有加粗过渡"
        );

        let tail = wave_style(18, 10, 20, 1.0);
        assert_eq!(
            tail.fg,
            Some(crate::theme::fade_color(crate::theme::accent_color(), 1.0))
        );
        assert!(
            !tail.add_modifier.contains(Modifier::BOLD),
            "远处应保持主色"
        );

        // 环绕：相位在最后一格时，波峰同时出现在最左格
        assert_eq!(
            wave_style(0, 19, 20, 1.0).fg,
            Some(crate::theme::fade_color(crate::theme::head(), 1.0)),
            "波应从右边界连续绕回左边"
        );
    }

    /// 大进度条：宽度不变、完成段 █、未完成段 ░，且同时存在波峰与底色
    #[test]
    fn big_bar_keeps_width_and_has_wave() {
        let spans = big_bar_spans(30, 40, 8, 1.0);
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(crate::ui::cell_width_of(&text), 40, "大进度条总宽不能变");
        assert_eq!(text.chars().filter(|c| *c == '█').count(), 30);
        assert_eq!(text.chars().filter(|c| *c == '░').count(), 10);
        let colors: std::collections::HashSet<_> =
            spans.iter().filter_map(|s| s.style.fg).collect();
        assert!(colors.len() >= 2, "涌动条至少要有波峰和底色两种颜色");
    }

    #[test]
    fn human_bytes_scales() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(2048), "2.0 KB");
        assert_eq!(human_bytes(5 * 1024 * 1024), "5.0 MB");
        assert_eq!(human_bytes(1024 * 1024 * 1024), "1.0 GB");
    }
}
