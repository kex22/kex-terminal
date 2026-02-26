use std::collections::HashMap;
use std::time::Duration;

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

/// Proxy state managed by CloudManager.
pub struct ProxyState {
    pub exposed_ports: HashMap<u16, ExposedPort>,
    pub pending_requests: HashMap<String, PendingProxyRequest>,
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
    }
}

/// Binary frame types for proxy data.
pub const FRAME_PROXY_REQUEST_BODY: u8 = 0x20;
pub const FRAME_PROXY_RESPONSE_BODY: u8 = 0x21;
pub const FRAME_PROXY_WS_DATA: u8 = 0x22;

/// WebSocket frame subtypes (first byte of payload after requestId).
pub const WS_SUBTYPE_TEXT: u8 = 0x01;
pub const WS_SUBTYPE_BINARY: u8 = 0x02;
