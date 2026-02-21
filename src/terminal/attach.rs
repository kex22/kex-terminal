use std::io::Write;

use tokio::io::AsyncReadExt;
use tokio::net::UnixStream;

use crate::error::{KexError, Result};
use crate::ipc::codec::{read_message, write_message};
use crate::ipc::message::StreamMessage;

pub async fn attach(mut stream: UnixStream) -> Result<()> {
    // Send initial resize
    let (cols, rows) = term_size();
    write_message(&mut stream, &StreamMessage::Resize { cols, rows }).await?;

    let _raw_guard = RawModeGuard::enter()?;

    let (mut sock_read, mut sock_write) = stream.into_split();

    // stdin → socket
    let stdin_task = tokio::spawn(async move {
        let mut buf = [0u8; 1024];
        let mut stdin = tokio::io::stdin();
        loop {
            let n = match stdin.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            let msg = StreamMessage::Data(buf[..n].to_vec());
            if write_message(&mut sock_write, &msg).await.is_err() {
                break;
            }
        }
    });

    // socket → stdout
    let stdout_task = tokio::spawn(async move {
        loop {
            let msg: std::result::Result<StreamMessage, _> =
                read_message(&mut sock_read).await;
            match msg {
                Ok(StreamMessage::Data(data)) => {
                    let mut stdout = std::io::stdout().lock();
                    if stdout.write_all(&data).is_err() || stdout.flush().is_err() {
                        break;
                    }
                }
                _ => break,
            }
        }
    });

    // SIGWINCH → resize messages (handled via a separate signal listener)
    // For Phase 1, we skip dynamic resize — initial size is sent above.

    tokio::select! {
        _ = stdin_task => {}
        _ = stdout_task => {}
    }

    Ok(())
}

fn term_size() -> (u16, u16) {
    unsafe {
        let mut ws: libc::winsize = std::mem::zeroed();
        if libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut ws) == 0 {
            (ws.ws_col, ws.ws_row)
        } else {
            (80, 24)
        }
    }
}

struct RawModeGuard {
    original: nix::sys::termios::Termios,
}

impl RawModeGuard {
    fn enter() -> Result<Self> {
        use nix::sys::termios;
        let original = termios::tcgetattr(std::io::stdin())
            .map_err(|e| KexError::Server(format!("tcgetattr: {e}")))?;
        let mut raw = original.clone();
        termios::cfmakeraw(&mut raw);
        termios::tcsetattr(std::io::stdin(), termios::SetArg::TCSANOW, &raw)
            .map_err(|e| KexError::Server(format!("tcsetattr: {e}")))?;
        Ok(Self { original })
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = nix::sys::termios::tcsetattr(
            std::io::stdin(),
            nix::sys::termios::SetArg::TCSANOW,
            &self.original,
        );
    }
}
