//! Runs the real daemon against a fake Steam library and a fake renderer.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

struct Daemon(Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
    }
}

fn request(sock: &Path, req: Value) -> Value {
    let mut s = UnixStream::connect(sock).expect("daemon socket");
    writeln!(s, "{req}").unwrap();
    let mut line = String::new();
    BufReader::new(s).read_line(&mut line).unwrap();
    serde_json::from_str(&line).unwrap()
}

fn wait_for(what: &str, mut cond: impl FnMut() -> bool) {
    let start = Instant::now();
    while !cond() {
        assert!(start.elapsed() < Duration::from_secs(10), "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Process state letter from /proc/<pid>/stat: R, S, T (stopped)...
fn proc_state(pid: u64) -> Option<char> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    stat.rsplit(')').next()?.trim().chars().next()
}

#[test]
fn apply_pause_resume_stop() {
    // Unix socket paths are limited to ~100 bytes, so the runtime dir must be short.
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into());
    let root = Path::new(&runtime).join(format!("omawe-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let steam = root.join("home/.local/share/Steam");
    let workshop = steam.join("steamapps/workshop/content/431960");
    for (id, json) in [
        (
            "111",
            r#"{"title":"Purple Nebula","type":"scene","preview":"preview.jpg","general":{"properties":{
                "speed":{"type":"slider","min":0,"max":2,"value":1,"order":1},
                "glow":{"type":"bool","value":false,"order":2}}}}"#,
        ),
        ("222", r#"{"title":"Some App","type":"application"}"#),
    ] {
        std::fs::create_dir_all(workshop.join(id)).unwrap();
        std::fs::write(workshop.join(id).join("project.json"), json).unwrap();
    }
    std::fs::write(
        steam.join("steamapps/libraryfolders.vdf"),
        format!("\"libraryfolders\" {{ \"0\" {{ \"path\" \"{}\" }} }}", steam.display()),
    )
    .unwrap();

    // The fake renderer writes the screenshot it was asked for, then idles like the real one.
    let backend = root.join("backend.sh");
    std::fs::write(
        &backend,
        "#!/bin/bash\nwhile (( $# )); do [[ $1 == --screenshot ]] && echo frame > \"$2\"; shift; done\nsleep 60\n",
    )
    .unwrap();
    std::fs::set_permissions(&backend, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::create_dir_all(root.join("config/omawe")).unwrap();
    std::fs::write(
        root.join("config/omawe/config.toml"),
        format!("backend = \"{}\"\nsync_omarchy_bg = false\nnotify = false\n", backend.display()),
    )
    .unwrap();

    let _daemon = Daemon(
        Command::new(env!("CARGO_BIN_EXE_omawed"))
            .env("HOME", root.join("home"))
            .env("XDG_RUNTIME_DIR", &root)
            .env("XDG_CONFIG_HOME", root.join("config"))
            .env("XDG_DATA_HOME", root.join("data"))
            .env("XDG_CACHE_HOME", root.join("cache"))
            .env_remove("HYPRLAND_INSTANCE_SIGNATURE")
            .env_remove("NOTIFY_SOCKET")
            .spawn()
            .unwrap(),
    );
    let sock = root.join("omawe.sock");
    wait_for("socket", || sock.exists());
    let status = || request(&sock, json!({"cmd": "status"}))["data"].clone();

    let list = request(&sock, json!({"cmd": "list"}));
    assert_eq!(list["data"].as_array().unwrap().len(), 2, "{list}");

    // Unsupported wallpapers are refused, not faked.
    let refused = request(&sock, json!({"cmd": "apply", "args": ["222"], "opts": {"monitor": "TEST-1"}}));
    assert_eq!(refused["ok"], false);

    let applied = request(&sock, json!({"cmd": "apply", "args": ["nebula"], "opts": {"monitor": "TEST-1"}}));
    assert_eq!(applied["ok"], true, "{applied}");
    wait_for("renderer", || status()["state"] == "running");
    let pid = status()["pid"].as_u64().unwrap();
    let cmdline = std::fs::read_to_string(format!("/proc/{pid}/cmdline")).unwrap().replace('\0', " ");
    assert!(cmdline.contains("--fps"), "{cmdline}");
    assert!(cmdline.contains(&format!("--screen-root TEST-1 --bg {} --scaling fill", workshop.join("111").display())), "{cmdline}");

    // The first frame becomes the still twin and marks the wallpaper as working.
    let twin = root.join("cache/omawe/twins/111.png");
    wait_for("twin", || twin.exists());
    assert_eq!(request(&sock, json!({"cmd": "list", "opts": {"search": "nebula"}}))["data"][0]["compat"], "ok");

    request(&sock, json!({"cmd": "pause"}));
    wait_for("SIGSTOP", || proc_state(pid) == Some('T'));
    assert_eq!(status()["state"], "paused");
    assert_eq!(status()["pid"], pid, "pause must not restart the renderer");

    request(&sock, json!({"cmd": "resume"}));
    wait_for("SIGCONT", || proc_state(pid) != Some('T'));

    // Changing a setting hot-reloads and swaps the renderer for one with new arguments.
    let cfg = root.join("config/omawe/config.toml");
    let text = std::fs::read_to_string(&cfg).unwrap();
    std::fs::write(&cfg, text + "scaling = \"fit\"\n").unwrap();
    wait_for("config reload", || status()["pid"].as_u64().is_some_and(|p| p != pid));
    wait_for("old renderer gone", || proc_state(pid).is_none());

    // A property change restarts that monitor's renderer with --set-property; bad values are refused.
    let pid = status()["pid"].as_u64().unwrap();
    let bad = request(&sock, json!({"cmd": "prop", "args": ["111", "speed", "fast"], "opts": {"monitor": "TEST-1"}}));
    assert_eq!(bad["ok"], false);
    let set = request(&sock, json!({"cmd": "prop", "args": ["111", "speed", "9"], "opts": {"monitor": "TEST-1"}}));
    assert_eq!(set["data"], "2", "slider values are clamped to the declared range: {set}");
    request(&sock, json!({"cmd": "prop", "args": ["111", "glow", "true"], "opts": {"monitor": "TEST-1"}}));
    wait_for("property restart", || status()["pid"].as_u64().is_some_and(|p| p != pid));
    let pid = status()["pid"].as_u64().unwrap();
    let cmdline = std::fs::read_to_string(format!("/proc/{pid}/cmdline")).unwrap().replace('\0', " ");
    assert!(cmdline.contains("--set-property glow=1 --set-property speed=2"), "{cmdline}");
    let props = request(&sock, json!({"cmd": "props", "args": ["111"], "opts": {"monitor": "TEST-1"}}));
    assert_eq!(props["data"]["props"][0]["key"], "speed");
    assert_eq!(props["data"]["props"][0]["value"], "2");

    // Presets: save, reset to defaults, load back.
    request(&sock, json!({"cmd": "preset", "args": ["save", "111", "mine"], "opts": {"monitor": "TEST-1"}}));
    request(&sock, json!({"cmd": "preset", "args": ["reset", "111"], "opts": {"monitor": "TEST-1"}}));
    let props = request(&sock, json!({"cmd": "props", "args": ["111"], "opts": {"monitor": "TEST-1"}}));
    assert_eq!(props["data"]["props"][0]["value"], "1");
    request(&sock, json!({"cmd": "preset", "args": ["load", "111", "mine"], "opts": {"monitor": "TEST-1"}}));
    let props = request(&sock, json!({"cmd": "props", "args": ["111"], "opts": {"monitor": "TEST-1"}}));
    assert_eq!(props["data"]["props"][0]["value"], "2");
    assert_eq!(props["data"]["presets"][0], "mine");

    // Settings overrides: sound on, and a rejected value changes nothing.
    assert_eq!(request(&sock, json!({"cmd": "set", "args": ["volume", "loud"]}))["ok"], false);
    assert_eq!(request(&sock, json!({"cmd": "set", "args": ["nonsense", "1"]}))["ok"], false);
    request(&sock, json!({"cmd": "volume", "args": ["40"]}));
    request(&sock, json!({"cmd": "mute"}));
    wait_for("audible renderer", || {
        let pid = status()["pid"].as_u64().unwrap_or(0);
        std::fs::read_to_string(format!("/proc/{pid}/cmdline")).is_ok_and(|c| c.replace('\0', " ").contains("--volume 40"))
    });

    // The compatibility test runs the renderer on the side and records a verdict.
    let tested = request(&sock, json!({"cmd": "test", "args": ["111"]}));
    assert_eq!(tested["data"], 1, "{tested}");
    wait_for("test verdict", || status()["testing"].is_null());
    let compat = request(&sock, json!({"cmd": "list", "opts": {"search": "nebula"}}))["data"][0]["compat"].clone();
    assert!(compat == "ok" || compat == "black", "{compat}");

    // A renderer that dies at once is a "crash" verdict, and the daemon survives reaping it.
    std::fs::write(root.join("config/omawe/config.toml"), "backend = \"/bin/false\"\nsync_omarchy_bg = false\nnotify = false\n").unwrap();
    wait_for("backend swap", || request(&sock, json!({"cmd": "config"}))["data"]["config"]["backend"] == "/bin/false");
    request(&sock, json!({"cmd": "test", "args": ["111"]}));
    wait_for("crash verdict", || request(&sock, json!({"cmd": "list", "opts": {"search": "nebula"}}))["data"][0]["compat"] == "crash");
    wait_for("test end", || status()["testing"].is_null());
    std::fs::write(root.join("config/omawe/config.toml"), format!("backend = \"{}\"\nsync_omarchy_bg = false\nnotify = false\n", backend.display())).unwrap();
    wait_for("backend back", || request(&sock, json!({"cmd": "config"}))["data"]["config"]["backend"] != "/bin/false");
    request(&sock, json!({"cmd": "apply", "args": ["111"], "opts": {"monitor": "TEST-1"}}));
    wait_for("renderer again", || status()["state"] == "running");

    // A workspace assignment is stored apart from the global one and can be removed on its own.
    request(&sock, json!({"cmd": "apply", "args": ["111"], "opts": {"workspace": "3"}}));
    assert_eq!(request(&sock, json!({"cmd": "stop", "opts": {"workspace": "3"}}))["ok"], true);
    assert_eq!(status()["state"], "running");

    request(&sock, json!({"cmd": "stop"}));
    let pid = status()["pid"].as_u64();
    wait_for("stop", || status()["state"] == "off" && pid.is_none_or(|p| proc_state(p).is_none()));

    let _ = std::fs::remove_dir_all(&root);
}
