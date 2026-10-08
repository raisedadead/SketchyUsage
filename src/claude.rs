use std::{fmt, fs, io::ErrorKind, path::PathBuf, process::Command, time::Duration};

use serde_json::Value;
use ureq::Agent;

use crate::{
    policy::Outcome,
    provider::{Failure, Fetched, run_with_timeout, window},
    state::Window,
};

pub const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
pub const KEYCHAIN_TIMEOUT: Duration = Duration::from_secs(10);

pub struct Token(String);

impl fmt::Debug for Token {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Token(<redacted>)")
    }
}

pub fn parse_credential(raw: &str, now_ms: i64) -> Result<Token, Failure> {
    let raw = raw.trim();
    let text = if raw.starts_with('{') {
        raw.to_owned()
    } else {
        decode_hex(raw).ok_or(Failure::local("Sign in to Claude CLI"))?
    };
    let data: Value =
        serde_json::from_str(&text).map_err(|_| Failure::local("Sign in to Claude CLI"))?;
    let oauth = &data["claudeAiOauth"];
    let token = oauth["accessToken"]
        .as_str()
        .filter(|token| !token.is_empty())
        .ok_or(Failure::local("Sign in to Claude CLI"))?;
    if oauth["expiresAt"]
        .as_f64()
        .is_some_and(|expires_at| expires_at <= now_ms as f64)
    {
        return Err(Failure::local("Open Claude CLI to renew login"));
    }
    Ok(Token(token.to_owned()))
}

fn decode_hex(text: &str) -> Option<String> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    let bytes = (0..text.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(text.get(index..index + 2)?, 16).ok())
        .collect::<Option<Vec<u8>>>()?;
    String::from_utf8(bytes).ok()
}

pub fn read_token(now: i64) -> Result<Token, Failure> {
    let custom = std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from);
    let config = custom
        .clone()
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".claude")))
        .ok_or(Failure::local("Sign in to Claude CLI"))?;
    let file = config.join(".credentials.json");
    let raw = if file.is_file() {
        fs::read_to_string(&file).map_err(|_| Failure::local("Sign in to Claude CLI"))?
    } else if custom.is_some() {
        return Err(Failure::local("Use Claude CLI credential file"));
    } else {
        keychain().ok_or(Failure::local("Unlock Keychain or sign in to Claude"))?
    };
    parse_credential(&raw, now * 1000)
}

fn keychain() -> Option<String> {
    let run = |account: Option<&str>| {
        let mut command = Command::new("/usr/bin/security");
        command.args(["find-generic-password", "-s", "Claude Code-credentials"]);
        if let Some(account) = account {
            command.args(["-a", account]);
        }
        command.arg("-w");
        run_with_timeout(&mut command, KEYCHAIN_TIMEOUT)
    };
    let user = std::env::var("USER").ok();
    let mut output = run(user.as_deref())?;
    if output.status.code() == Some(44) {
        output = run(None)?;
    }
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()
}

pub fn decode(data: &Value) -> Vec<Window> {
    let mut windows: Vec<Window> = [
        ("five_hour", "5-hour"),
        ("seven_day", "Weekly · All models"),
    ]
    .into_iter()
    .filter_map(|(key, label)| {
        let value = &data[key];
        window(
            label.into(),
            &value["utilization"],
            &value["resets_at"],
            key == "seven_day",
        )
    })
    .collect();
    let mut scoped: Vec<Window> = data["limits"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|limit| limit["kind"] == "weekly_scoped")
        .filter_map(|limit| {
            let name = limit["scope"]["model"]["display_name"].as_str()?;
            window(
                format!("Weekly · {name}"),
                &limit["percent"],
                &limit["resets_at"],
                false,
            )
        })
        .collect();
    if scoped.is_empty() {
        scoped = data
            .as_object()
            .into_iter()
            .flatten()
            .filter_map(|(key, value)| {
                let model = key.strip_prefix("seven_day_")?;
                window(
                    format!("Weekly · {}", title(model)),
                    &value["utilization"],
                    &value["resets_at"],
                    false,
                )
            })
            .collect();
    }
    windows.append(&mut scoped);
    windows
}

fn title(key: &str) -> String {
    key.split('_')
        .filter(|word| !word.is_empty())
        .map(|word| {
            let mut chars = word.chars();
            chars
                .next()
                .map(|first| {
                    first
                        .to_uppercase()
                        .chain(chars.flat_map(char::to_lowercase))
                        .collect()
                })
                .unwrap_or_default()
        })
        .collect::<Vec<String>>()
        .join(" ")
}

