//! omawed — keeps linux-wallpaperengine running the way the session needs it.
//!
//! One task owns all state and reacts to events (CLI requests, Hyprland, UPower, inotify,
//! renderer exit, playlist deadline). After every event it compares what should be running
//! with what is, and fixes the difference. Nothing wakes up on a timer while idle.
//!
//! Each monitor gets its own renderer process: that is what makes pause, properties and
//! switching independent per monitor, since the renderer has no runtime control.

mod config;
mod db;
mod hypr;
mod playlist;
mod project;
mod theme;
mod vdf;

use config::Config;
use db::{Db, Wallpaper};
use futures_util::StreamExt;
use inotify::{Inotify, WatchMask};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::mpsc::{self, UnboundedSender};
use tokio::sync::{oneshot, watch};
use tokio::time::Instant;

const APPID: &str = "431960";
/// Assignment key meaning "every monitor without its own wallpaper".
const ALL: &str = "*";
/// Slug of the generated Omarchy theme.
const THEME: &str = "omawe-live";

type Res<T> = Result<T, String>;
type Reply = (String, Value);
type Props = BTreeMap<String, String>;

enum Event {
    Request(Value, oneshot::Sender<Value>),
    Battery(bool),
    /// The renderer wrote its first frame (true) or the wait timed out (false).
    Frame(u32, bool),
    Exit(u32),
    LibraryChanged,
    ConfigChanged,
    ThumbsDone,
    /// Battery charge in percent.
    Percent(f64),
    /// Most vivid colour of a wallpaper's preview.
    Color(String, String),
    /// Compatibility test result: wallpaper id, verdict, frame it drew.
    Tested(String, &'static str, PathBuf),
    TestsDone,
    /// Raw GetPublishedFileDetails reply.
    Workshop(Vec<u8>),
}

#[derive(Serialize, Deserialize, Clone, PartialEq)]
struct Assign {
    id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    scaling: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    clamp: Option<String>,
}

struct Proc {
    pid: u32,
    args: Vec<String>,
    id: String,
    paused: bool,
    ready: bool,
    frame: PathBuf,
}

/// One monitor's renderer.
#[derive(Default)]
struct Slot {
    cur: Option<Proc>,
    /// Previous renderer, kept on screen until `cur` has drawn its first frame.
    old: Option<Proc>,
    /// Arguments that crashed the renderer; not retried until something changes.
    crashed: Option<Vec<String>>,
}

struct Dirs {
    config: PathBuf,
    cache: PathBuf,
    socket: PathBuf,
}

fn xdg(var: &str, home_rel: &str) -> PathBuf {
    match std::env::var_os(var) {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(home_rel),
    }
}

fn signal_group(pid: u32, sig: i32) {
    // kill(-0) would signal our own process group, the daemon included.
    if pid == 0 {
        return;
    }
    // The renderer runs in its own process group, so wrapper scripts and helpers get the signal too.
    unsafe { libc::kill(-(pid as i32), sig) };
}

fn terminate(slot: &mut Option<Proc>) {
    let Some(p) = slot.take() else { return };
    signal_group(p.pid, libc::SIGTERM);
    // A stopped process only sees SIGTERM once it is continued.
    signal_group(p.pid, libc::SIGCONT);
    let _ = std::fs::remove_file(p.frame);
}

/// Detached helper; tokio reaps it, so no zombie is left behind.
fn run(cmd: &str, args: &[&str]) {
    let _ = tokio::process::Command::new(cmd).args(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
}

/// Battery state before UPower answers, so a login on battery does not start the renderer twice.
fn discharging() -> bool {
    let Ok(dir) = std::fs::read_dir("/sys/class/power_supply") else { return false };
    dir.flatten().any(|e| std::fs::read_to_string(e.path().join("status")).is_ok_and(|s| s.trim() == "Discharging"))
}

/// Work out an image's most vivid colour and report it.
async fn send_color(tx: &UnboundedSender<Event>, id: String, image: &std::path::Path) {
    let out = tokio::process::Command::new("magick")
        .arg(image)
        .args(["-colors", "6", "-format", "%c", "histogram:info:-"])
        .stderr(Stdio::null())
        .output()
        .await;
    // "-" marks "looked, found nothing", so a broken image is not retried on every list.
    let color = out.ok().and_then(|o| theme::vivid(&theme::parse_histogram(&String::from_utf8_lossy(&o.stdout))));
    let _ = tx.send(Event::Color(id, color.unwrap_or_else(|| "-".into())));
}

/// Accent of the Omarchy theme in use.
fn theme_accent() -> Option<String> {
    let path = xdg("XDG_STATE_HOME", ".local/state").join("omarchy/current/theme/colors.toml");
    theme::accent(&std::fs::read_to_string(path).ok()?)
}

/// Where an apply/stop/step is aimed: one workspace, one monitor, or everything (None).
fn target_key(opts: &Value) -> Option<String> {
    let opt = |k: &str| opts.get(k).and_then(Value::as_str);
    opt("workspace").map(|w| format!("ws:{w}")).or(opt("monitor").map(String::from))
}

/// A project.json value the way `--set-property` takes it.
fn prop_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Bool(b) => (*b as u8).to_string(),
        other => other.to_string(),
    }
}

/// Check a user-supplied property value against its definition and normalise it.
fn check_prop(def: &Value, value: &str) -> Res<String> {
    let kind = def["type"].as_str().unwrap_or_default().to_lowercase();
    match kind.as_str() {
        "bool" => match value.to_lowercase().as_str() {
            "1" | "true" | "on" => Ok("1".into()),
            "0" | "false" | "off" => Ok("0".into()),
            _ => Err("нужно true или false".into()),
        },
        "slider" => {
            let n: f64 = value.parse().map_err(|_| "нужно число".to_string())?;
            let (min, max) = (def["min"].as_f64().unwrap_or(f64::MIN), def["max"].as_f64().unwrap_or(f64::MAX));
            if !n.is_finite() || max < min {
                return Err("нужно число".into());
            }
            Ok(n.clamp(min, max).to_string())
        }
        "color" => {
            let parts: Vec<f64> = value.split_whitespace().filter_map(|p| p.parse().ok()).collect();
            match parts.as_slice() {
                // Always with a decimal point: the renderer reads "1 0 0" as 0–255 integers.
                [r, g, b] if parts.iter().all(|c| (0.0..=1.0).contains(c)) => Ok(format!("{r:.5} {g:.5} {b:.5}")),
                _ => Err("нужен цвет как три числа от 0 до 1: «0.5 0.2 1»".into()),
            }
        }
        "combo" => {
            let options = def["options"].as_array().map(Vec::as_slice).unwrap_or_default();
            if options.iter().any(|o| prop_text(&o["value"]) == value) {
                Ok(value.into())
            } else {
                Err(format!("допустимые значения: {}", options.iter().map(|o| prop_text(&o["value"])).collect::<Vec<_>>().join(", ")))
            }
        }
        "textinput" => Ok(value.into()),
        other => Err(format!("свойства типа «{other}» менять нельзя")),
    }
}

