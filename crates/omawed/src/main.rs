//! omawed — keeps linux-wallpaperengine running the way the session needs it.
//!
//! One task owns all state and reacts to events (CLI requests, Hyprland, UPower, inotify,
//! renderer exit). After every event it compares what should be running with what is,
//! and fixes the difference. Nothing wakes up on a timer while idle.

mod config;
mod db;
mod hypr;
mod project;
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

const APPID: &str = "431960";
/// Assignment key meaning "every monitor without its own wallpaper".
const ALL: &str = "*";

type Res<T> = Result<T, String>;
type Reply = (String, Value);

enum Event {
    Request(Value, oneshot::Sender<Value>),
    Battery(bool),
    /// The renderer wrote its first frame (true) or the wait timed out (false).
    Frame(u32, bool),
    Exit(u32),
    LibraryChanged,
    ConfigChanged,
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
    paused: bool,
    ready: bool,
    frame: PathBuf,
    /// Distinct wallpaper ids shown by this process.
    ids: Vec<String>,
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

struct Daemon {
    cfg: Config,
    db: Db,
    dirs: Dirs,
    tx: UnboundedSender<Event>,
    status: watch::Sender<String>,
    sources: Vec<PathBuf>,
    assign: BTreeMap<String, Assign>,
    manual_pause: bool,
    on_battery: bool,
    snap: hypr::Snapshot,
    cur: Option<Proc>,
    /// Previous renderer, kept on screen until `cur` has drawn its first frame.
    old: Option<Proc>,
    /// Arguments that crashed the renderer; not retried until something changes.
    crashed: Option<Vec<String>>,
    error: String,
    spawned: u32,
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

    // ---- policy ------------------------------------------------------------

    fn monitors(&self) -> Vec<String> {
        if self.snap.monitors.is_empty() {
            // No compositor answer (tests, early start): trust explicit per-monitor assignments.
            self.assign.keys().filter(|k| *k != ALL).cloned().collect()
        } else {
            self.snap.monitors.clone()
        }
    }

    fn shown(&self) -> Vec<(String, Assign, Wallpaper)> {
        let mut out = Vec::new();
        for mon in self.monitors() {
            let Some(a) = self.assign.get(&mon).or_else(|| self.assign.get(ALL)) else { continue };
            if let Ok(Some(w)) = self.db.get(&a.id) {
                out.push((mon, a.clone(), w));
            }
        }
        out
    }

    fn fps(&self) -> u32 {
        if self.on_battery { self.cfg.fps_battery } else { self.cfg.fps_ac }
    }

    /// Why the renderer should not exist at all right now.
    fn stop_reason(&self) -> Option<&'static str> {
        if self.cfg.stop_on_game && self.snap.game {
            Some("game")
        } else if self.on_battery && self.cfg.battery == "static" {
            Some("battery")
        } else {
            None
        }
    }

