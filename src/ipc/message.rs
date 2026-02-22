use serde::{Deserialize, Serialize};
use serde_json::Value;

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
    MultiplexAttach {
        terminal_ids: Vec<String>,
        view_id: Option<String>,
    },
    TerminalSync {
        id: String,
    },
    TerminalUnsync {
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
    ViewAttach {
        id: String,
    },
    ViewUpdateLayout {
        view_id: String,
        layout: Value,
        focused: String,
    },
    ViewRemoveTerminal {
        view_id: String,
        terminal_id: String,
    },
}

#[derive(Debug, Serialize, Deserialize)]
pub enum Response {
    Ok,
    Error {
        message: String,
    },
    TerminalCreated {
        id: String,
    },
    TerminalList {
        terminals: Vec<TerminalInfo>,
    },
    MultiplexAttached {
        terminal_ids: Vec<String>,
    },
    SyncStatus {
        synced: bool,
    },
    ViewCreated {
        id: String,
    },
    ViewList {
        views: Vec<ViewInfo>,
    },
    ViewShow {
        view: ViewInfo,
    },
    ViewAttach {
        terminal_ids: Vec<String>,
        layout: Option<Value>,
        focused: Option<String>,
    },
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

#[derive(Debug, PartialEq)]
pub enum BinaryFrame {
    Data(Vec<u8>),
    Resize { cols: u16, rows: u16 },
    Detach,
    Control(Vec<u8>),
}

#[derive(Debug, Serialize, Deserialize)]
pub enum MuxRequest {
    CreateTerminal { name: Option<String> },
    AddTerminal { id: String },
    RemoveTerminal { id: String },
    KillTerminal { id: String },
    UpdateLayout { view_id: String, layout: Value, focused: String },
}

#[derive(Debug, Serialize, Deserialize)]
pub enum MuxResponse {
    Ok,
    TerminalCreated { id: String },
    Error { message: String },
}