struct Daemon {
    cfg: Config,
    db: Db,
    dirs: Dirs,
    tx: UnboundedSender<Event>,
    status: watch::Sender<String>,
    sources: Vec<PathBuf>,
    assign: BTreeMap<String, Assign>,
    /// `omawe set` values laid over config.toml.
    overrides: Props,
    manual_pause: bool,
    on_battery: bool,
    snap: hypr::Snapshot,
    slots: BTreeMap<String, Slot>,
    error: String,
    spawned: u32,
    thumbing: bool,
    percent: f64,
    /// Compatibility test progress: done, total.
    testing: Option<(usize, usize)>,
    /// Next playlist timer / time-of-day deadline.
    interval_at: Option<Instant>,
    hours_at: Option<Instant>,
    /// The monitor set changed recently. Hyprland can add an output and drop it again within a second
    /// (a config reload re-enables a panel that a script then turns off); a renderer started on it then
    /// dies in its own start-up and looks like a broken wallpaper.
    settle_until: Option<Instant>,
}

impl Daemon {
    // ---- library -----------------------------------------------------------

    fn find_sources(&self) -> Vec<PathBuf> {
        let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
        let mut roots = vec![
            home.join(".local/share/Steam"),
            home.join(".steam/steam"),
            home.join(".var/app/com.valvesoftware.Steam/.local/share/Steam"),
        ];
        roots.extend(self.cfg.steam_roots.iter().cloned());
        let mut libs = BTreeSet::new();
        for root in roots {
            let Ok(vdf) = std::fs::read_to_string(root.join("steamapps/libraryfolders.vdf")) else { continue };
            for lib in vdf::library_paths(&vdf).into_iter().chain([root]) {
                if let Ok(real) = lib.canonicalize() {
                    libs.insert(real);
                }
            }
        }
        let mut out = Vec::new();
        for lib in libs {
            let apps = lib.join("steamapps");
            out.push(apps.join("workshop/content").join(APPID));
            out.push(apps.join("common/wallpaper_engine/projects/myprojects"));
        }
        out.retain(|p| p.is_dir());
        out
    }

    /// Bring the index in line with the disk. Only projects whose project.json changed are parsed.
    fn rescan(&mut self) -> Res<(usize, usize)> {
        let known = self.db.mtimes()?;
        let mut seen = HashSet::new();
        let mut updated = 0;
        for src in &self.sources {
            let Ok(entries) = std::fs::read_dir(src) else { continue };
            for dir in entries.flatten().map(|e| e.path()) {
                let Ok(meta) = std::fs::metadata(dir.join("project.json")) else { continue };
                let id = dir.file_name().unwrap_or_default().to_string_lossy().into_owned();
                if known.get(&id) != Some(&meta.mtime()) {
                    let parsed = std::fs::read_to_string(dir.join("project.json"))
                        .map_err(|e| e.to_string())
                        .and_then(|s| project::parse(&s));
                    match parsed {
                        Ok(p) => {
                            self.db.upsert(&id, &dir.to_string_lossy(), meta.mtime(), &p)?;
                            // The preview may have changed with the project.
                            let _ = std::fs::remove_file(self.thumb(&id));
                            updated += 1;
                        }
                        Err(e) => eprintln!("skip {}: {e}", dir.display()),
                    }
                }
                seen.insert(id);
            }
        }
        let gone: Vec<_> = known.keys().filter(|id| !seen.contains(*id)).collect();
        for id in &gone {
            self.db.remove(id)?;
        }
        Ok((updated, gone.len()))
    }

    fn thumb(&self, id: &str) -> PathBuf {
        self.dirs.cache.join("thumbs").join(format!("{id}.webp"))
    }

    /// Fill in cached thumbnails and start generating the missing ones in the background.
    fn attach_thumbs(&mut self, list: &mut [Wallpaper]) {
        let mut jobs = Vec::new();
        for w in list.iter_mut().filter(|w| !w.preview.is_empty()) {
            let thumb = self.thumb(&w.id);
            if thumb.exists() {
                w.thumb = thumb.to_string_lossy().into_owned();
            }
            if w.thumb.is_empty() || w.color.is_empty() {
                jobs.push((w.id.clone(), w.preview.clone(), thumb));
            }
        }
        if jobs.is_empty() || self.thumbing {
            return;
        }
        self.thumbing = true;
        let tx = self.tx.clone();
        tokio::spawn(async move {
            // One low-priority conversion at a time: opening the browser must not spin up every core.
            for (id, preview, thumb) in jobs {
                if thumb.exists() {
                    send_color(&tx, id, &thumb).await;
                    continue;
                }
                let tmp = thumb.with_extension("tmp.webp");
                let done = tokio::process::Command::new("nice")
                    .args(["-n", "19", "magick"])
                    .arg(format!("{preview}[0]")) // first frame of an animated preview
                    .args(["-thumbnail", "480x270^", "-gravity", "center", "-extent", "480x270", "-quality", "80"])
                    .arg(&tmp)
                    .stderr(Stdio::null())
                    .status()
                    .await;
                if done.is_ok_and(|s| s.success()) {
                    let _ = std::fs::rename(&tmp, &thumb);
                    send_color(&tx, id, &thumb).await;
                } else {
                    let _ = std::fs::remove_file(&tmp);
                }
            }
            let _ = tx.send(Event::ThumbsDone);
        });
    }

    // ---- properties --------------------------------------------------------

    /// What actually renders for a wallpaper: its folder, property definitions and built-in overrides.
    /// A Workshop preset is another wallpaper plus overrides.
    fn base(&self, w: &Wallpaper) -> Option<(String, Value, Props)> {
        let mut preset = Props::new();
        let base = if w.kind == "preset" {
            if let Ok(Value::Object(values)) = serde_json::from_str(&w.preset) {
                preset = values.iter().map(|(k, v)| (k.clone(), prop_text(v))).collect();
            }
            self.db.get(&w.dependency).ok()??
        } else {
            w.clone()
        };
        let defs = serde_json::from_str(&base.props).unwrap_or(Value::Null);
        Some((base.dir, defs, preset))
    }

    /// Property list for the editor: definition plus the value in effect on `monitor`.
    fn props_json(&self, w: &Wallpaper, monitor: &str) -> Res<Vec<Value>> {
        let (_, defs, preset) = self.base(w).ok_or("базовые обои пресета не найдены")?;
        let user = self.db.props(&w.id, monitor);
        let mut out: Vec<(f64, Value)> = Vec::new();
        for (key, def) in defs.as_object().into_iter().flatten().filter(|(_, d)| d.is_object()) {
            let default = prop_text(&def["value"]);
            let value = user.get(key).or(preset.get(key)).cloned().unwrap_or_else(|| default.clone());
            out.push((
                def["order"].as_f64().unwrap_or(0.0),
                json!({
                    "key": key,
                    // Untyped entries are labels and separators.
                    "type": def["type"].as_str().unwrap_or("text").to_lowercase(),
                    "text": def["text"].as_str().unwrap_or(key),
                    "value": value,
                    "default": default,
                    "changed": user.contains_key(key),
                    "min": def["min"], "max": def["max"], "step": def["step"],
                    "options": def["options"],
                }),
            ));
        }
        out.sort_by(|a, b| a.0.total_cmp(&b.0));
        Ok(out.into_iter().map(|(_, v)| v).collect())
    }

