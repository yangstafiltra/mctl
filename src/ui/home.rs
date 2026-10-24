use crate::app::{App, HomeFocus, HomeView, InstallStatus};
use crate::models::Account;
use crate::theme;
use crate::ui::cell_width_of;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, List, ListItem, Paragraph};
use ratatui::Frame;

pub fn draw(frame: &mut Frame, app: &mut App, area: Rect) {
    // 完整日志视图（l 切换）：wm 外壳被绕过
    if app.home_log_full {
        game_log_full(frame, app, area);
        return;
    }
    // 单面板全屏（f 切换）：wm 外壳被绕过
    if app.home_panel_full {
        panel_full(frame, app, area);
        return;
    }
    // 正常路径：wm 外壳已拆分 dock/主区，这里仅渲染主区视图内容
    draw_view(frame, app, app.home_view, area);
}

/// 主区视图内容（横幅 + 列表）：wm 外壳调用
pub fn draw_view(frame: &mut Frame, app: &mut App, view: HomeView, area: Rect) {
    // 内容区固定 11 行（与左栏状态卡同高）；横幅占剩余空间
    let rows = Layout::vertical([Constraint::Min(0), Constraint::Length(11)]).split(area);
    let max_h = rows[0].height;

    // 横幅高度**自适应**：只有真的有图（自动截图/服务器封面）时才给大块空间。
    // // 之前不管有没有图都按 width*9/16 给横幅，在 80×50 的终端上就是 45 行——
    // 占掉近八成高度。而自动截图现已默认关（隐私），横幅退化成「一片空白 + logo」，
    // 那八成高度就纯属浪费，而下面的世界列表只分到 11 行、常常只显示 1 行。
    // 所以没图时收成紧凑头部，把空间还给内容。
    let has_image = match view {
        HomeView::Instance => app.banner_shot.is_some(),
        _ => false,
    };
    let banner_h = if has_image {
        max_h.min(((area.width as usize) * 9 / 16).max(6) as u16)
    } else {
        // logo 6 行 + 上下各 1 行边框，够放一行提示
        8.min(max_h)
    };
    let rows = Layout::vertical([Constraint::Length(banner_h), Constraint::Min(0)]).split(area);
    match view {
        HomeView::Server => {
            banner_panel(
                frame,
                app,
                rows[0],
                "Java 国际服（原版 Server List Ping · 网格 ↑↓←→ 选 · 回车用本地实例进服）",
                app.home_focus == HomeFocus::Banner,
            );
            servers_panel(frame, app, rows[1], app.home_focus == HomeFocus::List);
        }
        // 资源 / 插件 / 设置：整页自定义渲染（ui::maps / ui::extra），不走 banner+list 布局
        HomeView::Instance => {
            banner_panel(
                frame,
                app,
                rows[0],
                "当前实例 · ↑↓ 选世界 · 回车直进",
                app.home_focus == HomeFocus::Banner,
            );
            let cols = Layout::horizontal([Constraint::Ratio(4, 5), Constraint::Ratio(1, 5)])
                .split(rows[1]);
            // 世界列表是主体；右侧保留下载活动，和下载中心共享同一份任务队列。
            super::world_select::render(frame, app, cols[0], app.home_focus == HomeFocus::List);
            downloads_panel(frame, app, cols[1], app.home_focus == HomeFocus::Downloads);
        }
        HomeView::Resource | HomeView::Download | HomeView::Plugins | HomeView::Settings => {}
    }
}

/// bilibili-tui 风格面板：聚焦 = accent 粗体圆角边框，未聚焦 = 暗色圆角边框
pub(crate) fn panel_block(title: &str, focused: bool) -> Block<'static> {
    let (style, border_type) = if focused {
        (
            Style::default()
                .fg(theme::border_sel())
                .add_modifier(Modifier::BOLD),
            BorderType::Rounded,
        )
    } else {
        (Style::default().fg(theme::border()), BorderType::Rounded)
    };
    // 空标题 = 无标题开口（闭环边框，不画 ▌ 前缀）
    let block = Block::default()
        .border_style(style)
        .borders(Borders::ALL)
        .border_type(border_type);
    if title.is_empty() {
        block
    } else {
        let title = if focused {
            format!(" ▌{title} ")
        } else {
            format!("  {title} ")
        };
        block.title(Span::styled(
            title,
            if focused {
                theme::accent().bold()
            } else {
                theme::dim()
            },
        ))
    }
}

