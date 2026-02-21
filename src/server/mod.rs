pub mod daemon;
pub mod pid;
pub mod state;

use std::io::Write as _;
use std::sync::Arc;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{Mutex, Notify};

use crate::error::{KexError, Result};
use crate::ipc;
use crate::ipc::codec::{read_message, write_message};
use crate::ipc::message::{Request, Response, StreamMessage, ViewInfo};
use crate::server::state::StatePersister;
use crate::terminal::manager::TerminalManager;
use crate::view::manager::ViewManager;

pub struct Server {
    listener: UnixListener,
    shutdown: Arc<Notify>,
    manager: Arc<Mutex<TerminalManager>>,
    view_manager: Arc<Mutex<ViewManager>>,
    persister: StatePersister,
}

impl Server {
    pub async fn start() -> Result<()> {
        if pid::is_server_running() {
            return Err(KexError::Server("server is already running".into()));
        }

        ipc::ensure_socket_dir()?;

        let sock_path = ipc::socket_path();
        if sock_path.exists() {
            std::fs::remove_file(&sock_path)?;
        }

        let listener = UnixListener::bind(&sock_path)?;
        pid::write_pid()?;

        // Clean stale state from previous run (PTYs are dead after restart)
        let _ = std::fs::remove_file(state::state_path());

        let manager = Arc::new(Mutex::new(TerminalManager::new()));
        let view_manager = Arc::new(Mutex::new(ViewManager::new()));
        let persister = StatePersister::spawn(manager.clone(), view_manager.clone());

        let server = Server {
            listener,
            shutdown: Arc::new(Notify::new()),
            manager,
            view_manager,
            persister,
        };
        server.run().await
    }

    async fn run(self) -> Result<()> {
        let shutdown = self.shutdown.clone();

        let sig_shutdown = self.shutdown.clone();
        tokio::spawn(async move {
            let Ok(mut sigterm) =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            else {
                eprintln!("failed to register SIGTERM handler");
                return;
            };
            let Ok(mut sigint) =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
            else {
                eprintln!("failed to register SIGINT handler");
                return;
            };
            tokio::select! {
                _ = sigterm.recv() => {}
                _ = sigint.recv() => {}
            }
            sig_shutdown.notify_one();
        });

        loop {
            tokio::select! {
                result = self.listener.accept() => {
                    let (stream, _) = result?;
                    let notify = shutdown.clone();
                    let mgr = self.manager.clone();
                    let vmgr = self.view_manager.clone();
                    let persist = self.persister.clone();
                    tokio::spawn(async move {
                        if let Err(e) = handle_connection(stream, notify, mgr, vmgr, persist).await {
                            eprintln!("connection error: {e}");
                        }
                    });
                }
                _ = shutdown.notified() => {
                    self.shutdown().await?;
                    return Ok(());
                }
            }
        }
    }

    async fn shutdown(self) -> Result<()> {
        // Save final state
        self.persister.notify();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // Kill all terminals (sends SIGHUP to shell processes)
        let mut mgr = self.manager.lock().await;
        let ids: Vec<String> = mgr.list().into_iter().map(|t| t.id).collect();
        for id in ids {
            let _ = mgr.kill(&id);
        }
        drop(mgr);

        // Clean up socket and PID
        let _ = std::fs::remove_file(ipc::socket_path());
        pid::remove_pid()?;
        Ok(())
    }
}