    // ---- policy ------------------------------------------------------------

    fn monitors(&self) -> Vec<String> {
        if self.snap.monitors.is_empty() {
            // No compositor answer (tests, early start): trust explicit per-monitor assignments.
            self.assign.keys().filter(|k| *k != ALL && !k.starts_with("ws:")).cloned().collect()
        } else {
            self.snap.monitors.clone()
        }
    }

    fn shown(&self) -> Vec<(String, Assign, Wallpaper)> {
        let mut out = Vec::new();
        for mon in self.monitors() {
            // Most specific wins: the visible workspace, then the monitor, then "everywhere".
            let ws = self.snap.workspaces.get(&mon).map(|id| format!("ws:{id}"));
            let a = ws.and_then(|k| self.assign.get(&k)).or_else(|| self.assign.get(&mon)).or_else(|| self.assign.get(ALL));
            let Some(a) = a else { continue };
            if let Ok(Some(w)) = self.db.get(&a.id) {
                out.push((mon, a.clone(), w));
            }
        }
        out
    }

    fn fps(&self) -> u32 {
        if self.on_battery { self.cfg.fps_battery } else { self.cfg.fps_ac }
    }

    /// Power profile in force: the `battery` mode, or `low_battery` once the charge is low. None on AC.
    fn battery_mode(&self) -> Option<&str> {
        if !self.on_battery {
            None
        } else if self.percent <= f64::from(self.cfg.low_battery_percent) {
            Some(&self.cfg.low_battery)
        } else {
            Some(&self.cfg.battery)
        }
    }

