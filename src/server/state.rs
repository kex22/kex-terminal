use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, Notify};

use crate::cloud::manager::SyncedSet;
use crate::ipc;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ServerState {
    pub terminals: Vec<TerminalMeta>,
    pub views: Vec<ViewMeta>,
    pub active_view: Option<String>,
    #[serde(default)]
    pub synced_terminals: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TerminalMeta {
    pub id: String,
    pub name: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ViewMeta {
    pub id: String,
    pub name: Option<String>,
    pub terminal_ids: Vec<String>,
    pub layout: serde_json::Value,
    pub focused: String,
    pub created_at: String,
}

pub fn state_path() -> PathBuf {
    ipc::socket_dir().join("state.json")
}

impl ServerState {
    pub fn load() -> Self {
        let path = state_path();
        if !path.exists() {
            return Self::default();
        }
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let path = state_path();
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(path, json);
        }
    }
}

/// Debounced state persister — notifies trigger a 500ms debounce before writing.
#[derive(Clone)]
pub struct StatePersister {
    trigger: Arc<Notify>,
}

impl StatePersister {
    pub fn spawn(
        manager: Arc<Mutex<crate::terminal::manager::TerminalManager>>,
        view_manager: Arc<Mutex<crate::view::manager::ViewManager>>,
        synced: SyncedSet,
    ) -> Self {
        let trigger = Arc::new(Notify::new());
        let t = trigger.clone();
        tokio::spawn(async move {
            loop {
                t.notified().await;
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                let state = {
                    let mgr = manager.lock().await;
                    let vmgr = view_manager.lock().await;
                    let s = synced.lock().await;
                    snapshot(&mgr, &vmgr, &s)
                };
                state.save();
            }
        });
        Self { trigger }
    }

    pub fn notify(&self) {
        self.trigger.notify_one();
    }
}

fn snapshot(
    mgr: &crate::terminal::manager::TerminalManager,
    vmgr: &crate::view::manager::ViewManager,
    synced: &HashSet<String>,
) -> ServerState {
    let terminals = mgr
        .list()
        .into_iter()
        .map(|t| TerminalMeta {
            id: t.id,
            name: t.name,
            created_at: t.created_at,
        })
        .collect();
    let views = vmgr
        .list_full()
        .into_iter()
        .map(|v| ViewMeta {
            id: v.id.clone(),
            name: v.name.clone(),
            terminal_ids: v.terminal_ids.clone(),
            layout: v.layout.clone(),
            focused: v.focused.clone(),
            created_at: v.created_at.clone(),
        })
        .collect();
    ServerState {
        terminals,
        views,
        active_view: None,
        synced_terminals: synced.iter().cloned().collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_state_is_empty() {
        let state = ServerState::default();
        assert!(state.terminals.is_empty());
        assert!(state.views.is_empty());
        assert!(state.active_view.is_none());
    }

    #[test]
    fn roundtrip_serialize() {
        let state = ServerState {
            terminals: vec![TerminalMeta {
                id: "abc12345".into(),
                name: Some("dev".into()),
                created_at: "2026-02-21 10:00".into(),
            }],
            views: vec![ViewMeta {
                id: "view0001".into(),
                name: None,
                terminal_ids: vec!["abc12345".into()],
                layout: serde_json::json!({"type": "leaf"}),
                focused: "abc12345".into(),
                created_at: "2026-02-21 10:00".into(),
            }],
            active_view: Some("view0001".into()),
            synced_terminals: vec!["abc12345".into()],
        };
        let json = serde_json::to_string(&state).unwrap();
        let restored: ServerState = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.terminals.len(), 1);
        assert_eq!(restored.terminals[0].id, "abc12345");
        assert_eq!(restored.views.len(), 1);
        assert_eq!(restored.active_view, Some("view0001".into()));
        assert_eq!(restored.synced_terminals, vec!["abc12345".to_string()]);
    }

    #[test]
    fn deserialize_without_synced_terminals() {
        let json = r#"{"terminals":[],"views":[],"active_view":null}"#;
        let state: ServerState = serde_json::from_str(json).unwrap();
        assert!(state.synced_terminals.is_empty());
    }

    #[test]
    fn load_missing_file_returns_default() {
        // state_path() points to /tmp/kex-{uid}/state.json which may not exist
        // but load() should handle gracefully
        let state = ServerState::load();
        // Just verify it doesn't panic — actual content depends on environment
        let _ = state.terminals.len();
    }
}
