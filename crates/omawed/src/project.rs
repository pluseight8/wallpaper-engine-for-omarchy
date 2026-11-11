//! Wallpaper Engine project.json.

use serde_json::Value;

#[derive(Debug, Default, PartialEq)]
pub struct Project {
    pub title: String,
    /// scene | video | web | application | preset | unknown
    pub kind: String,
    pub tags: Vec<String>,
    pub rating: String,
    pub preview: String,
    /// Workshop id of the base wallpaper (presets only).
    pub dependency: String,
    /// JSON object of property overrides (presets only), or "".
    pub preset: String,
    /// JSON object of general.properties, or "".
    pub props: String,
}

fn text(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or_default().to_string()
}

fn object(v: Option<&Value>) -> String {
    match v {
        Some(o) if o.is_object() => o.to_string(),
        _ => String::new(),
    }
}

pub fn parse(src: &str) -> Result<Project, String> {
    // Some projects are saved with a UTF-8 BOM.
    let v: Value = serde_json::from_str(src.trim_start_matches('\u{feff}')).map_err(|e| e.to_string())?;
    let dependency = text(&v, "dependency");
    let mut kind = text(&v, "type").to_lowercase();
    if kind.is_empty() {
        kind = if dependency.is_empty() { "unknown" } else { "preset" }.to_string();
    }
    let tags = match v.get("tags") {
        Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).map(String::from).collect(),
        _ => Vec::new(),
    };
    Ok(Project {
        title: text(&v, "title"),
        kind,
        tags,
        rating: text(&v, "contentrating"),
        preview: text(&v, "preview"),
        dependency,
        preset: object(v.get("preset")),
        props: object(v.pointer("/general/properties")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scene_with_bom_and_properties() {
        let src = "\u{feff}{\"title\":\"Purple Nebula\",\"type\":\"Scene\",\"tags\":[\"Abstract\",7],\
            \"contentrating\":\"Everyone\",\"preview\":\"preview.jpg\",\
            \"general\":{\"properties\":{\"schemecolor\":{\"type\":\"color\",\"value\":\"0 0 0\"}}}}";
        let p = parse(src).unwrap();
        assert_eq!(p.title, "Purple Nebula");
        assert_eq!(p.kind, "scene");
        assert_eq!(p.tags, vec!["Abstract"]);
        assert_eq!(p.rating, "Everyone");
        assert!(p.props.contains("schemecolor"));
    }

    #[test]
    fn preset_has_no_type() {
        let p = parse(r#"{"title":"P","dependency":"2947302287","preset":{"speed":2}}"#).unwrap();
        assert_eq!(p.kind, "preset");
        assert_eq!(p.dependency, "2947302287");
        assert_eq!(p.preset, r#"{"speed":2}"#);
    }

    #[test]
    fn rejects_broken_json() {
        assert!(parse("{").is_err());
        assert_eq!(parse("{}").unwrap().kind, "unknown");
    }
}