    /// Why no renderer should exist at all right now.
    fn stop_reason(&self) -> Option<&'static str> {
        if self.cfg.stop_on_game && self.snap.game {
            Some("game")
        } else if self.battery_mode() == Some("static") {
            Some("battery")
        } else {
            None
        }
    }

    fn pause_reason(&self, monitor: &str) -> Option<&'static str> {
        if self.manual_pause {
            Some("manual")
        } else if self.cfg.pause_fullscreen && self.snap.covered.contains(monitor) {
            Some("fullscreen")
        } else if self.battery_mode() == Some("pause") {
            Some("battery")
        } else {
            None
        }
    }

    /// Renderer arguments and wallpaper id wanted on each monitor.
    fn wanted(&self) -> BTreeMap<String, (Vec<String>, String)> {
        let mut out = BTreeMap::new();
        if self.stop_reason().is_some() {
            return out;
        }
        let c = &self.cfg;
        for (i, (mon, a, w)) in self.shown().into_iter().enumerate() {
            let Some((dir, _, mut props)) = self.base(&w) else { continue };
            props.extend(self.db.props(&w.id, &mon));
            let mut args = vec!["--fps".to_string(), self.fps().to_string()];
            // One audible renderer: the same wallpaper on two monitors must not play twice.
            if c.silent || i > 0 {
                args.push("--silent".into());
            } else {
                args.extend(["--volume".into(), c.volume.to_string()]);
            }
            for (off, flag) in [
                (!c.automute, "--noautomute"),
                (!c.audio_processing, "--no-audio-processing"),
                (!c.pause_fullscreen, "--no-fullscreen-pause"),
            ] {
                if off {
                    args.push(flag.into());
                }
            }
            for (k, v) in props {
                args.extend(["--set-property".into(), format!("{k}={v}")]);
            }
            args.extend(["--screen-root".into(), mon.clone(), "--bg".into(), dir]);
            args.extend(["--scaling".into(), a.scaling.unwrap_or_else(|| c.scaling.clone())]);
            args.extend(["--clamp".into(), a.clamp.unwrap_or_else(|| c.clamp.clone())]);
            out.insert(mon, (args, w.id));
        }
        out
    }

    // ---- renderer ----------------------------------------------------------

    fn twin(&self, id: &str) -> PathBuf {
        self.dirs.cache.join("twins").join(format!("{id}.png"))
    }

    fn spawn(&mut self, args: Vec<String>, id: String) -> Res<Proc> {
        self.spawned += 1;
        let frame = self.dirs.cache.join("twins").join(format!(".frame-{}.png", self.spawned));
        let _ = std::fs::remove_file(&frame);
        let mut child = tokio::process::Command::new(&self.cfg.backend)
            .args(&args)
            .arg("--screenshot")
            .arg(&frame)
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("не удалось запустить {}: {e}", self.cfg.backend))?;
        let pid = child.id().unwrap_or_default();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let _ = child.wait().await;
            let _ = tx.send(Event::Exit(pid));
        });
        let (tx, path) = (self.tx.clone(), frame.clone());
        tokio::spawn(async move {
            // shortcut: the renderer has no "ready" signal, so its screenshot file is polled
            // for at most 5 s after each start; replace if upstream grows an IPC.
            for _ in 0..100 {
                tokio::time::sleep(Duration::from_millis(50)).await;
                if std::fs::metadata(&path).is_ok_and(|m| m.len() > 0) {
                    // Let the PNG finish writing.
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    let _ = tx.send(Event::Frame(pid, true));
                    return;
                }
            }
            let _ = tx.send(Event::Frame(pid, false));
        });
        Ok(Proc { pid, args, id, paused: false, ready: false, frame })
    }

    /// Make the running renderers match the policy.
    fn reconcile(&mut self) {
        let mut wanted = self.wanted();
        let monitors: BTreeSet<String> = self.slots.keys().chain(wanted.keys()).cloned().collect();
        for mon in monitors {
            let mut slot = self.slots.remove(&mon).unwrap_or_default();
            let Some((args, id)) = wanted.remove(&mon) else {
                // The still frame under the renderer stays as Omarchy's background.
                terminate(&mut slot.cur);
                terminate(&mut slot.old);
                continue;
            };
            // While monitors settle, renderers already on screen keep going but none start on a new output.
            let settling = self.settle_until.is_some() && slot.cur.is_none();
            if !settling && slot.cur.as_ref().is_none_or(|c| c.args != args) && slot.crashed.as_ref() != Some(&args) {
                match self.spawn(args, id) {
                    Ok(new) => {
                        self.error.clear();
                        // Swap in progress: drop the unfinished renderer, keep the visible one.
                        let mut prev = slot.cur.replace(new);
                        if slot.old.is_some() {
                            terminate(&mut prev);
                        } else {
                            slot.old = prev;
                        }
                    }
                    Err(e) => self.error = e,
                }
            }
            let want_paused = self.pause_reason(&mon).is_some();
            if let Some(c) = slot.cur.as_mut().filter(|c| c.ready && c.paused != want_paused) {
                signal_group(c.pid, if want_paused { libc::SIGSTOP } else { libc::SIGCONT });
                c.paused = want_paused;
            }
            self.slots.insert(mon, slot);
        }
    }

    /// Omarchy has one background for all monitors: use the first monitor's still.
    fn sync_omarchy_bg(&self) {
        let Some((_, _, w)) = self.shown().into_iter().next() else { return };
        let twin = self.twin(&w.id);
        if self.cfg.sync_omarchy_bg && twin.exists() {
            run("omarchy-theme-bg-set", &[&twin.to_string_lossy()]);
        }
    }

    /// Build the "omawe-live" theme from the first monitor's still and switch to it.
    /// Returns false when nothing was started, so the caller still sets the background itself.
    fn sync_theme(&mut self) -> bool {
        let Some((_, _, w)) = self.shown().into_iter().next() else { return false };
        let twin = self.twin(&w.id);
        // Once per wallpaper: a restart for a pause, a property or an FPS change must not re-theme the desktop.
        if !self.cfg.theme_sync || !twin.exists() || self.db.state("themed").as_deref() == Some(&w.id) {
            return false;
        }
        self.db.set_state("themed", &w.id);
        // Remember the user's own theme so turning the feature off can put it back.
        let current = std::fs::read_to_string(xdg("XDG_STATE_HOME", ".local/state").join("omarchy/current/theme.name"));
        match current.as_deref().map(str::trim) {
            Ok(name) if !name.is_empty() && name != THEME => self.db.set_state("theme_before", name),
            _ => {}
        }
        let dir = xdg("XDG_CONFIG_HOME", ".config").join("omarchy/themes").join(THEME);
        let id = w.id;
        tokio::spawn(async move {
            let histogram = tokio::process::Command::new("magick")
                .arg(&twin)
                .args(["-resize", "96x96", "-colors", "8", "-format", "%c", "histogram:info:-"])
                .output()
                .await;
            let Ok(histogram) = histogram else { return };
            let Some(colors) = theme::colors_toml(&theme::parse_histogram(&String::from_utf8_lossy(&histogram.stdout))) else { return };
            // The theme carries the still as its only background, so applying it also sets the right one.
            let backgrounds = dir.join("backgrounds");
            let _ = std::fs::remove_dir_all(&backgrounds);
            let written = std::fs::create_dir_all(&backgrounds)
                .and_then(|_| std::fs::copy(&twin, backgrounds.join(format!("{id}.png"))))
                .and_then(|_| std::fs::write(dir.join("colors.toml"), colors));
            match written {
                Ok(()) => run("omarchy-theme-set", &[THEME]),
                Err(e) => eprintln!("theme: {e}"),
            }
        });
        true
    }

    /// Theme sync was turned off: give back the theme the user had before it.
    fn restore_theme(&mut self) {
        if self.cfg.theme_sync {
            return;
        }
        if let Some(before) = self.db.state("theme_before").filter(|t| !t.is_empty()) {
            run("omarchy-theme-set", &[&before]);
        }
        self.db.set_state("theme_before", "");
        self.db.set_state("themed", "");
    }

    fn on_frame(&mut self, pid: u32, drawn: bool) {
        let Some(slot) = self.slots.values_mut().find(|s| s.cur.as_ref().is_some_and(|c| c.pid == pid)) else { return };
        terminate(&mut slot.old);
        let Some(c) = slot.cur.as_mut() else { return };
        c.ready = true;
        let (frame, id) = (c.frame.clone(), c.id.clone());
        if drawn {
            let _ = std::fs::rename(frame, self.twin(&id));
            self.db.set_compat(&id, "ok");
            if !self.sync_theme() {
                self.sync_omarchy_bg();
            }
        }
    }

    fn on_exit(&mut self, pid: u32) {
        let settling = self.settle_until.is_some();
        for (monitor, slot) in self.slots.iter_mut() {
            if slot.old.as_ref().is_some_and(|o| o.pid == pid) {
                slot.old = None;
            }
            let Some(dead) = slot.cur.take_if(|c| c.pid == pid) else { continue };
            let _ = std::fs::remove_file(&dead.frame);
            // Not the wallpaper's fault, so just start again: a renderer that died while stopped (dropped by
            // the compositor), or whose output went away or was being reconfigured.
            let output_changed = settling || !self.snap.monitors.contains(monitor);
            if !dead.paused && !output_changed {
                if !dead.ready {
                    self.db.set_compat(&dead.id, "crash");
                }
                self.error = "рендерер завершился с ошибкой".into();
                slot.crashed = Some(dead.args);
                if self.cfg.notify {
                    run("omarchy-notification-send", &["Wallpaper Engine: обои не запустились", "-t", "4000"]);
                }
            }
            // The previous renderer is still on screen: keep it.
            slot.cur = slot.old.take();
        }
    }

    fn retry_crashed(&mut self) {
        for slot in self.slots.values_mut() {
            slot.crashed = None;
        }
    }

    // ---- playlist ----------------------------------------------------------

    fn playlist_filter(&self) -> Value {
        let mut filter = json!(self.cfg.playlist.filter);
        // With theme sync the theme follows the wallpaper, so matching it back would be circular.
        if self.cfg.playlist.match_theme && !self.cfg.theme_sync {
            filter["match-theme"] = json!(true);
        }
        filter
    }

    /// `db.list` plus the one filter SQL cannot do: colour match with the current theme.
    fn listed(&self, opts: &Value) -> Res<Vec<Wallpaper>> {
        let mut list = self.db.list(opts)?;
        if opts.get("match-theme").is_some() {
            let accent = theme_accent().ok_or("не удалось прочитать цвета текущей темы")?;
            list.retain(|w| theme::matches(&w.color, &accent));
        }
        Ok(list)
    }

    // ---- compatibility test ------------------------------------------------

    /// Render each wallpaper for a moment on the background layer, under everything, and record
    /// whether it drew a frame, drew black, or crashed. Runs one at a time.
    fn start_test(&mut self, query: Option<&str>, all: bool) -> Res<usize> {
        if self.testing.is_some() {
            return Err("проверка уже идёт".into());
        }
        let monitor = self.monitors().into_iter().next().ok_or("нет мониторов")?;
        let candidates = match query {
            Some(q) => vec![self.db.find(q)?],
            None => self.db.list(&json!({}))?.into_iter().filter(|w| all || w.compat.is_empty()).collect(),
        };
        let jobs: Vec<(String, String)> = candidates
            .into_iter()
            .filter(|w| w.compat != "unsupported")
            .filter_map(|w| Some((w.id.clone(), self.base(&w)?.0)))
            .collect();
        let total = jobs.len();
        if total == 0 {
            return Ok(0);
        }
        self.testing = Some((0, total));
        let (tx, backend, twins) = (self.tx.clone(), self.cfg.backend.clone(), self.dirs.cache.join("twins"));
        tokio::spawn(async move {
            for (id, dir) in jobs {
                let frame = twins.join(format!(".test-{id}.png"));
                let _ = std::fs::remove_file(&frame);
                let child = tokio::process::Command::new(&backend)
                    .args(["--silent", "--fps", "10", "--layer", "background", "--screen-root", &monitor, "--bg", &dir, "--screenshot"])
                    .arg(&frame)
                    .process_group(0)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn();
                let mut verdict = "crash";
                if let Ok(mut child) = child {
                    // Taken now: after the child has been reaped it has no id any more.
                    let pid = child.id().unwrap_or_default();
                    verdict = "noframe";
                    for _ in 0..80 {
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        if std::fs::metadata(&frame).is_ok_and(|m| m.len() > 0) {
                            tokio::time::sleep(Duration::from_millis(100)).await;
                            verdict = "ok";
                            break;
                        }
                        if matches!(child.try_wait(), Ok(Some(_))) {
                            verdict = "crash";
                            break;
                        }
                    }
                    signal_group(pid, libc::SIGTERM);
                    let _ = child.wait().await;
                }
                if verdict == "ok" {
                    let mean = tokio::process::Command::new("magick").arg(&frame).args(["-format", "%[fx:mean]", "info:"]).output().await;
                    let mean = mean.ok().and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse::<f64>().ok());
                    if mean.is_some_and(|m| m < 0.02) {
                        verdict = "black";
                    }
                }
                let _ = tx.send(Event::Tested(id, verdict, frame));
            }
            let _ = tx.send(Event::TestsDone);
        });
        Ok(total)
    }

    fn on_tested(&mut self, id: &str, verdict: &str, frame: PathBuf) {
        self.db.set_compat(id, verdict);
        // A good frame doubles as the still twin, so the first real apply is seamless.
        if verdict != "ok" || self.twin(id).exists() || std::fs::rename(&frame, self.twin(id)).is_err() {
            let _ = std::fs::remove_file(&frame);
        }
        if let Some((done, _)) = self.testing.as_mut() {
            *done += 1;
        }
    }

    // ---- workshop ----------------------------------------------------------

    /// Ask Steam's public, keyless API about every Workshop item in the library.
    fn start_workshop_sync(&mut self) -> Res<usize> {
        let ids: Vec<String> = self
            .db
            .list(&json!({}))?
            .into_iter()
            .map(|w| w.id)
            .filter(|id| id.len() <= 20 && id.bytes().all(|b| b.is_ascii_digit()))
            .collect();
        let (tx, total) = (self.tx.clone(), ids.len());
        tokio::spawn(async move {
            for chunk in ids.chunks(100) {
                let mut body = format!("itemcount={}", chunk.len());
                for (i, id) in chunk.iter().enumerate() {
                    body += &format!("&publishedfileids[{i}]={id}");
                }
                // Fixed HTTPS endpoint, a time limit and a size cap; the reply is parsed, never executed.
                let out = tokio::process::Command::new("curl")
                    .args(["-sS", "--max-time", "20", "--max-filesize", "8000000", "--data", &body])
                    .arg("https://api.steampowered.com/ISteamRemoteStorage/GetPublishedFileDetails/v1/")
                    .stderr(Stdio::null())
                    .output()
                    .await;
                if let Ok(out) = out {
                    let _ = tx.send(Event::Workshop(out.stdout));
                }
            }
        });
        Ok(total)
    }

    fn on_workshop(&mut self, reply: &[u8]) {
        let Ok(v) = serde_json::from_slice::<Value>(reply) else { return };
        for item in v["response"]["publishedfiledetails"].as_array().into_iter().flatten() {
            let Some(id) = item["publishedfileid"].as_str() else { continue };
            // Only items we asked about and Steam knows.
            if item["result"] == 1 && matches!(self.db.get(id), Ok(Some(_))) {
                let n = |key: &str| item[key].as_i64().unwrap_or(0);
                self.db.set_workshop(id, n("subscriptions"), n("favorited"), n("time_updated"));
            }
        }
    }

    /// Apply the time-of-day entry if it is not already showing, and arm both deadlines.
    fn schedule(&mut self) {
        let p = self.cfg.playlist.clone();
        self.interval_at = (p.every > 0).then(|| Instant::now() + Duration::from_secs(p.every * 60));
        self.hours_at = None;
        if let Some((target, wait)) = playlist::due(&p.hours, playlist::local_seconds()) {
            // shortcut: sleeps do not advance while the machine is suspended, so the wait is capped
            // at 10 min to stay close to wall-clock time; replace with a CLOCK_REALTIME timer if that matters.
            self.hours_at = Some(Instant::now() + Duration::from_secs(u64::from(wait).min(600) + 1));
            match self.db.find(target) {
                Ok(w) if self.assign.get(ALL).is_none_or(|a| a.id != w.id) => {
                    if let Err(e) = self.apply(&w, &json!({})) {
                        eprintln!("playlist: {e}");
                    }
                }
                Ok(_) => {}
                Err(e) => eprintln!("playlist.hours: {e}"),
            }
        }
    }

    fn on_tick(&mut self) {
        let now = Instant::now();
        let interval_due = self.interval_at.is_some_and(|t| t <= now);
        // Nobody is watching a paused or stopped wallpaper; keep it until it plays again.
        let playing = self.slots.values().any(|s| s.cur.as_ref().is_some_and(|c| !c.paused));
        if interval_due && playing && self.cfg.playlist.hours.is_empty() {
            let cmd = if self.cfg.playlist.random { "random" } else { "next" };
            if let Err(e) = self.step(cmd, &self.playlist_filter()) {
                eprintln!("playlist: {e}");
            }
        }
        let keep = self.interval_at.filter(|_| !interval_due);
        self.schedule();
        if let Some(t) = keep {
            self.interval_at = Some(t);
        }
    }

    // ---- requests ----------------------------------------------------------

    fn save(&self) {
        self.db.set_state("assign", &serde_json::to_string(&self.assign).unwrap_or_default());
        self.db.set_state("overrides", &serde_json::to_string(&self.overrides).unwrap_or_default());
        self.db.set_state("manual_pause", if self.manual_pause { "1" } else { "0" });
    }

    fn status_json(&self) -> Value {
        let shown = self.shown();
        let stop = self.stop_reason().or((!self.error.is_empty()).then_some("error"));
        let mut monitors = BTreeMap::new();
        for (mon, _, w) in &shown {
            let cur = self.slots.get(mon).and_then(|s| s.cur.as_ref());
            let (state, reason) = match cur {
                Some(c) if c.paused => ("paused", self.pause_reason(mon)),
                Some(_) => ("running", None),
                None => ("stopped", stop),
            };
            monitors.insert(mon.clone(), json!({"id": w.id, "title": w.title, "state": state, "reason": reason, "pid": cur.map(|c| c.pid)}));
        }
        // The session is "running" if anything animates, "paused" if everything alive is stopped.
        let has = |state: &str| monitors.values().any(|m| m["state"] == state);
        let state = match () {
            _ if shown.is_empty() => "off",
            _ if has("running") => "running",
            _ if has("paused") => "paused",
            _ => "stopped",
        };
        let first = monitors.values().find(|m| m["state"] == state).or(monitors.values().next());
        json!({
            "state": state,
            "reason": first.map(|m| m["reason"].clone()),
            "error": self.error,
            "title": first.map(|m| m["title"].clone()),
            "id": first.map(|m| m["id"].clone()),
            "pid": first.map(|m| m["pid"].clone()),
            "fps": self.fps(),
            "on_battery": self.on_battery,
            "battery_percent": self.percent,
            "testing": self.testing,
            "silent": self.cfg.silent,
            "volume": self.cfg.volume,
            "monitors": monitors,
        })
    }

    fn apply(&mut self, w: &Wallpaper, opts: &Value) -> Res<Reply> {
        if w.compat == "unsupported" {
            return Err(format!("«{}»: обои типа application в linux-wallpaperengine не работают", w.title));
        }
        if self.base(w).is_none() {
            return Err(format!("«{}» — пресет, сначала подпишитесь на базовые обои {}", w.title, w.dependency));
        }
        let opt = |k: &str| opts.get(k).and_then(Value::as_str).map(String::from);
        let a = Assign { id: w.id.clone(), scaling: opt("scaling"), clamp: opt("clamp") };
        match target_key(opts) {
            Some(key) => self.assign.insert(key, a),
            None => {
                // "Everywhere" replaces per-monitor choices but leaves per-workspace ones in place.
                self.assign.retain(|k, _| k.starts_with("ws:"));
                self.assign.insert(ALL.into(), a)
            }
        };
        self.retry_crashed();
        self.save();
        Ok((w.title.clone(), json!(w)))
    }

    /// next / prev / random within the (optionally filtered) library.
    fn step(&mut self, cmd: &str, opts: &Value) -> Res<Reply> {
        let list: Vec<_> = self.listed(opts)?.into_iter().filter(|w| !matches!(w.compat.as_str(), "unsupported" | "crash")).collect();
        if list.is_empty() {
            return Err("подходящих обоев нет".into());
        }
        let key = target_key(opts).unwrap_or_else(|| ALL.into());
        let cur = self.assign.get(&key).or_else(|| self.assign.get(ALL)).map(|a| a.id.clone());
        let pos = list.iter().position(|w| Some(&w.id) == cur.as_ref());
        let n = list.len();
        let i = match (cmd, pos) {
            // Good enough to shuffle wallpapers; not for anything that needs real randomness.
            ("random", _) => std::time::UNIX_EPOCH.elapsed().map_or(0, |d| d.subsec_nanos() as usize) % n,
            ("prev", Some(p)) => (p + n - 1) % n,
            (_, Some(p)) => (p + 1) % n,
            (_, None) => 0,
        };
        // Only the monitor travels on: list filters are not apply options.
        self.apply(&list[i].clone(), &json!({"monitor": opts.get("monitor"), "workspace": opts.get("workspace")}))
    }

    /// Store a setting override and reload; a value the config rejects is not kept.
    fn set(&mut self, key: &str, value: Option<&str>) -> Res<()> {
        if !config::known_key(key) {
            return Err(format!("неизвестная настройка: {key}"));
        }
        let before = self.overrides.clone();
        match value {
            Some(v) => self.overrides.insert(key.into(), v.into()),
            None => self.overrides.remove(key),
        };
        match config::load(&self.dirs.config.join("config.toml"), &self.overrides) {
            Ok(cfg) => {
                self.cfg = cfg;
                self.retry_crashed();
                self.schedule();
                self.save();
                self.restore_theme();
                if !self.sync_theme() {
                    self.sync_omarchy_bg();
                }
                Ok(())
            }
            Err(e) => {
                self.overrides = before;
                Err(e)
            }
        }
    }

    fn handle(&mut self, req: &Value) -> Res<Reply> {
        let opts = req.get("opts").cloned().unwrap_or(json!({}));
        let arg = |i: usize| req.pointer(&format!("/args/{i}")).and_then(Value::as_str);
        let need = |i: usize, what: &str| arg(i).ok_or_else(|| format!("нужен аргумент: {what}"));
        let monitor = opts.get("monitor").and_then(Value::as_str).unwrap_or(ALL).to_string();
        match req.get("cmd").and_then(Value::as_str).unwrap_or_default() {
            "status" => {
                let s = self.status_json();
                let title = s["title"].as_str().unwrap_or("обои не выбраны");
                let reason = s["reason"].as_str().map(|r| format!(" ({r})")).unwrap_or_default();
                Ok((format!("{}{reason}: {title}", s["state"].as_str().unwrap_or_default()), s))
            }
            "list" => {
                let mut list = self.listed(&opts)?;
                if opts.get("thumbs").is_some() {
                    self.attach_thumbs(&mut list);
                }
                let text = list
                    .iter()
                    .map(|w| format!("{}\t{}\t{}\t{}{}", w.id, w.kind, w.title, w.compat, if w.fav { "\t★" } else { "" }))
                    .collect::<Vec<_>>()
                    .join("\n");
                Ok((text, json!(list)))
            }
            "apply" => {
                let w = self.db.find(need(0, "id или название")?)?;
                self.apply(&w, &opts)
            }
            cmd @ ("next" | "prev" | "random") => self.step(cmd, &opts),
            "stop" => {
                match target_key(&opts) {
                    Some(key) => drop(self.assign.remove(&key)),
                    None => self.assign.clear(),
                }
                self.save();
                Ok(("обои остановлены".into(), Value::Null))
            }
            cmd @ ("pause" | "resume" | "toggle") => {
                self.manual_pause = match cmd {
                    "pause" => true,
                    "resume" => false,
                    _ => !self.manual_pause,
                };
                self.retry_crashed();
                self.save();
                Ok((if self.manual_pause { "пауза" } else { "воспроизведение" }.into(), json!(self.manual_pause)))
            }
            "fav" => {
                let w = self.db.find(need(0, "id или название")?)?;
                let fav = self.db.toggle_fav(&w.id)?;
                Ok((format!("{}{}", if fav { "★ " } else { "" }, w.title), json!(fav)))
            }
            "props" => {
                let w = self.db.find(need(0, "id или название")?)?;
                let props = self.props_json(&w, &monitor)?;
                let text = props
                    .iter()
                    .filter(|p| !matches!(p["type"].as_str(), Some("text" | "group")))
                    .map(|p| format!("{}\t{}\t{}", p["key"].as_str().unwrap_or_default(), p["type"].as_str().unwrap_or_default(), p["value"].as_str().unwrap_or_default()))
                    .collect::<Vec<_>>()
                    .join("\n");
                Ok((text, json!({"id": w.id, "title": w.title, "props": props, "presets": self.db.presets(&w.id)?})))
            }
            // prop <wallpaper> <key> [value]; without a value the property goes back to its default.
            "prop" => {
                let w = self.db.find(need(0, "id или название")?)?;
                let key = need(1, "ключ свойства")?;
                let (_, defs, _) = self.base(&w).ok_or("базовые обои пресета не найдены")?;
                let def = defs.get(key).ok_or_else(|| format!("у «{}» нет свойства {key}", w.title))?;
                let value = arg(2).map(|v| check_prop(def, v)).transpose().map_err(|e| format!("{key}: {e}"))?;
                self.db.set_prop(&w.id, &monitor, key, value.as_deref())?;
                self.retry_crashed();
                Ok((String::new(), json!(value)))
            }
            // preset save|load|rm <wallpaper> <name>, preset reset <wallpaper>
            "preset" => {
                let action = need(0, "save, load, rm или reset")?;
                let w = self.db.find(need(1, "id или название")?)?;
                match action {
                    "reset" => self.db.clear_props(&w.id, &monitor)?,
                    "save" => self.db.save_preset(&w.id, need(2, "имя пресета")?, &self.db.props(&w.id, &monitor))?,
                    "rm" => self.db.remove_preset(&w.id, need(2, "имя пресета")?)?,
                    "load" => {
                        let values = self.db.preset(&w.id, need(2, "имя пресета")?)?;
                        self.db.clear_props(&w.id, &monitor)?;
                        for (k, v) in values {
                            self.db.set_prop(&w.id, &monitor, &k, Some(&v))?;
                        }
                    }
                    other => return Err(format!("неизвестное действие: {other}")),
                }
                self.retry_crashed();
                Ok((String::new(), json!(self.db.presets(&w.id)?)))
            }
            "set" => {
                self.set(need(0, "настройка")?, Some(need(1, "значение")?))?;
                Ok((String::new(), json!(self.cfg)))
            }
            "unset" => {
                self.set(need(0, "настройка")?, None)?;
                Ok((String::new(), json!(self.cfg)))
            }
            "mute" => {
                let silent = (!self.cfg.silent).to_string();
                self.set("silent", Some(&silent))?;
                Ok((if self.cfg.silent { "звук выключен" } else { "звук включён" }.into(), json!(self.cfg.silent)))
            }
            "volume" => {
                self.set("volume", Some(need(0, "громкость 0–100")?))?;
                Ok((String::new(), json!(self.cfg.volume)))
            }
            "rescan" => {
                self.sources = self.find_sources();
                let (updated, removed) = self.rescan()?;
                Ok((format!("обновлено {updated}, удалено {removed}"), json!({"updated": updated, "removed": removed})))
            }
            // After a theme change: re-assert the still frame as Omarchy's background, and with
            // playlist.match_theme move on if the wallpaper no longer suits the theme.
            "refresh" => {
                let filter = self.playlist_filter();
                let current = self.shown().into_iter().next().map(|(_, _, w)| w);
                let suits = theme_accent().zip(current).is_none_or(|(accent, w)| theme::matches(&w.color, &accent));
                if filter.get("match-theme").is_some() && !suits {
                    return self.step(if self.cfg.playlist.random { "random" } else { "next" }, &filter);
                }
                self.sync_omarchy_bg();
                Ok((String::new(), Value::Null))
            }
            // test [wallpaper] [--all]: without arguments, every wallpaper not tested yet.
            "test" => {
                let total = self.start_test(arg(0), opts.get("all").is_some())?;
                Ok((format!("проверяю обоев: {total}; ход — в omawe status"), json!(total)))
            }
            "workshop" => match arg(0) {
                Some("sync") => {
                    let total = self.start_workshop_sync()?;
                    Ok((format!("запрашиваю данные Workshop для {total} обоев"), json!(total)))
                }
                _ => Err("нужно: omawe workshop sync".into()),
            },
            // open [wallpaper]: its Workshop page in Steam, or the Workshop itself.
            "open" => {
                let url = match arg(0) {
                    Some(q) => {
                        let w = self.db.find(q)?;
                        if !w.id.bytes().all(|b| b.is_ascii_digit()) {
                            return Err(format!("«{}» — локальный проект, страницы в Workshop нет", w.title));
                        }
                        format!("steam://url/CommunityFilePage/{}", w.id)
                    }
                    None => format!("steam://url/SteamWorkshopPage/{APPID}"),
                };
                run("xdg-open", &[&url]);
                Ok((String::new(), json!(url)))
            }
            "config" => Ok((toml::to_string(&self.cfg).unwrap_or_default(), json!({"config": self.cfg, "overrides": self.overrides}))),
            other => Err(format!("неизвестная команда: {other}")),
        }
    }

    fn reload_config(&mut self) {
        match config::load(&self.dirs.config.join("config.toml"), &self.overrides) {
            Ok(cfg) => {
                self.retry_crashed();
                self.cfg = cfg;
                self.schedule();
                self.restore_theme();
                self.sync_theme();
            }
            // Keep the working settings; a half-saved file must not reset them.
            Err(e) => eprintln!("config.toml ignored: {e}"),
        }
    }

    async fn refresh_snapshot(&mut self) {
        if let Some(snap) = hypr::snapshot(self.cfg.pause_maximized).await {
            // A renderer started while a monitor was still coming up may have died; try again on hotplug.
            if snap.monitors != self.snap.monitors {
                self.retry_crashed();
                self.settle_until = Some(Instant::now() + Duration::from_millis(2500));
            }
            self.snap = snap;
        }
    }
}

