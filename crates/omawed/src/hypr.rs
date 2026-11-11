//! Hyprland IPC: the event socket wakes us up, the command socket answers what changed.

use serde_json::Value;
use std::path::PathBuf;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::mpsc::Sender;

#[derive(Default, Debug, PartialEq)]
pub struct Snapshot {
    pub monitors: Vec<String>,
    /// Every monitor's visible workspace is hidden behind a fullscreen (or maximized) window.
    pub covered: bool,
    /// A Steam game window is open.
    pub game: bool,
}

fn socket(name: &str) -> Option<PathBuf> {
    let sig = std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE")?;
    Some(PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR")?).join("hypr").join(sig).join(name))
}

async fn query(cmd: &str) -> Option<Value> {
    let mut s = UnixStream::connect(socket(".socket.sock")?).await.ok()?;
    s.write_all(cmd.as_bytes()).await.ok()?;
    let mut buf = Vec::new();
    s.read_to_end(&mut buf).await.ok()?;
    serde_json::from_slice(&buf).ok()
}

pub fn snapshot_from(monitors: &Value, clients: &Value, maximized: bool) -> Snapshot {
    let min_mode = if maximized { 1 } else { 2 };
    let clients = clients.as_array().map(Vec::as_slice).unwrap_or_default();
    let covers = |ws: i64| {
        clients.iter().any(|c| c["workspace"]["id"].as_i64() == Some(ws) && c["fullscreen"].as_i64() >= Some(min_mode))
    };
    let mons = monitors.as_array().map(Vec::as_slice).unwrap_or_default();
    Snapshot {
        monitors: mons.iter().filter_map(|m| m["name"].as_str().map(String::from)).collect(),
        covered: !mons.is_empty() && mons.iter().all(|m| covers(m["activeWorkspace"]["id"].as_i64().unwrap_or(0))),
        game: clients.iter().any(|c| c["class"].as_str().is_some_and(|s| s.starts_with("steam_app_"))),
    }
}

pub async fn snapshot(maximized: bool) -> Option<Snapshot> {
    Some(snapshot_from(&query("j/monitors").await?, &query("j/clients").await?, maximized))
}

/// Pings `tx` whenever window, workspace or monitor layout may have changed.
/// The channel holds one item, so a burst of events costs one refresh.
pub async fn listen(tx: Sender<()>) {
    let Some(path) = socket(".socket2.sock") else { return };
    let Ok(stream) = UnixStream::connect(path).await else { return };
    let mut lines = BufReader::new(stream).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let name = line.split(">>").next().unwrap_or_default();
        if matches!(
            name,
            "fullscreen" | "workspacev2" | "focusedmonv2" | "openwindow" | "closewindow" | "movewindowv2"
                | "monitoraddedv2" | "monitorremovedv2" | "activespecialv2" | "configreloaded"
        ) {
            let _ = tx.try_send(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn covered_needs_every_monitor() {
        let mons = json!([{"name": "A", "activeWorkspace": {"id": 1}}, {"name": "B", "activeWorkspace": {"id": 2}}]);
        let one = json!([{"workspace": {"id": 1}, "fullscreen": 2, "class": "mpv"}]);
        let both = json!([
            {"workspace": {"id": 1}, "fullscreen": 2, "class": "mpv"},
            {"workspace": {"id": 2}, "fullscreen": 1, "class": "steam_app_730"}
        ]);
        assert!(!snapshot_from(&mons, &one, true).covered);
        let s = snapshot_from(&mons, &both, true);
        assert!(s.covered && s.game && s.monitors == ["A", "B"]);
        // A maximized window only counts when asked to.
        assert!(!snapshot_from(&mons, &both, false).covered);
        assert!(!snapshot_from(&json!([]), &both, true).covered);
    }
}
