//! 租赁服密码输入浮层。
//! 只画一个居中小框：服务器名 + 掩码输入行，按键由 `App::handle_pwd_prompt_key` 处理。
//! 密码只在内存里活到这次启动请求用完，界面上永远只显示圆点。

use crate::app::App;
use crate::theme;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use ratatui::Frame;

pub fn draw(frame: &mut Frame, app: &App, area: Rect) {
    let Some(p) = &app.pwd_prompt else {
        return;
    };
    let box_w = 56u16.min(area.width);
    let box_h = 6u16.min(area.height);
    if box_w < 20 || box_h < 4 {
        return;
    }
    let rect = Rect {
        x: area.x + (area.width - box_w) / 2,
        y: area.y + (area.height - box_h) / 2,
        width: box_w,
        height: box_h,
    };
    // 浮层下面是服务器网格，先清底再画框
    frame.render_widget(Clear, rect);
    let block = Block::default()
        .title(Span::styled(" 租赁服密码 ", theme::accent().bold()))
        .title_bottom(Span::styled(" Enter 连接 · Esc 取消 ", theme::dim()))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme::border_sel()));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    // 圆点数量 = 输入字符数（CJK/emoji 也算一个，不按显示宽度算）
    let masked = "●".repeat(p.input.chars().count());
    let lines = vec![
        Line::from(vec![
            Span::styled("服务器 ", theme::dim()),
            Span::styled(p.title.clone(), theme::base()),
        ]),
        Line::from(vec![
            Span::styled("密  码 ", theme::dim()),
            Span::styled(format!("{masked}▊"), theme::accent().bold()),
        ]),
    ];
    frame.render_widget(Paragraph::new(lines), inner);
}
