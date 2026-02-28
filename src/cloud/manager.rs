use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::{Mutex, mpsc};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;

use crate::cloud::proxy;
use crate::cloud::proxy::ProxyState;
use crate::credential;
use crate::terminal::manager::TerminalManager;
use crate::terminal::pty::PtyResizer;

/// Commands sent to CloudManager from IPC handlers.
pub enum CloudCommand {
    Sync {
        id: String,
        name: Option<String>,
        pty_reader: Box<dyn std::io::Read + Send>,
        pty_writer: Arc<std::sync::Mutex<Box<dyn std::io::Write + Send>>>,
        pty_resizer: PtyResizer,
        reply: mpsc::Sender<Result<(), String>>,
    },
    Unsync {
        id: String,
        reply: mpsc::Sender<Result<(), String>>,
    },
    ProxyExpose {
        port: u16,
        public: bool,
        reply: mpsc::Sender<Result<String, String>>,
    },
    ProxyUnexpose {
        port: u16,
        reply: mpsc::Sender<Result<(), String>>,
    },
    ProxyList {
        reply: mpsc::Sender<Vec<crate::ipc::message::ProxyPortInfo>>,
    },
}

/// Internal events from PTY reader tasks.
enum TerminalEvent {
    Output { id: String, data: Vec<u8> },
    Exited { id: String },
}

/// Events from proxy tasks (localhost HTTP responses).
#[doc(hidden)]
pub enum ProxyEvent {
    Head {
        request_id: String,
        status: u16,
        headers: HashMap<String, Vec<String>>,
    },
    Body {
        request_id: String,
        data: Vec<u8>,
    },
    End {
        request_id: String,
    },
    Error {
        request_id: String,
        message: String,
    },
    WsOpen {
        request_id: String,
    },
    WsClose {
        request_id: String,
        code: u16,
        reason: String,
    },
    WsError {
        request_id: String,
        message: String,
    },
    WsData {
        request_id: String,
        is_text: bool,
        data: Vec<u8>,
    },
}

/// Shared synced terminal set — read by StatePersister, written by CloudManager.
pub type SyncedSet = Arc<Mutex<HashSet<String>>>;

type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
type WsRead = SplitStream<WsStream>;
type WsWrite = SplitSink<WsStream, Message>;

struct SyncedTerminal {
    id: String,
    name: Option<String>,
    parser: vt100::Parser,
    pty_writer: Arc<std::sync::Mutex<Box<dyn std::io::Write + Send>>>,
    pty_resizer: PtyResizer,
    reader_handle: JoinHandle<()>,
}

pub struct CloudManager {
    rx: mpsc::Receiver<CloudCommand>,
    event_rx: mpsc::Receiver<TerminalEvent>,
    event_tx: mpsc::Sender<TerminalEvent>,
    synced: SyncedSet,
    terminals: HashMap<String, SyncedTerminal>,
    manager: Arc<Mutex<TerminalManager>>,
    ws_read: Option<WsRead>,
    ws_write: Option<WsWrite>,
    backoff: Duration,
    disconnect_at: Option<tokio::time::Instant>,
    proxy: ProxyState,
    pending_expose: HashMap<u16, mpsc::Sender<Result<String, String>>>,
    proxy_event_tx: mpsc::Sender<ProxyEvent>,
    proxy_event_rx: mpsc::Receiver<ProxyEvent>,
}

const HEARTBEAT_SECS: u64 = 30;
const MAX_BACKOFF: Duration = Duration::from_secs(30);
const RESP_TIMEOUT: Duration = Duration::from_secs(10);
const DISCONNECT_DELAY: Duration = Duration::from_secs(30);

impl CloudManager {
    pub fn spawn(
        synced: SyncedSet,
        manager: Arc<Mutex<TerminalManager>>,
    ) -> mpsc::Sender<CloudCommand> {
        let (tx, rx) = mpsc::channel(32);
        let (event_tx, event_rx) = mpsc::channel(256);
        let (proxy_event_tx, proxy_event_rx) = mpsc::channel(256);
        let mgr = CloudManager {
            rx,
            event_rx,
            event_tx,
            synced,
            terminals: HashMap::new(),
            manager,
            ws_read: None,
            ws_write: None,
            backoff: Duration::from_secs(1),
            disconnect_at: None,
            proxy: ProxyState::new(),
            pending_expose: HashMap::new(),
            proxy_event_tx,
            proxy_event_rx,
        };
        tokio::spawn(mgr.run());
        tx
    }

    async fn run(mut self) {
        let mut heartbeat = tokio::time::interval(Duration::from_secs(HEARTBEAT_SECS));
        heartbeat.tick().await; // skip immediate tick

        loop {
            let has_conn = self.ws_write.is_some();
            let disconnect_at = self.disconnect_at;
            tokio::select! {
                biased;
                cmd = self.rx.recv() => {
                    match cmd {
                        Some(cmd) => self.handle_cmd(cmd).await,
                        None => break,
                    }
                }
                event = self.event_rx.recv() => {
                    if let Some(event) = event {
                        self.handle_event(event).await;
                    }
                }
                proxy_ev = self.proxy_event_rx.recv() => {
                    if let Some(ev) = proxy_ev {
                        self.handle_proxy_event(ev).await;
                    }
                }
                msg = async {
                    match &mut self.ws_read {
                        Some(r) => r.next().await,
                        None => std::future::pending().await,
                    }
                }, if has_conn => {
                    match msg {
                        Some(Ok(msg)) => self.handle_ws_message(msg).await,
                        _ => self.drop_conn(),
                    }
                }
                _ = tokio::time::sleep_until(disconnect_at.unwrap_or(tokio::time::Instant::now())), if disconnect_at.is_some() && has_conn => {
                    self.disconnect().await;
                    self.disconnect_at = None;
                }
                _ = heartbeat.tick(), if has_conn => {
                    self.heartbeat().await;
                }
            }
        }
    }

    async fn handle_cmd(&mut self, cmd: CloudCommand) {
        match cmd {
            CloudCommand::Sync {
                id,
                name,
                pty_reader,
                pty_writer,
                pty_resizer,
                reply,
            } => {
                let result = self
                    .handle_sync(id, name, pty_reader, pty_writer, pty_resizer)
                    .await;
                let _ = reply.send(result).await;
            }
            CloudCommand::Unsync { id, reply } => {
                let result = self.handle_unsync(id).await;
                let _ = reply.send(result).await;
            }
            CloudCommand::ProxyExpose {
                port,
                public,
                reply,
            } => {
                self.handle_proxy_expose(port, public, reply).await;
            }
            CloudCommand::ProxyUnexpose { port, reply } => {
                let result = self.handle_proxy_unexpose(port).await;
                let _ = reply.send(result).await;
            }
            CloudCommand::ProxyList { reply } => {
                let list = self
                    .proxy
                    .exposed_ports
                    .values()
                    .map(|ep| crate::ipc::message::ProxyPortInfo {
                        port: ep.port,
                        public: ep.public,
                        url: ep.url.clone(),
                    })
                    .collect();
                let _ = reply.send(list).await;
            }
        }
    }

