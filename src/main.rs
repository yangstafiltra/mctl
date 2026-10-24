mod api;
mod app;
mod cli;
mod crypto;
mod deps;
mod domain;
mod instance;
mod launcher;
mod models;
mod plugin;
mod storage;
mod text;
mod theme;
mod token;
mod ui;

use anyhow::Result;
use app::App;
use crossterm::event::{Event, KeyEventKind};
use crossterm::ExecutableCommand;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::io;
use std::sync::Mutex;
use std::time::Duration;

/// panic 消息暂存：panic hook 早于栈展开运行，那时终端还在 alternate screen 里，
/// 直接打印会被吞掉。存下来等 TerminalGuard 恢复完终端再打印。
static PANIC_MSG: Mutex<Option<String>> = Mutex::new(None);

/// 终端守卫：Drop 时恢复 raw mode / alternate screen / 鼠标捕获。
/// 为什么要它：原先恢复终端是线性写在 `main` 里的，只要 `run` 任何一处 panic，
/// 恢复代码就被整个跳过 —— 终端永久卡在 raw mode，没有回滚历史、光标乱飞，
/// 只能开新终端 `reset`。Drop 同时覆盖正常返回和栈展开两种路径。
struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        // 顺序和进入时相反；每步都忽略错误（panic 路径上终端可能已经是坏的）
        let _ = crossterm::execute!(
            io::stdout(),
            crossterm::terminal::LeaveAlternateScreen,
            crossterm::event::DisableMouseCapture
        );
        let _ = crossterm::terminal::disable_raw_mode();
        let _ = io::stdout().execute(crossterm::cursor::Show);

        // 终端已恢复，现在把 panic 原因打印到真正的 stderr
        if let Ok(mut slot) = PANIC_MSG.lock() {
            if let Some(msg) = slot.take() {
                eprintln!("\n mctl 异常退出：{msg}");
                eprintln!(
                    " 终端已恢复。上面的消息是崩溃原因，可加 RUST_BACKTRACE=1 重跑看调用栈。\n"
                );
            }
        }
    }
}

/// 装 panic hook：把 panic 消息存进 PANIC_MSG，由 TerminalGuard 在恢复终端后打印。
/// 同时保留默认 hook 的信息量（stderr 上还看不见，但 backtrace 已由默认 hook 格式化）。
fn install_panic_hook() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let msg = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| (*s).to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "未知 panic".to_string());
        if let Ok(mut slot) = PANIC_MSG.lock() {
            *slot = Some(msg);
        }
        prev(info);
    }));
}

#[tokio::main]
async fn main() -> Result<()> {
    // 恢复 Unix 惯例：下游关闭管道（如 `mctl tree | head`）时安静退出。
    // Rust 默认忽略 SIGPIPE，println! 会因 EPIPE 直接 panic。
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }

    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 {
        return cli::run(&args).await;
    }
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    crossterm::terminal::enable_raw_mode()?;
    crossterm::execute!(
        io::stdout(),
        crossterm::terminal::EnterAlternateScreen,
        crossterm::event::EnableMouseCapture,
        crossterm::cursor::Hide
    )?;
    // 从这里开始到函数结束，终端由 guard 负责恢复（含 panic 路径）
    let _guard = TerminalGuard;
    install_panic_hook();
    run(&mut terminal).await
}

async fn run(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
    let mut app = App::new()?;
    loop {
        terminal.draw(|f| ui::draw(f, &mut app))?;

        // 处理异步消息
        while let Ok(msg) = app.rx.try_recv() {
            app.handle_msg(msg);
        }

        // 处理封面下载队列
        if let Some(idx) = app.cover_queue.first().copied() {
            app.cover_queue.remove(0);
            app.spawn_cover(idx);
        }

        if app.quit {
            app.save_session();
            break;
        }

        // 动画期间（世界描边 / 视图刷色 / 卡片网格刷色 / 资源来源条刷色）提到 ~60fps，
        // 播完回到省电节奏；下载页有任务时用 ~30fps 让长进度条的涌动波动起来。
        let poll_ms = if app.world_anim_playing()
            || app.view_anim_playing()
            || app.sel_anim_playing()
            || app.src_anim_playing()
        {
            16
        } else if app.download_anim_playing() {
            33
        } else {
            100
        };
        if crossterm::event::poll(Duration::from_millis(poll_ms))? {
            match crossterm::event::read()? {
                Event::Key(key) => {
                    if key.kind == KeyEventKind::Press {
                        app.handle_key(key);
                    }
                }
                Event::Mouse(ev) => {
                    app.handle_mouse(ev);
                }
                Event::Resize(w, h) => {
                    app.term_size = (w, h);
                }
                _ => {}
            }
        }
    }
    Ok(())
}
