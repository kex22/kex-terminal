use std::collections::HashMap;
use std::io;

use crossterm::event::{Event, EventStream};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::{ExecutableCommand, cursor};
use futures_lite::StreamExt;
use tokio::net::UnixStream;
use tokio::net::unix::OwnedWriteHalf;

use crate::error::Result;
use crate::ipc::client::IpcClient;
use crate::ipc::codec::{read_message, write_message};
use crate::ipc::message::{Request, Response, StreamMessage};
use crate::tui::input::{Action, InputHandler};
use crate::tui::layout::{PaneLayout, SplitDirection};
use crate::tui::renderer::Renderer;
use crate::tui::screen::Screen;
use crate::tui::vterm::VirtualTerminal;

pub async fn attach(mut stream: UnixStream, terminal_name: &str) -> Result<()> {
    let (cols, rows) = terminal::size().unwrap_or((80, 24));
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

    let mut stdout = io::stdout();
    let _ = stdout.execute(cursor::Show);
    let _ = terminal::disable_raw_mode();
    let _ = stdout.execute(LeaveAlternateScreen);

    result
}

async fn run_tui(stream: UnixStream, terminal_name: &str, cols: u16, rows: u16) -> Result<()> {
    let mut screen = Screen::new(rows, cols);
    let mut renderer = Renderer::new(io::stdout());
    let mut input = InputHandler::new();

    // Multi-pane state
    let mut layout = PaneLayout::new(terminal_name.to_string());
    let mut vterms: HashMap<String, VirtualTerminal> = HashMap::new();
    let mut writers: HashMap<String, OwnedWriteHalf> = HashMap::new();

    // Initial pane setup
    let pane_area = screen.pane_area();
    vterms.insert(
        terminal_name.to_string(),
        VirtualTerminal::new(pane_area.height, pane_area.width),
    );

    let (tx, mut rx) = tokio::sync::mpsc::channel::<(String, Vec<u8>)>(64);
    let mut read_tasks: HashMap<String, tokio::task::JoinHandle<()>> = HashMap::new();

    // Spawn reader for initial terminal
    let (sock_read, sock_write) = stream.into_split();
    writers.insert(terminal_name.to_string(), sock_write);
    let tx_clone = tx.clone();
    let initial_id = terminal_name.to_string();
    read_tasks.insert(
        terminal_name.to_string(),
        tokio::spawn(async move {
            let mut sock_read = sock_read;
            while let Ok(StreamMessage::Data(data)) = read_message(&mut sock_read).await {
                if tx_clone.send((initial_id.clone(), data)).await.is_err() {
                    break;
                }
            }
        }),
    );
    let split_tx = tx.clone();
    drop(tx);

    render_all_panes(
        &mut renderer,
        &layout,
        &mut vterms,
        &screen,
        &input,
        terminal_name,
    )?;

    let mut event_reader = EventStream::new();

    loop {
        tokio::select! {
            Some((tid, data)) = rx.recv() => {
                handle_pty_data(&tid, &data, &layout, &mut vterms, &mut renderer, &screen)?;
            }
            Some(Ok(event)) = event_reader.next() => {
                if let Event::Resize(new_cols, new_rows) = event {
                    handle_resize(
                        new_rows, new_cols, &mut screen, &layout,
                        &mut vterms, &mut writers, &mut renderer, &input, terminal_name,
                    ).await?;
                    continue;
                }
                match input.handle_event(&event) {
                    Action::SendToTerminal(bytes) => {
                        let focused = layout.focused_terminal().to_string();
                        if let Some(w) = writers.get_mut(&focused)
                            && write_message(w, &StreamMessage::Data(bytes)).await.is_err()
                        {
                            break;
                        }
                    }
                    Action::ModeChanged(_) => {
                        render_status(&mut renderer, &screen, &input, terminal_name)?;
                    }
                    Action::PaneSplitHorizontal => {
                        handle_split(
                            SplitDirection::Horizontal, &mut layout, &mut vterms,
                            &mut writers, &mut read_tasks, &split_tx, &screen,
                            &mut renderer, &input, terminal_name,
                        ).await?;
                    }
                    Action::PaneSplitVertical => {
                        handle_split(
                            SplitDirection::Vertical, &mut layout, &mut vterms,
                            &mut writers, &mut read_tasks, &split_tx, &screen,
                            &mut renderer, &input, terminal_name,
                        ).await?;
                    }
                    Action::PaneNavigate(dir) => {
                        layout.navigate(dir, screen.pane_area());
                        render_all_panes(&mut renderer, &layout, &mut vterms, &screen, &input, terminal_name)?;
                    }
                    Action::PaneResize(dir) => {
                        layout.resize_focused(dir, 0.05);
                        resize_all_vterms(&layout, &mut vterms, &mut writers, &screen).await?;
                        render_all_panes(&mut renderer, &layout, &mut vterms, &screen, &input, terminal_name)?;
                    }
                    Action::PaneClose => {
                        if let Some(closed) = layout.close_focused() {
                            vterms.remove(&closed);
                            if let Some(task) = read_tasks.remove(&closed) {
                                task.abort();
                            }
                            if let Some(mut w) = writers.remove(&closed) {
                                let _ = write_message(&mut w, &StreamMessage::Detach).await;
                            }
                            resize_all_vterms(&layout, &mut vterms, &mut writers, &screen).await?;
                            render_all_panes(&mut renderer, &layout, &mut vterms, &screen, &input, terminal_name)?;
                        }
                    }
                    Action::Detach => break,
                    _ => {}
                }
            }
            else => break,
        }
    }

    // Detach all terminals
    for (_, mut w) in writers.drain() {
        let _ = write_message(&mut w, &StreamMessage::Detach).await;
    }

    Ok(())
}