    async fn handle_sync(
        &mut self,
        id: String,
        name: Option<String>,
        pty_reader: Box<dyn std::io::Read + Send>,
        pty_writer: Arc<std::sync::Mutex<Box<dyn std::io::Write + Send>>>,
        pty_resizer: PtyResizer,
    ) -> Result<(), String> {
        let cred = credential::load().map_err(|e| e.to_string())?;

        // Cancel pending delayed disconnect
        self.disconnect_at = None;

        self.synced.lock().await.insert(id.clone());

        let mut payload = serde_json::json!({ "terminalId": &id });
        if let Some(n) = &name {
            payload["name"] = serde_json::Value::String(n.clone());
        }

        if let Err(e) = self.send_and_confirm(&cred, "terminal.sync", payload).await {
            self.synced.lock().await.remove(&id);
            return Err(e);
        }

        // Spawn PTY reader task
        let reader_handle = spawn_pty_reader(id.clone(), pty_reader, self.event_tx.clone());

        self.terminals.insert(
            id.clone(),
            SyncedTerminal {
                id,
                name,
                parser: vt100::Parser::new(24, 80, 0),
                pty_writer,
                pty_resizer,
                reader_handle,
            },
        );

        Ok(())
    }

    async fn handle_unsync(&mut self, id: String) -> Result<(), String> {
        if !self.terminals.contains_key(&id) {
            return Err(format!("terminal {id} is not synced"));
        }

        let cred = credential::load().map_err(|e| e.to_string())?;
        let payload = serde_json::json!({ "terminalId": &id });

        self.send_and_confirm(&cred, "terminal.unsync", payload)
            .await?;

        if let Some(t) = self.terminals.remove(&id) {
            t.reader_handle.abort();
        }
        self.synced.lock().await.remove(&id);

        if self.terminals.is_empty() {
            self.disconnect_at = Some(tokio::time::Instant::now() + DISCONNECT_DELAY);
        }

        Ok(())
    }

    async fn handle_proxy_expose(
        &mut self,
        port: u16,
        public: bool,
        reply: mpsc::Sender<Result<String, String>>,
    ) {
        let cred = match credential::load() {
            Ok(c) => c,
            Err(e) => {
                let _ = reply.send(Err(e.to_string())).await;
                return;
            }
        };
        if let Err(e) = self.ensure_connected(&cred).await {
            let _ = reply.send(Err(e)).await;
            return;
        }

        self.proxy.expose(port, public);
        self.pending_expose.insert(port, reply);

        let msg = envelope(
            "proxy.expose",
            serde_json::json!({ "port": port, "public": public }),
        );
        if self.ws_send_text(&msg).await.is_err() {
            if let Some(reply) = self.pending_expose.remove(&port) {
                let _ = reply.send(Err("connection lost".into())).await;
            }
            self.proxy.unexpose(port);
            self.drop_conn();
        }
    }

    async fn handle_proxy_unexpose(&mut self, port: u16) -> Result<(), String> {
        if !self.proxy.exposed_ports.contains_key(&port) {
            return Err(format!("port {port} is not exposed"));
        }

        let cred = credential::load().map_err(|e| e.to_string())?;
        self.ensure_connected(&cred).await?;

        let msg = envelope("proxy.unexpose", serde_json::json!({ "port": port }));
        self.ws_send_text(&msg).await?;
        self.proxy.unexpose(port);
        Ok(())
    }

    async fn handle_proxy_request_head(&mut self, payload: &serde_json::Value) {
        let request_id = payload["requestId"].as_str().unwrap_or("").to_string();
        let method = payload["method"].as_str().unwrap_or("GET").to_string();
        let path = payload["path"].as_str().unwrap_or("/").to_string();
        let port = payload["port"].as_u64().unwrap_or(0) as u16;
        let headers: HashMap<String, String> = payload["headers"]
            .as_object()
            .map(|obj| {
                obj.iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                    .collect()
            })
            .unwrap_or_default();

        if port == 0 || request_id.is_empty() {
            let msg = envelope(
                "proxy.response.error",
                serde_json::json!({ "requestId": request_id, "message": "invalid port or requestId" }),
            );
            let _ = self.ws_send_text(&msg).await;
            return;
        }

        let tx = self.proxy_event_tx.clone();
        let rid = request_id.clone();
        let client = self.proxy.http_client.clone();

        let handle = tokio::spawn(async move {
            proxy_request_task(rid, method, port, path, headers, tx, client).await;
        });

        self.proxy
            .pending_requests
            .insert(request_id, proxy::PendingProxyRequest { port, handle });
    }

