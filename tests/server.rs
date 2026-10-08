use std::{
    fs,
    io::{Read, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

struct Sandbox {
    root: PathBuf,
    servers: Vec<Child>,
}

impl Sandbox {
    fn new(name: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = PathBuf::from("/tmp").join(format!(
            "su-{name}-{}-{}",
            std::process::id(),
            nanos % 1_000_000
        ));
        fs::create_dir_all(root.join("cache")).unwrap();
        fs::create_dir_all(root.join("claude")).unwrap();
        Self {
            root,
            servers: vec![],
        }
    }

    fn dir(&self) -> PathBuf {
        self.root.join("cache").join("sketchyusage")
    }

    fn bar_log(&self) -> PathBuf {
        self.root.join("bar.log")
    }

    fn codex_log(&self) -> PathBuf {
        self.root.join("codex.log")
    }

    fn command(&self, args: &[&str]) -> Command {
        let bin = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/bin");
        let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
        let mut command = Command::new(env!("CARGO_BIN_EXE_sketchyusage"));
        command
            .args(args)
            .env("PATH", path)
            .env("XDG_CACHE_HOME", self.root.join("cache"))
            .env("CLAUDE_CONFIG_DIR", self.root.join("claude"))
            .env("SKETCHYUSAGE_SKETCHYBAR", bin.join("sketchybar"))
            .env("SKETCHYUSAGE_TEST_BAR_LOG", self.bar_log())
            .env("SKETCHYUSAGE_TEST_CODEX_LOG", self.codex_log())
            .env("SKETCHYUSAGE_NO_PANEL", "1")
            .stdin(Stdio::null());
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().unwrap()
    }

    fn start_server(&mut self) {
        let child = self
            .command(&["serve"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        self.servers.push(child);
        let socket = self.dir().join("serve.sock");
        assert!(
            wait_for(|| UnixStream::connect(&socket).is_ok()),
            "server socket never came up"
        );
    }

    fn read(&self, path: &Path) -> String {
        fs::read_to_string(path).unwrap_or_default()
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        for server in &mut self.servers {
            let _ = server.kill();
            let _ = server.wait();
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn wait_for(mut condition: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if condition() {
            return true;
        }
        thread::sleep(Duration::from_millis(50));
    }
    false
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

#[test]
fn second_server_exits_cleanly_and_the_first_keeps_the_socket() {
    let mut sandbox = Sandbox::new("single");
    sandbox.start_server();
    let second = sandbox.run(&["serve"]);
    assert!(second.status.success());
    let mut stream = UnixStream::connect(sandbox.dir().join("serve.sock")).unwrap();
    stream.write_all(b"push\n").unwrap();
    let mut reply = String::new();
    stream.read_to_string(&mut reply).unwrap();
    assert_eq!(reply, "ok\n");
}

#[test]
fn push_sets_both_labels_through_sketchybar() {
    let mut sandbox = Sandbox::new("push");
    sandbox.start_server();
    fs::remove_file(sandbox.bar_log()).ok();
    assert!(sandbox.run(&["push"]).status.success());
    assert!(wait_for(|| {
        let log = sandbox.read(&sandbox.bar_log());
        log.contains("--set sketchyusage.claude label=")
            && log.contains("--set sketchyusage.codex label=")
    }));
}

#[test]
fn unreachable_server_marks_both_labels() {
    let sandbox = Sandbox::new("down");
    for command in ["push", "toggle"] {
        assert!(sandbox.run(&[command]).status.success());
    }
    let log = sandbox.read(&sandbox.bar_log());
    assert_eq!(
        log.lines()
            .filter(|line| *line
                == "--set sketchyusage.claude label=— ! label.color=0xff7f849c \
                    --set sketchyusage.codex label=— ! label.color=0xff7f849c")
            .count(),
        2,
        "{log}"
    );
}

#[test]
fn a_fetch_cycle_records_codex_usage_and_writes_the_heartbeat() {
    let mut sandbox = Sandbox::new("cycle");
    sandbox.start_server();
    let state = sandbox.dir().join("state.json");
    assert!(wait_for(|| sandbox
        .read(&state)
        .contains("\"remaining\":97")));
    let heartbeat = sandbox.dir().join("heartbeat");
    assert!(wait_for(|| !sandbox.read(&heartbeat).is_empty()));
    let text = sandbox.read(&heartbeat);
    let fields: Vec<i64> = text
        .split_whitespace()
        .map(|field| field.parse().unwrap())
        .collect();
    assert_eq!(fields.len(), 2, "{text}");
    assert!((fields[0] - now()).abs() < 30);
    assert!(fields[1] >= fields[0] - 1);
    assert!(wait_for(|| sandbox
        .read(&sandbox.bar_log())
        .contains("sketchyusage.codex label=97%")));
    let saved: serde_json::Value = serde_json::from_str(&sandbox.read(&state)).unwrap();
    assert_eq!(saved["providers"]["claude"]["policy"]["failures"], 0);
    assert!(saved["providers"]["claude"]["error"].is_string());
}

#[test]
fn a_saved_hold_blocks_the_request_after_a_restart() {
    let mut sandbox = Sandbox::new("hold");
    let dir = sandbox.dir();
    fs::create_dir_all(&dir).unwrap();
    let at = now();
    let hold = serde_json::json!({
        "providers": {
            "codex": {
                "windows": [],
                "credits": null,
                "updated_at": null,
                "error": null,
                "policy": {
                    "attempted_at": at - 10,
                    "next_attempt": at + 3600,
                    "failures": 0,
                    "last_ok": false
                },
                "samples": []
            }
        }
    });
    fs::write(dir.join("state.json"), hold.to_string()).unwrap();
    sandbox.start_server();
    assert!(sandbox.run(&["toggle"]).status.success());
    thread::sleep(Duration::from_millis(800));
    assert_eq!(sandbox.read(&sandbox.codex_log()), "");
}
