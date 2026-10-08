use crate::{
    burn::{self, Projection},
    label::STALE_AFTER,
    state::{Provider, ResetCredits},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Text,
    Subtle,
    Warn,
    Alert,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    pub text: String,
    pub tone: Tone,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowView {
    pub label: String,
    pub remaining: Line,
    pub reset: String,
    pub burn: Line,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderView {
    pub name: String,
    pub windows: Vec<WindowView>,
    pub credits: Option<String>,
    pub notes: Vec<Line>,
}

impl ProviderView {
    pub fn lines(&self) -> usize {
        1 + 2 * self.windows.len() + usize::from(self.credits.is_some()) + self.notes.len()
    }
}

pub fn duration(seconds: i64) -> String {
    let seconds = seconds.max(0);
    let (days, hours, minutes) = (
        seconds / 86_400,
        seconds % 86_400 / 3600,
        seconds % 3600 / 60,
    );
    match (days, hours, minutes) {
        (0, 0, 0) => "<1m".into(),
        (0, 0, _) => format!("{minutes}m"),
        (0, _, _) => format!("{hours}h {minutes}m"),
        _ => format!("{days}d {hours}h"),
    }
}

pub fn reset(resets_at: Option<i64>, now: i64) -> String {
    match resets_at {
        None => "reset unknown".into(),
        Some(at) if at <= now => "reset passed".into(),
        Some(at) => format!("resets in {}", duration(at - now)),
    }
}

pub fn burn(projection: Option<Projection>, now: i64) -> Line {
    match projection {
        None => line("not enough data", Tone::Subtle),
        Some(projection) if projection.before_reset => line(
            &format!(
                "runs out in {} — before reset",
                duration(projection.exhausted_at - now)
            ),
            Tone::Warn,
        ),
        Some(_) => line("lasts until reset", Tone::Subtle),
    }
}

pub fn credits(credits: &ResetCredits, now: i64) -> String {
    let base = format!("Reset credits: {} available", credits.available);
    match credits.nearest_expiry {
        Some(at) if at > now => format!("{base} · next expires in {}", duration(at - now)),
        _ => base,
    }
}

pub fn provider(id: &str, provider: Option<&Provider>, now: i64) -> ProviderView {
    let name = match id {
        "claude" => "Claude".into(),
        "codex" => "Codex".into(),
        other => other.into(),
    };
    let Some(provider) = provider else {
        return ProviderView {
            name,
            windows: vec![],
            credits: None,
            notes: vec![line("no data yet", Tone::Subtle)],
        };
    };
    let windows = provider
        .windows
        .iter()
        .map(|window| {
            let current = window.resets_at.is_none_or(|at| at > now);
            let remaining = match window.remaining {
                _ if !current => line("—", Tone::Subtle),
                left if left <= 10 => line(&format!("{left}%"), Tone::Alert),
                left => line(&format!("{left}%"), Tone::Text),
            };
            let projection = current
                .then(|| burn::project(&provider.samples_for(window), window.resets_at))
                .flatten();
            WindowView {
                label: window.label.clone(),
                remaining,
                reset: reset(window.resets_at, now),
                burn: burn(projection, now),
            }
        })
        .collect();
    let mut notes = vec![];
    if let Some(error) = &provider.error {
        notes.push(line(error, Tone::Warn));
    }
    match provider.updated_at {
        Some(at) if now - at > STALE_AFTER => {
            notes.push(line(
                &format!("data is {} old", duration(now - at)),
                Tone::Warn,
            ));
        }
        None if provider.error.is_none() => notes.push(line("no data yet", Tone::Subtle)),
        _ => {}
    }
    ProviderView {
        name,
        windows,
        credits: provider
            .credits
            .as_ref()
            .map(|credits| self::credits(credits, now)),
        notes,
    }
}

fn line(text: &str, tone: Tone) -> Line {
    Line {
        text: text.into(),
        tone,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{Sample, Window};

    const NOW: i64 = 1_800_000_000;

    fn window(remaining: u8, resets_at: Option<i64>) -> Window {
        Window {
            label: "Weekly".into(),
            remaining,
            resets_at,
            weekly: true,
        }
    }

    fn sample(at: i64, used: f64, resets_at: Option<i64>) -> Sample {
        Sample {
            window: "Weekly".into(),
            resets_at,
            at,
            used,
        }
    }

    #[test]
    fn durations_use_the_two_largest_units() {
        assert_eq!(duration(-5), "<1m");
        assert_eq!(duration(59), "<1m");
        assert_eq!(duration(60), "1m");
        assert_eq!(duration(3 * 3600 + 12 * 60 + 59), "3h 12m");
        assert_eq!(duration(2 * 86_400 + 4 * 3600 + 30 * 60), "2d 4h");
    }

    #[test]
    fn resets_count_down_until_they_pass() {
        assert_eq!(reset(Some(NOW + 3 * 3600 + 720), NOW), "resets in 3h 12m");
        assert_eq!(reset(Some(NOW), NOW), "reset passed");
        assert_eq!(reset(None, NOW), "reset unknown");
    }

    #[test]
    fn burn_lines_describe_the_projection() {
        assert_eq!(burn(None, NOW), line("not enough data", Tone::Subtle));
        let early = Projection {
            exhausted_at: NOW + 86_400 + 3 * 3600,
            before_reset: true,
        };
        assert_eq!(
            burn(Some(early), NOW),
            line("runs out in 1d 3h — before reset", Tone::Warn)
        );
        let late = Projection {
            exhausted_at: NOW + 86_400,
            before_reset: false,
        };
        assert_eq!(
            burn(Some(late), NOW),
            line("lasts until reset", Tone::Subtle)
        );
    }

    #[test]
    fn credits_name_the_next_expiry_while_it_is_ahead() {
        let mut credits_left = ResetCredits {
            available: 2,
            nearest_expiry: Some(NOW + 5 * 86_400),
        };
        assert_eq!(
            credits(&credits_left, NOW),
            "Reset credits: 2 available · next expires in 5d 0h"
        );
        credits_left.nearest_expiry = Some(NOW);
        assert_eq!(credits(&credits_left, NOW), "Reset credits: 2 available");
        credits_left.nearest_expiry = None;
        assert_eq!(credits(&credits_left, NOW), "Reset credits: 2 available");
    }

    #[test]
    fn remaining_turns_red_at_ten_percent_and_blanks_after_a_reset() {
        let data = Provider {
            windows: vec![
                window(10, Some(NOW + 60)),
                window(11, Some(NOW + 60)),
                window(50, Some(NOW)),
            ],
            updated_at: Some(NOW),
            ..Default::default()
        };
        let view = provider("claude", Some(&data), NOW);
        let remaining: Vec<_> = view.windows.iter().map(|w| w.remaining.clone()).collect();
        assert_eq!(
            remaining,
            [
                line("10%", Tone::Alert),
                line("11%", Tone::Text),
                line("—", Tone::Subtle)
            ]
        );
        assert_eq!(view.windows[2].reset, "reset passed");
    }

    #[test]
    fn windows_project_from_their_own_samples() {
        let resets_at = Some(NOW + 2 * 86_400);
        let data = Provider {
            windows: vec![window(80, resets_at)],
            samples: vec![
                sample(NOW - 3600, 10.0, resets_at),
                sample(NOW, 20.0, resets_at),
                sample(NOW - 7200, 0.0, Some(NOW - 1)),
            ],
            updated_at: Some(NOW),
            ..Default::default()
        };
        let view = provider("claude", Some(&data), NOW);
        assert_eq!(
            view.windows[0].burn,
            line("runs out in 8h 0m — before reset", Tone::Warn)
        );
    }

    #[test]
    fn a_passed_reset_has_no_projection() {
        let data = Provider {
            windows: vec![window(80, Some(NOW - 60))],
            samples: vec![
                sample(NOW - 3600, 10.0, Some(NOW - 60)),
                sample(NOW - 120, 20.0, Some(NOW - 60)),
            ],
            updated_at: Some(NOW),
            ..Default::default()
        };
        let view = provider("claude", Some(&data), NOW);
        assert_eq!(view.windows[0].burn, line("not enough data", Tone::Subtle));
    }

    #[test]
    fn notes_show_the_error_and_the_age_of_stale_data() {
        let data = Provider {
            windows: vec![window(80, None)],
            credits: Some(ResetCredits {
                available: 1,
                nearest_expiry: None,
            }),
            updated_at: Some(NOW - STALE_AFTER - 3600),
            error: Some("Rate limited".into()),
            ..Default::default()
        };
        let view = provider("codex", Some(&data), NOW);
        assert_eq!(view.name, "Codex");
        assert_eq!(view.credits.as_deref(), Some("Reset credits: 1 available"));
        assert_eq!(
            view.notes,
            [
                line("Rate limited", Tone::Warn),
                line("data is 1h 30m old", Tone::Warn)
            ]
        );
        assert_eq!(view.lines(), 1 + 2 + 1 + 2);
    }

    #[test]
    fn fresh_data_has_no_notes() {
        let data = Provider {
            windows: vec![window(80, None)],
            updated_at: Some(NOW - STALE_AFTER),
            ..Default::default()
        };
        assert!(provider("claude", Some(&data), NOW).notes.is_empty());
    }

    #[test]
    fn a_provider_without_data_says_so() {
        let view = provider("claude", None, NOW);
        assert_eq!(view.name, "Claude");
        assert!(view.windows.is_empty());
        assert_eq!(view.notes, [line("no data yet", Tone::Subtle)]);
        assert_eq!(view.lines(), 2);
    }
}