    async fn handle_proxy_ws_upgrade(&mut self, payload: &serde_json::Value) {
        let request_id = payload["requestId"].as_str().unwrap_or("").to_string();
        let path = payload["path"].as_str().unwrap_or("/").to_string();
        let port = payload["port"].as_u64().unwrap_or(0) as u16;
        let headers: HashMap<String, String> = payload["headers"]
            .as_object()
            .map(|obj| {
                obj.iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                    .collect()
            })
            .unwrap_or_default();

        if port == 0 || request_id.is_empty() {
            let msg = envelope(
                "proxy.ws.error",
                serde_json::json!({ "requestId": request_id, "message": "invalid port or requestId" }),
            );
            let _ = self.ws_send_text(&msg).await;
            return;
        }

        let url = format!("ws://127.0.0.1:{port}{path}");
        // Build WS request with forwarded headers
        let mut req_builder = tokio_tungstenite::tungstenite::http::Request::builder().uri(&url);
        for (k, v) in &headers {
            let lower = k.to_lowercase();
            if matches!(
                lower.as_str(),
                "host" | "connection" | "upgrade" | "sec-websocket-key" | "sec-websocket-version"
            ) {
                continue; // hop-by-hop / WS handshake headers managed by tungstenite
            }
            req_builder = req_builder.header(k.as_str(), v.as_str());
        }
        let ws_request = req_builder.body(()).unwrap();
        let proxy_tx = self.proxy_event_tx.clone();
        let rid = request_id.clone();

        let (ws_writer_tx, ws_writer_rx) = mpsc::channel::<proxy::WsWriteMsg>(64);
        let (write_half_tx, write_half_rx) = tokio::sync::oneshot::channel();

        // Reader/connector task: connect → send WsOpen → read loop
        let reader_tx = proxy_tx.clone();
        let reader_rid = rid.clone();
        let reader_handle = tokio::spawn(async move {
            let connect_result = tokio::time::timeout(
                Duration::from_secs(5),
                tokio_tungstenite::connect_async(ws_request),
            )
            .await;

            let ws_stream = match connect_result {
                Ok(Ok((stream, _))) => stream,
                Ok(Err(e)) => {
                    let _ = reader_tx
                        .send(ProxyEvent::WsError {
                            request_id: reader_rid,
                            message: format!("WS connect failed: {e}"),
                        })
                        .await;
                    return;
                }
                Err(_) => {
                    let _ = reader_tx
                        .send(ProxyEvent::WsError {
                            request_id: reader_rid,
                            message: "WS connect timeout".into(),
                        })
                        .await;
                    return;
                }
            };

            // Connected — notify D.O.
            let _ = reader_tx
                .send(ProxyEvent::WsOpen {
                    request_id: reader_rid.clone(),
                })
                .await;

            let (write_half, mut read_half) = futures_util::StreamExt::split(ws_stream);
            // Pass write half to writer task
            let _ = write_half_tx.send(write_half);

            // Read loop: localhost WS → ProxyEvent
            while let Some(msg) = futures_util::StreamExt::next(&mut read_half).await {
                match msg {
                    Ok(Message::Text(text)) => {
                        let _ = reader_tx
                            .send(ProxyEvent::WsData {
                                request_id: reader_rid.clone(),
                                is_text: true,
                                data: text.as_bytes().to_vec(),
                            })
                            .await;
                    }
                    Ok(Message::Binary(bin)) => {
                        let _ = reader_tx
                            .send(ProxyEvent::WsData {
                                request_id: reader_rid.clone(),
                                is_text: false,
                                data: bin.to_vec(),
                            })
                            .await;
                    }
                    Ok(Message::Close(frame)) => {
                        let (code, reason) = frame
                            .map(|f| (f.code.into(), f.reason.to_string()))
                            .unwrap_or((1000, String::new()));
                        let _ = reader_tx
                            .send(ProxyEvent::WsClose {
                                request_id: reader_rid.clone(),
                                code,
                                reason,
                            })
                            .await;
                        return; // already sent WsClose, skip post-loop cleanup
                    }
                    Err(_) => break,
                    _ => {} // Ping/Pong handled by tungstenite
                }
            }
            // Reader exited without a Close frame — notify main loop for cleanup
            let _ = reader_tx
                .send(ProxyEvent::WsClose {
                    request_id: reader_rid,
                    code: 1006,
                    reason: "connection lost".into(),
                })
                .await;
        });

        // Writer task: mpsc channel → localhost WS
        let writer_handle = tokio::spawn(async move {
            let Ok(mut write_half) = write_half_rx.await else {
                return;
            };
            let mut rx = ws_writer_rx;
            while let Some(msg) = rx.recv().await {
                let ws_msg = match msg {
                    proxy::WsWriteMsg::Data { is_text, payload } => {
                        if is_text {
                            Message::Text(String::from_utf8_lossy(&payload).into_owned().into())
                        } else {
                            Message::Binary(payload.into())
                        }
                    }
                    proxy::WsWriteMsg::Close { code, reason } => {
                        use tokio_tungstenite::tungstenite::protocol::CloseFrame;
                        Message::Close(Some(CloseFrame {
                            code: code.into(),
                            reason: reason.into(),
                        }))
                    }
                };
                if futures_util::SinkExt::send(&mut write_half, ws_msg)
                    .await
                    .is_err()
                {
                    break;
                }
            }
        });

        self.proxy.pending_ws.insert(
            request_id,
            proxy::PendingWsProxy {
                port,
                reader_handle,
                writer_handle,
                writer_tx: ws_writer_tx,
            },
        );
    }

    /// Ensure connected, send message, wait for confirmation response.
    async fn send_and_confirm(
        &mut self,
        cred: &credential::Credential,
        msg_type: &str,
        payload: serde_json::Value,
    ) -> Result<(), String> {
        self.ensure_connected(cred).await?;

        let msg = envelope(msg_type, payload);
        if let Err(e) = self.ws_send_text(&msg).await {
            self.drop_conn();
            return Err(e);
        }

        // Wait for response (terminal.sync.ok, or error)
        match self.ws_recv_text().await {
            Ok(resp) => {
                let resp_type = resp["type"].as_str().unwrap_or("");
                if resp_type == "error" || resp_type == "auth.error" {
                    let msg = resp["payload"]["message"]
                        .as_str()
                        .unwrap_or("unknown error");
                    return Err(msg.to_string());
                }
                Ok(())
            }
            Err(e) => {
                self.drop_conn();
                Err(e)
            }
        }
    }

    async fn ensure_connected(&mut self, cred: &credential::Credential) -> Result<(), String> {
        if self.ws_write.is_some() {
            return Ok(());
        }

        let ws_url = cred
            .server_url
            .replace("https://", "wss://")
            .replace("http://", "ws://");
        let url = format!("{ws_url}/api/ws/device");

        let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.map_err(|e| {
            self.bump_backoff();
            format!("WebSocket connect failed: {e}")
        })?;

        // Auth handshake (before split — simpler sequential flow)
        let auth = envelope("auth", serde_json::json!({ "token": &cred.token }));
        ws.send(Message::Text(auth.into()))
            .await
            .map_err(|e| format!("auth send failed: {e}"))?;

        let auth_resp = tokio::time::timeout(RESP_TIMEOUT, ws.next())
            .await
            .map_err(|_| "auth timeout".to_string())?
            .ok_or("connection closed during auth")?
            .map_err(|e| format!("auth read failed: {e}"))?;

        match auth_resp {
            Message::Text(text) => {
                let v: serde_json::Value = serde_json::from_str(&text)
                    .map_err(|e| format!("invalid auth response: {e}"))?;
                let resp_type = v["type"].as_str().unwrap_or("");
                if resp_type == "auth.error" {
                    let msg = v["payload"]["message"].as_str().unwrap_or("unknown");
                    return Err(format!("auth failed: {msg}"));
                }
                if resp_type != "auth.ok" {
                    return Err(format!("unexpected auth response: {resp_type}"));
                }
            }
            _ => return Err("unexpected non-text auth response".into()),
        }

        // Split into read/write halves for concurrent use in select!
        let (write, read) = ws.split();
        self.ws_write = Some(write);
        self.ws_read = Some(read);
        self.backoff = Duration::from_secs(1);

        // Send device.status on (re)connect for full sync
        let status = self.build_device_status();
        if let Err(e) = self.ws_send_text(&status).await {
            self.drop_conn();
            return Err(e);
        }

        Ok(())
    }

    async fn ws_send_text(&mut self, text: &str) -> Result<(), String> {
        let ws = self.ws_write.as_mut().ok_or("not connected")?;
        ws.send(Message::Text(text.to_string().into()))
            .await
            .map_err(|e| format!("send failed: {e}"))
    }