pub fn fetch(url: &str, token: &Token) -> Result<Fetched, Failure> {
    let agent: Agent = Agent::config_builder()
        .timeout_global(Some(REQUEST_TIMEOUT))
        .max_redirects(0)
        .max_redirects_will_error(false)
        .http_status_as_error(false)
        .user_agent(format!("sketchyusage/{}", env!("CARGO_PKG_VERSION")))
        .build()
        .into();
    let mut response = agent
        .get(url)
        .header("Authorization", format!("Bearer {}", token.0))
        .header("Accept", "application/json")
        .header("anthropic-beta", "oauth-2025-04-20")
        .call()
        .map_err(classify)?;
    let status = response.status().as_u16();
    let header_delay = response
        .headers()
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<i64>().ok());
    let body = response.body_mut().read_to_string().unwrap_or_default();
    let data: Option<Value> = serde_json::from_str(&body).ok();
    match status {
        200..=299 => {
            let windows = decode(&data.ok_or(Failure::server("Claude service unavailable"))?);
            if windows.is_empty() {
                return Err(Failure::server("No subscription limits"));
            }
            Ok(Fetched {
                windows,
                credits: None,
            })
        }
        429 => {
            let body_delay = data
                .as_ref()
                .and_then(|data| data["retry_after"].as_f64())
                .filter(|delay| delay.is_finite())
                .map(|delay| delay.ceil() as i64);
            Err(Failure {
                outcome: Outcome::RateLimited {
                    retry_after: header_delay.max(body_delay),
                },
                message: "Rate limited",
            })
        }
        401 | 403 => Err(Failure::server("Open Claude CLI to check login")),
        _ => Err(Failure::server("Claude service unavailable")),
    }
}

fn classify(error: ureq::Error) -> Failure {
    let unreachable = match &error {
        ureq::Error::HostNotFound | ureq::Error::ConnectionFailed => true,
        ureq::Error::Timeout(timeout) => {
            matches!(timeout, ureq::Timeout::Resolve | ureq::Timeout::Connect)
        }
        ureq::Error::Io(io) => matches!(
            io.kind(),
            ErrorKind::ConnectionRefused
                | ErrorKind::NotConnected
                | ErrorKind::AddrNotAvailable
                | ErrorKind::NetworkUnreachable
                | ErrorKind::HostUnreachable
        ),
        _ => false,
    };
    if unreachable {
        Failure::local("Network unavailable")
    } else {
        Failure::server("Claude service unavailable")
    }
}

