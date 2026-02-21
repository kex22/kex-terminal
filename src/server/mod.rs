pub mod daemon;
pub mod pid;

use std::sync::Arc;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{Mutex, Notify};

use crate::error::{KexError, Result};
use crate::ipc;
use crate::ipc::codec::{read_message, write_message};
use crate::ipc::message::{Request, Response, StreamMessage};
use crate::terminal::manager::TerminalManager;

pub struct Server {
    listener: UnixListener,
    shutdown: Arc<Notify>,
    manager: Arc<Mutex<TerminalManager>>,
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

        let server = Server {
            listener,
            shutdown: Arc::new(Notify::new()),
            manager: Arc::new(Mutex::new(TerminalManager::new())),
        };
        server.run().await
    }

    async fn run(self) -> Result<()> {
        let shutdown = self.shutdown.clone();

        let sig_shutdown = self.shutdown.clone();
        tokio::spawn(async move {
            let mut sigterm =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                    .expect("failed to register SIGTERM");
            let mut sigint =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
                    .expect("failed to register SIGINT");
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
                    tokio::spawn(async move {
                        if let Err(e) = handle_connection(stream, notify, mgr).await {
                            eprintln!("connection error: {e}");
                        }
                    });
                }
                _ = shutdown.notified() => {
                    Self::cleanup()?;
                    return Ok(());
                }
            }
        }
    }

    fn cleanup() -> Result<()> {
        let _ = std::fs::remove_file(ipc::socket_path());
        pid::remove_pid()?;
        Ok(())
    }
}

async fn handle_connection(
    mut stream: UnixStream,
    shutdown: Arc<Notify>,
    manager: Arc<Mutex<TerminalManager>>,
) -> Result<()> {
    let req: Request = read_message(&mut stream).await?;
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
                Ok(()) => Response::Ok,
                Err(e) => Response::Error {
                    message: e.to_string(),
                },
            }
        }
        Request::TerminalAttach { id } => {
            return handle_attach(stream, manager, &id).await;
        }
    };
    write_message(&mut stream, &resp).await
}

async fn handle_attach(
    mut stream: UnixStream,
    manager: Arc<Mutex<TerminalManager>>,
    id: &str,
) -> Result<()> {
    // Validate terminal exists and take reader/writer
    let (mut pty_reader, mut pty_writer, pty_resizer) = {
        let mut mgr = manager.lock().await;
        let terminal = match mgr.get_mut(id) {
            Some(t) => t,
            None => {
                let resp = Response::Error {
                    message: format!("terminal not found: {id}"),
                };
                return write_message(&mut stream, &resp).await;
            }
        };
        let reader = terminal.pty.take_reader()?;
        let writer = terminal.pty.take_writer()?;
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
                Ok(0) | Err(_) => break,
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
            let msg: std::result::Result<StreamMessage, _> =
                read_message(&mut sock_read).await;
            match msg {
                Ok(StreamMessage::Data(data)) => {
                    if pty_writer.write_all(&data).is_err() {
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