// ---- event sources ---------------------------------------------------------

#[zbus::proxy(
    interface = "org.freedesktop.UPower",
    default_service = "org.freedesktop.UPower",
    default_path = "/org/freedesktop/UPower"
)]
trait UPower {
    #[zbus(property)]
    fn on_battery(&self) -> zbus::Result<bool>;
}

#[zbus::proxy(
    interface = "org.freedesktop.UPower.Device",
    default_service = "org.freedesktop.UPower",
    default_path = "/org/freedesktop/UPower/devices/DisplayDevice"
)]
trait Device {
    #[zbus(property)]
    fn percentage(&self) -> zbus::Result<f64>;
}

async fn watch_battery(tx: UnboundedSender<Event>) -> zbus::Result<()> {
    let bus = zbus::Connection::system().await?;
    let (power, device) = (UPowerProxy::new(&bus).await?, DeviceProxy::new(&bus).await?);
    let _ = tx.send(Event::Battery(power.on_battery().await?));
    let _ = tx.send(Event::Percent(device.percentage().await?));
    let mut plugged = power.receive_on_battery_changed().await;
    let mut charge = device.receive_percentage_changed().await;
    loop {
        tokio::select! {
            Some(change) = plugged.next() => drop(tx.send(Event::Battery(change.get().await?))),
            Some(change) = charge.next() => drop(tx.send(Event::Percent(change.get().await?))),
            else => return Ok(()),
        }
    }
}

