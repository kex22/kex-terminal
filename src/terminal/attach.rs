use std::io;

use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyModifiers};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::{cursor, ExecutableCommand};
use futures_lite::StreamExt;
use tokio::net::UnixStream;

use crate::error::Result;
use crate::ipc::codec::{read_message, write_message};
use crate::ipc::message::StreamMessage;
use crate::tui::renderer::Renderer;
use crate::tui::screen::Screen;
use crate::tui::vterm::VirtualTerminal;

pub async fn attach(mut stream: UnixStream, terminal_name: &str) -> Result<()> {
    let (cols, rows) = terminal::size().unwrap_or((80, 24));
    write_message(&mut stream, &StreamMessage::Resize { cols, rows }).await?;

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

async fn run_tui(
    stream: UnixStream,
    terminal_name: &str,
    cols: u16,
    rows: u16,
) -> Result<()> {
    let mut screen = Screen::new(rows, cols);
    let pane = screen.pane_area();
    let mut vterm = VirtualTerminal::new(pane.height, pane.width);
    let mut renderer = Renderer::new(io::stdout());

    // Initial render: clear + status bar
    render_all(&mut renderer, &mut vterm, &screen, terminal_name)?;

    let (mut sock_read, mut sock_write) = stream.into_split();
    let mut event_reader = EventStream::new();

    // PTY output from server
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
            // Server → diff render
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
            // Input events (keyboard + resize)
            Some(Ok(event)) = event_reader.next() => {
                match event {
                    Event::Key(KeyEvent { code: KeyCode::Char('c'), modifiers: KeyModifiers::CONTROL, .. }) => {
                        // Forward Ctrl-C to terminal
                        let msg = StreamMessage::Data(vec![3]);
                        if write_message(&mut sock_write, &msg).await.is_err() { break; }
                    }
                    Event::Key(key_event) => {
                        if let Some(bytes) = key_event_to_bytes(&key_event) {
                            let msg = StreamMessage::Data(bytes);
                            if write_message(&mut sock_write, &msg).await.is_err() { break; }
                        }
                    }
                    Event::Resize(new_cols, new_rows) => {
                        screen.resize(new_rows, new_cols);
                        let pane = screen.pane_area();
                        vterm.resize(pane.height, pane.width);
                        write_message(&mut sock_write, &StreamMessage::Resize { cols: pane.width, rows: pane.height }).await?;
                        render_all(&mut renderer, &mut vterm, &screen, terminal_name)?;
                    }
                    _ => {}
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
    terminal_name: &str,
) -> Result<()> {
    let pane_area = screen.pane_area();
    let bar_area = screen.status_bar_area();
    renderer.render_vterm(vterm, &pane_area)?;
    let status = format!(" {terminal_name}");
    renderer.render_status_bar(&status, &bar_area)?;
    renderer.flush()?;
    // Reset dirty tracking after full render
    vterm.take_dirty_rows();
    Ok(())
}

fn key_event_to_bytes(event: &KeyEvent) -> Option<Vec<u8>> {
    let ctrl = event.modifiers.contains(KeyModifiers::CONTROL);
    match event.code {
        KeyCode::Char(c) if ctrl => {
            let byte = (c as u8).wrapping_sub(b'a').wrapping_add(1);
            Some(vec![byte])
        }
        KeyCode::Char(c) => {
            let mut buf = [0u8; 4];
            let s = c.encode_utf8(&mut buf);
            Some(s.as_bytes().to_vec())
        }
        KeyCode::Enter => Some(vec![b'\r']),
        KeyCode::Backspace => Some(vec![127]),
        KeyCode::Tab => Some(vec![b'\t']),
        KeyCode::Esc => Some(vec![0x1b]),
        KeyCode::Up => Some(b"\x1b[A".to_vec()),
        KeyCode::Down => Some(b"\x1b[B".to_vec()),
        KeyCode::Right => Some(b"\x1b[C".to_vec()),
        KeyCode::Left => Some(b"\x1b[D".to_vec()),
        KeyCode::Home => Some(b"\x1b[H".to_vec()),
        KeyCode::End => Some(b"\x1b[F".to_vec()),
        KeyCode::Delete => Some(b"\x1b[3~".to_vec()),
        _ => None,
    }
}
