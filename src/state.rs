//! What the service exposes to front ends (the TUI) over D-Bus, as JSON.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Snapshot {
    pub device_name: String,
    pub download_dir: String,
    /// Seconds of visibility left; 0 = hidden.
    pub visible_secs: u64,
    pub visible_minutes: u64,
    pub hotspot_ready: bool,
    pub transfers: Vec<TransferView>,
    pub history: Vec<HistoryItem>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Phase {
    /// Waiting for Accept/Decline.
    Asking,
    Receiving,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferView {
    pub id: String,
    pub sender: String,
    pub pin: String,
    pub what: String,
    pub files: Vec<String>,
    pub total_bytes: u64,
    pub ack_bytes: u64,
    pub via_wifi: bool,
    pub phase: Phase,
    /// Seconds left to answer (Asking only).
    pub answer_secs: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ItemKind {
    Files,
    Text,
    Link,
    Wifi,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryItem {
    /// Unix seconds.
    pub when: u64,
    pub sender: String,
    pub kind: ItemKind,
    /// Saved paths (Files).
    pub files: Vec<String>,
    /// Text, link, Wi-Fi name, or failure reason.
    pub text: String,
    pub total_bytes: u64,
    pub via_wifi: bool,
}

pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// History file: $STATE_DIRECTORY (systemd) or ~/.local/state/fqshare.
pub fn history_path() -> Option<std::path::PathBuf> {
    let dir = std::env::var_os("STATE_DIRECTORY")
        .map(std::path::PathBuf::from)
        .or_else(|| directories::BaseDirs::new()?.state_dir().map(|d| d.join("fqshare")))?;
    Some(dir.join("history.json"))
}

pub const HISTORY_MAX: usize = 100;

pub fn load_history() -> Vec<HistoryItem> {
    history_path()
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub fn save_history(items: &[HistoryItem]) {
    let Some(path) = history_path() else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = path.with_extension("json.tmp");
    if let Ok(data) = serde_json::to_vec(items) {
        if std::fs::write(&tmp, data).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }
}
