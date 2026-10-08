use std::{
    io::{BufRead, BufReader, Write},
    os::unix::process::CommandExt,
    path::PathBuf,
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{self, RecvTimeoutError},
    thread,
    time::{Duration, Instant},
};

use serde_json::{Value, json};

use crate::{
    provider::{Failure, Fetched, parse_timestamp, window},
    state::ResetCredits,
};

pub const DEADLINE: Duration = Duration::from_secs(25);
pub const GRACE: Duration = Duration::from_secs(3);

pub struct Options {
    pub binary: PathBuf,
    pub deadline: Duration,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            binary: PathBuf::from("codex"),
            deadline: DEADLINE,
        }
    }
}

pub fn decode(result: &Value) -> Fetched {
    let buckets: Vec<(&str, &Value)> = match result["rateLimitsByLimitId"].as_object() {
        Some(map) if !map.is_empty() => map
            .iter()
            .map(|(key, value)| (key.as_str(), value))
            .collect(),
        _ => vec![("codex", &result["rateLimits"])],
    };
    let mut windows = Vec::new();
    for (key, bucket) in buckets {
        for slot in ["primary", "secondary"] {
            let value = &bucket[slot];
            if !value.is_object() {
                continue;
            }
            let minutes = value["windowDurationMins"].as_i64();
            let mut label = match minutes {
                Some(10_080) => "Weekly".to_owned(),
                Some(300) => "5-hour".to_owned(),
                Some(minutes) => format!("{minutes} min"),
                None => "Limit".to_owned(),
            };
            if key != "codex" {
                label = format!("{label} · {}", bucket["limitName"].as_str().unwrap_or(key));
            }
            if label.to_lowercase().contains("codex-spark") {
                continue;
            }
            let weekly = key == "codex" && minutes == Some(10_080);
            windows.extend(window(
                label,
                &value["usedPercent"],
                &value["resetsAt"],
                weekly,
            ));
        }
    }
    Fetched {
        windows,
        credits: credits(&result["rateLimitResetCredits"]),
    }
}

fn credits(value: &Value) -> Option<ResetCredits> {
    let available: Vec<&Value> = value["credits"]
        .as_array()?
        .iter()
        .filter(|credit| credit["status"] == "available")
        .collect();
    Some(ResetCredits {
        available: available.len() as u32,
        nearest_expiry: available
            .iter()
            .filter_map(|credit| parse_timestamp(&credit["expiresAt"]))
            .min(),
    })
}

pub fn fetch(options: &Options) -> Result<Fetched, Failure> {
    let mut child = Command::new(&options.binary)
        .arg("app-server")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .map_err(|_| Failure::local("Codex CLI not found"))?;
    let result = converse(&mut child, options.deadline);
    stop(&mut child);
    result
}

fn converse(child: &mut Child, deadline: Duration) -> Result<Fetched, Failure> {
    let exited = Failure::server("Codex exited");
    let mut stdin = child.stdin.take().ok_or(exited)?;
    let stdout = child.stdout.take().ok_or(exited)?;
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if sender.send(line).is_err() {
                break;
            }
        }
    });
    let initialize = json!({
        "id": 1,
        "method": "initialize",
        "params": {"clientInfo": {"name": "sketchyusage", "version": env!("CARGO_PKG_VERSION")}},
    });
    send(&mut stdin, &initialize)?;
    let end = Instant::now() + deadline;
    loop {
        let line = match receiver.recv_timeout(end.saturating_duration_since(Instant::now())) {
            Ok(line) => line,
            Err(RecvTimeoutError::Timeout) => return Err(Failure::server("Codex timed out")),
            Err(RecvTimeoutError::Disconnected) => return Err(exited),
        };
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        match message["id"].as_i64() {
            Some(1) if message.get("error").is_some() => {
                return Err(Failure::server("Codex unavailable"));
            }
            Some(1) => {
                send(&mut stdin, &json!({"method": "initialized"}))?;
                send(
                    &mut stdin,
                    &json!({"id": 2, "method": "account/rateLimits/read"}),
                )?;
            }
            Some(2) if message.get("error").is_some() => {
                return Err(Failure::server("Check Codex login or service"));
            }
            Some(2) => {
                let fetched = decode(&message["result"]);
                if fetched.windows.is_empty() {
                    return Err(Failure::server("No subscription limits"));
                }
                return Ok(fetched);
            }
            _ => {}
        }
    }
}

