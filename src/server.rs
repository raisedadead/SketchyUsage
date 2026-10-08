use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io::{self, BufRead, BufReader, Write},
    os::{
        fd::AsRawFd,
        unix::{
            fs::{OpenOptionsExt, PermissionsExt},
            net::{UnixListener, UnixStream},
        },
    },
    path::Path,
    sync::{Arc, Mutex},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use crate::{
    bar, claude, client, codex, label,
    paths::Paths,
    policy::{self, Outcome, Trigger},
    provider::{Failure, Fetched},
    state::{self, State},
};

pub const TICK: Duration = Duration::from_secs(60);
pub const PROVIDERS: [&str; 2] = ["claude", "codex"];

pub type ToggleHook = Box<dyn Fn() + Send + Sync>;

pub enum Start {
    AlreadyRunning,
    Running(Arc<Server>),
}

struct Inner {
    state: State,
    in_flight: BTreeSet<&'static str>,
}

pub struct Server {
    paths: Paths,
    inner: Mutex<Inner>,
    publishing: Mutex<()>,
    on_toggle: ToggleHook,
    _lock: File,
}

pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs() as i64)
}

pub fn start(paths: Paths, on_toggle: ToggleHook) -> io::Result<Start> {
    paths.create()?;
    let lock = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(&paths.lock)?;
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::EWOULDBLOCK) {
            return Ok(Start::AlreadyRunning);
        }
        return Err(error);
    }
    match fs::remove_file(&paths.socket) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
        _ => {}
    }
    let listener = UnixListener::bind(&paths.socket)?;
    fs::set_permissions(&paths.socket, fs::Permissions::from_mode(0o600))?;
    let server = Arc::new(Server {
        inner: Mutex::new(Inner {
            state: state::load(&paths.state),
            in_flight: BTreeSet::new(),
        }),
        paths,
        publishing: Mutex::new(()),
        on_toggle,
        _lock: lock,
    });
    let scheduler = server.clone();
    thread::spawn(move || {
        loop {
            scheduler.tick(Trigger::Background);
            thread::sleep(TICK);
        }
    });
    let acceptor = server.clone();
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let handler = acceptor.clone();
            thread::spawn(move || handler.handle(stream));
        }
    });
    Ok(Start::Running(server))
}

impl Server {
    pub fn snapshot(&self) -> State {
        self.inner.lock().unwrap().state.clone()
    }

    pub fn tick(self: &Arc<Self>, trigger: Trigger) {
        let now = now();
        for name in PROVIDERS {
            let due = {
                let mut inner = self.inner.lock().unwrap();
                let busy = inner.in_flight.contains(name);
                let provider = inner.state.providers.entry(name.into()).or_default();
                let normalized = policy::normalize(&provider.policy, now);
                let changed = normalized != provider.policy;
                provider.policy = normalized;
                let due = !busy && policy::may_fetch(&provider.policy, trigger, now);
                if due {
                    provider.policy = policy::begin(&provider.policy, now);
                }
                let saved = (changed || due) && self.save(&inner.state);
                if due && saved {
                    inner.in_flight.insert(name);
                }
                due && saved
            };
            if due {
                let worker = self.clone();
                thread::spawn(move || worker.fetch(name));
            }
        }
        self.publish();
    }

    fn fetch(self: Arc<Self>, name: &'static str) {
        eprintln!("sketchyusage: {name} fetch started");
        let result: Result<Fetched, Failure> = match name {
            "claude" => claude::usage(now()),
            _ => codex::usage(),
        };
        let now = now();
        {
            let mut inner = self.inner.lock().unwrap();
            let provider = inner.state.providers.entry(name.into()).or_default();
            match result {
                Ok(fetched) => {
                    provider.policy = policy::finish(&provider.policy, Outcome::Success, now);
                    provider.record(fetched.windows, fetched.credits, now);
                    eprintln!("sketchyusage: {name} fetch succeeded");
                }
                Err(failure) => {
                    provider.policy = policy::finish(&provider.policy, failure.outcome, now);
                    provider.error = Some(failure.message.into());
                    eprintln!("sketchyusage: {name} fetch failed: {}", failure.message);
                }
            }
            inner.in_flight.remove(name);
            self.save(&inner.state);
        }
        self.publish();
    }