fn handle_pty_data(
    tid: &str,
    data: &[u8],
    layout: &PaneLayout,
    vterms: &mut HashMap<String, VirtualTerminal>,
    renderer: &mut Renderer<io::Stdout>,
    screen: &Screen,
) -> Result<()> {
    let Some(vterm) = vterms.get_mut(tid) else {
        return Ok(());
    };
    vterm.process(data);
    let rects = layout.compute_rects(screen.pane_area());
    let Some((_, rect)) = rects.iter().find(|(id, _)| id == tid) else {
        return Ok(());
    };
    let dirty = vterm.take_dirty_rows();
    if !dirty.is_empty() {
        renderer.render_vterm_rows(vterm, rect, &dirty)?;
    }
    // Show cursor in focused pane
    if tid == layout.focused_terminal() {
        let (cr, cc) = vterm.cursor_position();
        io::stdout().execute(cursor::MoveTo(rect.x + cc, rect.y + cr))?;
        io::stdout().execute(cursor::Show)?;
    }
    renderer.flush()?;
    Ok(())
}

fn render_all_panes(
    renderer: &mut Renderer<io::Stdout>,
    layout: &PaneLayout,
    vterms: &mut HashMap<String, VirtualTerminal>,
    screen: &Screen,
    input: &InputHandler,
    terminal_name: &str,
) -> Result<()> {
    let pane_area = screen.pane_area();
    let rects = layout.compute_rects(pane_area);
    for (tid, rect) in &rects {
        if let Some(vterm) = vterms.get_mut(tid) {
            renderer.render_vterm(vterm, rect)?;
            vterm.take_dirty_rows();
        }
    }
    for (is_vert, x, y, len) in layout.compute_separators(pane_area) {
        if is_vert {
            renderer.render_vsep(x, y, len)?;
        } else {
            renderer.render_hsep(x, y, len)?;
        }
    }
    render_status(renderer, screen, input, terminal_name)?;
    let focused = layout.focused_terminal();
    if let Some(vterm) = vterms.get(focused)
        && let Some((_, rect)) = rects.iter().find(|(id, _)| id == focused)
    {
        let (cr, cc) = vterm.cursor_position();
        io::stdout().execute(cursor::MoveTo(rect.x + cc, rect.y + cr))?;
        io::stdout().execute(cursor::Show)?;
    }
    renderer.flush()?;
    Ok(())
}