fn send(stdin: &mut ChildStdin, message: &Value) -> Result<(), Failure> {
    writeln!(stdin, "{message}")
        .and_then(|()| stdin.flush())
        .map_err(|_| Failure::server("Codex exited"))
}

fn stop(child: &mut Child) {
    let group = child.id() as libc::pid_t;
    unsafe { libc::killpg(group, libc::SIGTERM) };
    let end = Instant::now() + GRACE;
    while Instant::now() < end {
        if let Ok(Some(_)) = child.try_wait() {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    unsafe { libc::killpg(group, libc::SIGKILL) };
    let _ = child.wait();
}

pub fn usage() -> Result<Fetched, Failure> {
    fetch(&Options::default())
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    use super::*;
    use crate::policy::Outcome;

    fn fake(name: &str) -> Options {
        Options {
            binary: Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/bin")
                .join(name),
            deadline: Duration::from_secs(10),
        }
    }

    fn fixture() -> Value {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/codex-ratelimits.json");
        serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
    }

    fn alive(pid: i32) -> bool {
        unsafe { libc::kill(pid, 0) == 0 }
    }

    #[test]
    fn decode_reads_windows_and_hides_spark() {
        let fetched = decode(&fixture());
        let labels: Vec<_> = fetched.windows.iter().map(|w| w.label.as_str()).collect();
        assert_eq!(labels, ["Weekly", "5-hour", "Weekly · GPT-5.5 Pro"]);
        assert_eq!(fetched.windows[0].remaining, 97);
        assert_eq!(fetched.windows[1].remaining, 59);
        assert!(fetched.windows[0].weekly);
        assert!(!fetched.windows[1].weekly && !fetched.windows[2].weekly);
    }

    #[test]
    fn decode_counts_available_reset_credits() {
        assert_eq!(
            decode(&fixture()).credits,
            Some(ResetCredits {
                available: 3,
                nearest_expiry: Some(1_792_700_749),
            })
        );
    }

    #[test]
    fn decode_falls_back_to_rate_limits_without_credits() {
        let mut data = fixture();
        let object = data.as_object_mut().unwrap();
        object.remove("rateLimitsByLimitId");
        object.remove("rateLimitResetCredits");
        let fetched = decode(&data);
        assert_eq!(fetched.windows.len(), 1);
        assert_eq!(fetched.credits, None);
    }

    #[test]
    fn fetch_talks_json_rpc_to_the_app_server() {
        let fetched = fetch(&fake("codex-ok")).unwrap();
        assert_eq!(fetched, decode(&fixture()));
    }

    #[test]
    fn fetch_maps_a_json_rpc_error_to_server_failure() {
        let failure = fetch(&fake("codex-error")).unwrap_err();
        assert_eq!(failure.outcome, Outcome::ServerFailure);
    }

    #[test]
    fn fetch_maps_a_missing_binary_to_local_failure() {
        let failure = fetch(&fake("codex-missing")).unwrap_err();
        assert_eq!(failure.outcome, Outcome::LocalFailure);
    }

    #[test]
    fn fetch_kills_a_hung_server_and_its_children() {
        let mut options = fake("codex-hang");
        options.deadline = Duration::from_millis(500);
        let started = Instant::now();
        let failure = fetch(&options).unwrap_err();
        assert_eq!(failure.outcome, Outcome::ServerFailure);
        assert!(started.elapsed() < DEADLINE);
        let pids = fs::read_to_string(
            std::env::temp_dir().join(format!("sketchyusage-codex-hang-{}", std::process::id())),
        )
        .unwrap();
        let pids: Vec<i32> = pids
            .split_whitespace()
            .map(|pid| pid.parse().unwrap())
            .collect();
        let deadline = Instant::now() + Duration::from_secs(2);
        while pids.iter().any(|pid| alive(*pid)) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(50));
        }
        assert!(pids.iter().all(|pid| !alive(*pid)));
    }
}