    pub fn publish(&self) {
        let _publishing = self.publishing.lock().unwrap();
        let now = now();
        let (claude, codex, stale_at) = {
            let inner = self.inner.lock().unwrap();
            let providers = &inner.state.providers;
            (
                label::label(providers.get("claude"), now),
                label::label(providers.get("codex"), now),
                stale_at(&inner.state, now),
            )
        };
        if !bar::set_labels(&claude, &codex) {
            eprintln!("sketchyusage: sketchybar label update failed");
        }
        if let Err(error) = write_atomic(&self.paths.heartbeat, &format!("{now} {stale_at}\n")) {
            eprintln!("sketchyusage: heartbeat write failed: {error}");
        }
    }

    fn handle(self: Arc<Self>, stream: UnixStream) {
        if stream.set_read_timeout(Some(client::TIMEOUT)).is_err()
            || stream.set_write_timeout(Some(client::TIMEOUT)).is_err()
        {
            return;
        }
        let mut line = String::new();
        if BufReader::new(&stream).read_line(&mut line).is_err() {
            return;
        }
        let reply = match line.trim() {
            "push" => {
                let server = self.clone();
                thread::spawn(move || server.publish());
                "ok\n"
            }
            "toggle" => {
                (self.on_toggle)();
                let server = self.clone();
                thread::spawn(move || server.tick(Trigger::Forced));
                "ok\n"
            }
            _ => "error\n",
        };
        let _ = (&stream).write_all(reply.as_bytes());
    }

    fn save(&self, state: &State) -> bool {
        match state::save(&self.paths.state, state) {
            Ok(()) => true,
            Err(error) => {
                eprintln!("sketchyusage: state save failed: {error}");
                false
            }
        }
    }
}

fn stale_at(state: &State, now: i64) -> i64 {
    PROVIDERS
        .iter()
        .filter_map(|name| state.providers.get(*name))
        .filter_map(|provider| {
            if provider.error.is_some() {
                Some(now)
            } else {
                provider
                    .updated_at
                    .map(|at| (at + label::STALE_AFTER).max(now))
            }
        })
        .min()
        .unwrap_or(now + label::STALE_AFTER)
}

fn write_atomic(path: &Path, text: &str) -> io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::other("path has no directory"))?;
    let tmp = dir.join(format!(".heartbeat.{}.tmp", std::process::id()));
    let result = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)
        .and_then(|mut file| file.write_all(text.as_bytes()))
        .and_then(|()| fs::rename(&tmp, path));
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{Provider, Window};

    const NOW: i64 = 1_800_000_000;

    fn provider(updated_at: Option<i64>, error: Option<&str>) -> Provider {
        Provider {
            windows: vec![Window {
                label: "Weekly".into(),
                remaining: 50,
                resets_at: None,
                weekly: true,
            }],
            updated_at,
            error: error.map(Into::into),
            ..Default::default()
        }
    }

    fn state(providers: &[(&str, Provider)]) -> State {
        State {
            providers: providers
                .iter()
                .map(|(name, provider)| ((*name).into(), provider.clone()))
                .collect(),
        }
    }

    #[test]
    fn stale_at_is_the_earliest_data_expiry() {
        let state = state(&[
            ("claude", provider(Some(NOW - 600), None)),
            ("codex", provider(Some(NOW - 60), None)),
        ]);
        assert_eq!(stale_at(&state, NOW), NOW - 600 + label::STALE_AFTER);
    }

    #[test]
    fn stale_at_is_now_when_a_provider_failed_or_is_already_stale() {
        let failed = state(&[("claude", provider(Some(NOW), Some("down")))]);
        assert_eq!(stale_at(&failed, NOW), NOW);
        let old = state(&[("codex", provider(Some(NOW - 7200), None))]);
        assert_eq!(stale_at(&old, NOW), NOW);
    }

    #[test]
    fn stale_at_without_data_is_one_stale_period_ahead() {
        assert_eq!(stale_at(&State::default(), NOW), NOW + label::STALE_AFTER);
    }
}