pub fn usage(now: i64) -> Result<Fetched, Failure> {
    fetch(USAGE_URL, &read_token(now)?)
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Read, Write},
        net::TcpListener,
        sync::mpsc,
        thread,
    };

    use serde_json::json;

    use super::*;

    const NOW_MS: i64 = 1_800_000_000_000;

    fn credential(token: &str, expires_at: i64) -> String {
        json!({"claudeAiOauth": {"accessToken": token, "expiresAt": expires_at}}).to_string()
    }

    fn usage_body() -> Value {
        json!({
            "five_hour": {"utilization": 2.0, "resets_at": "2026-10-08T20:39:59+00:00"},
            "seven_day": {"utilization": 37.0, "resets_at": "2026-10-08T22:29:59+00:00"},
            "seven_day_opus": {"utilization": 90.0, "resets_at": null},
            "limits": [
                {
                    "kind": "weekly_scoped",
                    "scope": {"model": {"display_name": "Fable"}},
                    "percent": 1.0,
                    "resets_at": "2026-10-08T22:29:59+00:00"
                },
                {"kind": "other", "percent": 50.0}
            ]
        })
    }

    fn serve_once(response: String) -> (String, mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/api/oauth/usage", listener.local_addr().unwrap());
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let mut buffer = [0; 1024];
            while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                let read = stream.read(&mut buffer).unwrap();
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..read]);
            }
            sender
                .send(String::from_utf8_lossy(&request).into_owned())
                .unwrap();
            stream.write_all(response.as_bytes()).unwrap();
        });
        (url, receiver)
    }

    fn response(status: &str, headers: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 {status}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    fn token() -> Token {
        parse_credential(&credential("secret-token", NOW_MS + 60_000), NOW_MS).unwrap()
    }

    #[test]
    fn token_debug_is_redacted() {
        assert_eq!(format!("{:?}", token()), "Token(<redacted>)");
    }

    #[test]
    fn credential_parses_json_and_hex() {
        assert!(parse_credential(&credential("a", NOW_MS + 1), NOW_MS).is_ok());
        let hex: String = credential("a", NOW_MS + 1)
            .bytes()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        assert!(parse_credential(&hex, NOW_MS).is_ok());
    }

    #[test]
    fn expired_or_missing_token_fails_locally() {
        let expired = parse_credential(&credential("a", NOW_MS), NOW_MS).unwrap_err();
        assert_eq!(expired.outcome, Outcome::LocalFailure);
        let missing = parse_credential(&credential("", NOW_MS + 1), NOW_MS).unwrap_err();
        assert_eq!(missing.outcome, Outcome::LocalFailure);
        let garbage = parse_credential("not json", NOW_MS).unwrap_err();
        assert_eq!(garbage.outcome, Outcome::LocalFailure);
    }

    #[test]
    fn decode_reads_the_known_windows() {
        let windows = decode(&usage_body());
        let labels: Vec<_> = windows.iter().map(|w| w.label.as_str()).collect();
        assert_eq!(labels, ["5-hour", "Weekly · All models", "Weekly · Fable"]);
        assert_eq!(windows[1].remaining, 63);
        assert!(windows[1].weekly);
        assert!(!windows[0].weekly && !windows[2].weekly);
    }

    #[test]
    fn decode_falls_back_to_seven_day_keys() {
        let mut body = usage_body();
        body.as_object_mut().unwrap().remove("limits");
        let labels: Vec<_> = decode(&body).into_iter().map(|w| w.label).collect();
        assert_eq!(labels, ["5-hour", "Weekly · All models", "Weekly · Opus"]);
    }

    #[test]
    fn fetch_sends_the_expected_headers_and_decodes() {
        let (url, request) = serve_once(response("200 OK", "", &usage_body().to_string()));
        let fetched = fetch(&url, &token()).unwrap();
        assert_eq!(fetched.windows.len(), 3);
        assert_eq!(fetched.credits, None);
        let request = request.recv().unwrap().to_lowercase();
        assert!(request.starts_with("get /api/oauth/usage"));
        assert!(request.contains("authorization: bearer secret-token"));
        assert!(request.contains("anthropic-beta: oauth-2025-04-20"));
        assert!(request.contains("accept: application/json"));
        assert!(request.contains(&format!(
            "user-agent: sketchyusage/{}",
            env!("CARGO_PKG_VERSION")
        )));
    }

    #[test]
    fn fetch_maps_429_to_rate_limited_with_retry_after() {
        let (url, _request) = serve_once(response(
            "429 Too Many Requests",
            "Retry-After: 120\r\n",
            "{\"retry_after\": 300}",
        ));
        let failure = fetch(&url, &token()).unwrap_err();
        assert_eq!(
            failure.outcome,
            Outcome::RateLimited {
                retry_after: Some(300)
            }
        );
        let (url, _request) = serve_once(response(
            "429 Too Many Requests",
            "Retry-After: soon\r\n",
            "",
        ));
        let failure = fetch(&url, &token()).unwrap_err();
        assert_eq!(failure.outcome, Outcome::RateLimited { retry_after: None });
    }

    #[test]
    fn fetch_maps_server_errors_and_redirects_to_server_failure() {
        for status in ["500 Internal Server Error", "401 Unauthorized", "302 Found"] {
            let (url, _request) =
                serve_once(response(status, "Location: http://127.0.0.1:9/\r\n", ""));
            assert_eq!(
                fetch(&url, &token()).unwrap_err().outcome,
                Outcome::ServerFailure
            );
        }
        let (url, _request) = serve_once(response("200 OK", "", "{}"));
        assert_eq!(
            fetch(&url, &token()).unwrap_err().outcome,
            Outcome::ServerFailure
        );
    }

    #[test]
    fn fetch_maps_connection_refused_to_local_failure() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/api/oauth/usage", listener.local_addr().unwrap());
        drop(listener);
        assert_eq!(
            fetch(&url, &token()).unwrap_err().outcome,
            Outcome::LocalFailure
        );
    }
}
