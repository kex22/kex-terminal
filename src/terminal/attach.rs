use std::collections::HashMap;
use std::io;

use crossterm::event::{Event, EventStream};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::{ExecutableCommand, cursor};
use futures_lite::StreamExt;
use tokio::net::UnixStream;
use tokio::net::unix::OwnedWriteHalf;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::config::Config;
use crate::error::Result;
use crate::ipc::client::IpcClient;
use crate::ipc::codec::{read_binary_frame, write_binary_frame};
use crate::ipc::message::{BinaryFrame, Request, Response, ViewInfo};
use crate::tui::input::{Action, InputHandler};
use crate::tui::layout::{PaneLayout, SplitDirection};
use crate::tui::renderer::Renderer;
use crate::tui::screen::Screen;
use crate::tui::vterm::VirtualTerminal;

pub async fn attach(stream: UnixStream, terminal_name: &str) -> Result<()> {
    attach_view(stream, terminal_name, &[], None, None, None).await
}

pub async fn attach_view(
    mut stream: UnixStream,
    terminal_name: &str,
    extra_terminals: &[String],
    view_id: Option<&str>,
    saved_layout: Option<serde_json::Value>,
    saved_focused: Option<String>,
) -> Result<()> {
    let config = Config::load().unwrap_or_default();
    let (cols, rows) = terminal::size().unwrap_or((80, 24));
    let screen = Screen::new(rows, cols);
    let pane = screen.pane_area();
    write_binary_frame(
        &mut stream,
        terminal_name,
        &BinaryFrame::Resize {
            cols: pane.width,
            rows: pane.height,
        },
    )
    .await?;

    let mut stdout = io::stdout();
    stdout.execute(EnterAlternateScreen)?;
    terminal::enable_raw_mode()?;
    stdout.execute(cursor::Hide)?;

    let result = run_tui(TuiInit {
        stream, terminal_name, cols, rows, config,
        extra_terminals, view_id, saved_layout, saved_focused,
    }).await;

    let mut stdout = io::stdout();
    let _ = stdout.execute(cursor::Show);
    let _ = terminal::disable_raw_mode();
    let _ = stdout.execute(LeaveAlternateScreen);

    result
}

struct TuiSession {
    layout: PaneLayout,
    vterms: HashMap<String, VirtualTerminal>,
    writers: HashMap<String, OwnedWriteHalf>,
    read_tasks: HashMap<String, JoinHandle<()>>,
    screen: Screen,
    renderer: Renderer<io::Stdout>,
    input: InputHandler,
    config: Config,
    tx: mpsc::Sender<(String, Vec<u8>)>,
    view_id: Option<String>,
    label: String,
}

impl TuiSession {
    fn render_all(&mut self) -> Result<()> {
        let pane_area = self.screen.pane_area();
        let rects = self.layout.compute_rects(pane_area);
        for (tid, rect) in &rects {
            if let Some(vterm) = self.vterms.get_mut(tid) {
                self.renderer.render_vterm(vterm, rect)?;
                vterm.take_dirty_rows();
            }
        }
        for (is_vert, x, y, len) in self.layout.compute_separators(pane_area) {
            if is_vert {
                self.renderer.render_vsep(x, y, len)?;
            } else {
                self.renderer.render_hsep(x, y, len)?;
            }
        }
        self.render_status()?;
        let focused = self.layout.focused_terminal();
        if let Some(vterm) = self.vterms.get(focused)
            && let Some((_, rect)) = rects.iter().find(|(id, _)| id == focused)
        {
            let (cr, cc) = vterm.cursor_position();
            io::stdout().execute(cursor::MoveTo(rect.x + cc, rect.y + cr))?;
            io::stdout().execute(cursor::Show)?;
        }
        self.renderer.flush()?;
        Ok(())
    }

    fn render_status(&mut self) -> Result<()> {
        if !self.config.status_bar {
            return Ok(());
        }
        let bar = self.screen.status_bar_area();
        self.renderer.render_status_bar(
            &self.input.mode().status_text(&self.label, &self.config.prefix),
            &bar,
        )?;
        self.renderer.flush()?;
        Ok(())
    }

