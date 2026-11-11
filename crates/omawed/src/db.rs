//! Library index and daemon state, one SQLite file.

use crate::project::Project;
use rusqlite::{params, params_from_iter, Connection, OptionalExtension, Row};
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

type Res<T> = Result<T, String>;

fn e(err: rusqlite::Error) -> String {
    err.to_string()
}

#[derive(Serialize, Clone, Debug)]
pub struct Wallpaper {
    pub id: String,
    pub dir: String,
    pub title: String,
    pub kind: String,
    pub tags: Vec<String>,
    pub rating: String,
    /// Absolute path of the preview image, or "".
    pub preview: String,
    pub dependency: String,
    #[serde(skip)]
    pub preset: String,
    /// JSON object of property definitions from project.json, or "".
    #[serde(skip)]
    pub props: String,
    /// Cached WebP thumbnail, filled in on request.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub thumb: String,
    /// Workshop numbers from the last `workshop sync`; 0 when never fetched.
    pub subs: i64,
    pub favs: i64,
    /// The Workshop copy is newer than the files on disk.
    pub outdated: bool,
    /// Most vivid colour of the preview as #rrggbb, or "".
    pub color: String,
    pub fav: bool,
    /// "" (untested), "ok", "crash", "unsupported".
    pub compat: String,
}

const COLS: &str = "w.id, dir, title, kind, tags, rating, preview, dependency, preset, fav, compat, props,
    COALESCE(x.subs, 0), COALESCE(x.favs, 0), COALESCE(x.updated, 0) > w.mtime, COALESCE(x.color, '')";
const FROM: &str = "FROM wallpapers w LEFT JOIN extra x ON x.id = w.id";

fn row(r: &Row) -> rusqlite::Result<Wallpaper> {
    let dir: String = r.get(1)?;
    let preview: String = r.get(6)?;
    let tags: String = r.get(4)?;
    Ok(Wallpaper {
        id: r.get(0)?,
        preview: if preview.is_empty() { preview } else { format!("{dir}/{preview}") },
        dir,
        title: r.get(2)?,
        kind: r.get(3)?,
        tags: serde_json::from_str(&tags).unwrap_or_default(),
        rating: r.get(5)?,
        dependency: r.get(7)?,
        preset: r.get(8)?,
        fav: r.get(9)?,
        compat: r.get(10)?,
        props: r.get(11)?,
        thumb: String::new(),
        subs: r.get(12)?,
        favs: r.get(13)?,
        outdated: r.get(14)?,
        color: r.get(15)?,
    })
}

pub struct Db(Connection);

impl Db {
    pub fn open(path: &Path) -> Res<Db> {
        let c = Connection::open(path).map_err(e)?;
        c.execute_batch(
            "CREATE TABLE IF NOT EXISTS wallpapers(
               id TEXT PRIMARY KEY, dir TEXT NOT NULL, title TEXT NOT NULL, kind TEXT NOT NULL,
               tags TEXT NOT NULL, rating TEXT NOT NULL, preview TEXT NOT NULL, mtime INTEGER NOT NULL,
               dependency TEXT NOT NULL, preset TEXT NOT NULL, props TEXT NOT NULL,
               fav INTEGER NOT NULL DEFAULT 0, compat TEXT NOT NULL DEFAULT '');
             CREATE TABLE IF NOT EXISTS state(key TEXT PRIMARY KEY, value TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS props(wallpaper TEXT, monitor TEXT, key TEXT, value TEXT NOT NULL,
               PRIMARY KEY(wallpaper, monitor, key));
             CREATE TABLE IF NOT EXISTS extra(id TEXT PRIMARY KEY, subs INTEGER, favs INTEGER, updated INTEGER, color TEXT);
             CREATE TABLE IF NOT EXISTS presets(wallpaper TEXT, name TEXT, props TEXT NOT NULL,
               PRIMARY KEY(wallpaper, name));",
        )
        .map_err(e)?;
        Ok(Db(c))
    }

