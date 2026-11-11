use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Config {
    /// Renderer binary.
    pub backend: String,
    pub fps_ac: u32,
    pub fps_battery: u32,
    /// On battery: "fps" (lower the cap), "pause" (SIGSTOP), "static" (stop the renderer, keep the still).
    pub battery: String,
    pub pause_fullscreen: bool,
    /// Treat a maximized window like a fullscreen one.
    pub pause_maximized: bool,
    /// Stop the renderer while a Steam game window is open, freeing VRAM.
    pub stop_on_game: bool,
    pub silent: bool,
    pub volume: u32,
    pub automute: bool,
    pub audio_processing: bool,
    pub scaling: String,
    pub clamp: String,
    /// Extra Steam roots to search besides the standard ones.
    pub steam_roots: Vec<PathBuf>,
    /// Keep Omarchy's own background set to a still frame of the wallpaper.
    pub sync_omarchy_bg: bool,
    pub notify: bool,
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
        }
    }
}

/// A missing file means defaults; a broken file is an error so a typo never silently resets settings.
pub fn load(path: &Path) -> Result<Config, String> {
    match std::fs::read_to_string(path) {
        Ok(src) => toml::from_str(&src).map_err(|e| e.to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
        Err(e) => Err(e.to_string()),
    }
}