    fn handle_pty_data(&mut self, tid: &str, data: &[u8]) -> Result<()> {
        let Some(vterm) = self.vterms.get_mut(tid) else {
            return Ok(());
        };
        vterm.process(data);
        let rects = self.layout.compute_rects(self.screen.pane_area());
        let Some((_, rect)) = rects.iter().find(|(id, _)| id == tid) else {
            return Ok(());
        };
        let dirty = vterm.take_dirty_rows();
        if !dirty.is_empty() {
            self.renderer.render_vterm_rows(vterm, rect, &dirty)?;
        }
        if tid == self.layout.focused_terminal() {
            let (cr, cc) = vterm.cursor_position();
            io::stdout().execute(cursor::MoveTo(rect.x + cc, rect.y + cr))?;
            io::stdout().execute(cursor::Show)?;
        }
        self.renderer.flush()?;
        Ok(())
    }

    async fn resize_all_vterms(&mut self) -> Result<()> {
        for (tid, rect) in self.layout.compute_rects(self.screen.pane_area()) {
            if let Some(vterm) = self.vterms.get_mut(&tid) {
                vterm.resize(rect.height, rect.width);
            }
            if let Some(w) = self.writers.get_mut(&tid) {
                let _ = write_binary_frame(
                    w,
                    &tid,
                    &BinaryFrame::Resize {
                        cols: rect.width,
                        rows: rect.height,
                    },
                )
                .await;
            }
        }
        Ok(())
    }

    async fn handle_resize(&mut self, new_rows: u16, new_cols: u16) -> Result<()> {
        self.screen.resize(new_rows, new_cols);
        self.resize_all_vterms().await?;
        self.render_all()
    }

    async fn handle_split(&mut self, direction: SplitDirection) -> Result<()> {
        let focused_rect = self.layout
            .compute_rects(self.screen.pane_area())
            .into_iter()
            .find(|(id, _)| id == self.layout.focused_terminal())
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
            client.send(Request::TerminalAttach { id: new_id.clone() }).await?,
            Response::Ok
        ) {
            if let Ok(mut c) = IpcClient::connect().await {
                let _ = c.send(Request::TerminalKill { id: new_id }).await;
            }
            return Ok(());
        }

        self.spawn_terminal(new_id.clone(), client.into_stream()).await?;
        self.layout.split(direction, new_id.clone());
        self.resize_all_vterms().await?;
        self.render_all()?;

