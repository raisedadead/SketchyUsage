use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::{self, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::Path,
};

use serde::{Deserialize, Serialize};

use crate::policy::ProviderState;

pub const SAMPLE_RETENTION: i64 = 7 * 24 * 60 * 60;
const RESET_JITTER: u64 = 60;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct State {
    pub providers: BTreeMap<String, Provider>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Provider {
    pub windows: Vec<Window>,
    pub credits: Option<ResetCredits>,
    pub updated_at: Option<i64>,
    pub error: Option<String>,
    pub policy: ProviderState,
    pub samples: Vec<Sample>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Window {
    pub label: String,
    pub remaining: u8,
    pub resets_at: Option<i64>,
    pub weekly: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ResetCredits {
    pub available: u32,
    pub nearest_expiry: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Sample {
    pub window: String,
    pub resets_at: Option<i64>,
    pub at: i64,
    pub used: f64,
}

impl Provider {
    pub fn record(&mut self, windows: Vec<Window>, credits: Option<ResetCredits>, now: i64) {
        self.samples
            .retain(|sample| now - sample.at <= SAMPLE_RETENTION);
        self.samples.extend(windows.iter().map(|window| Sample {
            window: window.label.clone(),
            resets_at: window.resets_at,
            at: now,
            used: f64::from(100 - window.remaining.min(100)),
        }));
        self.windows = windows;
        self.credits = credits;
        self.updated_at = Some(now);
        self.error = None;
    }

    pub fn weekly(&self) -> Option<&Window> {
        self.windows.iter().find(|window| window.weekly)
    }

    pub fn samples_for(&self, window: &Window) -> Vec<(i64, f64)> {
        self.samples
            .iter()
            .filter(|sample| {
                sample.window == window.label && same_reset(sample.resets_at, window.resets_at)
            })
            .map(|sample| (sample.at, sample.used))
            .collect()
    }
}

fn same_reset(a: Option<i64>, b: Option<i64>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => a.abs_diff(b) <= RESET_JITTER,
        _ => a == b,
    }
}

pub fn load(path: &Path) -> State {
    fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

pub fn save(path: &Path, state: &State) -> io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::other("state path has no directory"))?;
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::other("state path has no file name"))?
        .to_string_lossy();
    let tmp = dir.join(format!(".{name}.{}.tmp", std::process::id()));
    let result =
        write_private(&tmp, &serde_json::to_vec(state)?).and_then(|()| fs::rename(&tmp, path));
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    file.write_all(bytes)?;
    file.sync_all()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    const NOW: i64 = 1_800_000_000;

    fn scratch_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "sketchyusage-test-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn weekly(remaining: u8, resets_at: i64) -> Window {
        Window {
            label: "Weekly".into(),
            remaining,
            resets_at: Some(resets_at),
            weekly: true,
        }
    }

    fn full_state() -> State {
        let mut provider = Provider {
            error: Some("Rate limited".into()),
            policy: ProviderState {
                attempted_at: NOW,
                next_attempt: NOW + 900,
                failures: 1,
                last_ok: false,
            },
            ..Default::default()
        };
        provider.record(
            vec![
                weekly(80, NOW + 86_400),
                Window {
                    label: "5-hour".into(),
                    remaining: 50,
                    resets_at: None,
                    weekly: false,
                },
            ],
            Some(ResetCredits {
                available: 3,
                nearest_expiry: Some(NOW + 1000),
            }),
            NOW,
        );
        let mut state = State::default();
        state.providers.insert("claude".into(), provider.clone());
        state.providers.insert("codex".into(), provider);
        state
    }

    fn keys(value: &serde_json::Value, out: &mut Vec<String>) {
        match value {
            serde_json::Value::Object(map) => {
                for (key, child) in map {
                    out.push(key.to_lowercase());
                    keys(child, out);
                }
            }
            serde_json::Value::Array(items) => items.iter().for_each(|item| keys(item, out)),
            _ => {}
        }
    }

    #[test]
    fn serialized_state_has_no_credential_field() {
        let value = serde_json::to_value(full_state()).unwrap();
        let mut found = vec![];
        keys(&value, &mut found);
        for key in found {
            for banned in [
                "token",
                "credential",
                "authorization",
                "secret",
                "bearer",
                "password",
            ] {
                assert!(!key.contains(banned), "{key}");
            }
        }
    }

    #[test]
    fn record_sets_data_and_samples_each_window() {
        let state = full_state();
        let provider = &state.providers["claude"];
        assert_eq!(provider.updated_at, Some(NOW));
        assert_eq!(provider.samples.len(), 2);
        assert_eq!(provider.weekly().unwrap().remaining, 80);
        assert_eq!(provider.credits.as_ref().unwrap().available, 3);
    }

    #[test]
    fn record_drops_samples_older_than_seven_days() {
        let mut provider = Provider::default();
        provider.record(
            vec![weekly(90, NOW + 10 * 86_400)],
            None,
            NOW - SAMPLE_RETENTION - 1,
        );
        provider.record(vec![weekly(80, NOW + 10 * 86_400)], None, NOW);
        assert_eq!(provider.samples.len(), 1);
        assert_eq!(provider.samples[0].at, NOW);
    }

    #[test]
    fn samples_for_keeps_only_the_same_reset_window() {
        let mut provider = Provider::default();
        provider.record(vec![weekly(90, NOW)], None, NOW - 7200);
        provider.record(vec![weekly(85, NOW + 604_800)], None, NOW - 3600);
        provider.record(vec![weekly(80, NOW + 604_800)], None, NOW);
        let current = provider.weekly().unwrap().clone();
        assert_eq!(
            provider.samples_for(&current),
            vec![(NOW - 3600, 15.0), (NOW, 20.0)]
        );
    }

    #[test]
    fn samples_for_tolerates_reset_jitter() {
        let mut provider = Provider::default();
        provider.record(vec![weekly(90, NOW - 1)], None, NOW - 3600);
        provider.record(vec![weekly(80, NOW)], None, NOW);
        let current = provider.weekly().unwrap().clone();
        assert_eq!(
            provider.samples_for(&current),
            vec![(NOW - 3600, 10.0), (NOW, 20.0)]
        );
    }

    #[test]
    fn save_then_load_round_trips_with_private_mode() {
        let dir = scratch_dir("roundtrip");
        let path = dir.join("state.json");
        let state = full_state();
        save(&path, &state).unwrap();
        assert_eq!(load(&path), state);
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(leftovers.len(), 1);
    }

    #[test]
    fn load_tolerates_missing_and_corrupt_files() {
        let dir = scratch_dir("corrupt");
        assert_eq!(load(&dir.join("missing.json")), State::default());
        let path = dir.join("state.json");
        std::fs::write(&path, "{not json").unwrap();
        assert_eq!(load(&path), State::default());
    }

    #[test]
    fn save_fails_when_the_directory_is_missing() {
        let path = scratch_dir("missing").join("gone").join("state.json");
        assert!(save(&path, &State::default()).is_err());
    }
}
