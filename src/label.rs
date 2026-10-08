use crate::state::Provider;

pub const STALE_AFTER: i64 = 30 * 60;

pub fn label(provider: Option<&Provider>, now: i64) -> String {
    let Some(provider) = provider else {
        return "—".into();
    };
    let current = provider
        .weekly()
        .filter(|window| window.resets_at.is_none_or(|reset| reset > now));
    let stale =
        provider.error.is_some() || provider.updated_at.is_some_and(|at| now - at > STALE_AFTER);
    let base = current.map_or_else(
        || "—".to_string(),
        |window| format!("{}%", window.remaining),
    );
    if stale { format!("{base} !") } else { base }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Window;

    const NOW: i64 = 1_800_000_000;

    fn provider(
        remaining: u8,
        resets_at: Option<i64>,
        updated_at: i64,
        error: Option<&str>,
    ) -> Provider {
        Provider {
            windows: vec![
                Window {
                    label: "5-hour".into(),
                    remaining: 10,
                    resets_at: None,
                    weekly: false,
                },
                Window {
                    label: "Weekly".into(),
                    remaining,
                    resets_at,
                    weekly: true,
                },
            ],
            updated_at: Some(updated_at),
            error: error.map(Into::into),
            ..Default::default()
        }
    }

    #[test]
    fn fresh_data_shows_the_weekly_remaining_percent() {
        assert_eq!(
            label(Some(&provider(63, Some(NOW + 3600), NOW - 60, None)), NOW),
            "63%"
        );
    }

    #[test]
    fn data_older_than_thirty_minutes_is_marked() {
        assert_eq!(
            label(Some(&provider(63, None, NOW - STALE_AFTER, None)), NOW),
            "63%"
        );
        assert_eq!(
            label(Some(&provider(63, None, NOW - STALE_AFTER - 1, None)), NOW),
            "63% !"
        );
    }

    #[test]
    fn a_failed_fetch_is_marked() {
        assert_eq!(
            label(Some(&provider(63, None, NOW, Some("Rate limited"))), NOW),
            "63% !"
        );
    }

    #[test]
    fn a_passed_reset_shows_a_dash() {
        assert_eq!(
            label(Some(&provider(63, Some(NOW), NOW - 60, None)), NOW),
            "—"
        );
        assert_eq!(
            label(Some(&provider(63, Some(NOW - 1), NOW - 60, None)), NOW),
            "—"
        );
    }

    #[test]
    fn missing_data_shows_a_dash() {
        assert_eq!(label(None, NOW), "—");
        assert_eq!(label(Some(&Provider::default()), NOW), "—");
        let failed = Provider {
            error: Some("Codex timed out".into()),
            ..Default::default()
        };
        assert_eq!(label(Some(&failed), NOW), "— !");
    }
}
