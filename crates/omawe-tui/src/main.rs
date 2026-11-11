//! omawe-tui — keyboard-first browser for the wallpaper library.
//!
//! A front end only: every action is a request to omawed over its socket, the same
//! protocol the `omawe` CLI speaks. Previews are drawn with half-block characters, which
//! every terminal shows; ImageMagick does the decoding and scaling.

use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Clear, List, ListItem, ListState, Paragraph, Wrap};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::process::Command;

const KINDS: [&str; 5] = ["", "scene", "video", "web", "preset"];

fn call(cmd: &str, args: &[&str], opts: Value) -> Result<Value, String> {
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_default();
    let mut stream = UnixStream::connect(format!("{runtime}/omawe.sock")).map_err(|_| "omawed is not running".to_string())?;
    writeln!(stream, "{}", json!({"cmd": cmd, "args": args, "opts": opts})).map_err(|e| e.to_string())?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).map_err(|e| e.to_string())?;
    let reply: Value = serde_json::from_str(&line).map_err(|e| e.to_string())?;
    if reply["ok"] == true {
        Ok(reply["data"].clone())
    } else {
        Err(reply["error"].as_str().unwrap_or("no reply").to_string())
    }
}

fn hex(value: &str) -> Option<Color> {
    let h = value.trim().trim_matches('"').strip_prefix('#')?;
    let byte = |i: usize| u8::from_str_radix(h.get(i..i + 2)?, 16).ok();
    Some(Color::Rgb(byte(0)?, byte(2)?, byte(4)?))
}

/// Colours of the Omarchy theme in use, so the TUI matches the rest of the desktop.
struct Palette {
    accent: Color,
    text: Color,
    muted: Color,
}

fn palette() -> Palette {
    let state = std::env::var("XDG_STATE_HOME").unwrap_or_else(|_| format!("{}/.local/state", std::env::var("HOME").unwrap_or_default()));
    let toml = std::fs::read_to_string(format!("{state}/omarchy/current/theme/colors.toml")).unwrap_or_default();
    let get = |key: &str| toml.lines().find_map(|l| l.strip_prefix(key)?.trim_start().strip_prefix('=').and_then(hex));
    Palette {
        accent: get("accent").unwrap_or(Color::Cyan),
        text: get("foreground").unwrap_or(Color::Reset),
        muted: get("dark_foreground").unwrap_or(Color::DarkGray),
    }
}