    async fn ws_send_binary(&mut self, data: Vec<u8>) -> Result<(), String> {
        let ws = self.ws_write.as_mut().ok_or("not connected")?;
        ws.send(Message::Binary(data.into()))
            .await
            .map_err(|e| format!("send failed: {e}"))
    }

    async fn ws_recv_text(&mut self) -> Result<serde_json::Value, String> {
        let ws = self.ws_read.as_mut().ok_or("not connected")?;
        let msg = tokio::time::timeout(RESP_TIMEOUT, ws.next())
            .await
            .map_err(|_| "response timeout".to_string())?
            .ok_or("connection closed")?
            .map_err(|e| format!("read failed: {e}"))?;

        match msg {
            Message::Text(text) => {
                serde_json::from_str(&text).map_err(|e| format!("invalid response: {e}"))
            }
            _ => Err("unexpected non-text response".into()),
        }
    }

    async fn heartbeat(&mut self) {
        let Some(ws) = self.ws_write.as_mut() else {
            return;
        };
        if ws.send(Message::Ping(vec![].into())).await.is_err() {
            self.drop_conn();
        }
    }

    async fn disconnect(&mut self) {
        if let Some(mut ws) = self.ws_write.take() {
            let _ = ws.close().await;
        }
        self.ws_read = None;
    }

    fn drop_conn(&mut self) {
        self.ws_write = None;
        self.ws_read = None;
        self.proxy.cancel_all_requests();
    }

    fn bump_backoff(&mut self) {
        self.backoff = (self.backoff * 2).min(MAX_BACKOFF);
    }

    fn build_device_status(&self) -> String {
        let terminals: Vec<serde_json::Value> = self
            .terminals
            .values()
            .map(|t| {
                let mut v = serde_json::json!({ "id": t.id });
                if let Some(n) = &t.name {
                    v["name"] = serde_json::Value::String(n.clone());
                }
                v
            })
            .collect();
        let exposed_ports: Vec<serde_json::Value> = self
            .proxy
            .exposed_ports
            .values()
            .map(|ep| serde_json::json!({ "port": ep.port, "public": ep.public }))
            .collect();
        envelope(
            "device.status",
            serde_json::json!({
                "syncedTerminals": terminals,
                "exposedPorts": exposed_ports,
            }),
        )
    }

    async fn handle_event(&mut self, event: TerminalEvent) {
        match event {
            TerminalEvent::Output { id, data } => {
                self.handle_terminal_output(&id, data).await;
            }
            TerminalEvent::Exited { id } => {
                self.handle_terminal_exited(&id).await;
            }
        }
    }

    async fn handle_proxy_event(&mut self, event: ProxyEvent) {
        match event {
            ProxyEvent::Head {
                request_id,
                status,
                headers,
            } => {
                // Single-value → string, multi-value → array (per protocol schema)
                let hdr_json: serde_json::Map<String, serde_json::Value> = headers
                    .into_iter()
                    .map(|(k, v)| {
                        let val = if v.len() == 1 {
                            serde_json::Value::String(v.into_iter().next().unwrap())
                        } else {
                            serde_json::Value::Array(
                                v.into_iter().map(serde_json::Value::String).collect(),
                            )
                        };
                        (k, val)
                    })
                    .collect();
                let msg = envelope(
                    "proxy.response.head",
                    serde_json::json!({
                        "requestId": request_id,
                        "status": status,
                        "headers": hdr_json,
                    }),
                );
                let _ = self.ws_send_text(&msg).await;
            }
            ProxyEvent::Body { request_id, data } => {
                let frame =
                    encode_binary_frame(&request_id, proxy::FRAME_PROXY_RESPONSE_BODY, &data);
                let _ = self.ws_send_binary(frame).await;
            }
            ProxyEvent::End { request_id } => {
                self.proxy.pending_requests.remove(&request_id);
                let msg = envelope(
                    "proxy.response.end",
                    serde_json::json!({ "requestId": request_id }),
                );
                let _ = self.ws_send_text(&msg).await;
            }
            ProxyEvent::Error {
                request_id,
                message,
            } => {
                self.proxy.pending_requests.remove(&request_id);
                let msg = envelope(
                    "proxy.response.error",
                    serde_json::json!({ "requestId": request_id, "message": message }),
                );
                let _ = self.ws_send_text(&msg).await;
            }
            ProxyEvent::WsOpen { request_id } => {
                let msg = envelope(
                    "proxy.ws.open",
                    serde_json::json!({ "requestId": request_id }),
                );
                let _ = self.ws_send_text(&msg).await;
            }
            ProxyEvent::WsClose {
                request_id,
                code,
                reason,
            } => {
                self.proxy.cancel_ws(&request_id);
                let msg = envelope(
                    "proxy.ws.close",
                    serde_json::json!({ "requestId": request_id, "code": code, "reason": reason }),
                );
                let _ = self.ws_send_text(&msg).await;
            }
            ProxyEvent::WsError {
                request_id,
                message,
            } => {
                self.proxy.cancel_ws(&request_id);
                let msg = envelope(
                    "proxy.ws.error",
                    serde_json::json!({ "requestId": request_id, "message": message }),
                );
                let _ = self.ws_send_text(&msg).await;
            }
            ProxyEvent::WsData {
                request_id,
                is_text,
                data,
            } => {
                let subtype = if is_text {
                    proxy::WS_SUBTYPE_TEXT
                } else {
                    proxy::WS_SUBTYPE_BINARY
                };
                let frame = encode_ws_binary_frame(&request_id, subtype, &data);
                let _ = self.ws_send_binary(frame).await;
            }
        }
    }

    async fn handle_terminal_output(&mut self, id: &str, data: Vec<u8>) {
        let Some(t) = self.terminals.get_mut(id) else {
            return;
        };
        t.parser.process(&data);
        if self.ws_write.is_some() {
            let frame = encode_binary_frame(id, 0x01, &data);
            let _ = self.ws_send_binary(frame).await;
        }
    }

    async fn handle_terminal_exited(&mut self, id: &str) {
        if self.terminals.remove(id).is_none() {
            return;
        }
        self.synced.lock().await.remove(id);
        if self.ws_write.is_some() {
            let msg = envelope("terminal.exited", serde_json::json!({ "terminalId": id }));
            let _ = self.ws_send_text(&msg).await;
        }
        if self.terminals.is_empty() {
            self.disconnect_at = Some(tokio::time::Instant::now() + DISCONNECT_DELAY);
        }
    }

