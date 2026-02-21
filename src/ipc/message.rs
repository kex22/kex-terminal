use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub enum Request {
    ServerStop,
    TerminalCreate {
        name: Option<String>,
    },
    TerminalList,
    TerminalKill {
        id: String,
    },
    TerminalAttach {
        id: String,
    },
    ViewCreate {
        name: Option<String>,
        terminal_id: String,
    },
    ViewList,
    ViewDelete {
        id: String,
    },
    ViewShow {
        id: String,
    },
    ViewAddTerminal {
        view_id: String,
        terminal_id: String,
    },
}

#[derive(Debug, Serialize, Deserialize)]
pub enum Response {
    Ok,
    Error { message: String },
    TerminalCreated { id: String },
    TerminalList { terminals: Vec<TerminalInfo> },
    ViewCreated { id: String },
    ViewList { views: Vec<ViewInfo> },
    ViewShow { view: ViewInfo },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TerminalInfo {
    pub id: String,
    pub name: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ViewInfo {
    pub id: String,
    pub name: Option<String>,
    pub terminal_ids: Vec<String>,
    pub created_at: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum StreamMessage {
    Data(Vec<u8>),
    Resize { cols: u16, rows: u16 },
    Detach,
}