    /// Insert or refresh a wallpaper; favourites and test results survive.
    pub fn upsert(&self, id: &str, dir: &str, mtime: i64, p: &Project) -> Res<()> {
        let title = if p.title.is_empty() { id } else { &p.title };
        let compat = if p.kind == "application" { "unsupported" } else { "" };
        self.0
            .execute(
                "INSERT INTO wallpapers(id, dir, title, kind, tags, rating, preview, mtime, dependency, preset, props, compat)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)
                 ON CONFLICT(id) DO UPDATE SET dir=?2, title=?3, kind=?4, tags=?5, rating=?6, preview=?7,
                   mtime=?8, dependency=?9, preset=?10, props=?11",
                params![
                    id,
                    dir,
                    title,
                    p.kind,
                    serde_json::to_string(&p.tags).unwrap_or_default(),
                    p.rating,
                    p.preview,
                    mtime,
                    p.dependency,
                    p.preset,
                    p.props,
                    compat
                ],
            )
            .map(drop)
            .map_err(e)
    }

    pub fn mtimes(&self) -> Res<HashMap<String, i64>> {
        let mut st = self.0.prepare("SELECT id, mtime FROM wallpapers").map_err(e)?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).map_err(e)?;
        rows.collect::<Result<_, _>>().map_err(e)
    }

    pub fn remove(&self, id: &str) -> Res<()> {
        self.0.execute("DELETE FROM wallpapers WHERE id=?1", [id]).map(drop).map_err(e)
    }

    pub fn get(&self, id: &str) -> Res<Option<Wallpaper>> {
        self.0.query_row(&format!("SELECT {COLS} {FROM} WHERE w.id=?1"), [id], row).optional().map_err(e)
    }

    /// Resolve an id, or a title fragment that matches exactly one wallpaper.
    pub fn find(&self, query: &str) -> Res<Wallpaper> {
        if let Some(w) = self.get(query)? {
            return Ok(w);
        }
        let mut hits = self.list(&serde_json::json!({ "search": query }))?;
        match hits.len() {
            0 => Err(format!("обои не найдены: {query}")),
            1 => Ok(hits.remove(0)),
            n => Err(format!("«{query}» подходит к {n} обоям, уточните название или укажите id")),
        }
    }

    /// Filters: type, tag, rating, search, fav. sort: "title" (default) or "recent".
    pub fn list(&self, f: &Value) -> Res<Vec<Wallpaper>> {
        let mut sql = format!("SELECT {COLS} {FROM} WHERE 1");
        let mut args: Vec<String> = Vec::new();
        let like = |s: &str| format!("%{}%", s.replace('%', "\\%").replace('_', "\\_"));
        for (key, clause) in [("type", "kind = ?"), ("rating", "rating = ? COLLATE NOCASE")] {
            if let Some(v) = f.get(key).and_then(Value::as_str) {
                sql += &format!(" AND {clause}");
                args.push(v.to_lowercase());
            }
        }
        if let Some(v) = f.get("tag").and_then(Value::as_str) {
            sql += " AND tags LIKE ? ESCAPE '\\'";
            args.push(like(&format!("\"{v}\"")));
        }
        if let Some(v) = f.get("search").and_then(Value::as_str) {
            sql += " AND title LIKE ? ESCAPE '\\'";
            args.push(like(v));
        }
        if f.get("fav").is_some_and(|v| v != &Value::Bool(false)) {
            sql += " AND fav = 1";
        }
        sql += match f.get("sort").and_then(Value::as_str) {
            Some("recent") => " ORDER BY mtime DESC",
            Some("popular") => " ORDER BY COALESCE(x.subs, 0) DESC, title COLLATE NOCASE",
            _ => " ORDER BY title COLLATE NOCASE, w.id",
        };
        let mut st = self.0.prepare(&sql).map_err(e)?;
        let rows = st.query_map(params_from_iter(args), row).map_err(e)?;
        rows.collect::<Result<_, _>>().map_err(e)
    }

    /// Returns the new favourite flag.
    pub fn toggle_fav(&self, id: &str) -> Res<bool> {
        self.0
            .query_row("UPDATE wallpapers SET fav = 1 - fav WHERE id=?1 RETURNING fav", [id], |r| r.get(0))
            .map_err(e)
    }

    pub fn set_compat(&self, id: &str, compat: &str) {
        let _ = self.0.execute("UPDATE wallpapers SET compat=?2 WHERE id=?1 AND compat != 'unsupported'", [id, compat]);
    }

    pub fn set_workshop(&self, id: &str, subs: i64, favs: i64, updated: i64) {
        let _ = self.0.execute(
            "INSERT INTO extra(id, subs, favs, updated) VALUES(?1, ?2, ?3, ?4)
             ON CONFLICT(id) DO UPDATE SET subs=?2, favs=?3, updated=?4",
            params![id, subs, favs, updated],
        );
    }

    pub fn set_color(&self, id: &str, color: &str) {
        let _ = self.0.execute("INSERT INTO extra(id, color) VALUES(?1, ?2) ON CONFLICT(id) DO UPDATE SET color=?2", [id, color]);
    }

    /// User property values for a wallpaper: those set for every monitor ("*"), then this monitor's on top.
    pub fn props(&self, wallpaper: &str, monitor: &str) -> BTreeMap<String, String> {
        let mut out = BTreeMap::new();
        for mon in ["*", monitor] {
            let Ok(mut st) = self.0.prepare("SELECT key, value FROM props WHERE wallpaper=?1 AND monitor=?2") else { continue };
            let rows = st.query_map([wallpaper, mon], |r| Ok((r.get(0)?, r.get(1)?)));
            out.extend(rows.into_iter().flatten().flatten());
        }
        out
    }

    /// `value: None` puts the property back to its default.
    pub fn set_prop(&self, wallpaper: &str, monitor: &str, key: &str, value: Option<&str>) -> Res<()> {
        match value {
            Some(v) => self.0.execute("INSERT OR REPLACE INTO props VALUES(?1, ?2, ?3, ?4)", [wallpaper, monitor, key, v]),
            None => self.0.execute("DELETE FROM props WHERE wallpaper=?1 AND monitor=?2 AND key=?3", [wallpaper, monitor, key]),
        }
        .map(drop)
        .map_err(e)
    }

    pub fn clear_props(&self, wallpaper: &str, monitor: &str) -> Res<()> {
        self.0.execute("DELETE FROM props WHERE wallpaper=?1 AND monitor=?2", [wallpaper, monitor]).map(drop).map_err(e)
    }

    pub fn save_preset(&self, wallpaper: &str, name: &str, props: &BTreeMap<String, String>) -> Res<()> {
        let json = serde_json::to_string(props).unwrap_or_default();
        self.0.execute("INSERT OR REPLACE INTO presets VALUES(?1, ?2, ?3)", [wallpaper, name, &json]).map(drop).map_err(e)
    }

    pub fn preset(&self, wallpaper: &str, name: &str) -> Res<BTreeMap<String, String>> {
        let json: Option<String> = self
            .0
            .query_row("SELECT props FROM presets WHERE wallpaper=?1 AND name=?2", [wallpaper, name], |r| r.get(0))
            .optional()
            .map_err(e)?;
        json.and_then(|j| serde_json::from_str(&j).ok()).ok_or_else(|| format!("пресета «{name}» нет"))
    }

    pub fn presets(&self, wallpaper: &str) -> Res<Vec<String>> {
        let mut st = self.0.prepare("SELECT name FROM presets WHERE wallpaper=?1 ORDER BY name").map_err(e)?;
        let rows = st.query_map([wallpaper], |r| r.get(0)).map_err(e)?;
        rows.collect::<Result<_, _>>().map_err(e)
    }

    pub fn remove_preset(&self, wallpaper: &str, name: &str) -> Res<()> {
        self.0.execute("DELETE FROM presets WHERE wallpaper=?1 AND name=?2", [wallpaper, name]).map(drop).map_err(e)
    }

    pub fn state(&self, key: &str) -> Option<String> {
        self.0.query_row("SELECT value FROM state WHERE key=?1", [key], |r| r.get(0)).ok()
    }

    pub fn set_state(&self, key: &str, value: &str) {
        let _ = self.0.execute("INSERT OR REPLACE INTO state(key, value) VALUES(?1, ?2)", [key, value]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_filters_and_keeps_favourites() {
        let db = Db::open(Path::new(":memory:")).unwrap();
        let p = |title: &str, kind: &str, tags: &[&str]| Project {
            title: title.into(),
            kind: kind.into(),
            tags: tags.iter().map(|s| s.to_string()).collect(),
            preview: "preview.jpg".into(),
            ..Default::default()
        };
        db.upsert("1", "/w/1", 10, &p("Purple Nebula", "scene", &["Abstract"])).unwrap();
        db.upsert("2", "/w/2", 20, &p("Rain 100%", "video", &["Nature"])).unwrap();
        db.upsert("3", "/w/3", 30, &p("App", "application", &[])).unwrap();

        assert_eq!(db.list(&serde_json::json!({})).unwrap().len(), 3);
        assert_eq!(db.list(&serde_json::json!({"type": "video"})).unwrap()[0].id, "2");
        assert_eq!(db.list(&serde_json::json!({"tag": "Abstract"})).unwrap()[0].id, "1");
        assert_eq!(db.list(&serde_json::json!({"search": "100%"})).unwrap().len(), 1);
        assert_eq!(db.list(&serde_json::json!({"sort": "recent"})).unwrap()[0].id, "3");
        assert_eq!(db.find("nebula").unwrap().preview, "/w/1/preview.jpg");
        assert!(db.find("a").is_err());
        assert_eq!(db.get("3").unwrap().unwrap().compat, "unsupported");

        assert!(db.toggle_fav("1").unwrap());
        db.set_compat("1", "ok");
        db.upsert("1", "/w/1", 11, &p("Purple Nebula II", "scene", &[])).unwrap();
        let w = db.get("1").unwrap().unwrap();
        assert!(w.fav && w.compat == "ok" && w.title == "Purple Nebula II");
        assert_eq!(db.list(&serde_json::json!({"fav": true})).unwrap().len(), 1);

        // Per-monitor values override the shared ones; presets round-trip.
        db.set_prop("1", "*", "speed", Some("1")).unwrap();
        db.set_prop("1", "*", "color", Some("1 0 0")).unwrap();
        db.set_prop("1", "DP-1", "speed", Some("3")).unwrap();
        assert_eq!(db.props("1", "DP-1")["speed"], "3");
        assert_eq!(db.props("1", "HDMI-A-1")["speed"], "1");
        db.save_preset("1", "fast", &db.props("1", "DP-1")).unwrap();
        db.set_prop("1", "DP-1", "speed", None).unwrap();
        db.clear_props("1", "*").unwrap();
        assert!(db.props("1", "DP-1").is_empty());
        assert_eq!(db.preset("1", "fast").unwrap()["color"], "1 0 0");
        assert_eq!(db.presets("1").unwrap(), ["fast"]);
        db.remove_preset("1", "fast").unwrap();
        assert!(db.preset("1", "fast").is_err());

        // Workshop numbers and colours join in without disturbing the index.
        db.set_workshop("2", 500, 7, 99);
        db.set_workshop("1", 100, 1, 5);
        db.set_color("2", "#ff0000");
        let popular = db.list(&serde_json::json!({"sort": "popular"})).unwrap();
        assert!(popular[0].id == "2" && popular[0].subs == 500 && popular[0].outdated && popular[0].color == "#ff0000");
        assert!(!db.get("1").unwrap().unwrap().outdated && db.get("3").unwrap().unwrap().subs == 0);

        db.set_state("k", "v");
        assert_eq!(db.state("k").as_deref(), Some("v"));
    }
}