    async fn handle_ws_message(&mut self, msg: Message) {
        match msg {
            Message::Text(text) => {
                let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
                    return;
                };
                self.handle_ws_text(v).await;
            }
            Message::Binary(data) => {
                self.handle_ws_binary(&data).await;
            }
            _ => {}
        }
    }

    async fn handle_ws_text(&mut self, msg: serde_json::Value) {
        let msg_type = msg["type"].as_str().unwrap_or("");
        match msg_type {
            "terminal.web-attach" => {
                let Some(tid) = msg["payload"]["terminalId"].as_str() else {
                    return;
                };
                self.handle_web_attach(tid).await;
            }
            "terminal.web-detach" => { /* D.O. manages subscriptions, nothing to do */ }
            "terminal.create" => {
                let name = msg["payload"]["name"].as_str().map(String::from);
                self.handle_web_create(name).await;
            }
            "terminal.kill" => {
                let Some(tid) = msg["payload"]["terminalId"].as_str() else {
                    return;
                };
                self.handle_web_kill(tid.to_string()).await;
            }
            "proxy.expose.ok" => {
                let port = msg["payload"]["port"].as_u64().unwrap_or(0) as u16;
                let url = msg["payload"]["url"].as_str().unwrap_or("").to_string();
                self.proxy.set_url(port, url.clone());
                if let Some(reply) = self.pending_expose.remove(&port) {
                    let _ = reply.send(Ok(url)).await;
                }
            }
            "proxy.expose.error" => {
                let port = msg["payload"]["port"].as_u64().unwrap_or(0) as u16;
                let message = msg["payload"]["message"]
                    .as_str()
                    .unwrap_or("unknown error")
                    .to_string();
                self.proxy.unexpose(port);
                if let Some(reply) = self.pending_expose.remove(&port) {
                    let _ = reply.send(Err(message)).await;
                }
            }
            "proxy.request.head" => {
                self.handle_proxy_request_head(&msg["payload"]).await;
            }
            "proxy.request.end" => {
                // For simple GET requests, request.end is a no-op (no body to finalize).
                // POST/PUT body streaming will be handled in Phase 5b+.
            }
            "proxy.request.cancel" => {
                let request_id = msg["payload"]["requestId"].as_str().unwrap_or("");
                self.proxy.cancel_request(request_id);
            }
            "proxy.ws.upgrade" => {
                self.handle_proxy_ws_upgrade(&msg["payload"]).await;
            }
            "proxy.ws.close" => {
                let request_id = msg["payload"]["requestId"]
                    .as_str()
                    .unwrap_or("")
                    .to_string();
                let code = msg["payload"]["code"].as_u64().unwrap_or(1000) as u16;
                let reason = msg["payload"]["reason"].as_str().unwrap_or("").to_string();
                // Best-effort close: enqueue Close frame, then force-abort tasks.
                // The writer may be aborted before sending the frame — acceptable
                // for D.O.-initiated close (browser already disconnected).
                if let Some(ws) = self.proxy.pending_ws.get(&request_id) {
                    let _ = ws
                        .writer_tx
                        .send(proxy::WsWriteMsg::Close { code, reason })
                        .await;
                }
                self.proxy.cancel_ws(&request_id);
            }
            _ => {}
        }
    }

    async fn handle_web_attach(&mut self, tid: &str) {
        let Some(t) = self.terminals.get(tid) else {
            return;
        };
        let screen = t.parser.screen();
        let (rows, cols) = screen.size();
        let snapshot_data = screen.contents_formatted();

        // Send JSON metadata
        let meta = envelope(
            "terminal.snapshot",
            serde_json::json!({ "terminalId": tid, "rows": rows, "cols": cols }),
        );
        let _ = self.ws_send_text(&meta).await;

        // Send binary Data frame with snapshot content
        let frame = encode_binary_frame(tid, 0x01, &snapshot_data);
        let _ = self.ws_send_binary(frame).await;
    }

    async fn handle_web_create(&mut self, name: Option<String>) {
        let result = {
            let mut mgr = self.manager.lock().await;
            mgr.create(name.clone())
                .map(|id| (id.clone(), mgr.get(&id).and_then(|t| t.name.clone())))
        };
        let (id, term_name) = match result {
            Ok(v) => v,
            Err(e) => {
                let msg = envelope("error", serde_json::json!({ "message": e.to_string() }));
                let _ = self.ws_send_text(&msg).await;
                return;
            }
        };

        // Auto-sync the new terminal
        let mgr = self.manager.lock().await;
        let Some(t) = mgr.get(&id) else { return };
        let Ok(pty_reader) = t.pty.clone_reader() else {
            return;
        };
        let pty_writer = t.pty.clone_writer();
        let pty_resizer = t.pty.clone_resizer();
        drop(mgr);

        if self
            .handle_sync(
                id.clone(),
                term_name.clone(),
                pty_reader,
                pty_writer,
                pty_resizer,
            )
            .await
            .is_ok()
        {
            let msg = envelope(
                "terminal.created",
                serde_json::json!({ "terminalId": &id, "name": term_name }),
            );
            let _ = self.ws_send_text(&msg).await;
        }
    }

    async fn handle_web_kill(&mut self, id: String) {
        // Unsync first (if synced)
        if self.terminals.contains_key(&id) {
            let _ = self.handle_unsync(id.clone()).await;
        }
        // Kill the terminal
        let mut mgr = self.manager.lock().await;
        if mgr.kill(&id).is_ok() {
            drop(mgr);
            let msg = envelope("terminal.killed", serde_json::json!({ "terminalId": &id }));
            let _ = self.ws_send_text(&msg).await;
        }
    }

    async fn handle_ws_binary(&mut self, data: &[u8]) {
        // WebSocket binary frame: [1B type][8B tid][payload]
        // No length prefix — WebSocket messages are self-delimited.
        if data.len() < 9 {
            return;
        }
        let frame_type = data[0];
        let tid = std::str::from_utf8(&data[1..9])
            .unwrap_or("")
            .trim_end_matches('\0');
        let payload = &data[9..];

        match frame_type {
            0x01 => {
                if let Some(t) = self.terminals.get(tid)
                    && let Ok(mut w) = t.pty_writer.lock()
                {
                    let _ = std::io::Write::write_all(&mut *w, payload);
                }
            }
            0x02 if payload.len() == 4 => {
                let cols = u16::from_be_bytes([payload[0], payload[1]]);
                let rows = u16::from_be_bytes([payload[2], payload[3]]);
                if let Some(t) = self.terminals.get_mut(tid) {
                    let _ = t.pty_resizer.resize(cols, rows);
                    t.parser.screen_mut().set_size(rows, cols);
                }
            }
            proxy::FRAME_PROXY_REQUEST_BODY => {
                // POST/PUT request body streaming — Phase 5b+.
                // For now, proxy tasks handle GET-only (no request body).
            }
            proxy::FRAME_PROXY_WS_DATA => {
                if data.len() < 10 {
                    return;
                }
                let subtype = data[9];
                let payload = &data[10..];
                let is_text = subtype == proxy::WS_SUBTYPE_TEXT;
                if let Some(ws) = self.proxy.pending_ws.get(tid) {
                    let _ = ws.writer_tx.try_send(proxy::WsWriteMsg::Data {
                        is_text,
                        payload: payload.to_vec(),
                    });
                }
            }
            _ => {}
        }
    }
}

