use crate::app::{App, HomeView};
use crate::theme;
use ratatui::layout::{Alignment, Constraint, Flex, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

pub fn draw(frame: &mut Frame, app: &App, area: Rect) {
    let rows = Layout::vertical([Constraint::Length(1)]).split(area);
    let bar = rows[0];
    let cols = Layout::horizontal([
        Constraint::Length(24),
        Constraint::Min(0),
        Constraint::Length(22),
    ])
    .flex(Flex::SpaceBetween)
    .split(bar);

    let title = Line::from(vec![
        Span::styled(" mctl ", theme::accent().bold()),
        Span::styled(env!("CARGO_PKG_VERSION"), theme::dim()),
    ]);
    frame.render_widget(Paragraph::new(title), cols[0]);

    // 右上角只显示用户名（登录态去账户面板看；未登录不显示任何东西）
    if !app.user_name.is_empty() && app.user_name != "未登录" {
        let user = Line::from(vec![
            Span::styled(&app.user_name, theme::dim()),
            Span::raw(" "),
        ]);
        frame.render_widget(Paragraph::new(user).alignment(Alignment::Right), cols[2]);
    }
}

pub fn status_bar(frame: &mut Frame, app: &App, area: Rect) {
    let cols = Layout::horizontal([Constraint::Min(0), Constraint::Min(60)]).split(area);
    if app.shell_input {
        // 命令模式：显示 : 提示与输入缓冲
        let prompt = Line::from(vec![
            Span::styled(" : ", theme::accent().bold()),
            Span::styled(&app.shell_buf, theme::base()),
            Span::styled("▏", theme::accent()),
        ]);
        frame.render_widget(Paragraph::new(prompt), cols[0]);
        let hint = Line::from(vec![Span::styled("[Esc] 取消  [Enter] 执行", theme::dim())]);
        frame.render_widget(Paragraph::new(hint).alignment(Alignment::Right), cols[1]);
        return;
    }
    let left = if app.status.is_empty() {
        Line::from(vec![
            Span::styled(" ● ", theme::ok()),
            Span::styled("就绪", theme::dim()),
        ])
    } else {
        Line::from(vec![Span::styled(&app.status, theme::warn())])
    };
    frame.render_widget(Paragraph::new(left), cols[0]);

    // 底栏右侧：根据 WM 模式显示动态提示（来自配置的键位）
    let help = if app.wm.mode == tui_wm_core::WmMode::Adjust {
        // 调整模式提示
        let k = &app.wm.keys;
        Line::from(vec![
            Span::styled(format!("[{}]宽  ", k.width_decrease.label()), theme::dim()),
            Span::styled(format!("[{}]高  ", k.flex_increase.label()), theme::dim()),
            Span::styled(format!("[{}]切换  ", k.cycle.label()), theme::dim()),
            Span::styled(format!("[{}]停靠  ", k.dock_toggle.label()), theme::dim()),
            Span::styled(
                format!(
                    "[{}]退出",
                    k.exit.first().map(|x| x.label()).unwrap_or("Esc".into())
                ),
                theme::dim(),
            ),
        ])
    } else {
        // 正常模式提示（动态：随当前视图补专属键位）
        let k = &app.wm.keys;
        let view_hint = match app.home_view {
            HomeView::Instance => "  [o]存档",
            HomeView::Server => "",
            HomeView::Resource => "  [/]搜索  [j/k]分类",
            HomeView::Download => "  [d]下载中心",
            HomeView::Plugins => "  [:]plugin  命令",
            HomeView::Settings => "  [:]设置命令",
        };
        Line::from(vec![
            Span::styled(format!("[{}]调整  ", k.adjust_mode.label()), theme::dim()),
            Span::styled("[Alt+数字]切视图  ", theme::dim()),
            Span::styled("[w]实例  ", theme::dim()),
            Span::styled("[m]资源  ", theme::dim()),
            Span::styled("[Enter]执行  ", theme::dim()),
            Span::styled("[F]全屏面板  ", theme::dim()),
            Span::styled("[a]登录  ", theme::dim()),
            Span::styled("[L]日志  ", theme::dim()),
            Span::styled("[r]刷新  ", theme::dim()),
            Span::styled(view_hint, theme::dim()),
            Span::styled("[:]命令  ", theme::dim()),
            Span::styled("[Alt+1-6]直达  ", theme::dim()),
            Span::styled("[q]退出", theme::dim()),
        ])
    };
    frame.render_widget(Paragraph::new(help).alignment(Alignment::Right), cols[1]);
}
