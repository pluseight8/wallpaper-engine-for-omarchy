use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Deserialize, Serialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Config {
    /// Renderer binary.
    pub backend: String,
    pub fps_ac: u32,
    pub fps_battery: u32,
    /// On battery: "fps" (lower the cap), "pause" (SIGSTOP), "static" (stop the renderer, keep the still).
    pub battery: String,
    /// Pause a monitor's wallpaper while a fullscreen window hides it.
    pub pause_fullscreen: bool,
    /// Treat a maximized window like a fullscreen one.
    pub pause_maximized: bool,
    /// Stop the renderers while a Steam game window is open, freeing VRAM.
    pub stop_on_game: bool,
    pub silent: bool,
    pub volume: u32,
    /// Mute the wallpaper while another application plays sound.
    pub automute: bool,
    /// Feed audio to audio-reactive wallpapers.
    pub audio_processing: bool,
    pub scaling: String,
    pub clamp: String,
    /// Extra Steam roots to search besides the standard ones.
    pub steam_roots: Vec<PathBuf>,
    /// Keep Omarchy's own background set to a still frame of the wallpaper.
    pub sync_omarchy_bg: bool,
    pub notify: bool,
    /// Generate an Omarchy theme from the wallpaper's colours and apply it on every change.
    pub theme_sync: bool,
    pub playlist: Playlist,
}

#[derive(Deserialize, Serialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct Playlist {
    /// Switch every N minutes; 0 turns the timer off.
    pub every: u64,
    /// Switch once each time the session starts.
    pub on_login: bool,
    pub random: bool,
    /// Which wallpapers take part: the same keys as `omawe list` (type, tag, rating, search, fav).
    pub filter: BTreeMap<String, String>,
    /// Time of day: "HH:MM" = wallpaper id or title, applied from that time on.
    pub hours: BTreeMap<String, String>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            backend: "linux-wallpaperengine".into(),
            fps_ac: 60,
            fps_battery: 30,
            battery: "fps".into(),
            pause_fullscreen: true,
            pause_maximized: true,
            stop_on_game: true,
            silent: true,
            volume: 15,
            automute: true,
            audio_processing: true,
            scaling: "fill".into(),
            clamp: "clamp".into(),
            steam_roots: Vec::new(),
            sync_omarchy_bg: true,
            notify: true,
            theme_sync: false,
            playlist: Playlist::default(),
        }
    }
}

/// Lay one `key = value` (key may be dotted once: "playlist.every") over a table.
/// The value is read as TOML when it parses (60, true, "x"), otherwise as a plain string.
fn overlay(table: &mut toml::Table, key: &str, value: &str) {
    let parsed = toml::from_str::<toml::Table>(&format!("v = {value}")).ok().and_then(|mut t| t.remove("v"));
    let value = parsed.unwrap_or_else(|| toml::Value::String(value.to_string()));
    match key.split_once('.') {
        Some((section, key)) => {
            let sub = table.entry(section).or_insert_with(|| toml::Value::Table(Default::default()));
            if let Some(sub) = sub.as_table_mut() {
                sub.insert(key.into(), value);
            }
        }
        None => drop(table.insert(key.into(), value)),
    }
}

/// config.toml with `omawe set` overrides on top. A missing file means defaults;
/// a broken one is an error, so a typo never silently resets settings.
pub fn load(path: &Path, overrides: &BTreeMap<String, String>) -> Result<Config, String> {
    let mut table: toml::Table = match std::fs::read_to_string(path) {
        Ok(src) => toml::from_str(&src).map_err(|e| e.to_string())?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Default::default(),
        Err(e) => return Err(e.to_string()),
    };
    for (key, value) in overrides {
        overlay(&mut table, key, value);
    }
    table.try_into().map_err(|e: toml::de::Error| e.to_string())
}

/// Whether `omawe set <key>` names a real setting.
pub fn known_key(key: &str) -> bool {
    let Ok(defaults) = toml::Table::try_from(Config::default()) else { return false };
    match key.split_once('.') {
        // Free-form maps: any key is valid inside them.
        Some(("playlist", sub)) if sub.starts_with("filter.") || sub.starts_with("hours.") => false,
        Some((section, sub)) => defaults.get(section).and_then(|s| s.as_table()).is_some_and(|t| t.contains_key(sub)),
        None => defaults.get(key).is_some_and(|v| !v.is_table()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overrides_win_and_are_typed() {
        let over = BTreeMap::from([
            ("volume".to_string(), "40".to_string()),
            ("silent".to_string(), "false".to_string()),
            ("scaling".to_string(), "fit".to_string()),
            ("playlist.every".to_string(), "30".to_string()),
        ]);
        let cfg = load(Path::new("/nonexistent/config.toml"), &over).unwrap();
        assert!(cfg.volume == 40 && !cfg.silent && cfg.scaling == "fit" && cfg.playlist.every == 30);
        assert_eq!(cfg.fps_ac, 60);
        // A value of the wrong type is rejected instead of being half-applied.
        assert!(load(Path::new("/nonexistent"), &BTreeMap::from([("volume".to_string(), "loud".to_string())])).is_err());
        assert!(known_key("volume") && known_key("playlist.random"));
        assert!(!known_key("nope") && !known_key("playlist") && !known_key("playlist.nope"));
    }
}
