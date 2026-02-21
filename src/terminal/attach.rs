use std::io;

use crossterm::event::{Event, EventStream};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::{ExecutableCommand, cursor};
use futures_lite::StreamExt;
use tokio::net::UnixStream;

use crate::error::Result;
use crate::ipc::codec::{read_message, write_message};
use crate::ipc::message::StreamMessage;
use crate::tui::input::{Action, InputHandler};
use crate::tui::renderer::Renderer;
use crate::tui::screen::Screen;
use crate::tui::vterm::VirtualTerminal;

pub async fn attach(mut stream: UnixStream, terminal_name: &str) -> Result<()> {
    let (cols, rows) = terminal::size().unwrap_or((80, 24));
    // Send pane-area size (excluding status bar) to PTY
    let screen = Screen::new(rows, cols);
    let pane = screen.pane_area();
    write_message(
        &mut stream,
        &StreamMessage::Resize {
            cols: pane.width,
            rows: pane.height,
        },
    )
    .await?;

    let mut stdout = io::stdout();
    stdout.execute(EnterAlternateScreen)?;
    terminal::enable_raw_mode()?;
    stdout.execute(cursor::Hide)?;

    let result = run_tui(stream, terminal_name, cols, rows).await;

    // Cleanup regardless of result
    let mut stdout = io::stdout();
    let _ = stdout.execute(cursor::Show);
    let _ = terminal::disable_raw_mode();
    let _ = stdout.execute(LeaveAlternateScreen);

    result
}

async fn run_tui(stream: UnixStream, terminal_name: &str, cols: u16, rows: u16) -> Result<()> {
    let mut screen = Screen::new(rows, cols);
    let pane = screen.pane_area();
    let mut vterm = VirtualTerminal::new(pane.height, pane.width);
    let mut renderer = Renderer::new(io::stdout());
    let mut input = InputHandler::new();

    render_all(&mut renderer, &mut vterm, &screen, &input, terminal_name)?;

    let (mut sock_read, mut sock_write) = stream.into_split();
    let mut event_reader = EventStream::new();

    let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<u8>>(64);
    let mut read_task = tokio::spawn(async move {
        loop {
            let msg: std::result::Result<StreamMessage, _> = read_message(&mut sock_read).await;
            match msg {
                Ok(StreamMessage::Data(data)) => {
                    if tx.send(data).await.is_err() {
                        break;
                    }
                }
                _ => break,
            }
        }
    });

    loop {
        tokio::select! {
            Some(data) = rx.recv() => {
                vterm.process(&data);
                let pane_area = screen.pane_area();
                let dirty = vterm.take_dirty_rows();
                if !dirty.is_empty() {
                    renderer.render_vterm_rows(&vterm, &pane_area, &dirty)?;
                }
                let (cr, cc) = vterm.cursor_position();
                io::stdout().execute(cursor::MoveTo(pane_area.x + cc, pane_area.y + cr))?;
                io::stdout().execute(cursor::Show)?;
                renderer.flush()?;
            }
            Some(Ok(event)) = event_reader.next() => {
                if let Event::Resize(new_cols, new_rows) = event {
                    screen.resize(new_rows, new_cols);
                    let pane = screen.pane_area();
                    vterm.resize(pane.height, pane.width);
                    write_message(&mut sock_write, &StreamMessage::Resize { cols: pane.width, rows: pane.height }).await?;
                    render_all(&mut renderer, &mut vterm, &screen, &input, terminal_name)?;
                    continue;
                }
                match input.handle_event(&event) {
                    Action::SendToTerminal(bytes) => {
                        let msg = StreamMessage::Data(bytes);
                        if write_message(&mut sock_write, &msg).await.is_err() { break; }
                    }
                    Action::ModeChanged(_) => {
                        let bar = screen.status_bar_area();
                        let text = input.mode().status_text(terminal_name);
                        renderer.render_status_bar(&text, &bar)?;
                        renderer.flush()?;
                    }
                    Action::Detach => break,
                    _ => {} // Other actions handled by future modules
                }
            }
            _ = &mut read_task => { break; }
        }
    }

    Ok(())
}

fn render_all(
    renderer: &mut Renderer<io::Stdout>,
    vterm: &mut VirtualTerminal,
    screen: &Screen,
    input: &InputHandler,
    terminal_name: &str,
) -> Result<()> {
    let pane_area = screen.pane_area();
    let bar_area = screen.status_bar_area();
    renderer.render_vterm(vterm, &pane_area)?;
    renderer.render_status_bar(&input.mode().status_text(terminal_name), &bar_area)?;
    renderer.flush()?;
    vterm.take_dirty_rows();
    Ok(())
}