async fn handle_connection(
    mut stream: UnixStream,
    shutdown: Arc<Notify>,
    manager: Arc<Mutex<TerminalManager>>,
    view_manager: Arc<Mutex<ViewManager>>,
    persister: StatePersister,
) -> Result<()> {
    let req: Request = read_message(&mut stream).await?;
    let is_mutation = matches!(
        req,
        Request::TerminalCreate { .. }
            | Request::TerminalKill { .. }
            | Request::ViewCreate { .. }
            | Request::ViewDelete { .. }
            | Request::ViewAddTerminal { .. }
            | Request::ViewUpdateLayout { .. }
            | Request::ViewRemoveTerminal { .. }
    );
    let resp = match req {
        Request::ServerStop => {
            shutdown.notify_one();
            Response::Ok
        }
        Request::TerminalCreate { name } => {
            let mut mgr = manager.lock().await;
            match mgr.create(name) {
                Ok(id) => Response::TerminalCreated { id },
                Err(e) => Response::Error {
                    message: e.to_string(),
                },
            }
        }
        Request::TerminalList => {
            let mgr = manager.lock().await;
            Response::TerminalList {
                terminals: mgr.list(),
            }
        }
        Request::TerminalKill { id } => {
            let mut mgr = manager.lock().await;
            match mgr.kill(&id) {
                Ok(()) => {
                    let mut vmgr = view_manager.lock().await;
                    vmgr.remove_terminal(&id);
                    Response::Ok
                }
                Err(e) => Response::Error {
                    message: e.to_string(),
                },
            }
        }
        Request::TerminalAttach { id } => {
            return handle_attach(stream, manager, &id).await;
        }
        Request::ViewCreate { name, terminal_id } => {
            let mgr = manager.lock().await;
            if mgr.get(&terminal_id).is_none() {
                Response::Error {
                    message: format!("terminal not found: {terminal_id}"),
                }
            } else {
                drop(mgr);
                let mut vmgr = view_manager.lock().await;
                let id = vmgr.create(name, terminal_id);
                Response::ViewCreated { id }
            }
        }
        Request::ViewList => {
            let vmgr = view_manager.lock().await;
            Response::ViewList { views: vmgr.list() }
        }
        Request::ViewDelete { id } => {
            let mut vmgr = view_manager.lock().await;
            match vmgr.delete(&id) {
                Ok(()) => Response::Ok,
                Err(e) => Response::Error {
                    message: e.to_string(),
                },
            }
        }
        Request::ViewShow { id } => {
            let vmgr = view_manager.lock().await;
            match vmgr.get(&id) {
                Some(v) => Response::ViewShow {
                    view: ViewInfo {
                        id: v.id.clone(),
                        name: v.name.clone(),
                        terminal_ids: v.terminal_ids.clone(),
                        created_at: v.created_at.clone(),
                    },
                },
                None => Response::Error {
                    message: format!("view not found: {id}"),
                },
            }
        }
        Request::ViewAddTerminal {
            view_id,
            terminal_id,
        } => {
            let mgr = manager.lock().await;
            if mgr.get(&terminal_id).is_none() {
                Response::Error {
                    message: format!("terminal not found: {terminal_id}"),
                }
            } else {
                drop(mgr);
                let mut vmgr = view_manager.lock().await;
                match vmgr.resolve_id(&view_id) {
                    Some(resolved) => {
                        vmgr.add_terminal(&resolved, &terminal_id);
                        Response::Ok
                    }
                    None => Response::Error {
                        message: format!("view not found: {view_id}"),
                    },
                }
            }
        }
        Request::ViewAttach { id } => {
            let vmgr = view_manager.lock().await;
            match vmgr.get(&id) {
                Some(v) => Response::ViewAttach {
                    terminal_ids: v.terminal_ids.clone(),
                    layout: Some(v.layout.clone()),
                    focused: Some(v.focused.clone()),
                },
                None => Response::Error {
                    message: format!("view not found: {id}"),
                },
            }
        }
        Request::ViewUpdateLayout {
            view_id,
            layout,
            focused,
        } => {
            let mut vmgr = view_manager.lock().await;
            vmgr.update_layout(&view_id, layout, focused);
            Response::Ok
        }
        Request::ViewRemoveTerminal {
            view_id,
            terminal_id,
        } => {
            let mut vmgr = view_manager.lock().await;
            vmgr.remove_terminal_from_view(&view_id, &terminal_id);
            Response::Ok
        }
    };
    let result = write_message(&mut stream, &resp).await;
    if is_mutation {
        persister.notify();
    }
    result
}

async fn handle_attach(
    mut stream: UnixStream,
    manager: Arc<Mutex<TerminalManager>>,
    id: &str,
) -> Result<()> {
    // Validate terminal exists and clone reader/writer (non-destructive)
    let (mut pty_reader, pty_writer, pty_resizer) = {
        let mgr = manager.lock().await;
        let terminal = match mgr.get(id) {
            Some(t) => t,
            None => {
                let resp = Response::Error {
                    message: format!("terminal not found: {id}"),
                };
                return write_message(&mut stream, &resp).await;
            }
        };
        let reader = terminal.pty.clone_reader()?;
        let writer = terminal.pty.clone_writer();
        let resizer = terminal.pty.clone_resizer();
        (reader, writer, resizer)
    };

    write_message(&mut stream, &Response::Ok).await?;

    let (mut sock_read, mut sock_write) = stream.into_split();

    // PTY → socket: blocking read in spawn_blocking, send via channel
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<u8>>(32);
    let pty_read_task = tokio::task::spawn_blocking(move || {
        let mut buf = [0u8; 4096];
        loop {
            match pty_reader.read(&mut buf) {
                Ok(0) => break,
                Err(e) => {
                    eprintln!("pty read error: {e}");
                    break;
                }
                Ok(n) => {
                    if tx.blocking_send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });

    // Channel → socket: forward PTY output as StreamMessage
    let sock_write_task = tokio::spawn(async move {
        use crate::ipc::codec::write_message;
        while let Some(data) = rx.recv().await {
            if write_message(&mut sock_write, &StreamMessage::Data(data))
                .await
                .is_err()
            {
                break;
            }
        }
    });

    // Socket → PTY: read StreamMessage, write to PTY
    let sock_read_task = tokio::spawn(async move {
        use crate::ipc::codec::read_message;
        loop {
            let msg: std::result::Result<StreamMessage, _> = read_message(&mut sock_read).await;
            match msg {
                Ok(StreamMessage::Data(data)) => {
                    let Ok(mut w) = pty_writer.lock() else { break };
                    if w.write_all(&data).is_err() {
                        break;
                    }
                }
                Ok(StreamMessage::Resize { cols, rows }) => {
                    let _ = pty_resizer.resize(cols, rows);
                }
                Ok(StreamMessage::Detach) | Err(_) => break,
            }
        }
    });

    // Wait for any task to finish, then clean up
    tokio::select! {
        _ = pty_read_task => {}
        _ = sock_write_task => {}
        _ = sock_read_task => {}
    }

    Ok(())
}