pub(crate) fn account_panel(frame: &mut Frame, app: &mut App, area: Rect, focused: bool) {
    let block = panel_block("◉ 账户", focused);
    let mut inner = block.inner(area);
    // 头像（有 avatar.png 时显示在账户列表上方，高度按宽度等比、最多 8 行）
    if app.avatar.is_some() {
        // 放大居中：头像占面板大头，列表/提示保底 5 行
        let h = inner.height.saturating_sub(5).min(22);
        let w = (h * 2).min(inner.width) & !1; // halfblock 宽≈2×行高，偶数
        let x_off = inner.width.saturating_sub(w) / 2;
        let av_rect = Rect {
            x: inner.x + x_off,
            y: inner.y,
            width: w.max(2),
            height: h.max(2),
        };
        if let Some(av) = app.avatar.as_mut() {
            // Scale：小图放大铺满 area（Fit 只缩不放，头像会缩成一小块）
            frame.render_stateful_widget(
                ratatui_image::StatefulImage::default().resize(ratatui_image::Resize::Scale(None)),
                av_rect,
                av,
            );
        }
        let rows = Layout::vertical([Constraint::Length(h + 1), Constraint::Min(5)]).split(inner);
        inner = rows[1];
    }
    // 内部行：账户列表 + 提示/输入区
    let hint_h = if app.ls.input {
        4 // LS 登记 3 字段 + 提示行
    } else if app.ls.login.is_some() {
        3 // 授权等待：user_code + URL + 状态
    } else if app.login_input {
        3
    } else {
        2
    };
    let rows = Layout::vertical([Constraint::Min(1), Constraint::Length(hint_h)]).split(inner);

    // 账户行 + [4399]/[LittleSkin] 两个独立登录入口（与网易/微软账户区分）
    let mut items: Vec<ListItem> = app.accounts.iter().map(account_line).collect();
    let sel = app.selected_account;
    let n = app.accounts.len();
    let entry = |i: usize, label: &str| -> ListItem<'static> {
        let marker = if sel == i { "▌" } else { " " };
        ListItem::new(Line::from(Span::styled(
            format!("{marker}{label}"),
            if sel == i {
                theme::accent().bold()
            } else {
                theme::base()
            },
        )))
    };
    items.push(entry(n, "[4399] 账号"));
    items.push(entry(n + 1, "[LittleSkin] 登录"));
    let list = List::new(items).highlight_style(theme::selected_block());
    let mut state = ratatui::widgets::ListState::default();
    state.select(Some(app.selected_account));
    frame.render_stateful_widget(list, rows[0], &mut state);

    let hint_area = rows[1];
    let lines: Vec<Line> = if app.login_input {
        let (user_vis, pass_vis) = if app.login_input_step == 0 {
            (app.login_user.clone(), "•".repeat(app.login_pass.len()))
        } else {
            (app.login_user.clone(), app.login_pass.clone())
        };
        vec![
            Line::from(vec![
                Span::styled(" 4399账号 ", theme::dim()),
                Span::styled(
                    user_vis,
                    if app.login_input_step == 0 {
                        theme::accent().bold()
                    } else {
                        theme::dim()
                    },
                ),
            ]),
            Line::from(vec![
                Span::styled(" 密 码   ", theme::dim()),
                Span::styled(
                    pass_vis,
                    if app.login_input_step == 1 {
                        theme::accent().bold()
                    } else {
                        theme::dim()
                    },
                ),
            ]),
            Line::from(Span::styled(
                " [Tab] 切换 [Enter] 登录 [Esc] 取消",
                theme::dim(),
            )),
        ]
    } else if app.ls.input {
        let f = |i: u8, s: &str| -> Span<'static> {
            Span::styled(
                s.to_string(),
                if app.ls.step == i {
                    theme::accent().bold()
                } else {
                    theme::dim()
                },
            )
        };
        vec![
            Line::from(vec![
                Span::styled(" 应用名 ", theme::dim()),
                f(0, &app.ls.app_name),
            ]),
            Line::from(vec![
                Span::styled(" 回调URL ", theme::dim()),
                f(1, &app.ls.callback_url),
            ]),
            Line::from(vec![
                Span::styled(" 客户端ID ", theme::dim()),
                f(2, &app.ls.client_id),
            ]),
            Line::from(Span::styled(
                " [Tab] 切换 [Enter] 保存 [Esc] 取消",
                theme::dim(),
            )),
        ]
    } else if let Some(login) = &app.ls.login {
        // 设备码流等待授权
        vec![
            Line::from(vec![
                Span::styled(" 授权码 ", theme::accent().bold()),
                Span::styled(&login.user_code, theme::accent().bold()),
                Span::styled("  ←在浏览器输入", theme::dim()),
            ]),
            Line::from(Span::styled(
                truncate(&login.verification_url, hint_area.width as usize),
                theme::dim(),
            )),
            Line::from(Span::styled(app.ls.status.clone(), theme::warn())),
        ]
    } else {
        let line1: String = if let Some(cred) = &app.cred {
            format!(" 已登录 {} (4399)", cred.user_id)
        } else {
            " 未登录 4399".to_string()
        };
        let line2 = if app.cred.is_none() {
            " 账户聚焦 [Enter] 4399 登录".to_string()
        } else if app.ls.status.is_empty() {
            " [↑↓]选账户/登录项 [Enter]进入详情".to_string()
        } else {
            app.ls.status.clone()
        };
        vec![
            Line::from(Span::styled(line1, theme::dim())),
            Line::from(Span::styled(line2, theme::dim())),
        ]
    };
    frame.render_widget(Paragraph::new(lines), hint_area);
    frame.render_widget(block, area);
}

/// 按显示列宽截断（中文 2 列，不能用 chars.count）
fn truncate(s: &str, max: usize) -> String {
    crate::ui::fit(s, max)
}

fn account_line(a: &Account) -> ListItem<'_> {
    if a.logged_in {
        ListItem::new(Line::from(vec![
            Span::raw(format!("{}  ", a.provider)),
            Span::styled(format!("● {}", a.name), theme::ok()),
        ]))
    } else {
        ListItem::new(Line::from(vec![
            Span::raw(format!("{}  ", a.provider)),
            Span::styled("○ 未登录", theme::dim()),
        ]))
    }
}

