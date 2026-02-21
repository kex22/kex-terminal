use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};

use crate::error::{KexError, Result};

pub struct Pty {
    master: Arc<Mutex<Box<dyn MasterPty + Send>>>,
    reader: Option<Box<dyn Read + Send>>,
    writer: Option<Box<dyn Write + Send>>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
}

#[derive(Clone)]
pub struct PtyResizer {
    master: Arc<Mutex<Box<dyn MasterPty + Send>>>,
}

impl PtyResizer {
    pub fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        let master = self
            .master
            .lock()
            .map_err(|_| KexError::Server("pty mutex poisoned".into()))?;
        master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| KexError::Server(format!("resize: {e}")))
    }
}

impl Pty {
    pub fn spawn() -> Result<Self> {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| KexError::Server(format!("openpty: {e}")))?;

        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
        let cmd = CommandBuilder::new(shell);
        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| KexError::Server(format!("spawn: {e}")))?;

        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| KexError::Server(format!("clone reader: {e}")))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| KexError::Server(format!("take writer: {e}")))?;

        Ok(Pty {
            master: Arc::new(Mutex::new(pair.master)),
            reader: Some(reader),
            writer: Some(writer),
            child,
        })
    }

    pub fn take_reader(&mut self) -> Result<Box<dyn Read + Send>> {
        self.reader
            .take()
            .ok_or_else(|| KexError::Server("reader already taken".into()))
    }

    pub fn take_writer(&mut self) -> Result<Box<dyn Write + Send>> {
        self.writer
            .take()
            .ok_or_else(|| KexError::Server("writer already taken".into()))
    }

    pub fn clone_resizer(&self) -> PtyResizer {
        PtyResizer {
            master: self.master.clone(),
        }
    }

    pub fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        let master = self
            .master
            .lock()
            .map_err(|_| KexError::Server("pty mutex poisoned".into()))?;
        master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| KexError::Server(format!("resize: {e}")))
    }

    pub fn kill(&mut self) -> Result<()> {
        self.child
            .kill()
            .map_err(|e| KexError::Server(format!("kill: {e}")))
    }
}
