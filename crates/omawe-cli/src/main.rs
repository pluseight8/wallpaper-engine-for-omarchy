//! omawe — thin client for omawed: turns argv into one JSON request and prints the reply.

use serde_json::{json, Map, Value};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::process::exit;

const USAGE: &str = "omawe — Wallpaper Engine для Omarchy

  omawe pick                         открыть выбор обоев с превью
  omawe apply <id|название> [--monitor M] [--scaling fill|fit|stretch|default] [--clamp clamp|border|repeat]
  omawe next | prev | random [--type T] [--tag T] [--fav] [--monitor M]
  omawe pause | resume | toggle
  omawe stop [--monitor M]           убрать живые обои, оставить статичный кадр
  omawe list [--type scene|video|web|preset] [--tag T] [--rating R] [--search S] [--fav] [--sort recent]
  omawe fav <id|название>            добавить в избранное или убрать
  omawe status | watch | rescan | refresh | config

  --json                             ответ демона как есть";

/// Flags that never take a value.
const SWITCHES: [&str; 2] = ["json", "fav"];

fn main() {
    let mut argv = std::env::args().skip(1);
    let cmd = match argv.next() {
        Some(c) if !matches!(c.as_str(), "help" | "-h" | "--help") => c,
        _ => return println!("{USAGE}"),
    };
    if cmd == "pick" {
        let err = std::process::Command::new("omawe-pick").args(argv).exec();
        eprintln!("omawe-pick: {err}");
        exit(1);
    }

    let (mut args, mut opts) = (Vec::new(), Map::new());
    while let Some(a) = argv.next() {
        match a.strip_prefix("--") {
            Some(key) if SWITCHES.contains(&key) => drop(opts.insert(key.into(), Value::Bool(true))),
            Some(key) => match argv.next() {
                Some(v) => drop(opts.insert(key.into(), Value::String(v))),
                None => {
                    eprintln!("omawe: у --{key} нет значения");
                    exit(2);
                }
            },
            None => args.push(a),
        }
    }
    let raw = opts.remove("json").is_some();

    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_default();
    let mut stream = UnixStream::connect(format!("{runtime}/omawe.sock")).unwrap_or_else(|_| {
        eprintln!("omawe: демон не запущен (systemctl --user start omawed)");
        exit(1);
    });
    let req = json!({"cmd": cmd, "args": args, "opts": opts});
    if writeln!(stream, "{req}").is_err() {
        exit(1);
    }

    let out = std::io::stdout();
    for line in BufReader::new(stream).lines().map_while(Result::ok) {
        if cmd == "watch" || raw {
            // A closed pipe (the bar went away) ends the watch.
            if writeln!(out.lock(), "{line}").is_err() {
                exit(0);
            }
            if cmd == "watch" {
                continue;
            }
            return;
        }
        let reply: Value = serde_json::from_str(&line).unwrap_or_default();
        if reply["ok"] != true {
            eprintln!("omawe: {}", reply["error"].as_str().unwrap_or("нет ответа от демона"));
            exit(1);
        }
        match reply["text"].as_str() {
            Some("") | None => {}
            Some(text) => println!("{text}"),
        }
        return;
    }
}