/// 状态卡（左栏底部，可聚焦换实例）：实例信息 + 状态 + ↑↓ 提示
pub(crate) fn status_panel(frame: &mut Frame, app: &App, area: Rect, focused: bool) {
    // 服务器视图：状态栏显示选中服务器 + 客户端登录名称（角色名，可编辑）
    if app.home_view == HomeView::Server {
        server_mode_status(frame, app, area, focused);
        return;
    }
    let Some(inst) = app.instances.get(app.selected_instance) else {
        frame.render_widget(
            Paragraph::new(vec![Line::from(Span::styled(
                " 没有已安装的实例（去 资源下载 视图安装官方版本）",
                theme::dim(),
            ))])
            .block(panel_block("◐ 状态", focused)),
            area,
        );
        return;
    };
    let status = if app.launching {
        vec![
            Span::styled("⏳ ", theme::warn()),
            Span::styled(&app.launch_status, theme::warn()),
        ]
    } else {
        vec![
            Span::styled(
                "● ",
                if app.launch_status.is_empty() {
                    theme::ok()
                } else {
                    theme::warn()
                },
            ),
            Span::styled(
                if app.launch_status.is_empty() {
                    "就绪"
                } else {
                    &app.launch_status
                },
                if app.launch_status.is_empty() {
                    theme::ok()
                } else {
                    theme::warn()
                },
            ),
        ]
    };
    let hint = if focused {
        Span::styled(" ↑↓ 换实例  o 存档", theme::accent().bold())
    } else {
        Span::styled(" ↑↓ 换实例  o 存档", theme::dim())
    };
    let lines = vec![
        Line::from(vec![
            Span::styled("实例 ", theme::dim()),
            Span::styled(inst.id.clone(), theme::base().bold()),
            Span::raw(" "),
            Span::styled(inst.version_type.clone(), theme::dim()),
        ]),
        Line::from(vec![
            Span::styled("Java ", theme::dim()),
            Span::raw(format!("{}  资产 {}", inst.java_major, inst.assets_index)),
        ]),
        Line::from(status),
        Line::from(hint),
    ];
    frame.render_widget(
        Paragraph::new(lines).block(panel_block("◐ 状态", focused)),
        area,
    );
}

/// 服务器视图的左下角状态栏：随选中服务器变化 + 客户端登录名称（角色名）可编辑
fn server_mode_status(frame: &mut Frame, app: &App, area: Rect, focused: bool) {
    // 行1：选中服务器信息（随 ↑↓ 变化）
    let server_line: Line = match app.servers.get(app.selected_server) {
        Some(s) => Line::from(vec![
            Span::styled("服务器 ", theme::dim()),
            Span::styled(s.name.clone(), theme::base().bold()),
            Span::raw("  "),
            Span::styled(s.addr(), theme::dim()),
            Span::raw("  "),
            Span::styled(
                format!("v{} · {}", s.version, s.online_text()),
                theme::dim(),
            ),
        ]),
        None => Line::from(Span::styled("服务器 （未加载，按 r 刷新）", theme::dim())),
    };
    // 行2：客户端登录名称（编辑态高亮输入框）
    let role_text: String = if app.role_name.trim().is_empty() {
        if let Some(cred) = &app.cred {
            format!("{}（默认 user_id）", cred.user_id)
        } else {
            "（未登录）".to_string()
        }
    } else {
        app.role_name.trim().to_string()
    };
    let role_line = if app.role_input {
        Line::from(vec![
            Span::styled("角色名 ", theme::dim()),
            Span::styled(format!("{}▊", role_text), theme::accent().bold()),
        ])
    } else {
        Line::from(vec![
            Span::styled("角色名 ", theme::dim()),
            Span::styled(role_text, theme::base()),
        ])
    };
    // 行3：状态（进服进度/错误优先，回退启动状态）
    let status = if !app.server_status.is_empty() {
        vec![
            Span::styled("● ", theme::warn()),
            Span::styled(&app.server_status, theme::warn()),
        ]
    } else if app.launching {
        vec![
            Span::styled("⏳ ", theme::warn()),
            Span::styled(&app.launch_status, theme::warn()),
        ]
    } else {
        vec![
            Span::styled("● ", theme::ok()),
            Span::styled("就绪", theme::ok()),
        ]
    };
    // 行4：提示（编辑态 / 聚焦态）
    let hint = if app.role_input {
        Span::styled(" [Enter] 保存 · [Esc] 取消", theme::accent().bold())
    } else if focused {
        Span::styled(" [Enter] 修改角色名", theme::accent().bold())
    } else {
        Span::styled(" 聚焦 [Enter] 修改角色名", theme::dim())
    };
    let lines = vec![server_line, role_line, Line::from(status), Line::from(hint)];
    frame.render_widget(
        Paragraph::new(lines).block(panel_block("◐ 状态", focused)),
        area,
    );
}

/// 左侧视图侧栏：视图切换指示器（操作走 ←→ / Alt+数字）。
/// 选中项是一整行**底色块**（`theme::selected_block`，经典主题 = 蓝底黑字），
/// 换视图时从左往右刷出来（`app.view_anim_progress`，见 `wipe_spans`）。
pub(crate) fn view_sidebar(frame: &mut Frame, app: &App, area: Rect) {
    // 底色的行宽 = 面板内宽（块铺满整行，两侧边框不算）
    let row_w = area.width.saturating_sub(2) as usize;
    let reveal = wipe_width(row_w, app.view_anim_progress());
    let items: Vec<ListItem> = HomeView::ALL
        .iter()
        .map(|v| {
            let active = *v == app.home_view;
            // 选中标记 │ 图标 名称 Alt+位
            // └ 几何字符，等宽安全
            let mut spans = vec![
                Span::styled(if active { "▌" } else { " " }, theme::accent().bold()),
                Span::raw(" "),
                Span::styled(
                    v.icon(),
                    if active {
                        theme::accent().bold()
                    } else {
                        theme::dim()
                    },
                ),
                Span::raw(" "),
                Span::styled(
                    v.label(),
                    if active {
                        theme::accent().bold()
                    } else {
                        theme::base()
                    },
                ),
                Span::styled(format!("  alt+{}", v.slot()), theme::dim()),
            ];
            if active {
                // 补空格把行铺满内宽，再按进度把 [0, reveal) 段刷上底色
                let used: usize = spans.iter().map(|s| cell_width_of(&s.content)).sum();
                spans.push(Span::raw(" ".repeat(row_w.saturating_sub(used))));
                spans = wipe_spans(spans, reveal, theme::selected_block());
            }
            ListItem::new(Line::from(spans))
        })
        .collect();
    frame.render_widget(
        List::new(items).block(
            Block::default()
                .title(Span::styled(" ⬡ 视图 ", theme::dim()))
                .borders(Borders::ALL),
        ),
        area,
    );
}

