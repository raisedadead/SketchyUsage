use serde_json::Value;

pub const GAP: f64 = 8.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Segment {
    pub display: String,
    pub rect: Rect,
}

impl Rect {
    pub fn contains(&self, x: f64, y: f64) -> bool {
        (self.x..=self.x + self.width).contains(&x) && (self.y..=self.y + self.height).contains(&y)
    }

    fn right(&self) -> f64 {
        self.x + self.width
    }
}

pub fn segments(query: &Value) -> Vec<Segment> {
    let Some(displays) = query["bounding_rects"].as_object() else {
        return vec![];
    };
    let mut segments: Vec<Segment> = displays
        .iter()
        .filter_map(|(display, rect)| {
            Some(Segment {
                display: display.clone(),
                rect: Rect {
                    x: rect["origin"][0].as_f64()?,
                    y: rect["origin"][1].as_f64()?,
                    width: rect["size"][0].as_f64()?,
                    height: rect["size"][1].as_f64()?,
                },
            })
        })
        .collect();
    segments.sort_by(|a, b| a.display.cmp(&b.display));
    segments
}

pub fn anchor(segments: &[Segment], mouse: (f64, f64), width: f64, height: f64) -> Option<Rect> {
    let display = &segments
        .iter()
        .find(|segment| segment.rect.contains(mouse.0, mouse.1))
        .or(segments.first())?
        .display;
    let rightmost = segments
        .iter()
        .filter(|segment| &segment.display == display)
        .map(|segment| segment.rect)
        .max_by(|a, b| a.right().total_cmp(&b.right()))?;
    Some(Rect {
        x: rightmost.right() - width,
        y: rightmost.y - GAP - height,
        width,
        height,
    })
}

pub fn from_cocoa(x: f64, y: f64, primary_height: f64) -> (f64, f64) {
    (x, primary_height - y)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn rect(x: f64, y: f64, width: f64, height: f64) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    fn segment(display: &str, rect: Rect) -> Segment {
        Segment {
            display: display.into(),
            rect,
        }
    }

    #[test]
    fn rects_include_their_edges() {
        let r = rect(10.0, 20.0, 30.0, 40.0);
        assert!(r.contains(10.0, 20.0));
        assert!(r.contains(40.0, 60.0));
        assert!(!r.contains(9.9, 30.0));
        assert!(!r.contains(20.0, 60.1));
    }

    #[test]
    fn segments_come_from_the_bounding_rects() {
        let query = json!({
            "name": "sketchyusage.claude",
            "bounding_rects": {
                "display-2": { "origin": [100.0, 1400.0], "size": [60.0, 32.0] },
                "display-1": { "origin": [3000.0, 1408], "size": [58, 32.0] },
                "display-3": { "origin": "bad" }
            }
        });
        assert_eq!(
            segments(&query),
            [
                segment("display-1", rect(3000.0, 1408.0, 58.0, 32.0)),
                segment("display-2", rect(100.0, 1400.0, 60.0, 32.0)),
            ]
        );
        assert!(segments(&json!({})).is_empty());
    }

    #[test]
    fn the_panel_ends_at_the_rightmost_segment_above_the_bar() {
        let bar = [
            segment("display-1", rect(3000.0, 1408.0, 58.0, 32.0)),
            segment("display-1", rect(3058.0, 1408.0, 60.0, 32.0)),
        ];
        assert_eq!(
            anchor(&bar, (0.0, 0.0), 420.0, 300.0),
            Some(rect(3118.0 - 420.0, 1408.0 - GAP - 300.0, 420.0, 300.0))
        );
        assert_eq!(anchor(&[], (0.0, 0.0), 420.0, 300.0), None);
    }

    #[test]
    fn the_display_under_the_mouse_wins() {
        let bar = [
            segment("display-1", rect(3000.0, 1408.0, 58.0, 32.0)),
            segment("display-2", rect(1000.0, 1000.0, 60.0, 32.0)),
        ];
        let panel = anchor(&bar, (1010.0, 1010.0), 420.0, 300.0).unwrap();
        assert_eq!(panel.x, 1060.0 - 420.0);
        let fallback = anchor(&bar, (0.0, 0.0), 420.0, 300.0).unwrap();
        assert_eq!(fallback.x, 3058.0 - 420.0);
    }

    #[test]
    fn cocoa_points_flip_against_the_primary_screen() {
        assert_eq!(from_cocoa(3050.0, 20.0, 1440.0), (3050.0, 1420.0));
    }
}
