use std::{
    io::Read,
    process::{Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

use crate::{
    policy::Outcome,
    state::{ResetCredits, Window},
};

#[derive(Clone, Debug, PartialEq)]
pub struct Fetched {
    pub windows: Vec<Window>,
    pub credits: Option<ResetCredits>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Failure {
    pub outcome: Outcome,
    pub message: &'static str,
}

impl Failure {
    pub fn local(message: &'static str) -> Self {
        Self {
            outcome: Outcome::LocalFailure,
            message,
        }
    }

    pub fn server(message: &'static str) -> Self {
        Self {
            outcome: Outcome::ServerFailure,
            message,
        }
    }
}

pub fn window(
    label: String,
    used: &serde_json::Value,
    reset: &serde_json::Value,
    weekly: bool,
) -> Option<Window> {
    let used = used
        .as_f64()
        .filter(|used| used.is_finite() && (0.0..=100.0).contains(used))?;
    Some(Window {
        label,
        remaining: (100.0 - used).floor() as u8,
        resets_at: parse_timestamp(reset),
        weekly,
    })
}

pub fn parse_timestamp(value: &serde_json::Value) -> Option<i64> {
    match value {
        serde_json::Value::Number(number) => number
            .as_f64()
            .filter(|seconds| seconds.is_finite())
            .map(|seconds| seconds.floor() as i64),
        serde_json::Value::String(text) => parse_iso(text),
        _ => None,
    }
}

fn parse_iso(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    if bytes.len() < 20 || bytes[4] != b'-' || bytes[7] != b'-' || bytes[10] != b'T' {
        return None;
    }
    if bytes[13] != b':' || bytes[16] != b':' {
        return None;
    }
    let field = |range: std::ops::Range<usize>| text.get(range)?.parse::<i64>().ok();
    let (year, month, day) = (field(0..4)?, field(5..7)?, field(8..10)?);
    let (hour, minute, second) = (field(11..13)?, field(14..16)?, field(17..19)?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    if hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    let mut rest = &text[19..];
    if let Some(fraction) = rest.strip_prefix('.') {
        let digits = fraction.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 {
            return None;
        }
        rest = &fraction[digits..];
    }
    let offset = match rest {
        "Z" | "z" => 0,
        _ => {
            let sign = match rest.as_bytes().first()? {
                b'+' => 1,
                b'-' => -1,
                _ => return None,
            };
            if rest.len() != 6 || rest.as_bytes()[3] != b':' {
                return None;
            }
            let hours = rest[1..3].parse::<i64>().ok()?;
            let minutes = rest[4..6].parse::<i64>().ok()?;
            sign * (hours * 3600 + minutes * 60)
        }
    };
    let days = days_from_civil(year, month, day);
    Some(days * 86_400 + hour * 3600 + minute * 60 + second - offset)
}

fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let day_of_year = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

pub fn run_with_timeout(command: &mut Command, timeout: Duration) -> Option<Output> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stdout.read_to_end(&mut bytes);
        bytes
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    Some(Output {
        status,
        stdout: reader.join().ok()?,
        stderr: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn window_floors_remaining_and_keeps_the_reset() {
        let window = window("Weekly".into(), &json!(36.6), &json!(1_800_000_000), true).unwrap();
        assert_eq!(window.remaining, 63);
        assert_eq!(window.resets_at, Some(1_800_000_000));
        assert!(window.weekly);
    }

    #[test]
    fn window_rejects_bad_usage_values() {
        for used in [
            json!(true),
            json!("40"),
            json!(-1),
            json!(100.5),
            json!(null),
        ] {
            assert_eq!(window("x".into(), &used, &json!(null), false), None);
        }
    }

    #[test]
    fn window_accepts_an_iso_reset_and_drops_a_bad_one() {
        let iso = window(
            "x".into(),
            &json!(0),
            &json!("2026-10-08T22:29:59.4+00:00"),
            false,
        );
        assert_eq!(iso.unwrap().resets_at, Some(1_791_498_599));
        let bad = window("x".into(), &json!(0), &json!("soon"), false);
        assert_eq!(bad.unwrap().resets_at, None);
    }

    #[test]
    fn parse_timestamp_handles_zulu_offsets_and_numbers() {
        assert_eq!(parse_timestamp(&json!("1970-01-01T00:00:00Z")), Some(0));
        assert_eq!(
            parse_timestamp(&json!("2000-03-01T00:00:00Z")),
            Some(951_868_800)
        );
        assert_eq!(
            parse_timestamp(&json!("2026-10-08T23:59:59-01:30")),
            Some(1_791_509_399)
        );
        assert_eq!(
            parse_timestamp(&json!(1_700_000_000.9)),
            Some(1_700_000_000)
        );
        assert_eq!(parse_timestamp(&json!(f64::NAN)), None);
        assert_eq!(parse_timestamp(&json!("2026-13-01T00:00:00Z")), None);
    }

    #[test]
    fn run_with_timeout_returns_output() {
        let output = run_with_timeout(
            Command::new("/bin/echo").arg("hello"),
            Duration::from_secs(5),
        )
        .unwrap();
        assert_eq!(output.stdout, b"hello\n");
    }

    #[test]
    fn run_with_timeout_kills_a_slow_command() {
        let started = Instant::now();
        let output = run_with_timeout(
            Command::new("/bin/sleep").arg("30"),
            Duration::from_millis(200),
        );
        assert!(output.is_none());
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}
