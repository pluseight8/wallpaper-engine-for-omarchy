//! Minimal parser for Valve's text KeyValues (libraryfolders.vdf).

use std::path::PathBuf;

#[derive(Debug, PartialEq)]
pub enum Vdf {
    Str(String),
    Obj(Vec<(String, Vdf)>),
}

impl Vdf {
    pub fn get(&self, key: &str) -> Option<&Vdf> {
        match self {
            Vdf::Obj(items) => items.iter().find(|(k, _)| k.eq_ignore_ascii_case(key)).map(|(_, v)| v),
            Vdf::Str(_) => None,
        }
    }
}

enum Tok {
    Str(String),
    Open,
    Close,
}

fn tokens(src: &str) -> Vec<Tok> {
    let mut out = Vec::new();
    let mut it = src.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '{' => out.push(Tok::Open),
            '}' => out.push(Tok::Close),
            '/' if it.peek() == Some(&'/') => {
                for c in it.by_ref() {
                    if c == '\n' {
                        break;
                    }
                }
            }
            '"' => {
                let mut s = String::new();
                while let Some(c) = it.next() {
                    match c {
                        '"' => break,
                        '\\' => s.extend(it.next()),
                        c => s.push(c),
                    }
                }
                out.push(Tok::Str(s));
            }
            _ => {}
        }
    }
    out
}

fn object(it: &mut std::vec::IntoIter<Tok>) -> Vdf {
    let mut items = Vec::new();
    while let Some(tok) = it.next() {
        let key = match tok {
            Tok::Str(s) => s,
            Tok::Close => break,
            Tok::Open => continue,
        };
        match it.next() {
            Some(Tok::Str(v)) => items.push((key, Vdf::Str(v))),
            Some(Tok::Open) => items.push((key, object(it))),
            _ => break,
        }
    }
    Vdf::Obj(items)
}

pub fn parse(src: &str) -> Vdf {
    object(&mut tokens(src).into_iter())
}

/// Library roots listed in libraryfolders.vdf.
pub fn library_paths(src: &str) -> Vec<PathBuf> {
    let root = parse(src);
    let Some(Vdf::Obj(folders)) = root.get("libraryfolders") else { return Vec::new() };
    folders
        .iter()
        .filter_map(|(_, v)| match v.get("path") {
            Some(Vdf::Str(p)) => Some(PathBuf::from(p)),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_library_folders() {
        let src = r#"
"libraryfolders"
{
	// comment
	"0"
	{
		"path"		"/home/u/.local/share/Steam"
		"apps" { "431960" "826555680" }
	}
	"1" { "path" "/mnt/games/Steam \"Lib\"" }
}"#;
        assert_eq!(
            library_paths(src),
            vec![PathBuf::from("/home/u/.local/share/Steam"), PathBuf::from("/mnt/games/Steam \"Lib\"")]
        );
    }

    #[test]
    fn survives_garbage() {
        assert!(library_paths("").is_empty());
        assert!(library_paths("\"libraryfolders\" { \"0\" { \"path\"").is_empty());
    }
}