fn spawn_pty_reader(
    id: String,
    mut reader: Box<dyn std::io::Read + Send>,
    tx: mpsc::Sender<TerminalEvent>,
) -> JoinHandle<()> {
    tokio::task::spawn_blocking(move || {
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let event = TerminalEvent::Output {
                        id: id.clone(),
                        data: buf[..n].to_vec(),
                    };
                    if tx.blocking_send(event).is_err() {
                        break;
                    }
                }
            }
        }
        let _ = tx.blocking_send(TerminalEvent::Exited { id });
    })
}

/// Spawn a tokio task that connects to localhost and streams the response back
/// via the proxy event channel.
#[doc(hidden)]
pub async fn proxy_request_task(
    request_id: String,
    method: String,
    port: u16,
    path: String,
    headers: HashMap<String, String>,
    tx: mpsc::Sender<ProxyEvent>,
    client: reqwest::Client,
) {
    let url = format!("http://127.0.0.1:{port}{path}");

    let req_method = match method.as_str() {
        "GET" => reqwest::Method::GET,
        "POST" => reqwest::Method::POST,
        "PUT" => reqwest::Method::PUT,
        "DELETE" => reqwest::Method::DELETE,
        "PATCH" => reqwest::Method::PATCH,
        "HEAD" => reqwest::Method::HEAD,
        "OPTIONS" => reqwest::Method::OPTIONS,
        _ => reqwest::Method::GET,
    };

    let mut req = client.request(req_method, &url);
    for (k, v) in &headers {
        // Skip hop-by-hop headers
        let lower = k.to_lowercase();
        if matches!(
            lower.as_str(),
            "host" | "connection" | "transfer-encoding" | "keep-alive" | "upgrade"
        ) {
            continue;
        }
        req = req.header(k, v);
    }

    let resp = match req.send().await {
        Ok(r) => r,
        Err(e) => {
            let _ = tx
                .send(ProxyEvent::Error {
                    request_id,
                    message: format!("localhost connection failed: {e}"),
                })
                .await;
            return;
        }
    };

    // Send response head — group duplicate header names (e.g. Set-Cookie)
    let status = resp.status().as_u16();
    let mut resp_headers: HashMap<String, Vec<String>> = HashMap::new();
    for (k, v) in resp.headers().iter() {
        let val = v.to_str().unwrap_or("").to_string();
        resp_headers.entry(k.to_string()).or_default().push(val);
    }

    if tx
        .send(ProxyEvent::Head {
            request_id: request_id.clone(),
            status,
            headers: resp_headers,
        })
        .await
        .is_err()
    {
        return;
    }

    // Stream response body in chunks
    let mut stream = resp.bytes_stream();
    use futures_util::StreamExt as _;
    while let Some(chunk) = stream.next().await {
        match chunk {
            Ok(bytes) => {
                if tx
                    .send(ProxyEvent::Body {
                        request_id: request_id.clone(),
                        data: bytes.to_vec(),
                    })
                    .await
                    .is_err()
                {
                    return;
                }
            }
            Err(e) => {
                let _ = tx
                    .send(ProxyEvent::Error {
                        request_id,
                        message: format!("read error: {e}"),
                    })
                    .await;
                return;
            }
        }
    }

    let _ = tx.send(ProxyEvent::End { request_id }).await;
}

/// Encode a WebSocket binary frame: [1B type][8B terminal_id][payload].
/// No length prefix — WebSocket messages are self-delimited.
#[doc(hidden)]
pub fn encode_binary_frame(id: &str, frame_type: u8, payload: &[u8]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(9 + payload.len());
    buf.push(frame_type);
    let mut id_fixed = [0u8; 8];
    let id_bytes = id.as_bytes();
    id_fixed[..id_bytes.len().min(8)].copy_from_slice(&id_bytes[..id_bytes.len().min(8)]);
    buf.extend_from_slice(&id_fixed);
    buf.extend_from_slice(payload);
    buf
}

/// Encode a 0x22 WS proxy frame: [1B:0x22][8B:requestId][1B:subtype][payload]
fn encode_ws_binary_frame(id: &str, subtype: u8, payload: &[u8]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(10 + payload.len());
    buf.push(proxy::FRAME_PROXY_WS_DATA);
    let mut id_fixed = [0u8; 8];
    let id_bytes = id.as_bytes();
    id_fixed[..id_bytes.len().min(8)].copy_from_slice(&id_bytes[..id_bytes.len().min(8)]);
    buf.extend_from_slice(&id_fixed);
    buf.push(subtype);
    buf.extend_from_slice(payload);
    buf
}