/// 刷色进度 p ∈ [0,1] → 底色块盖到第几列（0 = 还没刷到，width = 刷满）
pub(crate) fn wipe_width(width: usize, p: f32) -> usize {
    ((width as f32) * p.clamp(0.0, 1.0)).round() as usize
}

/// 把一行的 spans 在第 `cut` 列处切开：左边（被刷到的部分）套上选中块样式
/// （字色 + 底色），右边保持原样——于是看起来就是底色块在往右"刷"，
/// 刷过的字同步翻色。切口会按显示列宽对齐，汉字不会被劈成半个。
pub(crate) fn wipe_spans(
    spans: Vec<Span<'static>>,
    cut: usize,
    block: Style,
) -> Vec<Span<'static>> {
    use crate::ui::split_cells;
    let mut out: Vec<Span<'static>> = Vec::new();
    let mut x = 0usize;
    for s in spans {
        let w = cell_width_of(&s.content);
        let (content, style) = (s.content.into_owned(), s.style);
        if x + w <= cut {
            out.push(Span::styled(content, style.patch(block)));
        } else if x >= cut {
            out.push(Span::styled(content, style));
        } else {
            let (head, tail) = split_cells(&content, cut - x);
            out.push(Span::styled(head, style.patch(block)));
            out.push(Span::styled(tail, style));
        }
        x += w;
    }
    out
}

/// 横幅：16:9 大占比主视觉，显示当前世界最新自动截图；无截图时显示 MCTL 立体 ASCII 主视觉
fn banner_panel(frame: &mut Frame, app: &mut App, area: Rect, hint: &str, focused: bool) {
    // 有主视觉图（本地=最近世界自动截图）。服务器页**不放宣传图**：
    // 顶部就是 MCTL 大字（用户口径：服务器页顶部是字，不是图）。
    let shot = match app.home_view {
        HomeView::Instance => app.banner_shot.as_mut(),
        _ => None,
    };
    if let Some(shot) = shot {
        let block = panel_block("", focused);
        let inner = block.inner(area);
        frame.render_stateful_widget(
            ratatui_image::StatefulImage::<ratatui_image::protocol::StatefulProtocol>::new(),
            inner,
            shot,
        );
        frame.render_widget(block, area);
        return;
    }
    // 无截图：MCTL 立体大写 ASCII 主视觉（ANSI Shadow 风格，居中）
    const ART: [&str; 6] = [
        "███╗   ███╗ ██████╗████████╗██╗     ██╗",
        "████╗ ████║██╔════╝╚══██╔══╝██║     ██║",
        "██╔████╔██║██║        ██║   ██║     ██║",
        "██║╚██╔╝██║██║        ██║   ██║     ╚═╝",
        "██║ ╚═╝ ██║╚██████╗   ██║   ███████╗██╗",
        "╚═╝     ╚═╝ ╚═════╝   ╚═╝   ╚══════╝╚═╝",
    ];
    let play = match app.instances.get(app.selected_instance) {
        Some(inst) => match app.home_view {
            HomeView::Server => "▶ PLAY  连接所选 Java 服务器".to_string(),
            HomeView::Instance => format!("▶ PLAY  {}   ·   [回车] 进世界", inst.id),
            HomeView::Resource | HomeView::Download | HomeView::Plugins | HomeView::Settings => {
                String::new()
            }
        },
        None => String::new(),
    };
    let status = if app.launching {
        format!("⏳ {}", app.launch_status)
    } else if app.launch_status.is_empty() {
        "就绪 ●".to_string()
    } else {
        app.launch_status.clone()
    };
    let mut lines: Vec<Line> = ART
        .iter()
        .map(|l| Line::from(Span::styled(*l, theme::accent())).alignment(Alignment::Center))
        .collect();
    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(hint, theme::dim())).alignment(Alignment::Center));
    if !play.is_empty() {
        lines.push(
            Line::from(Span::styled(play, theme::accent().bold())).alignment(Alignment::Center),
        );
    }
    lines.push(Line::from(Span::styled(status, theme::dim())).alignment(Alignment::Center));
    let block = panel_block("", focused);
    let inner = block.inner(area);
    let rows = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(lines.len() as u16),
        Constraint::Fill(1),
    ])
    .split(inner);
    frame.render_widget(Paragraph::new(lines), rows[1]);
    frame.render_widget(block, area);
}

/// 卡片网格一屏几行几列的边界（每张卡至少这么宽/高，再大就跑满 3×3）
const CARD_MIN_W: usize = 22;
const CARD_MIN_H: usize = 7;

/// 卡片网格的可视窗口起点（每帧校正，带滞回）。
///
/// - `sel` 仍在窗口内 → 窗口不动（**别整页翻**：翻页会让选中框在屏幕上瞬移，
///   用户实测就是「↑↓←→ 选择是乱的」）
/// - 出界了才最小滑动，并贴住末尾（窗口不越过最后一项）
pub(crate) fn grid_start(sel: usize, page: usize, len: usize, start: usize) -> usize {
    if page == 0 || len <= page {
        return 0;
    }
    let max_start = len - page;
    let mut s = start.min(max_start);
    if sel < s {
        s = sel;
    } else if sel >= s + page {
        s = sel + 1 - page;
    }
    s.min(max_start)
}

