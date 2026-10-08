use serde::{Deserialize, Serialize};

pub const SUCCESS_INTERVAL: i64 = 15 * 60;
pub const ATTEMPT_HOLD: i64 = 60 * 60;
pub const RATE_LIMIT_FLOOR: i64 = 60 * 60;
pub const LOCAL_RETRY: i64 = 5 * 60;
pub const FORCE_GAP: i64 = 5 * 60;
pub const BASE_BACKOFF: i64 = 60 * 60;
pub const MAX_BACKOFF: i64 = 6 * 60 * 60;
pub const MAX_FAILURES: u32 = 6;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderState {
    pub attempted_at: i64,
    pub next_attempt: i64,
    pub failures: u32,
    pub last_ok: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trigger {
    Background,
    Forced,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Success,
    RateLimited { retry_after: Option<i64> },
    ServerFailure,
    LocalFailure,
}

pub fn normalize(state: &ProviderState, now: i64) -> ProviderState {
    if state.attempted_at <= now {
        return state.clone();
    }
    let shift = state.attempted_at - now;
    ProviderState {
        attempted_at: now,
        next_attempt: state.next_attempt - shift,
        ..state.clone()
    }
}

pub fn may_fetch(state: &ProviderState, trigger: Trigger, now: i64) -> bool {
    let state = normalize(state, now);
    match trigger {
        Trigger::Background => now >= state.next_attempt,
        Trigger::Forced => state.last_ok && now >= state.attempted_at + FORCE_GAP,
    }
}

pub fn begin(state: &ProviderState, now: i64) -> ProviderState {
    ProviderState {
        attempted_at: now,
        next_attempt: now + ATTEMPT_HOLD,
        last_ok: false,
        ..state.clone()
    }
}

pub fn finish(state: &ProviderState, outcome: Outcome, now: i64) -> ProviderState {
    let mut next = normalize(state, now);
    match outcome {
        Outcome::Success => {
            next.failures = 0;
            next.last_ok = true;
            next.next_attempt = now + SUCCESS_INTERVAL;
        }
        Outcome::RateLimited { retry_after } => {
            next.failures = (next.failures + 1).min(MAX_FAILURES);
            next.last_ok = false;
            next.next_attempt = now + retry_after.unwrap_or(0).max(RATE_LIMIT_FLOOR);
        }
        Outcome::ServerFailure => {
            next.failures = (next.failures + 1).min(MAX_FAILURES);
            next.last_ok = false;
            next.next_attempt = now + backoff(next.failures);
        }
        Outcome::LocalFailure => {
            next.last_ok = false;
            next.next_attempt = now + LOCAL_RETRY;
        }
    }
    next
}

fn backoff(failures: u32) -> i64 {
    (BASE_BACKOFF << failures.saturating_sub(1).min(4)).min(MAX_BACKOFF)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_800_000_000;

    fn ok_state(attempted_at: i64) -> ProviderState {
        finish(
            &begin(&ProviderState::default(), attempted_at),
            Outcome::Success,
            attempted_at,
        )
    }

    #[test]
    fn fresh_state_allows_a_background_fetch() {
        assert!(may_fetch(
            &ProviderState::default(),
            Trigger::Background,
            NOW
        ));
    }

    #[test]
    fn begin_holds_every_trigger_for_an_hour() {
        let held = begin(&ProviderState::default(), NOW);
        assert_eq!(held.attempted_at, NOW);
        assert_eq!(held.next_attempt, NOW + ATTEMPT_HOLD);
        assert!(!may_fetch(&held, Trigger::Background, NOW + 1));
        assert!(!may_fetch(&held, Trigger::Forced, NOW + FORCE_GAP));
    }

    #[test]
    fn restart_after_a_crash_mid_request_keeps_the_hold() {
        let persisted = begin(&ok_state(NOW - 3600), NOW);
        let reloaded: ProviderState =
            serde_json::from_str(&serde_json::to_string(&persisted).unwrap()).unwrap();
        assert!(!may_fetch(&reloaded, Trigger::Background, NOW + 10));
        assert!(!may_fetch(&reloaded, Trigger::Forced, NOW + 10));
        assert!(may_fetch(
            &reloaded,
            Trigger::Background,
            NOW + ATTEMPT_HOLD
        ));
    }

    #[test]
    fn success_schedules_the_next_fetch_in_fifteen_minutes() {
        let state = ok_state(NOW);
        assert_eq!(state.next_attempt, NOW + SUCCESS_INTERVAL);
        assert!(state.last_ok);
        assert_eq!(state.failures, 0);
        assert!(!may_fetch(
            &state,
            Trigger::Background,
            NOW + SUCCESS_INTERVAL - 1
        ));
        assert!(may_fetch(
            &state,
            Trigger::Background,
            NOW + SUCCESS_INTERVAL
        ));
    }

    #[test]
    fn forced_fetch_needs_five_minutes_since_the_last_attempt() {
        let state = ok_state(NOW);
        assert!(!may_fetch(&state, Trigger::Forced, NOW + FORCE_GAP - 1));
        assert!(may_fetch(&state, Trigger::Forced, NOW + FORCE_GAP));
    }

    #[test]
    fn forced_fetch_is_refused_after_any_failure() {
        for outcome in [
            Outcome::RateLimited { retry_after: None },
            Outcome::ServerFailure,
            Outcome::LocalFailure,
        ] {
            let state = finish(&begin(&ok_state(NOW - 7200), NOW), outcome, NOW);
            assert!(
                !may_fetch(&state, Trigger::Forced, NOW + 3 * 3600),
                "{outcome:?}"
            );
        }
    }

    #[test]
    fn rate_limit_waits_at_least_an_hour() {
        let short = finish(
            &begin(&ProviderState::default(), NOW),
            Outcome::RateLimited {
                retry_after: Some(30),
            },
            NOW,
        );
        assert_eq!(short.next_attempt, NOW + RATE_LIMIT_FLOOR);
        let long = finish(
            &begin(&ProviderState::default(), NOW),
            Outcome::RateLimited {
                retry_after: Some(7200),
            },
            NOW,
        );
        assert_eq!(long.next_attempt, NOW + 7200);
        let none = finish(
            &begin(&ProviderState::default(), NOW),
            Outcome::RateLimited { retry_after: None },
            NOW,
        );
        assert_eq!(none.next_attempt, NOW + RATE_LIMIT_FLOOR);
    }

    #[test]
    fn server_failures_back_off_from_one_to_six_hours() {
        let mut state = ProviderState::default();
        let mut now = NOW;
        let mut delays = vec![];
        for _ in 0..8 {
            state = finish(&begin(&state, now), Outcome::ServerFailure, now);
            delays.push(state.next_attempt - now);
            now = state.next_attempt;
        }
        assert_eq!(
            delays,
            vec![3600, 7200, 14400, 21600, 21600, 21600, 21600, 21600]
        );
        assert_eq!(state.failures, MAX_FAILURES);
        let recovered = finish(&begin(&state, now), Outcome::Success, now);
        assert_eq!(recovered.failures, 0);
    }

    #[test]
    fn local_failure_retries_in_five_minutes_without_counting() {
        let state = finish(
            &begin(&ProviderState::default(), NOW),
            Outcome::LocalFailure,
            NOW,
        );
        assert_eq!(state.next_attempt, NOW + LOCAL_RETRY);
        assert_eq!(state.failures, 0);
        assert!(!state.last_ok);
    }

    #[test]
    fn clock_moving_backwards_keeps_the_scheduled_delay() {
        let state = ok_state(NOW);
        let earlier = NOW - 86_400;
        let normalized = normalize(&state, earlier);
        assert_eq!(normalized.attempted_at, earlier);
        assert_eq!(normalized.next_attempt, earlier + SUCCESS_INTERVAL);
        assert!(!may_fetch(&normalized, Trigger::Background, earlier + 1));
        assert!(may_fetch(
            &normalized,
            Trigger::Background,
            earlier + SUCCESS_INTERVAL
        ));
    }

    #[test]
    fn normalize_leaves_a_past_attempt_alone() {
        let state = ok_state(NOW);
        assert_eq!(normalize(&state, NOW + 10), state);
    }
}