/// Wallpaper titles and property labels come from Workshop authors: keep them to one clean line.
fn clean(text: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in text.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag && !c.is_control() => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

struct Preview {
    key: String,
    width: u16,
    height: u16,
    /// width × height×2 RGB pixels: two per cell, drawn as an upper half block over a background.
    rgb: Vec<u8>,
}

fn load_preview(path: &str, width: u16, height: u16) -> Option<Vec<u8>> {
    let size = format!("{}x{}", width, height * 2);
    let out = Command::new("magick")
        .arg(format!("{path}[0]"))
        .args(["-resize", &format!("{size}^"), "-gravity", "center", "-extent", &size, "-depth", "8", "rgb:-"])
        .output()
        .ok()?;
    (out.stdout.len() == usize::from(width) * usize::from(height) * 6).then_some(out.stdout)
}

#[derive(PartialEq)]
enum Mode {
    Library,
    Search,
    Props,
    /// Typing a new value for the selected property.
    Edit(String),
}

struct App {
    ru: bool,
    colors: Palette,
    all: Vec<Value>,
    shown: Vec<usize>,
    list: ListState,
    query: String,
    kind: usize,
    fav_only: bool,
    status: Value,
    /// Index into the status' monitor list; None = every monitor.
    monitor: Option<usize>,
    mode: Mode,
    props: Vec<Value>,
    prop: ListState,
    message: String,
    preview: Option<Preview>,
}

impl App {
    fn t<'a>(&self, en: &'a str, ru: &'a str) -> &'a str {
        if self.ru { ru } else { en }
    }

    fn monitors(&self) -> Vec<String> {
        self.status["monitors"].as_object().map(|m| m.keys().cloned().collect()).unwrap_or_default()
    }

    fn target(&self) -> Value {
        match self.monitor.and_then(|i| self.monitors().get(i).cloned()) {
            Some(m) => json!({"monitor": m}),
            None => json!({}),
        }
    }

    fn selected(&self) -> Option<&Value> {
        self.shown.get(self.list.selected()?).map(|&i| &self.all[i])
    }

    /// The wallpaper the property editor works on: the one playing on the target monitor.
    fn current_id(&self) -> String {
        let per_monitor = self.target()["monitor"].as_str().map(|m| self.status["monitors"][m]["id"].clone());
        per_monitor.unwrap_or(self.status["id"].clone()).as_str().unwrap_or_default().to_string()
    }

    fn refresh(&mut self) {
        match call("status", &[], json!({})) {
            Ok(s) => self.status = s,
            Err(e) => self.message = e,
        }
    }

    fn reload(&mut self) {
        match call("list", &[], json!({"thumbs": true})) {
            Ok(Value::Array(list)) => self.all = list,
            Ok(_) => {}
            Err(e) => self.message = e,
        }
        self.filter();
        self.refresh();
    }

    fn filter(&mut self) {
        let query = self.query.to_lowercase();
        let before = self.shown.len();
        self.shown = (0..self.all.len())
            .filter(|&i| {
                let w = &self.all[i];
                w["compat"] != "unsupported"
                    && (self.kind == 0 || w["kind"] == KINDS[self.kind])
                    && (!self.fav_only || w["fav"] == true)
                    && w["title"].as_str().is_some_and(|t| t.to_lowercase().contains(&query))
            })
            .collect();
        // A narrower list starts from its top; an unchanged one (after a favourite toggle) keeps its place.
        let keep = if self.shown.len() == before { self.list.selected().unwrap_or(0).min(before.saturating_sub(1)) } else { 0 };
        self.list.select((!self.shown.is_empty()).then_some(keep));
    }

    fn load_props(&mut self) {
        let id = self.current_id();
        match call("props", &[&id], self.target()) {
            Ok(d) => {
                // Labels and separators carry nothing to edit.
                self.props = d["props"].as_array().cloned().unwrap_or_default();
                self.props.retain(|p| !matches!(p["type"].as_str(), Some("text" | "group")));
                let keep = self.prop.selected().unwrap_or(0).min(self.props.len().saturating_sub(1));
                self.prop.select((!self.props.is_empty()).then_some(keep));
            }
            Err(e) => self.message = e,
        }
    }

    /// Run a daemon command and show its error, if any, in the status line.
    fn act(&mut self, cmd: &str, args: &[&str], opts: Value) {
        self.message = call(cmd, args, opts).err().unwrap_or_default();
        self.refresh();
    }

    fn set_prop(&mut self, value: Option<&str>) {
        let Some(key) = self.prop.selected().and_then(|i| self.props[i]["key"].as_str().map(String::from)) else { return };
        let id = self.current_id();
        let mut args = vec![id.as_str(), key.as_str()];
        args.extend(value);
        self.act("prop", &args, self.target());
        self.load_props();
    }

    /// Enter on a property: flip a switch, step a list, or open the editor for anything typed.
    fn edit_prop(&mut self) {
        let Some(p) = self.prop.selected().map(|i| self.props[i].clone()) else { return };
        let value = p["value"].as_str().unwrap_or_default();
        match p["type"].as_str() {
            Some("bool") => self.set_prop(Some(if value == "1" || value == "true" { "0" } else { "1" })),
            Some("combo") => {
                let options: Vec<String> = p["options"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|o| o["value"].as_str().map(String::from).unwrap_or_else(|| o["value"].to_string()))
                    .collect();
                let next = options.iter().position(|o| o == value).map_or(0, |i| (i + 1) % options.len().max(1));
                if let Some(option) = options.get(next) {
                    self.set_prop(Some(option));
                }
            }
            Some("slider" | "color" | "textinput") => self.mode = Mode::Edit(value.to_string()),
            _ => self.message = self.t("this property cannot be edited", "это свойство нельзя изменить").into(),
        }
    }

    /// Left/right on a slider.
    fn nudge(&mut self, direction: f64) {
        let Some(p) = self.prop.selected().map(|i| self.props[i].clone()) else { return };
        if p["type"] != "slider" {
            return;
        }
        let (min, max) = (p["min"].as_f64().unwrap_or(0.0), p["max"].as_f64().unwrap_or(1.0));
        let step = p["step"].as_f64().filter(|s| *s > 0.0).unwrap_or((max - min) / 20.0);
        let value = p["value"].as_str().and_then(|v| v.parse::<f64>().ok()).unwrap_or(min);
        self.set_prop(Some(&format!("{:.4}", value + direction * step)));
    }

    fn key(&mut self, code: KeyCode, ctrl: bool) -> bool {
        self.message.clear();
        match (&mut self.mode, code) {
            (_, KeyCode::Char('c')) if ctrl => return false,

            (Mode::Search, KeyCode::Esc | KeyCode::Enter) => self.mode = Mode::Library,
            (Mode::Search, KeyCode::Backspace) => {
                self.query.pop();
                self.filter();
            }
            (Mode::Search, KeyCode::Char(c)) => {
                self.query.push(c);
                self.filter();
            }

            (Mode::Edit(_), KeyCode::Esc) => self.mode = Mode::Props,
            (Mode::Edit(text), KeyCode::Backspace) => drop(text.pop()),
            (Mode::Edit(text), KeyCode::Char(c)) => text.push(c),
            (Mode::Edit(text), KeyCode::Enter) => {
                let value = std::mem::take(text);
                self.mode = Mode::Props;
                self.set_prop(Some(&value));
            }

            (Mode::Props, KeyCode::Esc | KeyCode::Char('p' | 'q')) => self.mode = Mode::Library,
            (Mode::Props, KeyCode::Down | KeyCode::Char('j')) => self.prop.select_next(),
            (Mode::Props, KeyCode::Up | KeyCode::Char('k')) => self.prop.select_previous(),
            (Mode::Props, KeyCode::Enter | KeyCode::Char(' ')) => self.edit_prop(),
            (Mode::Props, KeyCode::Left | KeyCode::Char('h')) => self.nudge(-1.0),
            (Mode::Props, KeyCode::Right | KeyCode::Char('l')) => self.nudge(1.0),
            (Mode::Props, KeyCode::Char('r')) => self.set_prop(None),
            (Mode::Props, KeyCode::Char('R')) => {
                let id = self.current_id();
                self.act("preset", &["reset", &id], self.target());
                self.load_props();
            }

            (Mode::Library, KeyCode::Char('q')) => return false,
            (Mode::Library, KeyCode::Esc) if self.query.is_empty() => return false,
            (Mode::Library, KeyCode::Esc) => {
                self.query.clear();
                self.filter();
            }
            (Mode::Library, KeyCode::Char('/')) => self.mode = Mode::Search,
            (Mode::Library, KeyCode::Down | KeyCode::Char('j')) => self.list.select_next(),
            (Mode::Library, KeyCode::Up | KeyCode::Char('k')) => self.list.select_previous(),
            (Mode::Library, KeyCode::PageDown) => self.list.scroll_down_by(10),
            (Mode::Library, KeyCode::PageUp) => self.list.scroll_up_by(10),
            (Mode::Library, KeyCode::Home | KeyCode::Char('g')) => self.list.select_first(),
            (Mode::Library, KeyCode::End | KeyCode::Char('G')) => self.list.select_last(),
            (Mode::Library, KeyCode::Enter) => {
                if let Some(id) = self.selected().and_then(|w| w["id"].as_str().map(String::from)) {
                    self.act("apply", &[&id], self.target());
                }
            }
            (Mode::Library, KeyCode::Char('f')) => {
                if let Some(id) = self.selected().and_then(|w| w["id"].as_str().map(String::from)) {
                    self.act("fav", &[&id], json!({}));
                    self.reload();
                }
            }
            (Mode::Library, KeyCode::Char('F')) => {
                self.fav_only = !self.fav_only;
                self.filter();
            }
            (Mode::Library, KeyCode::Char('t')) => {
                self.kind = (self.kind + 1) % KINDS.len();
                self.filter();
            }
            (Mode::Library, KeyCode::Char('m') | KeyCode::Tab) => {
                let count = self.monitors().len();
                self.monitor = match self.monitor {
                    None if count > 0 => Some(0),
                    Some(i) if i + 1 < count => Some(i + 1),
                    _ => None,
                };
            }
            (Mode::Library, KeyCode::Char(' ')) => self.act("toggle", &[], json!({})),
            (Mode::Library, KeyCode::Char('r')) => self.act("random", &[], self.target()),
            (Mode::Library, KeyCode::Char('s')) => self.act("mute", &[], json!({})),
            (Mode::Library, KeyCode::Char('o')) => {
                if let Some(id) = self.selected().and_then(|w| w["id"].as_str().map(String::from)) {
                    self.act("open", &[&id], json!({}));
                }
            }
            (Mode::Library, KeyCode::Char('p')) => {
                if self.current_id().is_empty() {
                    self.message = self.t("no wallpaper is playing", "обои не выбраны").into();
                } else {
                    self.mode = Mode::Props;
                    self.load_props();
                }
            }
            _ => {}
        }
        true
    }

    fn draw_preview(&mut self, frame: &mut Frame, area: Rect) {
        let Some((id, path)) = self.selected().map(|w| {
            let image = w["thumb"].as_str().filter(|t| !t.is_empty()).or(w["preview"].as_str()).unwrap_or_default();
            (w["id"].as_str().unwrap_or_default().to_string(), image.to_string())
        }) else {
            return;
        };
        if area.width == 0 || area.height == 0 || path.is_empty() {
            return;
        }
        let fresh = self.preview.as_ref().is_some_and(|p| p.key == id && p.width == area.width && p.height == area.height);
        if !fresh {
            let rgb = load_preview(&path, area.width, area.height).unwrap_or_default();
            self.preview = Some(Preview { key: id, width: area.width, height: area.height, rgb });
        }
        let Some(p) = self.preview.as_ref().filter(|p| !p.rgb.is_empty()) else { return };
        let pixel = |x: u16, y: u16| {
            let i = (usize::from(y) * usize::from(p.width) + usize::from(x)) * 3;
            Color::Rgb(p.rgb[i], p.rgb[i + 1], p.rgb[i + 2])
        };
        for y in 0..area.height {
            for x in 0..area.width {
                if let Some(cell) = frame.buffer_mut().cell_mut((area.x + x, area.y + y)) {
                    cell.set_symbol("▀").set_fg(pixel(x, y * 2)).set_bg(pixel(x, y * 2 + 1));
                }
            }
        }
    }

    fn draw(&mut self, frame: &mut Frame) {
        let (accent, text, muted) = (self.colors.accent, self.colors.text, self.colors.muted);
        let [head, body, foot] = Layout::vertical([Constraint::Length(1), Constraint::Fill(1), Constraint::Length(1)]).areas(frame.area());
        let [left, right] = Layout::horizontal([Constraint::Percentage(42), Constraint::Fill(1)]).areas(body);

        // ---- Header: what is playing, and what the next action is aimed at.
        let state = self.status["state"].as_str().unwrap_or("?");
        let reason = self.status["reason"].as_str().map(|r| format!(" ({r})")).unwrap_or_default();
        let playing = clean(self.status["title"].as_str().unwrap_or("—"));
        let target = self.target()["monitor"].as_str().map(String::from).unwrap_or_else(|| self.t("all monitors", "все мониторы").into());
        frame.render_widget(
            Line::from(vec![
                Span::styled(" omawe ", Style::new().fg(Color::Black).bg(accent).bold()),
                Span::styled(format!(" {state}{reason} "), Style::new().fg(accent)),
                Span::styled(playing, Style::new().fg(text)),
                Span::styled(format!("  → {target}"), Style::new().fg(muted)),
            ]),
            head,
        );

        // ---- Left: the library, or the properties of the playing wallpaper.
        let border = Style::new().fg(muted);
        if matches!(self.mode, Mode::Props | Mode::Edit(_)) {
            let items: Vec<ListItem> = self
                .props
                .iter()
                .map(|p| {
                    let label = clean(p["text"].as_str().unwrap_or_default());
                    let label = if label.is_empty() || label.starts_with("ui_") { p["key"].as_str().unwrap_or_default().to_string() } else { label };
                    let changed = if p["changed"] == true { "● " } else { "  " };
                    ListItem::new(Line::from(vec![
                        Span::styled(changed, Style::new().fg(accent)),
                        Span::styled(format!("{label}: "), Style::new().fg(text)),
                        Span::styled(clean(p["value"].as_str().unwrap_or_default()), Style::new().fg(accent)),
                        Span::styled(format!("  {}", p["type"].as_str().unwrap_or_default()), Style::new().fg(muted)),
                    ]))
                })
                .collect();
            let title = format!(" {} ", self.t("Properties", "Свойства"));
            let list = List::new(items).block(Block::bordered().title(title).border_style(border)).highlight_style(Style::new().reversed());
            frame.render_stateful_widget(list, left, &mut self.prop);
            if self.props.is_empty() {
                let empty = Paragraph::new(self.t("This wallpaper has no settings", "У этих обоев нет настроек")).style(Style::new().fg(muted));
                frame.render_widget(empty, Block::bordered().inner(left));
            }
        } else {
            let items: Vec<ListItem> = self
                .shown
                .iter()
                .map(|&i| {
                    let w = &self.all[i];
                    let mut marks = String::new();
                    if w["fav"] == true {
                        marks += "★ ";
                    }
                    if matches!(w["compat"].as_str(), Some("crash" | "black" | "noframe")) {
                        marks += "⚠ ";
                    }
                    if w["outdated"] == true {
                        marks += "↑ ";
                    }
                    ListItem::new(Line::from(vec![
                        Span::styled(marks, Style::new().fg(accent)),
                        Span::styled(clean(w["title"].as_str().unwrap_or_default()), Style::new().fg(text)),
                    ]))
                })
                .collect();
            let filters = [KINDS[self.kind], if self.fav_only { "★" } else { "" }].iter().filter(|s| !s.is_empty()).copied().collect::<Vec<_>>().join(" ");
            let title = format!(" {} {}/{} {filters} ", self.t("Library", "Библиотека"), self.shown.len(), self.all.len());
            let list = List::new(items).block(Block::bordered().title(title).border_style(border)).highlight_style(Style::new().reversed());
            frame.render_stateful_widget(list, left, &mut self.list);
        }

        // ---- Right: preview over details.
        let [picture, details] = Layout::vertical([Constraint::Fill(1), Constraint::Length(6)]).areas(right);
        let block = Block::bordered().border_style(border);
        let inner = block.inner(picture);
        frame.render_widget(block, picture);
        self.draw_preview(frame, inner);
        if let Some(w) = self.selected() {
            let tags = w["tags"].as_array().map(|t| t.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", ")).unwrap_or_default();
            let compat = match w["compat"].as_str() {
                Some("") | None => self.t("not tested", "не проверено"),
                Some(c) => c,
            };
            let lines = vec![
                Line::styled(clean(w["title"].as_str().unwrap_or_default()), Style::new().fg(accent).bold()),
                Line::styled(
                    [w["kind"].as_str().unwrap_or_default(), &clean(&tags), w["rating"].as_str().unwrap_or_default()]
                        .into_iter()
                        .filter(|part| !part.is_empty())
                        .collect::<Vec<_>>()
                        .join(" · "),
                    Style::new().fg(text),
                ),
                Line::styled(format!("{}: {compat}", self.t("compatibility", "совместимость")), Style::new().fg(text)),
                Line::styled(format!("{}: {}  ★ {}", self.t("subscribers", "подписчики"), w["subs"], w["favs"]), Style::new().fg(muted)),
            ];
            frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }).block(Block::bordered().border_style(border)), details);
        }

        // ---- Footer: the input line while typing, an error if there is one, otherwise the keys.
        let hint = match &self.mode {
            Mode::Search => Line::from(vec![Span::styled(" / ", Style::new().fg(accent)), Span::raw(format!("{}▏", self.query))]),
            Mode::Edit(value) => Line::from(vec![Span::styled(" = ", Style::new().fg(accent)), Span::raw(format!("{value}▏"))]),
            _ if !self.message.is_empty() => Line::styled(format!(" {}", self.message), Style::new().fg(Color::Red)),
            Mode::Props => Line::styled(
                self.t(" ↑↓ select · Enter change · ←→ slider · r reset · R reset all · Esc back", " ↑↓ выбор · Enter изменить · ←→ слайдер · r сброс · R сбросить все · Esc назад"),
                Style::new().fg(muted),
            ),
            Mode::Library => Line::styled(
                self.t(
                    " ↑↓ select · Enter apply · / search · t type · f ★ · F only ★ · m monitor · p properties · Space pause · s sound · r random · o Workshop · q quit",
                    " ↑↓ выбор · Enter применить · / поиск · t тип · f ★ · F только ★ · m монитор · p свойства · Пробел пауза · s звук · r случайные · o Workshop · q выход",
                ),
                Style::new().fg(muted),
            ),
        };
        frame.render_widget(Clear, foot);
        frame.render_widget(hint, foot);
    }
}