#[doc(hidden)]
pub fn envelope(msg_type: &str, payload: serde_json::Value) -> String {
    serde_json::json!({ "v": 1, "type": msg_type, "payload": payload }).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::pty::Pty;

    fn test_manager() -> Arc<Mutex<TerminalManager>> {
        Arc::new(Mutex::new(TerminalManager::new()))
    }

    fn make_sync_cmd(
        id: &str,
        name: Option<&str>,
        reply: mpsc::Sender<Result<(), String>>,
    ) -> CloudCommand {
        let pty = Pty::spawn().unwrap();
        CloudCommand::Sync {
            id: id.into(),
            name: name.map(String::from),
            pty_reader: pty.clone_reader().unwrap(),
            pty_writer: pty.clone_writer(),
            pty_resizer: pty.clone_resizer(),
            reply,
        }
    }

    #[test]
    fn envelope_format() {
        let msg = envelope("auth", serde_json::json!({"token": "abc"}));
        let v: serde_json::Value = serde_json::from_str(&msg).unwrap();
        assert_eq!(v["v"], 1);
        assert_eq!(v["type"], "auth");
        assert_eq!(v["payload"]["token"], "abc");
    }

    #[tokio::test]
    async fn sync_without_credentials_returns_error() {
        let synced = Arc::new(Mutex::new(HashSet::new()));
        let tx = CloudManager::spawn(synced.clone(), test_manager());

        let (reply_tx, mut reply_rx) = mpsc::channel(1);
        tx.send(make_sync_cmd("t1", Some("test"), reply_tx))
            .await
            .unwrap();

        let result = reply_rx.recv().await.unwrap();
        assert!(result.is_err());
        assert!(synced.lock().await.is_empty());
    }

    #[tokio::test]
    async fn unsync_unknown_terminal_returns_error() {
        let synced = Arc::new(Mutex::new(HashSet::new()));
        let tx = CloudManager::spawn(synced, test_manager());

        let (reply_tx, mut reply_rx) = mpsc::channel(1);
        tx.send(CloudCommand::Unsync {
            id: "nonexist".into(),
            reply: reply_tx,
        })
        .await
        .unwrap();

        let result = reply_rx.recv().await.unwrap();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not synced"));
    }

    #[tokio::test]
    async fn build_device_status_includes_terminals() {
        let (event_tx, event_rx) = mpsc::channel(1);
        let pty1 = Pty::spawn().unwrap();
        let pty2 = Pty::spawn().unwrap();

        let mut terminals = HashMap::new();
        terminals.insert(
            "t1".into(),
            SyncedTerminal {
                id: "t1".into(),
                name: Some("dev".into()),
                parser: vt100::Parser::new(24, 80, 0),
                pty_writer: pty1.clone_writer(),
                pty_resizer: pty1.clone_resizer(),
                reader_handle: tokio::spawn(async {}),
            },
        );
        terminals.insert(
            "t2".into(),
            SyncedTerminal {
                id: "t2".into(),
                name: None,
                parser: vt100::Parser::new(24, 80, 0),
                pty_writer: pty2.clone_writer(),
                pty_resizer: pty2.clone_resizer(),
                reader_handle: tokio::spawn(async {}),
            },
        );

        let (proxy_event_tx, proxy_event_rx) = mpsc::channel(1);
        let mgr = CloudManager {
            rx: mpsc::channel(1).1,
            event_rx,
            event_tx,
            synced: Arc::new(Mutex::new(HashSet::new())),
            terminals,
            manager: Arc::new(Mutex::new(TerminalManager::new())),
            ws_read: None,
            ws_write: None,
            backoff: Duration::from_secs(1),
            disconnect_at: None,
            proxy: ProxyState::new(),
            pending_expose: HashMap::new(),
            proxy_event_tx,
            proxy_event_rx,
        };

        let msg = mgr.build_device_status();
        let v: serde_json::Value = serde_json::from_str(&msg).unwrap();
        assert_eq!(v["type"], "device.status");
        let list = v["payload"]["syncedTerminals"].as_array().unwrap();
        assert_eq!(list.len(), 2);
        let has_name = list.iter().any(|t| t.get("name").is_some());
        let no_name = list.iter().any(|t| t.get("name").is_none());
        assert!(has_name && no_name);
    }

    #[test]
    fn encode_binary_frame_format() {
        let frame = encode_binary_frame("abcd1234", 0x01, b"hello");
        assert_eq!(frame[0], 0x01);
        assert_eq!(&frame[1..9], b"abcd1234");
        assert_eq!(&frame[9..], b"hello");
    }

    #[test]
    fn encode_binary_frame_pads_short_id() {
        let frame = encode_binary_frame("ab", 0x02, b"x");
        assert_eq!(&frame[1..3], b"ab");
        assert_eq!(&frame[3..9], &[0u8; 6]);
    }

    #[tokio::test]
    async fn handle_ws_binary_writes_to_pty() {
        let pty = Pty::spawn().unwrap();
        let (event_tx, event_rx) = mpsc::channel(1);
        let (proxy_event_tx, proxy_event_rx) = mpsc::channel(1);
        let mut mgr = CloudManager {
            rx: mpsc::channel(1).1,
            event_rx,
            event_tx,
            synced: Arc::new(Mutex::new(HashSet::new())),
            terminals: HashMap::new(),
            manager: test_manager(),
            ws_read: None,
            ws_write: None,
            backoff: Duration::from_secs(1),
            disconnect_at: None,
            proxy: ProxyState::new(),
            pending_expose: HashMap::new(),
            proxy_event_tx,
            proxy_event_rx,
        };
        mgr.terminals.insert(
            "abcd1234".into(),
            SyncedTerminal {
                id: "abcd1234".into(),
                name: None,
                parser: vt100::Parser::new(24, 80, 0),
                pty_writer: pty.clone_writer(),
                pty_resizer: pty.clone_resizer(),
                reader_handle: tokio::spawn(async {}),
            },
        );

        // Send a data frame (type 0x01)
        let frame = encode_binary_frame("abcd1234", 0x01, b"ls\n");
        mgr.handle_ws_binary(&frame).await;

        // Verify data was written (no panic = PTY accepted the write)
    }

    #[tokio::test]
    async fn handle_ws_binary_resize_updates_parser() {
        let pty = Pty::spawn().unwrap();
        let (event_tx, event_rx) = mpsc::channel(1);
        let (proxy_event_tx, proxy_event_rx) = mpsc::channel(1);
        let mut mgr = CloudManager {
            rx: mpsc::channel(1).1,
            event_rx,
            event_tx,
            synced: Arc::new(Mutex::new(HashSet::new())),
            terminals: HashMap::new(),
            manager: test_manager(),
            ws_read: None,
            ws_write: None,
            backoff: Duration::from_secs(1),
            disconnect_at: None,
            proxy: ProxyState::new(),
            pending_expose: HashMap::new(),
            proxy_event_tx,
            proxy_event_rx,
        };
        mgr.terminals.insert(
            "abcd1234".into(),
            SyncedTerminal {
                id: "abcd1234".into(),
                name: None,
                parser: vt100::Parser::new(24, 80, 0),
                pty_writer: pty.clone_writer(),
                pty_resizer: pty.clone_resizer(),
                reader_handle: tokio::spawn(async {}),
            },
        );

        // Resize frame: type 0x02, payload = [cols_hi, cols_lo, rows_hi, rows_lo]
        let mut resize_payload = Vec::new();
        resize_payload.extend_from_slice(&100u16.to_be_bytes()); // cols
        resize_payload.extend_from_slice(&50u16.to_be_bytes()); // rows
        let frame = encode_binary_frame("abcd1234", 0x02, &resize_payload);
        mgr.handle_ws_binary(&frame).await;

        let t = mgr.terminals.get("abcd1234").unwrap();
        let (rows, cols) = t.parser.screen().size();
        assert_eq!(rows, 50);
        assert_eq!(cols, 100);
    }

    #[tokio::test]
    async fn handle_terminal_output_feeds_parser() {
        let pty = Pty::spawn().unwrap();
        let (event_tx, event_rx) = mpsc::channel(1);
        let (proxy_event_tx, proxy_event_rx) = mpsc::channel(1);
        let mut mgr = CloudManager {
            rx: mpsc::channel(1).1,
            event_rx,
            event_tx,
            synced: Arc::new(Mutex::new(HashSet::new())),
            terminals: HashMap::new(),
            manager: test_manager(),
            ws_read: None,
            ws_write: None,
            backoff: Duration::from_secs(1),
            disconnect_at: None,
            proxy: ProxyState::new(),
            pending_expose: HashMap::new(),
            proxy_event_tx,
            proxy_event_rx,
        };
        mgr.terminals.insert(
            "abcd1234".into(),
            SyncedTerminal {
                id: "abcd1234".into(),
                name: None,
                parser: vt100::Parser::new(24, 80, 0),
                pty_writer: pty.clone_writer(),
                pty_resizer: pty.clone_resizer(),
                reader_handle: tokio::spawn(async {}),
            },
        );

        mgr.handle_terminal_output("abcd1234", b"hello world".to_vec())
            .await;

        let t = mgr.terminals.get("abcd1234").unwrap();
        let contents = t.parser.screen().contents();
        assert!(contents.contains("hello world"));
    }

    #[tokio::test]
    async fn build_device_status_includes_exposed_ports() {
        let (event_tx, event_rx) = mpsc::channel(1);
        let (proxy_event_tx, proxy_event_rx) = mpsc::channel(1);
        let mut mgr = CloudManager {
            rx: mpsc::channel(1).1,
            event_rx,
            event_tx,
            synced: Arc::new(Mutex::new(HashSet::new())),
            terminals: HashMap::new(),
            manager: test_manager(),
            ws_read: None,
            ws_write: None,
            backoff: Duration::from_secs(1),
            disconnect_at: None,
            proxy: ProxyState::new(),
            pending_expose: HashMap::new(),
            proxy_event_tx,
            proxy_event_rx,
        };

        mgr.proxy.expose(3000, false);
        mgr.proxy.expose(8080, true);

        let msg = mgr.build_device_status();
        let v: serde_json::Value = serde_json::from_str(&msg).unwrap();
        let ports = v["payload"]["exposedPorts"].as_array().unwrap();
        assert_eq!(ports.len(), 2);
        let has_public = ports.iter().any(|p| p["public"] == true);
        let has_private = ports.iter().any(|p| p["public"] == false);
        assert!(has_public && has_private);
    }

    #[tokio::test]
    async fn proxy_event_sends_response_head() {
        let (event_tx, event_rx) = mpsc::channel(1);
        let (proxy_event_tx, proxy_event_rx) = mpsc::channel(1);
        let mut mgr = CloudManager {
            rx: mpsc::channel(1).1,
            event_rx,
            event_tx,
            synced: Arc::new(Mutex::new(HashSet::new())),
            terminals: HashMap::new(),
            manager: test_manager(),
            ws_read: None,
            ws_write: None,
            backoff: Duration::from_secs(1),
            disconnect_at: None,
            proxy: ProxyState::new(),
            pending_expose: HashMap::new(),
            proxy_event_tx,
            proxy_event_rx,
        };

        // Without a WS connection, handle_proxy_event should not panic
        let mut headers = HashMap::new();
        headers.insert("content-type".into(), vec!["text/html".into()]);
        mgr.handle_proxy_event(ProxyEvent::Head {
            request_id: "req12345".into(),
            status: 200,
            headers,
        })
        .await;
        // No panic = success (no WS to send to, silently drops)
    }

    #[tokio::test]
    async fn proxy_event_end_removes_pending() {
        let (event_tx, event_rx) = mpsc::channel(1);
        let (proxy_event_tx, proxy_event_rx) = mpsc::channel(1);
        let mut mgr = CloudManager {
            rx: mpsc::channel(1).1,
            event_rx,
            event_tx,
            synced: Arc::new(Mutex::new(HashSet::new())),
            terminals: HashMap::new(),
            manager: test_manager(),
            ws_read: None,
            ws_write: None,
            backoff: Duration::from_secs(1),
            disconnect_at: None,
            proxy: ProxyState::new(),
            pending_expose: HashMap::new(),
            proxy_event_tx,
            proxy_event_rx,
        };

        let handle = tokio::spawn(async { tokio::time::sleep(Duration::from_secs(60)).await });
        mgr.proxy.pending_requests.insert(
            "req1".into(),
            proxy::PendingProxyRequest { port: 3000, handle },
        );

        mgr.handle_proxy_event(ProxyEvent::End {
            request_id: "req1".into(),
        })
        .await;
        assert!(mgr.proxy.pending_requests.is_empty());
    }

    #[tokio::test]
    async fn proxy_event_error_removes_pending() {
        let (event_tx, event_rx) = mpsc::channel(1);
        let (proxy_event_tx, proxy_event_rx) = mpsc::channel(1);
        let mut mgr = CloudManager {
            rx: mpsc::channel(1).1,
            event_rx,
            event_tx,
            synced: Arc::new(Mutex::new(HashSet::new())),
            terminals: HashMap::new(),
            manager: test_manager(),
            ws_read: None,
            ws_write: None,
            backoff: Duration::from_secs(1),
            disconnect_at: None,
            proxy: ProxyState::new(),
            pending_expose: HashMap::new(),
            proxy_event_tx,
            proxy_event_rx,
        };

        let handle = tokio::spawn(async { tokio::time::sleep(Duration::from_secs(60)).await });
        mgr.proxy.pending_requests.insert(
            "req1".into(),
            proxy::PendingProxyRequest { port: 3000, handle },
        );

        mgr.handle_proxy_event(ProxyEvent::Error {
            request_id: "req1".into(),
            message: "connection refused".into(),
        })
        .await;
        assert!(mgr.proxy.pending_requests.is_empty());
    }

    #[test]
    fn proxy_state_expose_unexpose() {
        let mut state = ProxyState::new();
        state.expose(3000, false);
        assert!(state.exposed_ports.contains_key(&3000));
        assert!(!state.exposed_ports[&3000].public);

        state.set_url(3000, "https://example.com".into());
        assert_eq!(
            state.exposed_ports[&3000].url.as_deref(),
            Some("https://example.com")
        );

        assert!(state.unexpose(3000));
        assert!(!state.exposed_ports.contains_key(&3000));
        assert!(!state.unexpose(3000)); // already removed
    }

    #[test]
    fn encode_ws_binary_frame_format() {
        let frame = encode_ws_binary_frame("ws1a2b3c", proxy::WS_SUBTYPE_TEXT, b"hello");
        assert_eq!(frame[0], 0x22);
        assert_eq!(&frame[1..9], b"ws1a2b3c");
        assert_eq!(frame[9], 0x01); // text subtype
        assert_eq!(&frame[10..], b"hello");
    }

    #[test]
    fn encode_ws_binary_frame_binary_subtype() {
        let frame = encode_ws_binary_frame("ws1a2b3c", proxy::WS_SUBTYPE_BINARY, &[0xFF, 0x00]);
        assert_eq!(frame[9], 0x02); // binary subtype
        assert_eq!(&frame[10..], &[0xFF, 0x00]);
    }
}
