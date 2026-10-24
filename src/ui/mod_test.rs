#[cfg(test)]
mod tests {
    use crate::app::{App, HomeView};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use tokio;

    fn render(app: &mut App, w: u16, h: u16) -> TestBackend {
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| crate::ui::draw(f, app)).unwrap();
        terminal.backend().clone()
    }

    fn dump(backend: &TestBackend) -> Vec<String> {
        let buf = backend.buffer();
        let mut out = Vec::new();
        for y in 0..buf.area.height {
            let mut line = String::new();
            for x in 0..buf.area.width {
                line.push(buf[(x, y)].symbol().chars().next().unwrap_or(' '));
            }
            out.push(line);
        }
        out
    }

    fn contains_line(lines: &[String], needle: &str) -> bool {
        lines.iter().any(|l| l.contains(needle))
    }

    #[tokio::test]
    async fn wm_shell_renders_dock_and_main() {
        let mut app = App::new_from(crate::storage::tmp_config_dir("ui")).unwrap();
        let backend = render(&mut app, 100, 34);
        let lines = dump(&backend);
        for l in &lines {
            eprintln!("|{l}");
        }
        // dock 面板标题（账户/视图/状态）
        assert!(contains_line(&lines, "账 户"), "dock 应有账户卡");
        assert!(contains_line(&lines, "视 图"), "dock 应有视图侧栏");
        assert!(contains_line(&lines, "状 态"), "dock 应有状态卡");
        // 主区窗口标题栏
        // 渲染层给 CJK 逐字插空格（"主 界 面"），断言要跟着这个格式
        assert!(contains_line(&lines, "主 界 面"), "侧栏应有主界面视图");
        // 分隔条
        assert!(contains_line(&lines, "│"), "应有 dock 分隔条");
        for l in &lines {
            eprintln!("|{l}");
        }
    }

    #[tokio::test]
    async fn ctrl_w_adjust_mode_shows_hint_and_resize() {
        let mut app = App::new_from(crate::storage::tmp_config_dir("ui")).unwrap();
        let w0 = app.wm.dock_width;
        app.handle_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('w'),
            crossterm::event::KeyModifiers::CONTROL,
        ));
        assert_eq!(app.wm.mode, tui_wm_core::WmMode::Adjust);
        let backend = render(&mut app, 100, 34);
        let lines = dump(&backend);
        for l in &lines {
            eprintln!("|{l}");
        }
        // 调整模式提示现在在底栏（统一底栏），不在标题栏
        let empty = String::new();
        let bottom_line = lines.last().unwrap_or(&empty);
        // ratatui CJK 渲染会在字符间加空格，"切换" → "切 换"
        assert!(
            bottom_line.contains("宽")
                && bottom_line.contains("高")
                && (bottom_line.contains("切换") || bottom_line.contains("切 换")),
            "调整模式底栏应显示键位提示: {:?}",
            bottom_line
        );
        // ←→ 调整 dock 宽度
        app.handle_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Right,
            crossterm::event::KeyModifiers::NONE,
        ));
        assert_eq!(app.wm.dock_width, w0 + 1);
        // Esc 退出
        app.handle_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Esc,
            crossterm::event::KeyModifiers::NONE,
        ));
        assert_eq!(app.wm.mode, tui_wm_core::WmMode::Normal);
    }

    #[tokio::test]
    async fn dock_view_switches_main_window() {
        let mut app = App::new_from(crate::storage::tmp_config_dir("ui")).unwrap();
        app.term_size = (100, 34);
        // 点击 dock「视图」第二项（服务器）→ 主区切换
        let w = 100u16;
        let area = ratatui::layout::Rect::new(0, 1, w, 32);
        let views_rect = app.wm.window_rect(area, crate::app::win_id::VIEWS).unwrap();
        let x = views_rect.x + 2;
        let y = views_rect.y + 2; // 第二行 = 实例（插在主界面之后）
        let ev = crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: x,
            row: y,
            modifiers: crossterm::event::KeyModifiers::NONE,
        };
        app.handle_mouse(ev);
        assert_eq!(
            app.home_view,
            HomeView::Instance,
            "点击视图第二项应切到实例"
        );
        assert_eq!(app.wm.main_win, crate::app::win_id::INSTANCE);
    }

    #[tokio::test]
    async fn dock_then_undock_main_window() {
        let mut app = App::new_from(crate::storage::tmp_config_dir("ui")).unwrap();
        app.term_size = (100, 34);
        assert_eq!(app.wm.main_win, crate::app::win_id::LOCAL);
        // Ctrl+W → d 停靠本地 → 主区换到服务器
        app.handle_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('w'),
            crossterm::event::KeyModifiers::CONTROL,
        ));
        app.handle_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('d'),
            crossterm::event::KeyModifiers::NONE,
        ));
        assert!(
            app.wm.windows[&crate::app::win_id::LOCAL].docked,
            "本地应停靠"
        );
        // 断言「主区换到了别的视图」而不是「换到某一个具体视图」：
        // 停靠 LOCAL 后 WM 会落到 add_window 顺序里的下一个窗口，而插入 Instance
        // 之后那个「下一个」就变了。写死具体 id 会在每次加视图时静默失效。
        assert_ne!(
            app.wm.main_win,
            crate::app::win_id::LOCAL,
            "停靠本地后主区应换到别的视图"
        );
        // 关键不变式：主区窗口与 home_view 必须一致（否则拖动窗口会切错视图）
        let expect = match app.wm.main_win {
            crate::app::win_id::INSTANCE => HomeView::Instance,
            crate::app::win_id::SERVER => HomeView::Server,
            crate::app::win_id::RESOURCE => HomeView::Resource,
            crate::app::win_id::PLUGINS => HomeView::Plugins,
            crate::app::win_id::SETTINGS => HomeView::Settings,
            other => panic!("主区落在未知窗口 {other}"),
        };
        assert_eq!(app.home_view, expect, "home_view 应与主区窗口同步");
        // Tab 轮转：资源下载 → 本地（已停靠也可聚焦）
        for _ in 0..2 {
            app.handle_key(crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Tab,
                crossterm::event::KeyModifiers::NONE,
            ));
        }
        app.handle_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Tab,
            crossterm::event::KeyModifiers::NONE,
        ));
        assert_eq!(
            app.wm.active,
            crate::app::win_id::LOCAL,
            "Tab 可聚焦已停靠窗口"
        );
        // d 收回主区
        app.handle_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('d'),
            crossterm::event::KeyModifiers::NONE,
        ));
        assert!(!app.wm.windows[&crate::app::win_id::LOCAL].docked);
        assert_eq!(app.wm.main_win, crate::app::win_id::LOCAL);
        assert_eq!(app.home_view, HomeView::Local);
    }

    /// 「视图」选中行是整行底色块（经典主题 = 蓝底黑字），刷色中途只盖左边一段
    #[tokio::test]
    async fn view_sidebar_selected_row_is_a_full_width_block() {
        use ratatui::buffer::Buffer;
        use ratatui::style::Color;
        use std::time::{Duration, Instant};

        // 配色断言要独占调色板（换主题的测试并行跑会串味）
        let _palette = crate::theme::test_palette_guard();
        let mut app = App::new_from(crate::storage::tmp_config_dir("ui")).unwrap();
        app.home_view = HomeView::Local;
        app.view_anim_start = None; // 静止态 = 刷满
        let buf = render(&mut app, 100, 34).buffer().clone();
        let line = |y: u16, buf: &Buffer| -> String {
            (0..buf.area.width)
                .map(|x| buf[(x, y)].symbol().chars().next().unwrap_or(' '))
                .collect()
        };
        // 宽字符占两格，逐格 dump 会在汉字间插空格——找行时先滤掉空白
        let flat = |y: u16, buf: &Buffer| -> String {
            line(y, buf)
                .chars()
                .filter(|c| !c.is_whitespace())
                .collect()
        };
        let y = (0..buf.area.height)
            .find(|&y| flat(y, &buf).contains("主界面"))
            .expect("dock 视图列表里应有「主界面」");
        // 「有选中底色」= 格子上真的铺了背景色（不写死经典主题的蓝：
        // 并行跑的换主题测试会改全局主题，写死就成竞态）
        let has_block =
            |buf: &Buffer, x: u16| matches!(buf[(x, y)].style().bg, Some(c) if c != Color::Reset);
        // dock 面板：左边框在 x=0，内容从 x=1 起
        assert_eq!(buf[(1, y)].symbol(), "▌", "选中项行首还有标记");
        assert!(has_block(&buf, 1), "静止态：标记带选中底色");
        assert!(
            has_block(&buf, 21),
            "静止态：补齐的空格也要铺满底色（整行才是块）"
        );

        // 刷到一半：左边有底色，右边还没有
        app.view_anim_start = Some(Instant::now() - Duration::from_millis(40));
        let buf = render(&mut app, 100, 34).buffer().clone();
        assert!(has_block(&buf, 1), "刷过的行首应有底色");
        assert!(!has_block(&buf, 21), "还没刷到的行尾不该有底色");
    }

    /// 卡片选中框的流光描边：只描边框格，p=0 不描、p=1 整圈描满
    #[test]
    fn border_flow_paints_border_cells_only() {
        use ratatui::layout::Rect;
        use ratatui::widgets::{Block, Borders};

        // 配色断言要独占调色板（换主题的测试并行跑会串味）
        let _palette = crate::theme::test_palette_guard();
        let area = Rect::new(0, 0, 8, 4);
        let mut terminal = Terminal::new(TestBackend::new(8, 4)).unwrap();
        let paint = |terminal: &mut Terminal<TestBackend>, p: f32| -> ratatui::buffer::Buffer {
            terminal
                .draw(|f| {
                    f.render_widget(Block::default().borders(Borders::ALL), area);
                    crate::ui::home::border_flow(f, area, p);
                })
                .unwrap();
            terminal.backend().buffer().clone()
        };
        let accent = crate::theme::accent_color();
        let head = crate::theme::head();
        let painted = |buf: &ratatui::buffer::Buffer| -> usize {
            (0..buf.area.height)
                .map(|y| {
                    (0..buf.area.width)
                        .filter(|&x| {
                            let fg = buf[(x, y)].style().fg;
                            fg == Some(accent) || fg == Some(head)
                        })
                        .count()
                })
                .sum()
        };
        let buf = paint(&mut terminal, 0.0);
        assert_eq!(painted(&buf), 0, "p=0 不该描");
        let buf = paint(&mut terminal, 0.5);
        let mid = painted(&buf);
        assert!(mid > 0 && mid < 20, "p=0.5 应只描到一部分（{mid}/20）");
        let buf = paint(&mut terminal, 1.0);
        assert_eq!(painted(&buf), 20, "p=1 整圈 20 格都该描上");
        // 内部格子不参与描边（只有外框会被流光扫到）
        for y in 1..3 {
            for x in 1..7 {
                let fg = buf[(x, y)].style().fg;
                assert!(
                    fg != Some(accent) && fg != Some(head),
                    "({x},{y}) 是内部格子"
                );
            }
        }
    }

    /// 服务器卡片：选中动画在**边框**上（文字不铺底色块），静止态整圈描满
    #[tokio::test]
    async fn server_card_selection_animates_border_not_text() {
        // 配色断言要独占调色板（换主题的测试并行跑会串味）
        let _palette = crate::theme::test_palette_guard();
        let mut app = App::new_from(crate::storage::tmp_config_dir("ui")).unwrap();
        app.servers = vec![crate::api::mcserver::JavaServer {
            name: "测试服".into(),
            host: "example.com".into(),
            port: 25565,
            version: "1.21.8".into(),
            online: 1,
            max: 20,
            ..Default::default()
        }];
        app.home_view = HomeView::Server;
        app.selected_server = 0;

        let count_border = |buf: &ratatui::buffer::Buffer| -> usize {
            let accent = crate::theme::accent_color();
            let head = crate::theme::head();
            (0..buf.area.height)
                .map(|y| {
                    (0..buf.area.width)
                        .filter(|&x| {
                            let c = &buf[(x, y)];
                            let fg = c.style().fg;
                            (fg == Some(accent) || fg == Some(head))
                                && matches!(c.symbol(), "─" | "│" | "╭" | "╮" | "╰" | "╯")
                        })
                        .count()
                })
                .sum()
        };

        // 动画刚开播：边框基本还没描到
        app.sel_anim_start = Some(std::time::Instant::now());
        let mid = count_border(render(&mut app, 100, 34).buffer());
        // 静止态（progress=1）：整圈描满
        app.sel_anim_start = None;
        let buf = render(&mut app, 100, 34).buffer().clone();
        let done = count_border(&buf);
        assert!(
            done > mid && done >= 8,
            "选中卡外框应随流光描边长出来（中途 {mid} → 静止 {done}）"
        );

        // 卡名那几个格子不能有选中底色块（动画只在边框上，不在字上）
        let brow = (0..buf.area.height)
            .find(|&y| (0..buf.area.width).any(|x| buf[(x, y)].symbol() == "测"))
            .expect("应渲染出服务器卡名");
        let bx = (0..buf.area.width)
            .find(|&x| buf[(x, brow)].symbol() == "测")
            .unwrap();
        let block_bg = crate::theme::selected_block().bg;
        for x in bx..(bx + 6).min(buf.area.width) {
            assert_ne!(
                buf[(x, brow)].style().bg,
                block_bg,
                "卡名不该铺底色块（选择动画只在边框上）"
            );
        }
    }

    /// 服务器卡片：列表项带宣传图时，封面未就绪先显示占位图标（不空一块）
    #[tokio::test]
    async fn server_card_shows_cover_placeholder() {
        let mut app = App::new_from(crate::storage::tmp_config_dir("ui")).unwrap();
        app.servers = vec![crate::api::mcserver::JavaServer {
            name: "带封面服".into(),
            host: "example.com".into(),
            port: 25565,
            version: "1.21.8".into(),
            online: 3,
            max: 20,
            favicon: Some(
                "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg=="
                    .into(),
            ),
            ..Default::default()
        }];
        app.home_view = HomeView::Server;
        app.selected_server = 0;
        app.sel_anim_start = None;
        let lines = dump(&render(&mut app, 100, 34));
        assert!(
            lines.iter().any(|l| l.contains('🖼')),
            "封面没加载出来时应显示占位图标"
        );
        // 渲染层会给 CJK 逐字插空格，断言前先压掉空白
        assert!(
            lines.iter().any(|l| l
                .chars()
                .filter(|c| !c.is_whitespace())
                .collect::<String>()
                .contains("带封面服")),
            "卡名不能因为封面被挤掉"
        );
    }

    /// 服务器页顶部：必须是 MCTL 大字（用户口径），**不放服务器宣传图**
    /// （卡片封面可以有图，横幅不行——之前横幅拉的是服务器图，观感像详情页）
    #[tokio::test]
    async fn server_page_banner_is_wordmark_not_server_cover() {
        let mut app = App::new_from(crate::storage::tmp_config_dir("ui")).unwrap();
        app.servers = vec![crate::api::mcserver::JavaServer {
            name: "带封面服".into(),
            host: "example.com".into(),
            port: 25565,
            version: "1.21.8".into(),
            online: 3,
            max: 20,
            ..Default::default()
        }];
        app.home_view = HomeView::Server;
        app.selected_server = 0;
        let lines = dump(&render(&mut app, 100, 34));
        let logo_y = lines
            .iter()
            .position(|l| l.contains("███╗"))
            .unwrap_or_else(|| panic!("服务器页顶部应是 MCTL 大字：\n{}", lines.join("\n")));
        assert!(logo_y < 8, "MCTL 大字应在页面顶部，实际在第 {logo_y} 行");
        // 横幅一旦改成拉服务器宣传图，就会走「有图」分支、MCTL 大字整个消失
        //（上面已断言它必须在）——这里再钉一条：大字上方不能有图片占位
        assert!(
            !lines.iter().take(logo_y).any(|l| l.contains('🖼')),
            "大字上方不该有图片占位（横幅不放宣传图，占位图标只属于卡片封面）"
        );
    }

    /// 下载中心：进服链路的任务也要在面板里（任务名 · 阶段 + 单文件细线）
    /// 用户口径：所有下载（包括服务器进服相关）都要能在下载中心看到进度
    #[tokio::test]
    async fn download_center_shows_server_launch_task() {
        let mut app = App::new_from(crate::storage::tmp_config_dir("ui")).unwrap();
        // 直接摆状态：进度消息的落位逻辑在 app.rs 的测试里，这里只看渲染
        app.install_tasks.push(crate::app::InstallTask {
            id: 7,
            name: "进服 · 布吉岛".into(),
            phase: "下载网易客户端 base".into(),
            done: 0,
            total: 0,
            status: crate::app::InstallStatus::Active,
            started: std::time::Instant::now(),
            files: vec![crate::app::FileSlot {
                name: "netease-client-base.zip".into(),
                done: 12 * 1024 * 1024,
                total: 40 * 1024 * 1024,
                started: std::time::Instant::now(),
                failed: false,
                finished: false,
            }],
            bytes_done: 12 * 1024 * 1024,
            bytes_total: 40 * 1024 * 1024,
        });
        app.home_view = HomeView::Download;
        app.sync_wm_from_view();
        let lines = dump(&render(&mut app, 110, 34));
        // 渲染层给 CJK 逐字插空格，断言前压掉所有空白
        let flat: String = lines
            .join("\n")
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        assert!(flat.contains("进服·布吉岛"), "任务名要在下载中心出现");
        assert!(flat.contains("下载网易客户端base"), "阶段文本要在");
        assert!(
            flat.contains("netease-client-base.zip"),
            "单文件进度要在（细线 + 名字）"
        );
        assert!(flat.contains("30%") || flat.contains("29%"), "百分比要在");
    }

    // ---- 卡片网格窗口（租赁服/联机服的方向键手感）----

    #[test]
    fn card_grid_window_keeps_selection_visible() {
        use crate::ui::home::grid_start;
        let page = 9usize; // 3×3
        for len in 1..=40usize {
            for sel in 0..len {
                for start in [0usize, sel / 2, len, usize::MAX] {
                    let s = grid_start(sel, page, len, start);
                    if len <= page {
                        assert_eq!(s, 0, "不满一屏就不动窗口");
                        continue;
                    }
                    assert!(
                        s <= sel,
                        "窗口不越过选中项：len={len} sel={sel} start={start}"
                    );
                    assert!(sel < s + page, "选中项必须在窗口里：len={len} sel={sel}");
                    assert!(s + page <= len, "窗口不越过末尾：len={len} sel={sel} s={s}");
                }
            }
        }
    }

    #[test]
    fn card_grid_window_has_hysteresis() {
        use crate::ui::home::grid_start;
        // 10 项、一屏 9：第 10 项只把窗口滑到 1（旧行为是整页翻到 9，
        // 选中框会从第三行瞬移到左上角 —— 用户说的「选择是乱的」）
        assert_eq!(grid_start(9, 9, 10, 0), 1);
        // 选中项还在窗口里 → 窗口一动不动（避免来回抖）
        assert_eq!(grid_start(6, 9, 10, 1), 1);
        // 出界了才滑回去
        assert_eq!(grid_start(0, 9, 10, 1), 0);
        // 长列表：8 → 11 只滑一行（窗口 0 → 3），↑ 回到 8 时窗口不跟着跳
        assert_eq!(grid_start(11, 9, 30, 0), 3);
        assert_eq!(grid_start(8, 9, 30, 3), 3);
    }

    /// 资源中心来源条：选中项是**主题色块**（不是下划线），切来源时按进度刷出来。
    /// 用户口径：资源下载中心顶栏选中部分要有底色，且要和主题系统结合。
    #[tokio::test]
    async fn source_bar_selected_chip_has_theme_block_and_wipes() {
        use crate::app::{HomeView, ResourceSource};
        let mut app = App::new_from(crate::storage::tmp_config_dir("ui-srcbar")).unwrap();
        app.home_view = HomeView::Resource;
        app.source = ResourceSource::NetEase;
        app.src_anim_start = None; // 静止态 = 直接刷满
        app.sync_wm_from_view();

        assert!(
            crate::theme::selected_block().bg.is_some(),
            "选中块必须有底色（主题里 selection_bg）"
        );
        // 只数「来源条那一行」带底色的格子：不依赖当前主题的具体颜色
        // （主题是全局状态，别的测试会并行切主题）
        let count_bg = |app: &mut App| -> usize {
            let mut terminal = Terminal::new(TestBackend::new(110, 30)).unwrap();
            terminal.draw(|f| crate::ui::draw(f, app)).unwrap();
            let buf = terminal.backend().buffer();
            let mut row = None;
            for y in 0..buf.area.height {
                // dump 按单元格拼字符，宽字符之间会留空格——压掉空白再找
                let text: String = (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol().chars().next().unwrap_or(' '))
                    .filter(|c| !c.is_whitespace())
                    .collect();
                // 「切换来源」只出现在来源条上；底部状态栏也有"资源"二字，不能拿来定位
                if text.contains("切换来源") {
                    row = Some(y);
                    break;
                }
            }
            let Some(y) = row else { panic!("界面上找不到来源条") };
            (0..buf.area.width)
                .filter(|&x| {
                    matches!(buf[(x, y)].style().bg, Some(c) if c != ratatui::style::Color::Reset)
                })
                .count()
        };

        let full = count_bg(&mut app);
        // 选中 chip 至少这几个字宽（" NetEase "），必须都铺上底色
        assert!(
            full >= 8,
            "来源条选中项应有底色块（至少覆盖标签宽度），实际只有 {full} 格"
        );

        // 刚切来源 → 刷色刚开始，铺到底色的格子应明显少于刷满态
        app.bump_src_anim();
        assert!(app.src_anim_playing(), "切来源后动画应在播");
        let early = count_bg(&mut app);
        assert!(
            early < full,
            "刷色刚开始时底色块应还未铺满（early={early} full={full}）"
        );

        // 动画结束（progress = 1）→ 回到刷满态
        app.src_anim_start = None;
        assert_eq!(count_bg(&mut app), full, "刷完后应回到满底色");
    }

    /// 下载中心：每个下载各一条长进度条（整条按字节），下面保留文件细线。
    /// 两个下载同时跑时两条都在——并发下载不能互相顶掉。
    #[tokio::test]
    async fn download_center_draws_one_long_bar_per_download() {
        use crate::app::{FileSlot, InstallStatus, InstallTask};
        let mut app = App::new_from(crate::storage::tmp_config_dir("ui-dlcenter")).unwrap();
        app.home_view = HomeView::Download;
        app.sync_wm_from_view();
        let mk = |id: u64, name: &str, done: u64, total: u64| InstallTask {
            id,
            name: name.into(),
            phase: "下载中".into(),
            done: 0,
            total: 0,
            status: InstallStatus::Active,
            started: std::time::Instant::now(),
            files: vec![FileSlot {
                name: format!("{name}.part"),
                done,
                total,
                started: std::time::Instant::now(),
                failed: false,
                finished: false,
            }],
            bytes_done: done,
            bytes_total: total,
        };
        app.install_tasks.push(mk(1, "地图A.zip", 30, 100));
        app.install_tasks.push(mk(2, "模组B.jar", 5, 10));
        let raw = dump(&render(&mut app, 120, 40)).join("\n");
        // dump 按单元格拼字符，宽字符后面会多一个空格；压掉空白再断言
        let out: String = raw.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(out.contains("地图A.zip"), "第一个下载要在");
        assert!(out.contains("模组B.jar"), "第二个下载也要在");
        assert!(
            out.matches("进度").count() >= 2,
            "每个下载各一条长进度条，实际只画了 {} 条",
            out.matches("进度").count()
        );
        assert!(out.contains("地图A.zip.part"), "文件细线要保留");
        assert!(out.contains("进行中"), "顶部汇总行");
        assert!(out.contains("总速度"), "汇总要给总速度");
    }

}
#[cfg(test)]
mod extra_view_tests {
    use crate::app::{win_id, App, HomeView, LoginScreen, SettingBtn, SettingField, SettingGroup};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn render(app: &mut App, w: u16, h: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal.draw(|f| crate::ui::draw(f, app)).unwrap();
        let buf = terminal.backend().buffer();
        let mut out = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    /// 断言前压掉所有空白。
    /// /// 注意这不是"字距"：ratatui 的 `set_stringn` 给宽字符（中文 2 列）写完主格后，
    /// 会把第二格 `reset` 成空格 —— 那一格是字形的右半部分，被终端遮住。
    /// 逐格 dump 时它看起来像插在字中间，实际渲染没有间距。
    fn flat(s: &str) -> String {
        s.chars().filter(|c| !c.is_whitespace()).collect()
    }

    /// 横幅高度自适应：没有截图/封面时**不能**再占掉近八成高度。
    /// 自动截图现已默认关（隐私），若横幅仍按 width*9/16 固定高度，80×50 的终端上
    /// 就是 45 行空白 logo，而下面的世界列表只分到 11 行（实测常显示 1 行）。
    #[tokio::test]
    async fn banner_shrinks_when_there_is_no_image() {
        let mut app = App::new_from(crate::storage::tmp_config_dir("ui")).unwrap();
        app.home_view = HomeView::Local;
        app.sync_wm_from_view();
        app.banner_shot = None;
        let out = render(&mut app, 100, 40);
        // 世界面板必须分到多行；无图时若横幅又抢回高度，这里就只剩 1 行
        let wrows = out
            .lines()
            .filter(|l| l.contains("saves") || l.contains("暂无世界"))
            .count();
        assert!(
            wrows <= 1,
            "无图时世界面板应占多行，实际只有 {wrows} 行（横幅是不是又抢回去了）"
        );
        assert!(out.lines().count() > 20, "渲染行数异常");
    }

    /// 视图栏要列出全部视图 + 图标 + alt 位
    #[tokio::test]
    async fn sidebar_lists_every_view_with_icon_and_alt_slot() {
        let mut app = App::new_from(crate::storage::tmp_config_dir("ui")).unwrap();
        let text = flat(&render(&mut app, 100, 34));
        for v in HomeView::ALL {
            assert!(text.contains(&flat(v.label())), "视图栏缺少 {}", v.label());
            assert!(text.contains(v.icon()), "视图栏缺少 {} 的图标", v.label());
            assert!(
                text.contains(&flat(&format!("alt+{}", v.slot()))),
                "视图栏缺少 {} 的 alt 位",
                v.label()
            );
        }
    }

    /// 插件页：空态给安装提示，不留白
    #[tokio::test]
    async fn plugins_view_empty_state_points_at_install() {
        let mut app = App::new_from(crate::storage::tmp_config_dir("ui")).unwrap();
        app.home_view = HomeView::Plugins;
        app.sync_wm_from_view();
        let text = flat(&render(&mut app, 100, 34));
        assert!(text.contains("插件"), "标题应含「插件」");
        assert!(text.contains("plugininstall"), "空态应提示如何安装插件");
    }

    /// 登录专页：两种方式都在、无重复字、整块垂直居中、卡片右边框逐行对齐
    #[tokio::test]
    async fn login_page_is_centered_and_lists_both_methods() {
        let mut app = App::new_from(crate::storage::tmp_config_dir("ui")).unwrap();
        app.login_screen = Some(LoginScreen {
            selected: 0,
            input_4399: false,
        });
        let rows: Vec<String> = render(&mut app, 100, 30)
            .lines()
            .map(String::from)
            .collect();
        let joined: String = flat(&rows.join("\n"));

        assert!(joined.contains("4399账号登录"), "缺少 4399 选项");
        assert!(joined.contains("LittleSkin登录"), "缺少 LittleSkin 选项");
        assert!(!joined.contains("[4399]4399"), "4399 不该重复（旧版文案）");

        // 卡片右边框逐行对齐。
        // 必须只量卡片区域：logo 用的是 ANSI Shadow 块字符，里面也有 ║，会把结果搅乱。
        let top = rows
            .iter()
            // 用 trim_start.starts_with 定位卡片：logo 是 ANSI Shadow 块字符，
            // 行内也含 ╔ ╚ ╝，直接 contains 会匹配到 logo 行上去
            .position(|r| r.trim_start().starts_with('╔'))
            .expect("应有卡片上边框");
        let cols: Vec<usize> = rows[top + 1..]
            .iter()
            .take_while(|r| !r.trim_start().starts_with('╚'))
            .filter_map(|r| {
                let v: Vec<char> = r.chars().collect();
                v.iter().rposition(|&c| c == '║' || c == '╢')
            })
            .collect();
        assert!(cols.len() >= 4, "卡片内容行太少: {cols:?}");
        assert!(
            cols.windows(2).all(|w| w[0] == w[1]),
            "右边框未对齐: {cols:?}"
        );

        // 垂直居中：logo 不贴顶，提示紧跟卡片而非钉在屏幕底
        // 逐行 flat 后再匹配：raw 行里 CJK 之间夹着宽字符的隐藏格
        let frows: Vec<String> = rows.iter().map(|r| flat(r)).collect();
        let logo_row = frows
            .iter()
            .position(|r| r.contains('█'))
            .expect("应渲染 logo");
        assert!(logo_row >= 2, "logo 贴顶（行 {logo_row}），整块没居中");
        let hint_row = frows
            .iter()
            .position(|r| r.contains("选择方式"))
            .expect("应有底部提示");
        assert!(hint_row < 27, "提示贴底（行 {hint_row}）");
    }

    /// 登录页三档布局：完整版 / 紧凑版 / 整体跳过
    #[tokio::test]
    async fn login_page_adapts_to_terminal_height() {
        let shot = |h: u16| {
            let mut app = App::new_from(crate::storage::tmp_config_dir("ui")).unwrap();
            app.login_screen = Some(LoginScreen {
                selected: 0,
                input_4399: false,
            });
            render(&mut app, 100, h)
        };
        // 30 行：完整版 —— 双线卡片 + 卡内标题 + 2 条分隔线 + 留白
        let tall = shot(30);
        assert!(
            tall.contains('╔') && tall.contains('╚'),
            "30 行应渲染双线卡片"
        );
        assert_eq!(tall.matches('╟').count(), 2, "完整版应有 2 条分隔线");
        // 20 行：紧凑版 —— 放不下标题与留白，但功能完整
        let short = shot(20);
        assert!(short.contains('╔'), "20 行仍应渲染卡片（紧凑版）");
        assert_eq!(short.matches('╟').count(), 1, "紧凑版只保留 1 条分隔线");
        assert!(short.contains("4399"), "紧凑版也要有选项");
        // 16 行：连紧凑版都放不下 —— 整体跳过，不画半截
        let tiny = flat(&shot(16));
        assert!(!tiny.contains('█'), "16 行应整体跳过（不画 logo）");
        assert!(!tiny.contains("4399账号登录"), "16 行不应画卡片");
    }

    /// 分隔线横跨整个卡片宽度并覆盖两侧边框，不能嵌成 ║╟──╢║（双线嵌套很噪）
    #[tokio::test]
    async fn login_page_separator_spans_full_card() {
        let mut app = App::new_from(crate::storage::tmp_config_dir("ui")).unwrap();
        app.login_screen = Some(LoginScreen {
            selected: 0,
            input_4399: false,
        });
        let rows: Vec<String> = render(&mut app, 100, 30)
            .lines()
            .map(String::from)
            .collect();
        let sep = rows.iter().find(|r| r.contains('╟')).expect("应有分隔线");
        let t = sep.trim();
        assert!(
            t.starts_with('╟'),
            "分隔线应以 ╟ 起始: {:?}",
            t.chars().take(6).collect::<String>()
        );
        assert!(t.ends_with('╢'), "分隔线应以 ╢ 结束");
        assert!(
            !t.contains('║'),
            "分隔线行不应同时含边框 ║（否则成了 ║╟──╢║）: {}",
            t.chars().take(20).collect::<String>()
        );
    }

    /// 终端太矮时整体跳过，而不是画出半截 logo
    #[tokio::test]
    async fn login_page_skips_when_terminal_too_short() {
        let mut app = App::new_from(crate::storage::tmp_config_dir("ui")).unwrap();
        app.login_screen = Some(LoginScreen {
            selected: 0,
            input_4399: false,
        });
        // topbar 总会画，所以不能断言整体为空——只断言 logo/卡片没出现
        let text = flat(&render(&mut app, 60, 10));
        assert!(!text.contains('█'), "高度不足时不应画 logo");
        assert!(!text.contains("4399账号登录"), "高度不足时不应画卡片");
    }

    /// GitHub 页：搜索框 + 列表 + topic 徽章 + 选中第二行详情
    #[tokio::test]
    async fn github_view_renders_search_box_and_badge() {
        let mut app = App::new_from(crate::storage::tmp_config_dir("ui")).unwrap();
        app.home_view = HomeView::Resource;
        app.sync_wm_from_view();
        app.source = crate::app::ResourceSource::Github;
        app.gh_loaded = true;
        let mk = |full: &str, desc: &str, topics: Vec<&str>| crate::api::github::Repo {
            full_name: full.into(),
            description: Some(desc.into()),
            stargazers_count: 42,
            updated_at: Some("2026-09-20T08:00:00Z".into()),
            topics: topics.into_iter().map(String::from).collect(),
            html_url: format!("https://github.com/{full}"),
        };
        app.gh_results = vec![
            mk("u/mctl-bedrock", "基岩版多版本管理", vec!["mctl-plugin"]),
            mk("u/other", "无关项目", vec![]),
        ];
        app.refilter_github();
        app.dl.install_status = "找到 2 个仓库· 剩余配额 9".into();
        let text = flat(&render(&mut app, 100, 34));
        assert!(text.contains("topic:mctl-plugin"), "标题应显示注册表 topic");
        assert!(text.contains("mctl-bedrock"), "应列出仓库短名");
        assert!(text.contains("基岩版多版本管理"), "应列出描述");
        assert!(text.contains("2026-09-20"), "选中项应显示更新日期");
        assert!(text.contains("剩余配额9"), "应透出 GitHub 配额");
    }

    /// 设置页：焦点字段名铺整块底色（与列表选中项同一口径，不是全屏背景）
    #[tokio::test]
    async fn settings_focused_field_has_block_background() {
        let mut app = App::new_from(crate::storage::tmp_config_dir("ui-set-block")).unwrap();
        app.home_view = HomeView::Settings;
        app.sync_wm_from_view();
        app.enter_settings();
        app.set_focus = 0;
        let backend = TestBackend::new(110, 44);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
        let buf = terminal.backend().buffer();
        let want = crate::theme::selected_block().bg.expect("主题要有选中底色");
        let label = SettingField::ALL[0].label();
        let mut hit = false;
        for y in 0..buf.area.height {
            let row: String = (0..buf.area.width)
                .map(|x| buf[(x, y)].symbol())
                .collect::<String>()
                .chars()
                .filter(|c| !c.is_whitespace())
                .collect();
            if !row.contains(label) {
                continue;
            }
            let n = (0..buf.area.width)
                .filter(|&x| buf[(x, y)].style().bg == Some(want))
                .count();
            if n >= label.chars().count() {
                hit = true;
                break;
            }
        }
        assert!(hit, "焦点字段「{label}」应有整块底色");
    }

    /// 设置页：可编辑字段（输入框）+ 动作按钮 + 只读区
    #[tokio::test]
    async fn settings_view_is_editable_with_buttons() {
        let mut app = App::new_from(crate::storage::tmp_config_dir("ui")).unwrap();
        app.home_view = HomeView::Settings;
        app.sync_wm_from_view();
        app.enter_settings();
        let text = flat(&render(&mut app, 100, 44));

        // 窄屏走紧凑模式：字段名一个不少，说明只画焦点那条
        for f in SettingField::ALL {
            assert!(text.contains(&flat(f.label())), "缺少字段 {}", f.label());
        }
        // 分组小标题：找设置先找分类（路径 / 联机 / 资源中心 / 界面 / 隐私）
        for g in [
            SettingGroup::Path,
            SettingGroup::Online,
            SettingGroup::Resource,
            SettingGroup::Ui,
            SettingGroup::Privacy,
        ] {
            assert!(
                text.contains(&flat(&format!("── {}", g.title()))),
                "缺少分组标题 {}",
                g.title()
            );
        }
        assert!(
            text.contains(&flat(SettingField::GameDir.hint())),
            "焦点字段的说明应常驻"
        );
        assert!(text.contains('│'), "应有输入框边框");
        for b in SettingBtn::ALL {
            assert!(text.contains(&flat(b.label())), "缺少按钮 {}", b.label());
        }
        // wm.toml 的项写不回去，必须标明只读并说明去哪改
        assert!(text.contains("只读"), "应标明只读区");
        assert!(text.contains("wm.toml"), "只读区应指向 wm.toml");
        assert!(text.contains("Ctrl+W"), "只读区应显示键位");

        // 大终端（装得下）：每个字段的说明都完整可见
        let tall = flat(&render(&mut app, 100, 60));
        for f in SettingField::ALL {
            assert!(
                tall.contains(&flat(f.hint())),
                "大终端上说明应全量显示：{}",
                f.label()
            );
        }
    }

    /// 焦点在字段/按钮间移动
    #[tokio::test]
    async fn settings_focus_moves_across_fields_and_buttons() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut app = App::new_from(crate::storage::tmp_config_dir("ui")).unwrap();
        app.home_view = HomeView::Settings;
        app.enter_settings();
        assert_eq!(
            app.set_focused_field(),
            Some(SettingField::GameDir),
            "初始在第 1 个字段"
        );
        assert_eq!(app.set_focused_btn(), None, "初始不在按钮上");

        let down = KeyEvent::new(KeyCode::Down, KeyModifiers::NONE);
        for _ in 0..SettingField::ALL.len() {
            app.handle_key(down);
        }
        assert_eq!(
            app.set_focused_btn(),
            Some(SettingBtn::Save),
            "走过字段后进入按钮区"
        );
        app.handle_key(down);
        assert_eq!(app.set_focused_btn(), Some(SettingBtn::Revert));
        // 从第一个往上应回绕到最后一个按钮
        app.set_focus = 0;
        app.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(
            app.set_focused_btn(),
            Some(SettingBtn::Defaults),
            "回绕到最后一个按钮"
        );
    }