fn render_status(
    renderer: &mut Renderer<io::Stdout>,
    screen: &Screen,
    input: &InputHandler,
    terminal_name: &str,
) -> Result<()> {
    let bar = screen.status_bar_area();
    renderer.render_status_bar(&input.mode().status_text(terminal_name), &bar)?;
    renderer.flush()?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn handle_split(
    direction: SplitDirection,
    layout: &mut PaneLayout,
    vterms: &mut HashMap<String, VirtualTerminal>,
    writers: &mut HashMap<String, OwnedWriteHalf>,
    read_tasks: &mut HashMap<String, tokio::task::JoinHandle<()>>,
    tx: &tokio::sync::mpsc::Sender<(String, Vec<u8>)>,
    screen: &Screen,
    renderer: &mut Renderer<io::Stdout>,
    input: &InputHandler,
    terminal_name: &str,
) -> Result<()> {
    // Check minimum size before splitting
    let focused_rect = layout
        .compute_rects(screen.pane_area())
        .into_iter()
        .find(|(id, _)| id == layout.focused_terminal())
        .map(|(_, r)| r);
    if let Some(r) = focused_rect {
        let too_small = match direction {
            SplitDirection::Vertical => r.width < 4,
            SplitDirection::Horizontal => r.height < 4,
        };
        if too_small {
            return Ok(());
        }
    }

    let mut client = IpcClient::connect().await?;
    let new_id = match client.send(Request::TerminalCreate { name: None }).await? {
        Response::TerminalCreated { id } => id,
        _ => return Ok(()),
    };

    let mut client = IpcClient::connect().await?;
    if !matches!(
        client
            .send(Request::TerminalAttach { id: new_id.clone() })
            .await?,
        Response::Ok
    ) {
        // Clean up orphaned terminal
        if let Ok(mut c) = IpcClient::connect().await {
            let _ = c.send(Request::TerminalKill { id: new_id }).await;
        }
        return Ok(());
    }
    let stream = client.into_stream();

    layout.split(direction, new_id.clone());

    let rects = layout.compute_rects(screen.pane_area());
    let (h, w) = rects
        .iter()
        .find(|(id, _)| id == &new_id)
        .map(|(_, r)| (r.height, r.width))
        .unwrap_or((24, 80));
    vterms.insert(new_id.clone(), VirtualTerminal::new(h, w));

    let (sock_read, mut sock_write) = stream.into_split();
    write_message(&mut sock_write, &StreamMessage::Resize { cols: w, rows: h }).await?;
    writers.insert(new_id.clone(), sock_write);

    let tx_clone = tx.clone();
    let tid = new_id.clone();
    read_tasks.insert(
        new_id.clone(),
        tokio::spawn(async move {
            let mut sock_read = sock_read;
            while let Ok(StreamMessage::Data(data)) = read_message(&mut sock_read).await {
                if tx_clone.send((tid.clone(), data)).await.is_err() {
                    break;
                }
            }
        }),
    );

    resize_all_vterms(layout, vterms, writers, screen).await?;
    render_all_panes(renderer, layout, vterms, screen, input, terminal_name)?;
    Ok(())
}

async fn resize_all_vterms(
    layout: &PaneLayout,
    vterms: &mut HashMap<String, VirtualTerminal>,
    writers: &mut HashMap<String, OwnedWriteHalf>,
    screen: &Screen,
) -> Result<()> {
    for (tid, rect) in layout.compute_rects(screen.pane_area()) {
        if let Some(vterm) = vterms.get_mut(&tid) {
            vterm.resize(rect.height, rect.width);
        }
        if let Some(w) = writers.get_mut(&tid) {
            let _ = write_message(
                w,
                &StreamMessage::Resize {
                    cols: rect.width,
                    rows: rect.height,
                },
            )
            .await;
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn handle_resize(
    new_rows: u16,
    new_cols: u16,
    screen: &mut Screen,
    layout: &PaneLayout,
    vterms: &mut HashMap<String, VirtualTerminal>,
    writers: &mut HashMap<String, OwnedWriteHalf>,
    renderer: &mut Renderer<io::Stdout>,
    input: &InputHandler,
    terminal_name: &str,
) -> Result<()> {
    screen.resize(new_rows, new_cols);
    resize_all_vterms(layout, vterms, writers, screen).await?;
    render_all_panes(renderer, layout, vterms, screen, input, terminal_name)?;
    Ok(())
}
