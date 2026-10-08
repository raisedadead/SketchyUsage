pub const MIN_SPAN: i64 = 30 * 60;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Projection {
    pub exhausted_at: i64,
    pub before_reset: bool,
}

pub fn project(samples: &[(i64, f64)], resets_at: Option<i64>) -> Option<Projection> {
    let first = samples.iter().min_by_key(|sample| sample.0)?;
    let last = samples.iter().max_by_key(|sample| sample.0)?;
    let span = last.0 - first.0;
    let delta = last.1 - first.1;
    if span < MIN_SPAN || delta <= 0.0 {
        return None;
    }
    let rate = delta / span as f64;
    let left = (100.0 - last.1).max(0.0);
    let exhausted_at = last.0 + (left / rate).round() as i64;
    Some(Projection {
        exhausted_at,
        before_reset: resets_at.is_some_and(|reset| exhausted_at < reset),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_800_000_000;

    #[test]
    fn needs_two_samples() {
        assert_eq!(project(&[], None), None);
        assert_eq!(project(&[(NOW, 20.0)], None), None);
    }

    #[test]
    fn needs_thirty_minutes_between_samples() {
        assert_eq!(
            project(&[(NOW - MIN_SPAN + 1, 10.0), (NOW, 20.0)], None),
            None
        );
    }

    #[test]
    fn needs_a_positive_delta() {
        assert_eq!(project(&[(NOW - 3600, 20.0), (NOW, 20.0)], None), None);
        assert_eq!(project(&[(NOW - 3600, 30.0), (NOW, 20.0)], None), None);
    }

    #[test]
    fn projects_linearly_from_the_first_and_last_sample() {
        let samples = [(NOW - 3600, 10.0), (NOW - 1800, 12.0), (NOW, 20.0)];
        let projection = project(&samples, Some(NOW + 100_000)).unwrap();
        assert_eq!(projection.exhausted_at, NOW + 8 * 3600);
        assert!(projection.before_reset);
    }

    #[test]
    fn reports_when_the_reset_comes_first() {
        let projection = project(&[(NOW - 3600, 10.0), (NOW, 20.0)], Some(NOW + 3600)).unwrap();
        assert!(!projection.before_reset);
        let unknown = project(&[(NOW - 3600, 10.0), (NOW, 20.0)], None).unwrap();
        assert!(!unknown.before_reset);
    }

    #[test]
    fn unsorted_samples_are_ordered_by_time() {
        let projection = project(&[(NOW, 20.0), (NOW - 3600, 10.0)], None).unwrap();
        assert_eq!(projection.exhausted_at, NOW + 8 * 3600);
    }

    #[test]
    fn an_exhausted_window_projects_now() {
        let projection = project(&[(NOW - 3600, 90.0), (NOW, 100.0)], Some(NOW + 60)).unwrap();
        assert_eq!(projection.exhausted_at, NOW);
        assert!(projection.before_reset);
    }
}
