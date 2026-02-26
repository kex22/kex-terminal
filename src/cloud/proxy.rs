use std::collections::HashMap;
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::task::JoinHandle;

/// An exposed port registered via `kex proxy create`.
#[derive(Debug, Clone)]
pub struct ExposedPort {
    pub port: u16,
    pub public: bool,
    pub url: Option<String>,
}

/// State for an in-flight proxy request from D.O. → CLI → localhost.
pub struct PendingProxyRequest {
    pub port: u16,
    pub handle: tokio::task::JoinHandle<()>,
}

/// A WS message to forward to localhost (received from D.O. via 0x22 frame).
pub enum WsWriteMsg {
    Data { is_text: bool, payload: Vec<u8> },
    Close { code: u16, reason: String },
}

/// State for an active WebSocket proxy connection.
pub struct PendingWsProxy {
    pub port: u16,
    pub reader_handle: JoinHandle<()>,
    pub writer_handle: JoinHandle<()>,
    pub writer_tx: mpsc::Sender<WsWriteMsg>,
}

/// Proxy state managed by CloudManager.
pub struct ProxyState {
    pub exposed_ports: HashMap<u16, ExposedPort>,
    pub pending_requests: HashMap<String, PendingProxyRequest>,
    pub pending_ws: HashMap<String, PendingWsProxy>,
    pub http_client: reqwest::Client,
}

impl Default for ProxyState {
    fn default() -> Self {
        Self::new()
    }
}

impl ProxyState {
    pub fn new() -> Self {
        let http_client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .no_proxy()
            .build()
            .unwrap_or_default();
        Self {
            exposed_ports: HashMap::new(),
            pending_requests: HashMap::new(),
            pending_ws: HashMap::new(),
            http_client,
        }
    }

    pub fn expose(&mut self, port: u16, public: bool) {
        self.exposed_ports.insert(
            port,
            ExposedPort {
                port,
                public,
                url: None,
            },
        );
    }

    pub fn unexpose(&mut self, port: u16) -> bool {
        self.exposed_ports.remove(&port).is_some()
    }

    pub fn set_url(&mut self, port: u16, url: String) {
        if let Some(ep) = self.exposed_ports.get_mut(&port) {
            ep.url = Some(url);
        }
    }

    pub fn cancel_request(&mut self, request_id: &str) {
        if let Some(req) = self.pending_requests.remove(request_id) {
            req.handle.abort();
        }
    }

    pub fn cancel_all_requests(&mut self) {
        for (_, req) in self.pending_requests.drain() {
            req.handle.abort();
        }
        self.cancel_all_ws();
    }

    pub fn cancel_ws(&mut self, request_id: &str) {
        if let Some(ws) = self.pending_ws.remove(request_id) {
            ws.reader_handle.abort();
            ws.writer_handle.abort();
        }
    }

    pub fn cancel_all_ws(&mut self) {
        for (_, ws) in self.pending_ws.drain() {
            ws.reader_handle.abort();
            ws.writer_handle.abort();
        }
    }
}

/// Binary frame types for proxy data.
pub const FRAME_PROXY_REQUEST_BODY: u8 = 0x20;
pub const FRAME_PROXY_RESPONSE_BODY: u8 = 0x21;
pub const FRAME_PROXY_WS_DATA: u8 = 0x22;

/// WebSocket frame subtypes (first byte of payload after requestId).
pub const WS_SUBTYPE_TEXT: u8 = 0x01;
pub const WS_SUBTYPE_BINARY: u8 = 0x02;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ws_frame_constants() {
        assert_eq!(FRAME_PROXY_WS_DATA, 0x22);
        assert_eq!(WS_SUBTYPE_TEXT, 0x01);
        assert_eq!(WS_SUBTYPE_BINARY, 0x02);
        // No overlap with existing frame types
        assert_ne!(FRAME_PROXY_WS_DATA, FRAME_PROXY_REQUEST_BODY);
        assert_ne!(FRAME_PROXY_WS_DATA, FRAME_PROXY_RESPONSE_BODY);
    }

    #[tokio::test]
    async fn cancel_ws_aborts_tasks() {
        let mut state = ProxyState::new();
        let reader =
            tokio::spawn(async { tokio::time::sleep(std::time::Duration::from_secs(60)).await });
        let writer =
            tokio::spawn(async { tokio::time::sleep(std::time::Duration::from_secs(60)).await });
        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        state.pending_ws.insert(
            "req12345".into(),
            PendingWsProxy {
                port: 3000,
                reader_handle: reader,
                writer_handle: writer,
                writer_tx: tx,
            },
        );
        assert_eq!(state.pending_ws.len(), 1);
        state.cancel_ws("req12345");
        assert_eq!(state.pending_ws.len(), 0);
    }

    #[tokio::test]
    async fn cancel_all_ws_clears_all() {
        let mut state = ProxyState::new();
        for i in 0..3 {
            let reader = tokio::spawn(async {
                tokio::time::sleep(std::time::Duration::from_secs(60)).await
            });
            let writer = tokio::spawn(async {
                tokio::time::sleep(std::time::Duration::from_secs(60)).await
            });
            let (tx, _rx) = tokio::sync::mpsc::channel(1);
            state.pending_ws.insert(
                format!("req{i}"),
                PendingWsProxy {
                    port: 3000,
                    reader_handle: reader,
                    writer_handle: writer,
                    writer_tx: tx,
                },
            );
        }
        assert_eq!(state.pending_ws.len(), 3);
        state.cancel_all_ws();
        assert_eq!(state.pending_ws.len(), 0);
    }
}