fn main() -> std::io::Result<()> {
    let home = std::env::var("HOME").unwrap_or_default();
    let config = std::env::var("XDG_CONFIG_HOME").unwrap_or_else(|_| format!("{home}/.config"));
    let mut app = App {
        ru: std::fs::read_to_string(format!("{config}/omarchy/language")).is_ok_and(|l| l.starts_with("ru")),
        colors: palette(),
        all: Vec::new(),
        shown: Vec::new(),
        list: ListState::default(),
        query: String::new(),
        kind: 0,
        fav_only: false,
        status: Value::Null,
        monitor: None,
        mode: Mode::Library,
        props: Vec::new(),
        prop: ListState::default(),
        message: String::new(),
        preview: None,
    };
    app.reload();
    // Start on the wallpaper that is playing.
    let playing = app.current_id();
    if let Some(row) = app.shown.iter().position(|&i| app.all[i]["id"] == playing.as_str()) {
        app.list.select(Some(row));
    }

    let mut terminal = ratatui::init();
    let result = loop {
        if let Err(e) = terminal.draw(|frame| app.draw(frame)) {
            break Err(e);
        }
        match event::read() {
            Ok(Event::Key(key)) if key.kind == KeyEventKind::Press => {
                if !app.key(key.code, key.modifiers.contains(KeyModifiers::CONTROL)) {
                    break Ok(());
                }
            }
            Ok(_) => {}
            Err(e) => break Err(e),
        }
    };
    ratatui::restore();
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn untrusted_text_is_flattened() {
        assert_eq!(clean("<b>Custom\n Text</b>\u{1b}[31m  2"), "Custom Text[31m 2");
        assert_eq!(hex("\"#89b4fa\""), Some(Color::Rgb(0x89, 0xb4, 0xfa)));
        assert_eq!(hex("#zzzzzz"), None);
    }
}