/// 服务器：卡片网格（同资源下载的卡片视觉）。
/// 宽高够就是 **3 列 × 3 行**一屏 9 张；选中卡片 = 封面 + 名字高亮，
/// 外框走「流光描边」（`app.sel_anim_progress`，同世界列表那套，文字不刷色）。
fn server_cards(
    frame: &mut Frame,
    app: &mut App,
    area: Rect,
    focused: bool,
    title: &str,
    empty: &str,
    cards: &[(String, String)],
    selected: usize,
) {
    let block = panel_block(title, focused);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if cards.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(empty.to_string(), theme::dim()))),
            inner,
        );
        return;
    }
    let cols = (inner.width as usize / CARD_MIN_W).clamp(1, 3);
    let rows = (inner.height as usize / CARD_MIN_H).clamp(1, 3);
    app.server_grid_cols = cols; // ↑↓ 的步长（app.rs 按键时用）
    let page = cols * rows;
    // 可视窗口起点：带滞回（见 grid_start），选中项跟着滑动而不是整页跳。
    let start = grid_start(selected, page, cards.len(), app.srv_scroll);
    app.srv_scroll = start;
    let row_areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints((0..rows).map(|_| Constraint::Fill(1)).collect::<Vec<_>>())
        .split(inner);
    for (r, row_area) in row_areas.iter().enumerate() {
        let cells = Layout::default()
            .direction(Direction::Horizontal)
            .constraints((0..cols).map(|_| Constraint::Fill(1)).collect::<Vec<_>>())
            .split(*row_area);
        for (c, cell) in cells.iter().enumerate() {
            let idx = start + r * cols + c;
            let Some(card) = cards.get(idx) else {
                continue;
            };
            server_card(frame, app, *cell, idx, idx == selected, card);
        }
    }
}

/// 单张服务器卡片：封面 + 卡名 + 一行副标题（版本 · 人数）。
/// 选中动画只在**边框**上（流光描边，同世界列表），文字不参与动画。
fn server_card(
    frame: &mut Frame,
    app: &mut App,
    area: Rect,
    idx: usize,
    selected: bool,
    card: &(String, String),
) {
    let (name, meta) = (&card.0, &card.1);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme::border()));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if selected {
        // 流光描边：与世界列表同一套（从左往右包围一整圈，描完边框常驻）
        border_flow(frame, area, app.sel_anim_progress());
    }
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    // 封面：Java 状态响应自带 favicon；解码失败/没图时退回纯文字
    let show_cover = !app.srv_cover_none.get(idx).copied().unwrap_or(false);
    let mut text_area = inner;
    if show_cover {
        let [img_area, text] = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(55), Constraint::Fill(1)])
            .areas(inner);
        text_area = text;
        // 惰性加载：没缓存且没在队列里 → 入队 + 解码
        let cached = app.srv_covers.get(idx).is_some_and(|c| c.is_some());
        let queued = app.srv_cover_queue.contains(&idx);
        if !cached && !queued {
            app.srv_cover_queue.push(idx);
            app.spawn_srv_cover(idx);
        }
        if let Some(protocol) = app.srv_covers.get_mut(idx).and_then(|c| c.as_mut()) {
            frame.render_stateful_widget(
                ratatui_image::StatefulImage::<ratatui_image::protocol::StatefulProtocol>::new(),
                img_area,
                protocol,
            );
        } else {
            frame.render_widget(
                Paragraph::new("🖼")
                    .style(theme::fg_muted())
                    .alignment(Alignment::Center),
                img_area,
            );
        }
    }
    let w = text_area.width as usize;
    let mut lines = vec![Line::from(Span::styled(
        truncate(name, w),
        if selected {
            Style::default()
                .fg(theme::primary())
                .add_modifier(Modifier::BOLD)
        } else {
            theme::base()
        },
    ))];
    if text_area.height > 1 {
        lines.push(Line::from(Span::styled(
            truncate(meta, w),
            theme::fg_muted(),
        )));
    }
    frame.render_widget(Paragraph::new(lines), text_area);
}

/// 选中卡片的**流光描边**：与世界列表同一套节奏（`world_select::border_cell`）——
/// 上下边线一起从左往右扫、左端先合拢、右端最后关门；头部亮一档（`theme::head`），
/// 描过的格子转 accent，没描到的保持普通边框不动。
/// 进度 p 来自 `App::sel_anim_progress()`（0 → 1）；p = 1 时整圈 accent 常驻。
pub(crate) fn border_flow(frame: &mut Frame, area: Rect, p: f32) {
    if area.width < 2 || area.height < 2 || p <= 0.0 {
        return;
    }
    let head = Style::default()
        .fg(theme::head())
        .add_modifier(Modifier::BOLD);
    let (w, h) = (area.width, area.height);
    let buf = frame.buffer_mut();
    for y in 0..h {
        for x in 0..w {
            let Some(is_head) = crate::ui::world_select::border_cell(w, h, x, y, p) else {
                continue;
            };
            if let Some(c) = buf.cell_mut((area.x + x, area.y + y)) {
                c.set_style(if is_head { head } else { theme::accent() });
            }
        }
    }
}

fn servers_panel(frame: &mut Frame, app: &mut App, area: Rect, focused: bool) {
    let title = if app.server_status.is_empty() {
        "▤ Java 国际服 · ↑↓←→ 选择 · 回车进服".to_string()
    } else {
        format!("▤ Java 国际服 · {}", app.server_status)
    };
    let empty = if app.server_loading {
        "正在 ping Java 服务器…"
    } else {
        "（没有服务器，按 r 刷新）"
    };
    let cards: Vec<(String, String)> = app
        .servers
        .iter()
        .map(|s| {
            let version = if s.version.is_empty() {
                "?".to_string()
            } else {
                format!("v{}", s.version)
            };
            let status = if s.error.is_some() {
                "无法连接".to_string()
            } else {
                s.online_text()
            };
            (s.name.clone(), format!("{version} · {status}"))
        })
        .collect();
    let selected = app.selected_server;
    server_cards(
        frame,
        app,
        area,
        focused,
        &title,
        empty,
        &cards,
        selected,
    );
}