        if let Some(vid) = &self.view_id {
            let vid = vid.clone();
            if let Ok(mut c) = IpcClient::connect().await {
                let _ = c.send(Request::ViewAddTerminal {
                    view_id: vid.clone(),
                    terminal_id: new_id,
                }).await;
            }
            self.sync_layout().await;
        }
        Ok(())
    }

    async fn handle_close(&mut self) -> Result<()> {
        if let Some(closed) = self.layout.close_focused() {
            self.vterms.remove(&closed);
            if let Some(task) = self.read_tasks.remove(&closed) {
                task.abort();
            }
            if let Some(mut w) = self.writers.remove(&closed) {
                let _ = write_binary_frame(&mut w, &closed, &BinaryFrame::Detach).await;
            }
            self.resize_all_vterms().await?;
            self.render_all()?;

            if let Some(vid) = &self.view_id {
                let vid = vid.clone();
                if let Ok(mut c) = IpcClient::connect().await {
                    let _ = c.send(Request::ViewRemoveTerminal {
                        view_id: vid.clone(),
                        terminal_id: closed,
                    }).await;
                }
                self.sync_layout().await;
            }
        }
        Ok(())
    }

    async fn handle_pane_resize(&mut self, dir: crate::tui::input::Direction) -> Result<()> {
        self.layout.resize_focused(dir, 0.05);
        self.resize_all_vterms().await?;
        self.render_all()?;
        if self.view_id.is_some() {
            self.sync_layout().await;
        }
        Ok(())
    }

    async fn switch_view(&mut self, terminal_ids: &[String]) -> Result<()> {
        if terminal_ids.is_empty() {
            return Ok(());
        }

        let new_set: std::collections::HashSet<&String> = terminal_ids.iter().collect();
        let old_ids: Vec<String> = self.writers.keys().cloned().collect();

        for id in &old_ids {
            if !new_set.contains(id) {
                if let Some(task) = self.read_tasks.remove(id) {
                    task.abort();
                }
                if let Some(mut w) = self.writers.remove(id) {
                    let _ = write_binary_frame(&mut w, id, &BinaryFrame::Detach).await;
                }
                self.vterms.remove(id);
            }
        }

        let first = &terminal_ids[0];
        self.layout = PaneLayout::new(first.clone());

        let pane_area = self.screen.pane_area();
        for tid in terminal_ids {
            if !self.writers.contains_key(tid) {
                let mut client = IpcClient::connect().await?;
                if !matches!(
                    client.send(Request::TerminalAttach { id: tid.clone() }).await?,
                    Response::Ok
                ) {
                    continue;
                }
                let stream = client.into_stream();
                let (sock_read, mut sock_write) = stream.into_split();
                let _ = write_binary_frame(
                    &mut sock_write,
                    tid,
                    &BinaryFrame::Resize {
                        cols: pane_area.width,
                        rows: pane_area.height,
                    },
                )
                .await;
                self.writers.insert(tid.clone(), sock_write);
                self.vterms.insert(
                    tid.clone(),
                    VirtualTerminal::new(pane_area.height, pane_area.width),
                );
                let tx_clone = self.tx.clone();
                let tid_clone = tid.clone();
                self.read_tasks.insert(
                    tid.clone(),
                    tokio::spawn(async move {
                        let mut sock_read = sock_read;
                        while let Ok((_, BinaryFrame::Data(data))) = read_binary_frame(&mut sock_read).await {
                            if tx_clone.send((tid_clone.clone(), data)).await.is_err() {
                                break;
                            }
                        }
                    }),
                );
            }
            if tid != first
                && !self.layout.compute_rects(pane_area).iter().any(|(id, _)| id == tid)
            {
                self.layout.split(SplitDirection::Vertical, tid.clone());
            }
        }

        self.resize_all_vterms().await?;
        self.render_all()
    }

    async fn spawn_terminal(&mut self, id: String, stream: UnixStream) -> Result<()> {
        let rects = self.layout.compute_rects(self.screen.pane_area());
        let (h, w) = rects
            .iter()
            .find(|(tid, _)| tid == &id)
            .map(|(_, r)| (r.height, r.width))
            .unwrap_or((24, 80));
        self.vterms.insert(id.clone(), VirtualTerminal::new(h, w));

        let (sock_read, mut sock_write) = stream.into_split();
        write_binary_frame(&mut sock_write, &id, &BinaryFrame::Resize { cols: w, rows: h }).await?;
        self.writers.insert(id.clone(), sock_write);

        let tx_clone = self.tx.clone();
        let tid = id.clone();
        self.read_tasks.insert(
            id,
            tokio::spawn(async move {
                let mut sock_read = sock_read;
                while let Ok((_, BinaryFrame::Data(data))) = read_binary_frame(&mut sock_read).await {
                    if tx_clone.send((tid.clone(), data)).await.is_err() {
                        break;
                    }
                }
            }),
        );
        Ok(())
    }

    async fn sync_layout(&self) {
        if let Some(vid) = &self.view_id
            && let Ok(mut c) = IpcClient::connect().await
        {
            let _ = c
                .send(Request::ViewUpdateLayout {
                    view_id: vid.clone(),
                    layout: self.layout.to_value(),
                    focused: self.layout.focused_terminal().to_string(),
                })
                .await;
        }
    }

    async fn detach_all(&mut self) {
        for (tid, mut w) in self.writers.drain() {
            let _ = write_binary_frame(&mut w, &tid, &BinaryFrame::Detach).await;
        }
    }
}

struct TuiInit<'a> {
    stream: UnixStream,
    terminal_name: &'a str,
    cols: u16,
    rows: u16,
    config: Config,
    extra_terminals: &'a [String],
    view_id: Option<&'a str>,
    saved_layout: Option<serde_json::Value>,
    saved_focused: Option<String>,
}