    fn pause_reason(&self) -> Option<&'static str> {
        if self.manual_pause {
            Some("manual")
        } else if self.cfg.pause_fullscreen && self.snap.covered {
            Some("fullscreen")
        } else if self.on_battery && self.cfg.battery == "pause" {
            Some("battery")
        } else {
            None
        }
    }

    fn renderer_args(&self) -> Option<(Vec<String>, Vec<String>)> {
        let shown = self.shown();
        if shown.is_empty() || self.stop_reason().is_some() {
            return None;
        }
        let c = &self.cfg;
        let mut args = vec!["--fps".to_string(), self.fps().to_string()];
        if c.silent {
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
        let mut ids = BTreeSet::new();
        for (mon, a, w) in shown {
            // A preset is another wallpaper plus property overrides.
            let mut dir = w.dir.clone();
            if w.kind == "preset" {
                match self.db.get(&w.dependency) {
                    Ok(Some(base)) => dir = base.dir,
                    _ => continue,
                }
                if let Ok(Value::Object(props)) = serde_json::from_str(&w.preset) {
                    for (k, v) in props {
                        let v = match v {
                            Value::String(s) => s,
                            Value::Bool(b) => (b as u8).to_string(),
                            other => other.to_string(),
                        };
                        args.extend(["--set-property".into(), format!("{k}={v}")]);
                    }
                }
            }
            args.extend(["--screen-root".into(), mon, "--bg".into(), dir]);
            args.extend(["--scaling".into(), a.scaling.unwrap_or_else(|| c.scaling.clone())]);
            args.extend(["--clamp".into(), a.clamp.unwrap_or_else(|| c.clamp.clone())]);
            ids.insert(w.id);
        }
        (!ids.is_empty()).then(|| (args, ids.into_iter().collect()))
    }

    // ---- renderer ----------------------------------------------------------

    fn twin(&self, ids: &[String]) -> PathBuf {
        self.dirs.cache.join("twins").join(format!("{}.png", ids.join("+")))
    }

    fn spawn(&mut self, args: Vec<String>, ids: Vec<String>) -> Res<Proc> {
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
        Ok(Proc { pid, args, paused: false, ready: false, frame, ids })
    }

    /// Make the running renderer match the policy.
    fn reconcile(&mut self) {
        let Some((args, ids)) = self.renderer_args() else {
            // The still frame under the renderer stays as Omarchy's background.
            terminate(&mut self.cur);
            terminate(&mut self.old);
            return;
        };
        if self.cur.as_ref().is_none_or(|c| c.args != args) && self.crashed.as_ref() != Some(&args) {
            match self.spawn(args, ids) {
                Ok(new) => {
                    self.error.clear();
                    // Swap in progress: drop the unfinished renderer, keep the visible one.
                    let mut prev = self.cur.replace(new);
                    if self.old.is_some() {
                        terminate(&mut prev);
                    } else {
                        self.old = prev;
                    }
                }
                Err(e) => {
                    self.error = e;
                    return;
                }
            }
        }
        let want_paused = self.pause_reason().is_some();
        if let Some(c) = self.cur.as_mut().filter(|c| c.ready && c.paused != want_paused) {
            signal_group(c.pid, if want_paused { libc::SIGSTOP } else { libc::SIGCONT });
            c.paused = want_paused;
        }
    }

    fn sync_omarchy_bg(&self) {
        let Some(c) = &self.cur else { return };
        let twin = self.twin(&c.ids);
        if self.cfg.sync_omarchy_bg && twin.exists() {
            run("omarchy-theme-bg-set", &[&twin.to_string_lossy()]);
        }
    }

    fn on_frame(&mut self, pid: u32, drawn: bool) {
        let Some(c) = self.cur.as_mut().filter(|c| c.pid == pid) else { return };
        c.ready = true;
        let (frame, ids) = (c.frame.clone(), c.ids.clone());
        terminate(&mut self.old);
        if drawn {
            let _ = std::fs::rename(frame, self.twin(&ids));
            self.sync_omarchy_bg();
            if let [id] = ids.as_slice() {
                self.db.set_compat(id, "ok");
            }
        }
    }

    fn on_exit(&mut self, pid: u32) {
        if self.old.as_ref().is_some_and(|o| o.pid == pid) {
            self.old = None;
        }
        let Some(dead) = self.cur.take_if(|c| c.pid == pid) else { return };
        let _ = std::fs::remove_file(&dead.frame);
        // A renderer that died while stopped was most likely dropped by the compositor; just start again.
        if !dead.paused {
            if let ([id], false) = (dead.ids.as_slice(), dead.ready) {
                self.db.set_compat(id, "crash");
            }
            self.error = "рендерер завершился с ошибкой".into();
            self.crashed = Some(dead.args);
            if self.cfg.notify {
                run("omarchy-notification-send", &["Wallpaper Engine: обои не запустились", "-t", "4000"]);
            }
        }
        // The previous renderer is still on screen: keep it.
        self.cur = self.old.take();
    }

    // ---- requests ----------------------------------------------------------

    fn save(&self) {
        self.db.set_state("assign", &serde_json::to_string(&self.assign).unwrap_or_default());
        self.db.set_state("manual_pause", if self.manual_pause { "1" } else { "0" });
    }

    fn status_json(&self) -> Value {
        let shown = self.shown();
        let state = match (&self.cur, shown.is_empty()) {
            (_, true) => "off",
            (Some(c), _) if c.paused => "paused",
            (Some(_), _) => "running",
            (None, _) => "stopped",
        };
        let reason = match state {
            "paused" => self.pause_reason(),
            "stopped" => self.stop_reason().or((!self.error.is_empty()).then_some("error")),
            _ => None,
        };
        json!({
            "state": state,
            "reason": reason,
            "error": self.error,
            "title": shown.first().map(|(_, _, w)| w.title.as_str()),
            "id": shown.first().map(|(_, _, w)| w.id.as_str()),
            "pid": self.cur.as_ref().map(|c| c.pid),
            "fps": self.fps(),
            "on_battery": self.on_battery,
            "monitors": shown.iter().map(|(m, _, w)| (m.clone(), json!({"id": w.id, "title": w.title}))).collect::<BTreeMap<_, _>>(),
        })
    }

    fn apply(&mut self, w: &Wallpaper, opts: &Value) -> Res<Reply> {
        if w.compat == "unsupported" {
            return Err(format!("«{}»: обои типа application в linux-wallpaperengine не работают", w.title));
        }
        if w.kind == "preset" && self.db.get(&w.dependency)?.is_none() {
            return Err(format!("«{}» — пресет, сначала подпишитесь на базовые обои {}", w.title, w.dependency));
        }
        let opt = |k: &str| opts.get(k).and_then(Value::as_str).map(String::from);
        let a = Assign { id: w.id.clone(), scaling: opt("scaling"), clamp: opt("clamp") };
        match opt("monitor") {
            Some(mon) => self.assign.insert(mon, a),
            None => {
                self.assign.clear();
                self.assign.insert(ALL.into(), a)
            }
        };
        self.crashed = None;
        self.save();
        Ok((w.title.clone(), json!(w)))
    }

    /// next / prev / random within the (optionally filtered) library.
    fn step(&mut self, cmd: &str, opts: &Value) -> Res<Reply> {
        let list: Vec<_> = self.db.list(opts)?.into_iter().filter(|w| w.compat != "unsupported" && w.compat != "crash").collect();
        if list.is_empty() {
            return Err("подходящих обоев нет".into());
        }
        let mon = opts.get("monitor").and_then(Value::as_str).unwrap_or(ALL);
        let cur = self.assign.get(mon).or_else(|| self.assign.get(ALL)).map(|a| a.id.clone());
        let pos = list.iter().position(|w| Some(&w.id) == cur.as_ref());
        let n = list.len();
        let i = match (cmd, pos) {
            // Good enough to shuffle wallpapers; not for anything that needs real randomness.
            ("random", _) => std::time::UNIX_EPOCH.elapsed().map_or(0, |d| d.subsec_nanos() as usize) % n,
            ("prev", Some(p)) => (p + n - 1) % n,
            (_, Some(p)) => (p + 1) % n,
            (_, None) => 0,
        };
        self.apply(&list[i].clone(), opts)
    }

    fn handle(&mut self, req: &Value) -> Res<Reply> {
        let opts = req.get("opts").cloned().unwrap_or(json!({}));
        let arg = req.pointer("/args/0").and_then(Value::as_str);
        let need = |what: &str| arg.ok_or_else(|| format!("нужен аргумент: {what}"));
        match req.get("cmd").and_then(Value::as_str).unwrap_or_default() {
            "status" => {
                let s = self.status_json();
                let title = s["title"].as_str().unwrap_or("обои не выбраны");
                let reason = s["reason"].as_str().map(|r| format!(" ({r})")).unwrap_or_default();
                Ok((format!("{}{reason}: {title}", s["state"].as_str().unwrap_or_default()), s))
            }
            "list" => {
                let list = self.db.list(&opts)?;
                let text = list
                    .iter()
                    .map(|w| format!("{}\t{}\t{}\t{}{}", w.id, w.kind, w.title, w.compat, if w.fav { "\t★" } else { "" }))
                    .collect::<Vec<_>>()
                    .join("\n");
                Ok((text, json!(list)))
            }
            "apply" => {
                let w = self.db.find(need("id или название")?)?;
                self.apply(&w, &opts)
            }
            cmd @ ("next" | "prev" | "random") => self.step(cmd, &opts),
            "stop" => {
                match opts.get("monitor").and_then(Value::as_str) {
                    Some(mon) => drop(self.assign.remove(mon)),
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
                self.crashed = None;
                self.save();
                Ok((if self.manual_pause { "пауза" } else { "воспроизведение" }.into(), json!(self.manual_pause)))
            }
            "fav" => {
                let w = self.db.find(need("id или название")?)?;
                let fav = self.db.toggle_fav(&w.id)?;
                Ok((format!("{}{}", if fav { "★ " } else { "" }, w.title), json!(fav)))
            }
            "rescan" => {
                self.sources = self.find_sources();
                let (updated, removed) = self.rescan()?;
                Ok((format!("обновлено {updated}, удалено {removed}"), json!({"updated": updated, "removed": removed})))
            }
            // Re-assert the still frame as Omarchy's background (after a theme change).
            "refresh" => {
                self.sync_omarchy_bg();
                Ok((String::new(), Value::Null))
            }
            "config" => Ok((format!("{:#?}", self.cfg), Value::Null)),
            other => Err(format!("неизвестная команда: {other}")),
        }
    }

    fn reload_config(&mut self) {
        match config::load(&self.dirs.config.join("config.toml")) {
            Ok(cfg) => {
                self.crashed = None;
                self.cfg = cfg;
            }
            // Keep the working settings; a half-saved file must not reset them.
            Err(e) => eprintln!("config.toml ignored: {e}"),
        }
    }

    async fn refresh_snapshot(&mut self) {
        if let Some(snap) = hypr::snapshot(self.cfg.pause_maximized).await {
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

async fn watch_battery(tx: UnboundedSender<Event>) -> zbus::Result<()> {
    let proxy = UPowerProxy::new(&zbus::Connection::system().await?).await?;
    let _ = tx.send(Event::Battery(proxy.on_battery().await?));
    let mut changes = proxy.receive_on_battery_changed().await;
    while let Some(change) = changes.next().await {
        let _ = tx.send(Event::Battery(change.get().await?));
    }
    Ok(())
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
    for dir in [&dirs.config, &data, &dirs.cache.join("twins")] {
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
    let mut d = Daemon {
        cfg: config::load(&dirs.config.join("config.toml")).unwrap_or_else(|e| {
            eprintln!("config.toml ignored: {e}");
            Config::default()
        }),
        assign: db.state("assign").and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default(),
        manual_pause: db.state("manual_pause").as_deref() == Some("1"),
        db,
        dirs,
        tx: tx.clone(),
        status,
        sources: Vec::new(),
        on_battery: discharging(),
        snap: Default::default(),
        cur: None,
        old: None,
        crashed: None,
        error: String::new(),
        spawned: 0,
    };
    d.sources = d.find_sources();
    d.rescan()?;
    d.refresh_snapshot().await;

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
            },
            Some(()) = hypr_rx.recv() => d.refresh_snapshot().await,
            _ = term.recv() => break,
            _ = int.recv() => break,
        }
    }

    terminate(&mut d.cur);
    terminate(&mut d.old);
    let _ = std::fs::remove_file(&d.dirs.socket);
    Ok(())
}