async fn watch_files(tx: UnboundedSender<Event>, config_dir: PathBuf, sources: Vec<PathBuf>) -> std::io::Result<()> {
    let inotify = Inotify::init()?;
    let config_wd = inotify.watches().add(&config_dir, WatchMask::CLOSE_WRITE | WatchMask::MOVED_TO | WatchMask::DELETE)?;
    for src in &sources {
        let mask = WatchMask::CREATE | WatchMask::DELETE | WatchMask::MOVED_TO | WatchMask::MOVED_FROM;
        inotify.watches().add(src, mask)?;
    }
    let mut events = inotify.into_event_stream([0u8; 4096])?;
    while let Some(Ok(ev)) = events.next().await {
        if ev.wd == config_wd {
            if ev.name.is_some_and(|n| n == "config.toml") {
                let _ = tx.send(Event::ConfigChanged);
            }
            continue;
        }
        // Steam touches a workshop folder many times while downloading: wait for 2 s of quiet.
        while let Ok(Some(Ok(more))) = tokio::time::timeout(Duration::from_secs(2), events.next()).await {
            if more.wd == config_wd {
                let _ = tx.send(Event::ConfigChanged);
            }
        }
        let _ = tx.send(Event::LibraryChanged);
    }
    Ok(())
}

async fn serve_client(stream: UnixStream, tx: UnboundedSender<Event>, mut status: watch::Receiver<String>) {
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let Ok(req) = serde_json::from_str::<Value>(&line) else { return };
        if req["cmd"] == "watch" {
            // One status line now, then one per change, until the client goes away.
            loop {
                let line = status.borrow_and_update().clone() + "\n";
                if write.write_all(line.as_bytes()).await.is_err() || status.changed().await.is_err() {
                    return;
                }
            }
        }
        let (reply_tx, reply_rx) = oneshot::channel();
        if tx.send(Event::Request(req, reply_tx)).is_err() {
            return;
        }
        let Ok(reply) = reply_rx.await else { return };
        if write.write_all(format!("{reply}\n").as_bytes()).await.is_err() {
            return;
        }
    }
}