fn downloads_panel(frame: &mut Frame, app: &App, area: Rect, focused: bool) {
    let block = panel_block("⇩ 下载活动", focused);
    let mut lines = Vec::new();

    // 汇总一行：几个在跑/几个完事 + 总速度（Alt+6 是完整下载中心）
    let (active, done, failed, bd, bt, speed) = app.download_summary();
    if active + done + failed > 0 {
        let mut head = vec![Span::styled(
            format!("进行中 {active} · 完成 {done}"),
            theme::fg(),
        )];
        if failed > 0 {
            head.push(Span::styled(format!(" · 失败 {failed}"), theme::error()));
        }
        if active > 0 {
            head.push(Span::styled(" · ", theme::dim()));
            head.push(Span::styled(crate::app::human_rate(speed), theme::primary()));
            if bt > 0 {
                head.push(Span::styled(
                    format!(
                        " {}/{}",
                        crate::app::human_size(bd),
                        crate::app::human_size(bt)
                    ),
                    theme::dim(),
                ));
            }
        }
        lines.push(Line::from(head));
    }

    // 实例/资源下载队列（真实进度）
    for t in &app.install_tasks {
        let (icon, color) = match t.status {
            InstallStatus::Active => ("●", theme::warn()),
            InstallStatus::Done => ("✓", theme::ok()),
            InstallStatus::Failed => ("✗", ratatui::style::Color::Red),
        };
        // 任务名 + 阶段
        let mut header = vec![
            Span::styled(format!("{icon} "), color),
            Span::styled(t.name.as_str(), theme::fg()),
        ];
        if !t.phase.is_empty() {
            header.push(Span::styled(format!(" · {}", t.phase), theme::dim()));
        }
        lines.push(Line::from(header));
        // 进度条：优先按字节（资源下载），退回到文件计数（装版本/整合包）
        if t.status == InstallStatus::Active {
            let bar_width = area.width.saturating_sub(10) as usize;
            if t.bytes_total > 0 {
                let pct = (t.pct() * 100.0) as u32;
                let filled = ((t.pct() * bar_width as f64) as usize).min(bar_width);
                let bar = format!("{}{}", "█".repeat(filled), "░".repeat(bar_width - filled));
                lines.push(Line::from(vec![
                    Span::styled("  ", theme::fg()),
                    Span::styled(bar, theme::warn()),
                    Span::styled(
                        format!(
                            " {}/{} ({pct}%)",
                            crate::app::human_size(t.bytes_done),
                            crate::app::human_size(t.bytes_total)
                        ),
                        theme::dim(),
                    ),
                ]));
            } else if t.total > 0 {
                let pct = t.done * 100 / t.total;
                let filled = (t.done * bar_width / t.total).min(bar_width);
                let bar = format!("{}{}", "█".repeat(filled), "░".repeat(bar_width - filled));
                lines.push(Line::from(vec![
                    Span::styled("  ", theme::fg()),
                    Span::styled(bar, theme::warn()),
                    Span::styled(format!(" {}/{} ({pct}%)", t.done, t.total), theme::dim()),
                ]));
            }
        }
    }

    if lines.is_empty() {
        // 下载中心还在测试中：没有网易账号就别暗示去资源下载视图，
        // 直接说清楚——官方 / Modrinth 安装不需要网易账号。
        let msg = if app.cred.is_none() {
            "（暂无下载 · 网易资源需登录，按 a 打开登录页；官方/整合包不需要）"
        } else {
            "（暂无下载，去 资源下载 视图安装版本/整合包）"
        };
        lines.push(Line::from(Span::styled(msg, theme::dim())));
    }
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

/// 单面板全屏：当前聚焦面板占满整个 body（如 f 聚焦最近世界 → 只全屏世界面板）
fn panel_full(frame: &mut Frame, app: &mut App, area: Rect) {
    match app.home_focus {
        HomeFocus::Banner => {
            let hint = match app.home_view {
                HomeView::Instance => "当前实例 · ↑↓ 选世界 · 回车直进",
                HomeView::Server => "Java 国际服（原版 Server List Ping · 网格 ↑↓←→ 选 · 回车进服）",
                HomeView::Resource
                | HomeView::Download
                | HomeView::Plugins
                | HomeView::Settings => "",
            };
            banner_panel(frame, app, area, hint, true);
        }
        HomeFocus::List => match app.home_view {
            HomeView::Instance => {
                super::world_select::render(frame, app, area, true)
            }
            HomeView::Server => servers_panel(frame, app, area, true),
            // 资源/下载中心/插件/设置是整页视图，f 全屏时无「列表面板」可言
            HomeView::Resource | HomeView::Download | HomeView::Plugins | HomeView::Settings => {}
        },
        HomeFocus::Downloads => downloads_panel(frame, app, area, true),
        HomeFocus::Accounts => account_panel(frame, app, area, true),
        HomeFocus::Status => status_panel(frame, app, area, true),
    }
}

/// 完整日志视图
fn game_log_full(frame: &mut Frame, app: &mut App, area: Rect) {
    let block = Block::default()
        .title(format!(" 完整日志 (l/Esc 返回) {} ", app.launch_status))
        .borders(Borders::ALL);
    let lines: Vec<Line> = app
        .game_log
        .iter()
        .map(|l| log_line(l, area.width.saturating_sub(2) as usize))
        .collect();
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn log_line(text: &str, max_width: usize) -> Line<'static> {
    let t = if text.chars().count() > max_width {
        let mut s: String = text.chars().take(max_width).collect();
        s.push('…');
        s
    } else {
        text.to_string()
    };
    let is_error = t.contains("ERROR")
        || t.contains("Exception")
        || t.contains("Failed")
        || t.contains("Caused by")
        || t.contains("出错")
        || t.contains("崩溃");
    Line::from(vec![Span::styled(
        t,
        if is_error {
            theme::error()
        } else {
            theme::base()
        },
    )])
}
/// 全屏登录界面（Trawl 风格）：上方 MCTL 立体 logo，下方居中 [4399]/[LittleSkin]
/// 登录专页：logo + 登录方式选择。
/// 布局上把「logo / tagline / 卡片 / 提示」当作**一个整体**垂直居中——
/// 原先 logo 从 height/6 起画、提示钉在屏幕最底，中间六成是空的。
/// 登录专页：logo（带投影）+ 双线边框卡片 + 整行高亮选择。
/// 高度策略：`spacious` 版带卡内标题、分隔线与留白；终端放不下时自动降级为
/// 紧凑版（24 行终端的 body 只有 22 行，放不下 23 行的完整版）。
/// 登录专页：logo + 双线边框卡片 + 选中项整行高亮。
/// 高度策略：`spacious` 版带卡内标题、分隔线与留白；终端放不下时自动降级为
/// 紧凑版（24 行终端 body 只有 22 行，容纳不下 23 行的完整版）。
/// 分隔线横跨整个卡片宽度并覆盖两侧 `║`——嵌在框内会变成 `║╟──╢║`，双线嵌套很噪。
pub fn draw_login_full(frame: &mut Frame, app: &mut App, area: Rect) {
    use crate::ui::{MCTL_BANNER, MCTL_TAGLINE};
    use ratatui::widgets::{Block, BorderType, Borders};

    let Some(ls) = app.login_screen.as_ref() else {
        return;
    };
    let sel = ls.selected;
    let input_4399 = ls.input_4399;

    // 两种登录方式：图标 + 名称 + 说明（说明这方式能干什么，别只写名字）
    const OPTS: [(&str, &str, &str); 2] = [
        (
            "◉",
            "4399 账号登录",
            "网易渠道账号 · 资源大厅 / 资源下载",
        ),
        ("⬡", "LittleSkin 登录", "皮肤站外置登录（authlib-injector）"),
    ];

    // 输入区高度：4399 账号密码 4 行 / LittleSkin 授权码 3 行
    let input_h: u16 = if input_4399 && sel == 0 {
        4
    } else if sel == 1 && app.ls.login.is_some() {
        3
    } else {
        0
    };

    const W: usize = 64; // 卡片内部宽度
    let logo_h = MCTL_BANNER.len() as u16;
    // 紧凑：选项 4 + 提示 1 + 输入区；完整：另加 卡内标题/两条分隔线/两处留白
    // 紧凑：选项 4 + 分隔 1 + 提示 1
    let inner_min = 6 + input_h;
    // 完整：标题 1 + 分隔 1 + 留白 1 + 选项 4 + 留白 1 + 分隔 1 + 提示 1 = 10
    let inner_full = 10 + input_h;
    let chrome = logo_h + 3; // logo + 间隔 + tagline + 间隔
    let spacious = area.height >= chrome + inner_full + 2 + 1;
    let inner_h = if spacious { inner_full } else { inner_min };
    let card_h = inner_h + 2;
    let total = chrome + card_h;
    if total > area.height {
        return; // 连紧凑版都放不下：整体跳过，不画半截
    }

    // ---- 组装卡片内容：(行, 是否为分隔线) ----
    let sep = || (Line::from(Span::raw("")), true);
    let blank = || (Line::raw(String::new()), false);
    // 整行高亮：补齐到卡片宽度，背景铺满
    let fill = |spans: Vec<Span<'static>>, hl: bool| -> Line<'static> {
        let used: usize = spans
            .iter()
            .map(|s| crate::ui::cell_width_of(s.content.as_ref()))
            .sum();
        let pad = " ".repeat(W.saturating_sub(used));
        let mut v = spans;
        if hl {
            v.push(Span::styled(pad, theme::selected_block()));
            Line::from(v).style(theme::selected_block())
        } else {
            v.push(Span::raw(pad));
            Line::from(v)
        }
    };

    let mut body: Vec<(Line<'static>, bool)> = Vec::new();
    if spacious {
        // 标题按**显示列宽**居中（chars.count 会偏，中文 2 列）
        let t = "登  录";
        let pad = W.saturating_sub(crate::ui::cell_width_of(t)) / 2;
        body.push((
            Line::from(Span::styled(
                format!("{}{}", " ".repeat(pad), t),
                theme::accent().bold(),
            )),
            false,
        ));
        body.push(sep());
        body.push(blank());
    }

    for (i, (icon, name, desc)) in OPTS.iter().enumerate() {
        let on = sel == i;
        body.push((
            fill(
                vec![
                    Span::styled(if on { "▌" } else { " " }, theme::accent().bold()),
                    Span::raw(" "),
                    Span::styled(
                        *icon,
                        if on {
                            theme::primary()
                        } else {
                            theme::fg_muted()
                        },
                    ),
                    Span::raw("  "),
                    Span::styled(
                        *name,
                        if on {
                            theme::accent().bold()
                        } else {
                            theme::base()
                        },
                    ),
                    Span::styled(if on { "   ←" } else { "" }, theme::accent()),
                ],
                on,
            ),
            false,
        ));
        body.push((
            fill(
                vec![Span::raw("    "), Span::styled(*desc, theme::dim())],
                on,
            ),
            false,
        ));
        if spacious && i == 0 {
            body.push(blank());
        }
    }

    if input_h > 0 {
        if spacious {
            body.push(blank());
        }
        if input_4399 {
            // 密码与验证码都只在**聚焦时**显示明文，否则一律打点
            let masked = |v: &str, on: bool| {
                if on {
                    v.to_string()
                } else {
                    "•".repeat(v.chars().count())
                }
            };
            // 第 3 行（验证码）只在验证码图已就绪时出现
            let cap_ready = app.login_challenge.is_some();
            let mut rows: Vec<(&str, String, u8)> = vec![
                ("账号", app.login_user.clone(), 0),
                (
                    "密码",
                    masked(&app.login_pass, app.login_input_step == 1),
                    1,
                ),
            ];
            if cap_ready {
                rows.push((
                    "验证码",
                    masked(&app.login_captcha, app.login_input_step == 2),
                    2,
                ));
            }
            for (label, val, step) in rows {
                let on = app.login_input_step == step;
                body.push((
                    fill(
                        vec![
                            Span::raw("  "),
                            Span::styled(
                                format!("{label} "),
                                if on { theme::accent() } else { theme::dim() },
                            ),
                            Span::styled(val, if on { theme::fg() } else { theme::fg_muted() }),
                        ],
                        on,
                    ),
                    false,
                ));
            }
            // 验证码是图片，TUI 画不出来——必须给出文件路径让人去看
            if let Some(ch) = &app.login_challenge {
                body.push((
                    fill(
                        vec![
                            Span::raw("  "),
                            Span::styled("图片 ", theme::dim()),
                            Span::styled(ch.captcha_path.display().to_string(), theme::warn()),
                        ],
                        false,
                    ),
                    false,
                ));
            }
            let hint = if cap_ready {
                "  Enter 提交 · 认错请重新登录换图 · Esc 收起"
            } else {
                "  Tab 切换字段 · Enter 下一步（取验证码）· Esc 收起"
            };
            body.push((fill(vec![Span::styled(hint, theme::dim())], false), false));
        } else if let Some(login) = &app.ls.login {
            body.push((
                fill(
                    vec![
                        Span::raw("  "),
                        Span::styled("授权码 ", theme::dim()),
                        Span::styled(login.user_code.clone(), theme::accent().bold()),
                    ],
                    false,
                ),
                false,
            ));
            body.push((
                fill(
                    vec![
                        Span::raw("  "),
                        Span::styled(login.verification_url.clone(), theme::dim()),
                    ],
                    false,
                ),
                false,
            ));
            body.push((
                fill(
                    vec![Span::styled(
                        "  请在浏览器完成授权，正在等待…",
                        theme::dim(),
                    )],
                    false,
                ),
                false,
            ));
        }
    }

    if !spacious {
        body.push(sep());
    }
    let st = app.login_status.clone().unwrap_or_default();
    if !st.is_empty() && input_4399 && sel == 0 {
        body.push((
            fill(
                vec![Span::raw("  "), Span::styled(st, theme::warn())],
                false,
            ),
            false,
        ));
    }
    body.push((
        Line::from(Span::styled(
            " [↑↓] 选择方式    [Enter/e] 编辑    [Esc] 退出",
            theme::dim(),
        )),
        false,
    ));
    if spacious {
        body.insert(body.len() - 1, sep());
    }
    // 补齐/截断到预留高度（避免底部留白或溢出边框）
    while (body.len() as u16) < inner_h {
        body.push(blank());
    }
    body.truncate(inner_h as usize);

    // ---- 垂直居中绘制 ----
    let mut y = area.y + (area.height - total) / 2;
    let cx = area.x + area.width.saturating_sub(W as u16 + 2) / 2;
    let center = |text: &str, yy: u16| -> Rect {
        let tw = crate::ui::cell_width_of(text) as u16;
        Rect {
            x: area.x + area.width.saturating_sub(tw) / 2,
            y: yy,
            width: tw,
            height: 1,
        }
    };

    for (i, line) in MCTL_BANNER.iter().enumerate() {
        let w = crate::ui::cell_width_of(line) as u16;
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                *line,
                Style::default()
                    .fg(theme::accent_color())
                    .add_modifier(Modifier::BOLD),
            ))),
            Rect {
                x: area.x + area.width.saturating_sub(w) / 2,
                y: y + i as u16,
                width: w,
                height: 1,
            },
        );
    }
    y += logo_h + 1;
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(MCTL_TAGLINE, theme::dim()))),
        center(MCTL_TAGLINE, y),
    );
    y += 2;

    // 卡片：双线边框（比圆角更有面板感）
    let card = Rect {
        x: cx,
        y,
        width: W as u16 + 2,
        height: card_h,
    };
    frame.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Double)
            .border_style(Style::default().fg(theme::border_sel())),
        card,
    );
    for (i, (l, is_sep)) in body.iter().enumerate() {
        let ry = card.y + 1 + i as u16;
        if *is_sep {
            // 横跨整个卡片：╟ + ─ + ╢，覆盖两侧的 ║
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    format!("╟{}╢", "─".repeat(W)),
                    theme::dim(),
                ))),
                Rect {
                    x: card.x,
                    y: ry,
                    width: W as u16 + 2,
                    height: 1,
                },
            );
        } else {
            frame.render_widget(
                Paragraph::new(l.clone()),
                Rect {
                    x: card.x + 1,
                    y: ry,
                    width: W as u16,
                    height: 1,
                },
            );
        }
    }
}
