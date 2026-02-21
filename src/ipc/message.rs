use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub enum Request {
    ServerStop,
    TerminalCreate { name: Option<String> },
    TerminalList,
    TerminalKill { id: String },
    TerminalAttach { id: String },
}

#[derive(Debug, Serialize, Deserialize)]
pub enum Response {
    Ok,
    Error { message: String },
    TerminalCreated { id: String },
    TerminalList { terminals: Vec<TerminalInfo> },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TerminalInfo {
    pub id: String,
    pub name: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum StreamMessage {
    Data(Vec<u8>),
    Resize { cols: u16, rows: u16 },
    Detach,
}