    /// 数值字段 ←→ 增减，且夹在 1..12
    #[tokio::test]
    async fn settings_numeric_bump_clamps() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut app = App::new_from(crate::storage::tmp_config_dir("ui")).unwrap();
        app.home_view = HomeView::Settings;
        app.enter_settings();
        let idx = SettingField::ALL
            .iter()
            .position(|f| *f == SettingField::GridCols)
            .unwrap();
        app.set_focus = idx;
        let before = app.grid_cols;
        app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(app.grid_cols, before + 1, "→ 应加 1");
        app.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        assert_eq!(app.grid_cols, before, "← 应减 1");
        // 一路减到下界
        for _ in 0..30 {
            app.set_focus = idx;
            app.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        }
        assert_eq!(app.grid_cols, 1, "不应低于 1");
    }

    /// 校验失败不改 App，且把原因显示到状态栏、留在编辑态
    #[tokio::test]
    async fn settings_reject_invalid_values_without_mutating() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut app = App::new_from(crate::storage::tmp_config_dir("ui")).unwrap();
        app.home_view = HomeView::Settings;
        app.enter_settings();
        let before = app.mc_dir.clone();
        app.set_focus = 0;
        app.set_buf = "/definitely/not/here/xyz".into();
        app.set_editing = true;
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.mc_dir, before, "校验失败不应改动 mc_dir");
        assert!(app.set_msg.contains('✗'), "应显示失败原因: {}", app.set_msg);
        assert!(app.set_editing, "校验失败应留在编辑态让用户改");
    }

    /// 保存 → 落盘 → 新实例重读能拿回来
    #[tokio::test]
    async fn settings_save_persists_and_reloads() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let dir = crate::storage::tmp_config_dir("settings");
        let mut app = App::new_from(dir.clone()).unwrap();
        app.home_view = HomeView::Settings;
        app.enter_settings();
        let idx = SettingField::ALL
            .iter()
            .position(|f| *f == SettingField::GridCols)
            .unwrap();
        app.set_focus = idx;
        app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        let want = app.grid_cols;
        assert!(app.set_dirty, "改动后应为未保存状态");

        app.set_focus = SettingField::ALL.len(); // 第一个按钮 = Save
        assert_eq!(app.set_focused_btn(), Some(SettingBtn::Save));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(!app.set_dirty, "保存后应清掉未保存标记");
        // 回归：曾经 load/save_settings 用全局 config_dir，测试把 game_dir 写成了
        // 临时目录，直接毁掉用户真实的 ~/.config/mctl/settings.json。
        assert!(
            dir.join("settings.json").exists(),
            "设置必须写进 cfg_root，不能落到全局 ~/.config/mctl"
        );
        assert!(app.set_msg.contains('✓'), "应提示保存成功: {}", app.set_msg);

        let app2 = App::new_from(dir).unwrap();
        assert_eq!(app2.grid_cols, want, "重启后应恢复网格列数");
    }

    /// 切换视图后 WM 主区窗口要跟着换（新视图必须有对应窗口）
    #[tokio::test]
    async fn new_views_have_wm_windows_and_round_trip() {
        let mut app = App::new_from(crate::storage::tmp_config_dir("ui")).unwrap();
        for (view, id) in [
            (HomeView::Instance, win_id::INSTANCE),
            (HomeView::Plugins, win_id::PLUGINS),
            (HomeView::Settings, win_id::SETTINGS),
        ] {
            assert!(
                app.wm.windows.contains_key(&id),
                "{} 没有对应窗口",
                view.label()
            );
            app.home_view = view;
            app.sync_wm_from_view();
            assert_eq!(app.wm.main_win, id, "{} 未聚焦到自己的窗口", view.label());
            // 反向：窗口变化要能把视图切回来
            app.sync_view_from_wm();
            assert_eq!(app.home_view, view, "wm→view 反向映射断了");
        }
    }

    /// CJK 宽度：截断必须按显示列宽，不能按 char 数
    #[test]
    fn cjk_truncation_respects_cell_width() {
        use crate::ui::{cell_width_of, fit};
        // 15 个汉字 = 30 列
        let s = "配置根目录测试内容一二三四五六";
        assert_eq!(cell_width_of(s), 30);
        // 截到 10 列：最多 4 个汉字 + 省略号(1) = 9 列，不超 10
        let out = fit(s, 10);
        assert!(
            cell_width_of(&out) <= 10,
            "截断后仍超宽: {} 列",
            cell_width_of(&out)
        );
        assert!(out.ends_with('…'), "超宽应加省略号");
        // 不超宽时原样返回
        assert_eq!(fit("abc", 10), "abc");
        // 图标按 1 列算
        for v in HomeView::ALL {
            assert_eq!(cell_width_of(v.icon()), 1, "{} 图标应单列", v.label());
        }
    }

    /// 「视图」选中块刷色：按显示列宽切行，宽字符不会被劈成半个
    #[test]
    fn wipe_helpers_split_by_display_cells() {
        use crate::ui::home::{wipe_spans, wipe_width};
        use crate::ui::split_cells;
        use ratatui::style::{Color, Style};
        use ratatui::text::Span;

        // 进度 → 覆盖列数（超界夹住）
        assert_eq!(wipe_width(20, 0.0), 0);
        assert_eq!(wipe_width(20, 0.5), 10);
        assert_eq!(wipe_width(20, 1.0), 20);
        assert_eq!(wipe_width(20, 2.0), 20);

        // 切字符串：ASCII 一刀两断；切口落在汉字中间时整个字归左边（宁可多盖一列）
        assert_eq!(split_cells("abc", 1), ("a".into(), "bc".into()));
        assert_eq!(split_cells("abc", 9), ("abc".into(), String::new()));
        assert_eq!(split_cells("主界面", 3), ("主界".into(), "面".into()));
        assert_eq!(split_cells("主界面", 0), (String::new(), "主界面".into()));

        // 切行：刷到的部分套选中块样式（底色 + 块上字色），没刷到的原样
        let plain = Style::default().fg(Color::Red);
        let block = Style::default().fg(Color::Black).bg(Color::Blue);
        let spans = vec![
            Span::styled("ab", plain),
            Span::styled("cd", plain),
            Span::styled("ef", plain),
        ];
        let out = wipe_spans(spans, 3, block);
        let text: String = out.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "abcdef", "切行不能丢字或重复");
        // 逐段量过去：第 3 列之前一律块样式，之后保持原样
        let mut col = 0usize;
        for s in &out {
            let want = if col < 3 { block } else { plain };
            assert_eq!(
                s.style, want,
                "第 {col} 列起的「{}」样式不对（cut = 3）",
                s.content
            );
            col += crate::ui::cell_width_of(&s.content);
        }
        assert_eq!(out[1].content.as_ref(), "c", "跨段的那半截要在块里");

        // 刷满 = 整行都在块里
        let spans = vec![Span::styled("ab", plain)];
        let out = wipe_spans(spans, 99, block);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].style, block);
    }

    /// 世界选择滚动窗口：选中项永远可见，底部允许露半行
    #[test]
    fn world_select_window_keeps_selected_visible() {
        use crate::ui::world_select::view_window;
        // 每行 8 行高（上框线 + 缩略图 6 + 下框线），24 行一屏 3 行
        assert_eq!(view_window(0, 10, 24, 8), (0, 3));
        assert_eq!(view_window(2, 10, 24, 8), (0, 3));
        assert_eq!(view_window(3, 10, 24, 8), (1, 3));
        assert_eq!(view_window(9, 10, 24, 8), (7, 3));
        // 不满一屏：全部可见
        assert_eq!(view_window(1, 3, 24, 8), (0, 3));
        // 底部露半行（ceil）：17 行高也显示 3 行，最后一行半截
        assert_eq!(view_window(0, 10, 17, 8), (0, 3));
        // 空列表 / 零高度
        assert_eq!(view_window(0, 0, 24, 8), (0, 0));
        assert_eq!(view_window(0, 5, 0, 8), (0, 0));
    }

    /// 选中框描边：从左向右"同时包围"——
    /// 上边线和下边线一起从左往右扫，左端先包过来，右端最后合拢
    #[test]
    fn world_border_wraps_from_left_to_right() {
        use crate::ui::world_select::border_cell;
        let (w, h) = (30u16, 8u16);
        // 起点：还没画上
        assert_eq!(border_cell(w, h, 0, 0, 0.0), None);
        // 刚开始：左上/左下两角先出，上下边线同时起步，远处还没到
        assert_eq!(border_cell(w, h, 0, 0, 0.01), Some(true));
        assert_eq!(border_cell(w, h, 0, h - 1, 0.01), Some(true));
        assert_eq!(border_cell(w, h, 5, 0, 0.01), None);
        // 扫到一半：过掉的格已定型，靠前的一小段在发光，右角还没到
        assert_eq!(border_cell(w, h, 5, 0, 0.5), Some(false));
        assert_eq!(border_cell(w, h, 20, 0, 0.5), Some(true));
        assert_eq!(border_cell(w, h, 20, h - 1, 0.5), Some(true));
        assert_eq!(border_cell(w, h, 28, 0, 0.5), None);
        // 右边线要到关门阶段才出现
        assert_eq!(border_cell(w, h, w - 1, 1, 0.5), None);
        // 左边线：早期从上下两端往中间长
        assert_eq!(border_cell(w, h, 0, 1, 0.02), None);
        assert!(border_cell(w, h, 0, 1, 0.2).is_some());
        assert!(border_cell(w, h, 0, h / 2, 0.2).is_some());
        // 收尾：右角画到、右边线合拢、亮头已溜出（全部定型）
        assert_eq!(border_cell(w, h, w - 1, 0, 1.0), Some(false));
        assert_eq!(border_cell(w, h, w - 1, h - 1, 1.0), Some(false));
        assert_eq!(border_cell(w, h, w - 1, 1, 1.0), Some(false));
        assert_eq!(border_cell(w, h, w - 1, h - 2, 1.0), Some(false));
        // 内部永远不是边框
        assert_eq!(border_cell(w, h, 15, 4, 1.0), None);
    }

    /// 世界选择界面：世界名列出来（显示名优先 levelname.txt，不显示目录名），
    /// 没有 icon.png 的世界用 ◻ 占位
    #[tokio::test]
    async fn world_select_lists_worlds_and_marks_missing_thumbnails() {
        let mut app = App::new_from(crate::storage::tmp_config_dir("world-select")).unwrap();
        let inst_dir =
            std::env::temp_dir().join(format!("mctl-world-select-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&inst_dir);
        let saves = inst_dir.join("saves");

        // 有游戏自带缩略图的世界
        let w1 = saves.join("新的世界");
        std::fs::create_dir_all(&w1).unwrap();
        std::fs::write(w1.join("level.dat"), b"x").unwrap();
        image::RgbImage::from_pixel(8, 8, image::Rgb([120, 180, 240]))
            .save(w1.join("icon.png"))
            .unwrap();

        // 没有缩略图、且目录名和显示名不同的世界
        let w2 = saves.join("old_world_dir");
        std::fs::create_dir_all(&w2).unwrap();
        std::fs::write(w2.join("level.dat"), b"x").unwrap();
        std::fs::write(w2.join("levelname.txt"), "旧世界").unwrap();

        app.instances = vec![crate::launcher::Instance {
            id: "1.21.1".into(),
            version_type: "release".into(),
            main_class: "net.minecraft.client.main.Main".into(),
            assets_index: "5".into(),
            java_major: 21,
            dir: inst_dir.clone(),
            bedrock: false,
        }];
        app.selected_instance = 0;
        app.refresh_worlds();
        assert_eq!(app.worlds.len(), 2, "两个世界都应列出");

        app.home_view = HomeView::Instance;
        app.sync_wm_from_view();
        let out = flat(&render(&mut app, 120, 40));

        assert!(out.contains(&flat("新的世界")), "世界名要显示：{out}");
        assert!(out.contains(&flat("旧世界")), "levelname.txt 的显示名优先");
        assert!(!out.contains("old_world_dir"), "目录名不该当显示名露出来");
        assert!(out.contains(&flat("最后游玩")), "要有最后游玩时间");
        assert!(out.contains('◻'), "没有缩略图的世界用 ◻ 占位");

        // 选中框描边：进度 0 时没有框，描完后边框完整出现
        let corners = |s: &str| {
            s.matches('╭').count()
                + s.matches('╮').count()
                + s.matches('╯').count()
                + s.matches('╰').count()
        };
        app.world_anim_start = Some(std::time::Instant::now());
        let no_box = flat(&render(&mut app, 120, 40));
        app.world_anim_start =
            Some(std::time::Instant::now() - std::time::Duration::from_millis(2000));
        let with_box = flat(&render(&mut app, 120, 40));
        assert!(
            corners(&with_box) >= corners(&no_box) + 2,
            "描完后选中行应出现完整边框：{} → {}",
            corners(&no_box),
            corners(&with_box)
        );

        // 缩略图异步加载：渲染触发入队，消息回来后缓存里应是解码好的协议
        let (key, proto) = loop {
            match tokio::time::timeout(std::time::Duration::from_secs(5), app.rx.recv()).await {
                Ok(Some(crate::app::TaskMsg::WorldIconLoaded { key, proto })) => {
                    break (key, proto);
                }
                Ok(Some(_)) => continue, // 构造 App 时可能还有别的启动消息，跳过
                Ok(None) => panic!("消息通道关闭"),
                Err(_) => panic!("等缩略图加载消息超时"),
            }
        };
        assert!(
            key.ends_with("/新的世界"),
            "缓存键应带实例和世界目录名：{key}"
        );
        assert!(proto.is_some(), "8×8 PNG 应能解码成协议");

        let _ = std::fs::remove_dir_all(&inst_dir);
    }

    /// 设置页新增两项：选中框动画 + 主题选择器（Enter 开列表，预览/落定/还原）
    #[tokio::test]
    async fn settings_page_has_anim_toggle_and_theme_picker() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        // 本测试会换全局调色板：与断言配色的测试互斥（见 theme::test_palette_guard）
        let _palette = crate::theme::test_palette_guard();
        let mut app = App::new_from(crate::storage::tmp_config_dir("ui")).unwrap();
        app.home_view = HomeView::Settings;
        app.sync_wm_from_view();
        let out = flat(&render(&mut app, 100, 40));
        assert!(out.contains(&flat("选中框动画")), "设置页缺少动画开关行");
        assert!(out.contains(&flat("←→ 开关")), "开关行要有操作提示");
        // 主题行回车 → 打开选择器浮层（列表里有主题名和键位提示）
        app.set_focus = SettingField::ALL
            .iter()
            .position(|f| *f == SettingField::Theme)
            .unwrap();
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.theme_picker.is_some(), "回车应打开主题列表");
        let pick = flat(&render(&mut app, 100, 40));
        assert!(pick.contains(&flat("mctl 经典")), "列表里应能看到主题名");
        assert!(pick.contains(&flat("Enter 确定")), "浮层要给出键位提示");
        // 底边提示要完整：裁剪提示说明框宽算少了（ratatui 的口径才作数）
        assert!(
            pick.contains(&flat("● 已保存")),
            "底部提示被裁了：缺『● 已保存』"
        );
        // 「完整呈现所有主题」：一屏列全，40 个名字一个不漏（不靠滚动翻页）
        for id in crate::theme::theme_ids() {
            let label = flat(&crate::theme::theme_label(&id));
            assert!(pick.contains(&label), "浮层没一屏列全：缺 {label}");
        }
        // ↑↓ 只预览（不动已选项、不标脏），Enter 才落定
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.theme, crate::theme::LEGACY_ID, "预览阶段不能改已选主题");
        assert_eq!(
            crate::theme::cur(),
            crate::theme::palette_of(crate::theme::theme_ids()[1].as_str()),
            "预览要立刻变色"
        );
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(
            app.theme,
            crate::theme::theme_ids()[1],
            "Enter 落定到光标那条"
        );
        assert!(app.theme_picker.is_none());
        assert!(app.set_dirty, "落定后设置页应处于未保存态");
        // Esc 还原：重开 → 下移 → Esc → 回到打开时的主题，配色同步回来
        app.open_theme_picker();
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(
            app.theme,
            crate::theme::theme_ids()[1],
            "Esc 应还原到打开时的主题"
        );
        assert_eq!(
            crate::theme::cur(),
            crate::theme::palette_of(&app.theme),
            "界面配色要跟着还原"
        );
        // ←→ 跳一列（列优先网格）：100×40 → 2 列 20 行，←→ 的步长就是 20
        app.term_size = (100, 40);
        app.open_theme_picker();
        let before = app.theme_picker.as_ref().unwrap().cursor;
        app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(
            app.theme_picker.as_ref().unwrap().cursor,
            (before + 20) % crate::theme::theme_ids().len(),
            "→ 应跳到下一列同一行"
        );
        app.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        assert_eq!(
            app.theme_picker.as_ref().unwrap().cursor,
            before,
            "← 应跳回来"
        );
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        // 复原全局配色，别串到别的测试
        crate::theme::set_theme(crate::theme::LEGACY_ID);
    }
}

#[cfg(test)]
mod login_screen_tests {
    use crate::app::{App, LoginScreen};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    #[tokio::test]
    async fn login_full_renders_logo_and_options() {
        let mut app = App::new_from(crate::storage::tmp_config_dir("ui")).unwrap();
        app.login_screen = Some(LoginScreen {
            selected: 0,
            input_4399: false,
        });
        let backend = TestBackend::new(100, 34);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
        let buf = terminal.backend().buffer();
        let mut lines = Vec::new();
        for y in 0..buf.area.height {
            let mut line = String::new();
            for x in 0..buf.area.width {
                line.push(buf[(x, y)].symbol().chars().next().unwrap_or(' '));
            }
            lines.push(line);
        }
        for (i, l) in lines.iter().enumerate() {
            eprintln!("L{i}: |{l}|");
        }
        assert!(lines.iter().any(|l| l.contains("███╗")), "应有 MCTL 立体字");
        assert!(lines.iter().any(|l| l.contains("4399")), "应有 4399 选项");
    }
}