fn notify_ready() {
    if let Some(path) = std::env::var_os("NOTIFY_SOCKET") {
        if let Ok(sock) = std::os::unix::net::UnixDatagram::unbound() {
            let _ = sock.send_to(b"READY=1", path);
        }
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Res<()> {
    let io = |e: std::io::Error| e.to_string();
    let dirs = Dirs {
        config: xdg("XDG_CONFIG_HOME", ".config").join("omawe"),
        cache: xdg("XDG_CACHE_HOME", ".cache").join("omawe"),
        socket: xdg("XDG_RUNTIME_DIR", ".cache").join("omawe.sock"),
    };
    let data = xdg("XDG_DATA_HOME", ".local/share").join("omawe");
    for dir in [&dirs.config, &data, &dirs.cache.join("twins"), &dirs.cache.join("thumbs")] {
        std::fs::create_dir_all(dir).map_err(io)?;
    }

    if std::os::unix::net::UnixStream::connect(&dirs.socket).is_ok() {
        return Err("omawed уже запущен".into());
    }
    let _ = std::fs::remove_file(&dirs.socket);
    let listener = UnixListener::bind(&dirs.socket).map_err(io)?;

    let (tx, mut rx) = mpsc::unbounded_channel();
    let (hypr_tx, mut hypr_rx) = mpsc::channel(1);
    let (status, status_rx) = watch::channel(String::new());
    let db = Db::open(&data.join("library.db"))?;
    let overrides: Props = db.state("overrides").and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    let mut d = Daemon {
        cfg: config::load(&dirs.config.join("config.toml"), &overrides).unwrap_or_else(|e| {
            eprintln!("config.toml ignored: {e}");
            Config::default()
        }),
        assign: db.state("assign").and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default(),
        overrides,
        manual_pause: db.state("manual_pause").as_deref() == Some("1"),
        db,
        dirs,
        tx: tx.clone(),
        status,
        sources: Vec::new(),
        on_battery: discharging(),
        snap: Default::default(),
        slots: BTreeMap::new(),
        error: String::new(),
        spawned: 0,
        thumbing: false,
        percent: 100.0,
        testing: None,
        interval_at: None,
        hours_at: None,
        settle_until: None,
    };
    d.sources = d.find_sources();
    d.rescan()?;
    d.refresh_snapshot().await;
    if d.cfg.playlist.on_login {
        let cmd = if d.cfg.playlist.random { "random" } else { "next" };
        if let Err(e) = d.step(cmd, &d.playlist_filter()) {
            eprintln!("playlist: {e}");
        }
    }
    d.schedule();

    tokio::spawn(hypr::listen(hypr_tx));
    tokio::spawn(watch_battery(tx.clone()));
    tokio::spawn(watch_files(tx.clone(), d.dirs.config.clone(), d.sources.clone()));
    let client_tx = tx.clone();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            tokio::spawn(serve_client(stream, client_tx.clone(), status_rx.clone()));
        }
    });

    let mut term = signal(SignalKind::terminate()).map_err(io)?;
    let mut int = signal(SignalKind::interrupt()).map_err(io)?;
    notify_ready();

    loop {
        d.reconcile();
        let now = d.status_json().to_string();
        d.status.send_if_modified(|s| if *s != now { *s = now; true } else { false });

        // Earliest playlist deadline, if a playlist is configured at all.
        let deadline = [d.interval_at, d.hours_at, d.settle_until].into_iter().flatten().min();
        tokio::select! {
            Some(ev) = rx.recv() => match ev {
                Event::Request(req, reply) => {
                    let out = match d.handle(&req) {
                        Ok((text, data)) => json!({"ok": true, "text": text, "data": data}),
                        Err(e) => json!({"ok": false, "error": e}),
                    };
                    let _ = reply.send(out);
                }
                Event::Battery(on) => d.on_battery = on,
                Event::Frame(pid, drawn) => d.on_frame(pid, drawn),
                Event::Exit(pid) => d.on_exit(pid),
                Event::LibraryChanged => {
                    if let Err(e) = d.rescan() {
                        eprintln!("rescan failed: {e}");
                    }
                }
                Event::ConfigChanged => {
                    d.reload_config();
                    d.refresh_snapshot().await;
                }
                Event::ThumbsDone => d.thumbing = false,
                Event::Percent(p) => d.percent = p,
                Event::Color(id, color) => d.db.set_color(&id, &color),
                Event::Tested(id, verdict, frame) => d.on_tested(&id, verdict, frame),
                Event::TestsDone => d.testing = None,
                Event::Workshop(reply) => d.on_workshop(&reply),
            },
            Some(()) = hypr_rx.recv() => d.refresh_snapshot().await,
            _ = tokio::time::sleep_until(deadline.unwrap_or_else(Instant::now)), if deadline.is_some() => {
                if d.settle_until.is_some_and(|t| t <= Instant::now()) {
                    d.settle_until = None;
                }
                if [d.interval_at, d.hours_at].into_iter().flatten().any(|t| t <= Instant::now()) {
                    d.on_tick();
                }
            }
            _ = term.recv() => break,
            _ = int.recv() => break,
        }
    }

    for slot in d.slots.values_mut() {
        terminate(&mut slot.cur);
        terminate(&mut slot.old);
    }
    let _ = std::fs::remove_file(&d.dirs.socket);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn property_values_are_checked_and_normalised() {
        let color = json!({"type": "color"});
        assert_eq!(check_prop(&color, "1 0 0.5").unwrap(), "1.00000 0.00000 0.50000");
        assert!(check_prop(&color, "255 0 0").is_err());
        assert!(check_prop(&color, "red").is_err());
        assert_eq!(check_prop(&json!({"type": "Bool"}), "true").unwrap(), "1");
        assert_eq!(check_prop(&json!({"type": "slider", "min": 0, "max": 2}), "9").unwrap(), "2");
        assert!(check_prop(&json!({"type": "slider"}), "NaN").is_err());
        let combo = json!({"type": "combo", "options": [{"label": "A", "value": "1"}, {"label": "B", "value": 2}]});
        assert_eq!(check_prop(&combo, "2").unwrap(), "2");
        assert!(check_prop(&combo, "3").is_err());
        assert!(check_prop(&json!({"type": "file"}), "/etc/passwd").is_err());
    }
}
