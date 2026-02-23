use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::sync::{Mutex, mpsc};
use tokio_tungstenite::tungstenite::Message;

use crate::credential;

/// Commands sent to CloudManager from IPC handlers.
#[derive(Debug)]
pub enum CloudCommand {
    Sync {
        id: String,
        name: Option<String>,
        reply: mpsc::Sender<Result<(), String>>,
    },
    Unsync {
        id: String,
        reply: mpsc::Sender<Result<(), String>>,
    },
}

/// Shared synced terminal set — read by StatePersister, written by CloudManager.
pub type SyncedSet = Arc<Mutex<HashSet<String>>>;

type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

#[derive(Clone)]
struct SyncedTerminal {
    id: String,
    name: Option<String>,
}

pub struct CloudManager {
    rx: mpsc::Receiver<CloudCommand>,
    synced: SyncedSet,
    terminals: HashMap<String, SyncedTerminal>,
    conn: Option<WsStream>,
    backoff: Duration,
    disconnect_at: Option<tokio::time::Instant>,
}

const HEARTBEAT_SECS: u64 = 30;
const MAX_BACKOFF: Duration = Duration::from_secs(30);
const RESP_TIMEOUT: Duration = Duration::from_secs(10);
const DISCONNECT_DELAY: Duration = Duration::from_secs(30);

impl CloudManager {
    pub fn spawn(synced: SyncedSet) -> mpsc::Sender<CloudCommand> {
        let (tx, rx) = mpsc::channel(32);
        let mgr = CloudManager {
            rx,
            synced,
            terminals: HashMap::new(),
            conn: None,
            backoff: Duration::from_secs(1),
            disconnect_at: None,
        };
        tokio::spawn(mgr.run());
        tx
    }

    async fn run(mut self) {
        let mut heartbeat = tokio::time::interval(Duration::from_secs(HEARTBEAT_SECS));
        heartbeat.tick().await; // skip immediate tick

        loop {
            let has_conn = self.conn.is_some();
            let disconnect_at = self.disconnect_at;
            tokio::select! {
                biased;
                cmd = self.rx.recv() => {
                    match cmd {
                        Some(cmd) => self.handle_cmd(cmd).await,
                        None => break,
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
            CloudCommand::Sync { id, name, reply } => {
                let result = self.handle_sync(id, name).await;
                let _ = reply.send(result).await;
            }
            CloudCommand::Unsync { id, reply } => {
                let result = self.handle_unsync(id).await;
                let _ = reply.send(result).await;
            }
        }
    }

    async fn handle_sync(&mut self, id: String, name: Option<String>) -> Result<(), String> {
        let cred = credential::load().map_err(|e| e.to_string())?;

        // Cancel pending delayed disconnect
        self.disconnect_at = None;

        // Insert BEFORE sending so device.status includes this terminal
        self.terminals.insert(
            id.clone(),
            SyncedTerminal {
                id: id.clone(),
                name: name.clone(),
            },
        );
        self.synced.lock().await.insert(id.clone());

        let mut payload = serde_json::json!({ "terminalId": &id });
        if let Some(n) = &name {
            payload["name"] = serde_json::Value::String(n.clone());
        }

        if let Err(e) = self.send_and_confirm(&cred, "terminal.sync", payload).await {
            // Rollback on failure
            self.terminals.remove(&id);
            self.synced.lock().await.remove(&id);
            return Err(e);
        }

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

        self.terminals.remove(&id);
        self.synced.lock().await.remove(&id);

        // Schedule delayed disconnect if no more synced terminals
        if self.terminals.is_empty() {
            self.disconnect_at = Some(tokio::time::Instant::now() + DISCONNECT_DELAY);
        }

        Ok(())
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
        if let Err(e) = self.ws_send(&msg).await {
            self.conn = None;
            return Err(e);
        }

        // Wait for response (terminal.sync.ok, or error)
        match self.ws_recv().await {
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
                self.conn = None;
                Err(e)
            }
        }
    }

    async fn ensure_connected(&mut self, cred: &credential::Credential) -> Result<(), String> {
        if self.conn.is_some() {
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

        // Auth handshake
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

        self.conn = Some(ws);
        self.backoff = Duration::from_secs(1);

        // Send device.status on (re)connect for full sync
        let status = self.build_device_status();
        if let Err(e) = self.ws_send(&status).await {
            self.conn = None;
            return Err(e);
        }

        Ok(())
    }

    async fn ws_send(&mut self, text: &str) -> Result<(), String> {
        let ws = self.conn.as_mut().ok_or("not connected")?;
        ws.send(Message::Text(text.to_string().into()))
            .await
            .map_err(|e| format!("send failed: {e}"))
    }

    async fn ws_recv(&mut self) -> Result<serde_json::Value, String> {
        let ws = self.conn.as_mut().ok_or("not connected")?;
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
        let Some(ws) = self.conn.as_mut() else {
            return;
        };
        if ws.send(Message::Ping(vec![].into())).await.is_err() {
            self.conn = None;
        }
    }

    async fn disconnect(&mut self) {
        if let Some(mut ws) = self.conn.take() {
            let _ = ws.close(None).await;
        }
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
        envelope(
            "device.status",
            serde_json::json!({ "syncedTerminals": terminals }),
        )
    }
}

fn envelope(msg_type: &str, payload: serde_json::Value) -> String {
    serde_json::json!({ "v": 1, "type": msg_type, "payload": payload }).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let tx = CloudManager::spawn(synced.clone());

        let (reply_tx, mut reply_rx) = mpsc::channel(1);
        tx.send(CloudCommand::Sync {
            id: "t1".into(),
            name: Some("test".into()),
            reply: reply_tx,
        })
        .await
        .unwrap();

        let result = reply_rx.recv().await.unwrap();
        assert!(result.is_err());
        assert!(synced.lock().await.is_empty());
    }

    #[tokio::test]
    async fn unsync_unknown_terminal_returns_error() {
        let synced = Arc::new(Mutex::new(HashSet::new()));
        let tx = CloudManager::spawn(synced);

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

    #[test]
    fn build_device_status_includes_terminals() {
        // Verify device.status payload structure
        let mut terminals = HashMap::new();
        terminals.insert(
            "t1".into(),
            SyncedTerminal {
                id: "t1".into(),
                name: Some("dev".into()),
            },
        );
        terminals.insert(
            "t2".into(),
            SyncedTerminal {
                id: "t2".into(),
                name: None,
            },
        );

        let mgr = CloudManager {
            rx: mpsc::channel(1).1,
            synced: Arc::new(Mutex::new(HashSet::new())),
            terminals,
            conn: None,
            backoff: Duration::from_secs(1),
            disconnect_at: None,
        };

        let msg = mgr.build_device_status();
        let v: serde_json::Value = serde_json::from_str(&msg).unwrap();
        assert_eq!(v["type"], "device.status");
        let list = v["payload"]["syncedTerminals"].as_array().unwrap();
        assert_eq!(list.len(), 2);
        // One should have name, one should not
        let has_name = list.iter().any(|t| t.get("name").is_some());
        let no_name = list.iter().any(|t| t.get("name").is_none());
        assert!(has_name && no_name);
    }
}