async fn run_tui(init: TuiInit<'_>) -> Result<()> {
    let has_saved_layout = matches!(&init.saved_layout, Some(v) if !v.is_null());
    let layout = match init.saved_layout {
        Some(v) if !v.is_null() => PaneLayout::from_value(v, init.saved_focused.as_deref(), init.terminal_name),
        _ => PaneLayout::new(init.terminal_name.to_string()),
    };

    let (tx, mut rx) = mpsc::channel::<(String, Vec<u8>)>(64);

    let pane_area = Screen::new(init.rows, init.cols).pane_area();
    let mut session = TuiSession {
        layout,
        vterms: HashMap::new(),
        writers: HashMap::new(),
        read_tasks: HashMap::new(),
        screen: Screen::new(init.rows, init.cols),
        renderer: Renderer::new(io::stdout()),
        input: InputHandler::with_prefix(init.config.prefix.clone()),
        config: init.config,
        tx: tx.clone(),
        view_id: init.view_id.map(String::from),
        label: init.terminal_name.to_string(),
    };

    session.vterms.insert(
        init.terminal_name.to_string(),
        VirtualTerminal::new(pane_area.height, pane_area.width),
    );

    // Spawn reader for initial terminal
    let (sock_read, sock_write) = init.stream.into_split();
    session.writers.insert(init.terminal_name.to_string(), sock_write);
    let tx_clone = tx.clone();
    let initial_id = init.terminal_name.to_string();
    session.read_tasks.insert(
        init.terminal_name.to_string(),
        tokio::spawn(async move {
            let mut sock_read = sock_read;
            while let Ok((_, BinaryFrame::Data(data))) = read_binary_frame(&mut sock_read).await {
                if tx_clone.send((initial_id.clone(), data)).await.is_err() {
                    break;
                }
            }
        }),
    );
    drop(tx);

    // Connect extra terminals for view restore
    for tid in init.extra_terminals {
        let mut client = IpcClient::connect().await?;
        if !matches!(
            client.send(Request::TerminalAttach { id: tid.clone() }).await?,
            Response::Ok
        ) {
            continue;
        }
        let stream = client.into_stream();
        if !has_saved_layout {
            session.layout.split(SplitDirection::Vertical, tid.clone());
        }
        session.spawn_terminal(tid.clone(), stream).await?;
    }

    session.render_all()?;

    let mut event_reader = EventStream::new();

    loop {
        tokio::select! {
            Some((tid, data)) = rx.recv() => {
                session.handle_pty_data(&tid, &data)?;
            }
            Some(Ok(event)) = event_reader.next() => {
                if let Event::Resize(new_cols, new_rows) = event {
                    session.handle_resize(new_rows, new_cols).await?;
                    continue;
                }
                match session.input.handle_event(&event) {
                    Action::SendToTerminal(bytes) => {
                        let focused = session.layout.focused_terminal().to_string();
                        if let Some(w) = session.writers.get_mut(&focused)
                            && write_binary_frame(w, &focused, &BinaryFrame::Data(bytes)).await.is_err()
                        {
                            break;
                        }
                    }
                    Action::ModeChanged(_) => {
                        session.render_status()?;
                    }
                    Action::PaneSplitHorizontal => {
                        session.handle_split(SplitDirection::Horizontal).await?;
                    }
                    Action::PaneSplitVertical => {
                        session.handle_split(SplitDirection::Vertical).await?;
                    }
                    Action::PaneNavigate(dir) => {
                        session.layout.navigate(dir, session.screen.pane_area());
                        session.render_all()?;
                    }
                    Action::PaneResize(dir) => {
                        session.handle_pane_resize(dir).await?;
                    }
                    Action::PaneClose => {
                        session.handle_close().await?;
                    }
                    Action::ViewList => {
                        if let Ok(text) = fetch_view_list_text().await {
                            let bar = session.screen.status_bar_area();
                            session.renderer.render_status_bar(&text, &bar)?;
                            session.renderer.flush()?;
                        }
                    }
                    Action::ViewSwitch(n) => {
                        if let Ok(info) = fetch_view_by_index(n).await {
                            session.switch_view(&info.terminal_ids).await?;
                        }
                    }
                    Action::Detach => break,
                    _ => {}
                }
            }
            else => break,
        }
    }

    session.detach_all().await;
    Ok(())
}

async fn fetch_view_list_text() -> Result<String> {
    let mut client = IpcClient::connect().await?;
    match client.send(Request::ViewList).await? {
        Response::ViewList { views } => {
            if views.is_empty() {
                return Ok(" [VIEWS] (none)".into());
            }
            let names: Vec<String> = views
                .iter()
                .enumerate()
                .map(|(i, v)| {
                    let label = v.name.as_deref().unwrap_or(&v.id);
                    format!("{}:{label}", i + 1)
                })
                .collect();
            Ok(format!(" [VIEWS] {}", names.join(" | ")))
        }
        _ => Ok(" [VIEWS] error".into()),
    }
}

async fn fetch_view_by_index(n: usize) -> Result<ViewInfo> {
    let mut client = IpcClient::connect().await?;
    match client.send(Request::ViewList).await? {
        Response::ViewList { views } => {
            if n == 0 || n > views.len() {
                return Err(crate::error::KexError::Server(format!(
                    "view index {n} out of range"
                )));
            }
            Ok(views.into_iter().nth(n - 1).unwrap())
        }
        _ => Err(crate::error::KexError::Server("unexpected response".into())),
    }
}
