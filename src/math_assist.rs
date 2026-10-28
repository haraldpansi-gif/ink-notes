//! Computes handwritten math after an `=` (Issue #46, stage 3).
//!
//! When the last two strokes on a page form an `=` and the pen then rests for
//! [`TRIGGER_DELAY`], the strokes left of it are recognized ([`crate::math_ink`]) and computed
//! ([`crate::math::solve`]). The result is offered next to the `=` as a suggestion that also
//! shows what was read (`2⁴ = 16`), so that a misread symbol is noticed. Tapping it with the
//! pen or clicking it inserts the result as a text box; fingers and palms are ignored. Any new
//! stroke drops the suggestion. Off by default (☰ → Input).

use std::sync::Arc;
use std::time::{Duration, Instant};

use glam::Vec2;

use crate::document::{Page, Stroke, StrokeTool};
use crate::math_ink::Recognizer;

/// Pause after the `=` before its expression is computed; a new stroke cancels it
pub const TRIGGER_DELAY: Duration = Duration::from_millis(500);
/// Estimated symbol height relative to the width of the `=`
const SYMBOL_PER_EQUALS: f32 = 1.3;
/// Strokes left of the `=` belong to the expression up to a gap of this many symbol heights
const MAX_GAP: f32 = 2.0;
/// At most this many strokes are read as the expression
const MAX_STROKES: usize = 80;
/// Cap height of the text font relative to its size (Ubuntu: 0.69 em)
const CAP_HEIGHT_PER_EM: f32 = 0.7;

/// A result offered next to an `=`
#[derive(Debug, Clone, PartialEq)]
pub struct Suggestion {
    pub page_id: u64,
    /// The two strokes of the `=`
    pub equals: [u64; 2],
    /// What was read, written neatly (`2⁴`)
    pub expression: String,
    /// The result (`16`)
    pub result: String,
    /// Where the result goes: left end of the text on the vertical middle of the `=`
    pub anchor: Vec2,
    /// Height of the handwritten digits in page units
    pub symbol_height: f32,
    /// Ink color of the `=`
    pub color: [f32; 4],
}

impl Suggestion {
    /// Top left corner and size (em) of a text box that puts the result next to the `=`, as
    /// high as the handwriting
    pub fn text_placement(&self, ascent_per_em: f32) -> (Vec2, f32) {
        let size = self.symbol_height / CAP_HEIGHT_PER_EM;
        // Baseline at the bottom of digits centered on the `=`
        let baseline = self.anchor.y + self.symbol_height / 2.0;
        (
            Vec2::new(self.anchor.x, baseline - ascent_per_em * size),
            size,
        )
    }
}

#[derive(Debug, Clone)]
struct Pending {
    page_id: u64,
    equals: [u64; 2],
    due: Instant,
}

/// State of the math assistant: waiting after an `=`, and the suggestion shown
#[derive(Default, Clone)]
pub struct MathAssist {
    pub enabled: bool,
    /// Built on first use
    recognizer: Option<Arc<Recognizer>>,
    pending: Option<Pending>,
    suggestion: Option<Suggestion>,
    /// Where the UI drew the suggestion (egui points), for [`Self::take_hit`]
    pub chip_rect: Option<egui::Rect>,
}

impl std::fmt::Debug for MathAssist {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MathAssist")
            .field("enabled", &self.enabled)
            .field("pending", &self.pending)
            .field("suggestion", &self.suggestion)
            .finish_non_exhaustive()
    }
}

impl MathAssist {
    /// After a stroke was added to `page`: drops the suggestion and waits if the stroke
    /// completed an `=`
    pub fn stroke_committed(&mut self, page: &Page, now: Instant) {
        self.suggestion = None;
        self.chip_rect = None;
        self.pending = None;
        if !self.enabled {
            return;
        }
        if let Some((a, b)) = equals_at_end(&page.strokes) {
            self.pending = Some(Pending {
                page_id: page.id,
                equals: [page.strokes[a].id, page.strokes[b].id],
                due: now + TRIGGER_DELAY,
            });
        }
    }

    /// When [`Self::update`] has something to do
    pub fn deadline(&self) -> Option<Instant> {
        self.pending.as_ref().map(|p| p.due)
    }

    /// Computes the expression once the pause after the `=` is over (not while a stroke is
    /// being drawn). True if a suggestion appeared.
    pub fn update(
        &mut self,
        page: &Page,
        now: Instant,
        drawing: bool,
        decimal_comma: bool,
    ) -> bool {
        let Some(pending) = self.pending.as_ref() else {
            return false;
        };
        if drawing || now < pending.due {
            return false;
        }
        let pending = self.pending.take().expect("checked above");
        if pending.page_id != page.id || !self.enabled {
            return false;
        }
        let recognizer = self
            .recognizer
            .get_or_insert_with(|| Arc::new(Recognizer::new()));
        self.suggestion = suggest(recognizer, page, pending.equals, decimal_comma);
        self.suggestion.is_some()
    }

    /// The suggestion if it belongs to `page` and its `=` is still there
    pub fn suggestion_for(&self, page: &Page) -> Option<&Suggestion> {
        self.suggestion.as_ref().filter(|s| {
            s.page_id == page.id
                && s.equals
                    .iter()
                    .all(|id| page.strokes.iter().any(|stroke| stroke.id == *id))
        })
    }

    /// The suggestion if `pos` (egui points) is on it; it is then taken away
    pub fn take_hit(&mut self, pos: egui::Pos2) -> Option<Suggestion> {
        if !self.chip_rect.is_some_and(|r| r.contains(pos)) {
            return None;
        }
        self.chip_rect = None;
        self.suggestion.take()
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
        if !enabled {
            self.pending = None;
            self.suggestion = None;
            self.chip_rect = None;
        }
    }
}

/// Handwriting that can be read: pen strokes (also snapped lines), no highlighter, text or
/// images
fn is_ink(stroke: &Stroke) -> bool {
    !stroke.is_object() && stroke.tool != StrokeTool::Highlighter && !stroke.points.is_empty()
}

fn positions(stroke: &Stroke) -> Vec<Vec2> {
    stroke.points.iter().map(|s| s.position).collect()
}

/// A straight, roughly horizontal stroke: one bar of an `=`
fn is_bar(stroke: &Stroke) -> bool {
    let size = stroke.aabb.max - stroke.aabb.min;
    let length: f32 = positions(stroke)
        .windows(2)
        .map(|w| w[0].distance(w[1]))
        .sum();
    size.x > 0.0 && size.x >= 2.5 * size.y && length <= 1.3 * size.x
}

/// Indices of the last two ink strokes if they form an `=`: two bars of similar length, one
/// above the other
fn equals_at_end(strokes: &[Stroke]) -> Option<(usize, usize)> {
    let mut ink = strokes.iter().enumerate().rev().filter(|(_, s)| is_ink(s));
    let (b, second) = ink.next()?;
    let (a, first) = ink.next()?;
    if !is_bar(first) || !is_bar(second) {
        return None;
    }
    let (w1, w2) = (
        first.aabb.max.x - first.aabb.min.x,
        second.aabb.max.x - second.aabb.min.x,
    );
    let (short, long) = (w1.min(w2), w1.max(w2));
    let overlap = first.aabb.max.x.min(second.aabb.max.x) - first.aabb.min.x.max(second.aabb.min.x);
    let dy = ((first.aabb.min.y + first.aabb.max.y) - (second.aabb.min.y + second.aabb.max.y))
        .abs()
        / 2.0;
    (overlap >= 0.5 * short && long <= 2.0 * short && dy >= 0.1 * long && dy <= long)
        .then_some((a, b))
}

/// Indices of the strokes left of the `=` that make up its expression, in the order they were
/// written: ink in a band around the `=`, going left until a wide gap
fn expression_strokes(strokes: &[Stroke], equals: (usize, usize)) -> Vec<usize> {
    let (e1, e2) = (&strokes[equals.0], &strokes[equals.1]);
    let min = e1.aabb.min.min(e2.aabb.min);
    let max = e1.aabb.max.max(e2.aabb.max);
    let height = (max.x - min.x) * SYMBOL_PER_EQUALS;
    let middle = (min.y + max.y) / 2.0;
    let mut candidates: Vec<usize> = (0..strokes.len())
        .filter(|&i| i != equals.0 && i != equals.1 && is_ink(&strokes[i]))
        .filter(|&i| {
            let s = &strokes[i].aabb;
            let center = (s.min.y + s.max.y) / 2.0;
            (center - middle).abs() <= 2.0 * height
                && s.max.y - s.min.y <= 3.0 * height
                && s.max.x <= min.x + 0.2 * height
        })
        .collect();
    candidates.sort_by(|&a, &b| strokes[b].aabb.max.x.total_cmp(&strokes[a].aabb.max.x));
    let mut left = min.x;
    let mut chosen = Vec::new();
    for i in candidates {
        let s = &strokes[i].aabb;
        if s.max.x < left - MAX_GAP * height || chosen.len() >= MAX_STROKES {
            break;
        }
        left = left.min(s.min.x);
        chosen.push(i);
    }
    chosen.sort_unstable();
    chosen
}

/// Reads and computes the expression in front of the `=` made of the strokes `equals`
fn suggest(
    recognizer: &Recognizer,
    page: &Page,
    equals: [u64; 2],
    decimal_comma: bool,
) -> Option<Suggestion> {
    let strokes = &page.strokes;
    let index = |id: u64| strokes.iter().position(|s| s.id == id);
    let pair = (index(equals[0])?, index(equals[1])?);
    let mut chosen = expression_strokes(strokes, pair);
    if chosen.is_empty() {
        return None;
    }
    chosen.extend([pair.0, pair.1]);
    let points: Vec<Vec<Vec2>> = chosen.iter().map(|&i| positions(&strokes[i])).collect();
    let recognition = recognizer.recognize(&points)?;
    // The `=` must be read as the last symbol, from exactly its two strokes
    let last = recognition.symbols.last()?;
    let equals_strokes = [points.len() - 2, points.len() - 1];
    let mut last_strokes = last.strokes.clone();
    last_strokes.sort_unstable();
    if last.ch != '=' || last_strokes != equals_strokes {
        return None;
    }
    // In a chain like `x = 2 + 3 =` only the part after the previous `=` is computed
    let text = recognition.text.strip_suffix('=')?;
    let expression = text.rsplit('=').next()?;
    // A dot or comma that was written decides the separator of the result
    let decimal_comma = if expression.contains(',') {
        true
    } else if expression.contains('.') {
        false
    } else {
        decimal_comma
    };
    let result = crate::math::solve(expression, decimal_comma)?;
    let shown = crate::math::display(expression, decimal_comma)?;

    let (e1, e2) = (&strokes[pair.0], &strokes[pair.1]);
    let right = e1.aabb.max.x.max(e2.aabb.max.x);
    let middle = (e1.aabb.min.y.min(e2.aabb.min.y) + e1.aabb.max.y.max(e2.aabb.max.y)) / 2.0;
    let mut heights: Vec<f32> = recognition
        .symbols
        .iter()
        .filter(|s| s.ch.is_ascii_digit() || s.ch.is_ascii_alphabetic())
        .map(|s| s.max.y - s.min.y)
        .collect();
    heights.sort_by(f32::total_cmp);
    let width = right - e1.aabb.min.x.min(e2.aabb.min.x);
    let symbol_height = heights
        .last()
        .copied()
        .unwrap_or(width * SYMBOL_PER_EQUALS)
        .max(1.0);
    Some(Suggestion {
        page_id: page.id,
        equals,
        expression: shown,
        result,
        anchor: Vec2::new(right + 0.4 * symbol_height, middle),
        symbol_height,
        color: e2.color,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::StylusSample;
    use crate::math_ink::tests::{write_line, Jitter};

    const BLACK: [f32; 4] = [0.0, 0.0, 0.0, 1.0];

    fn stroke(id: u64, points: &[Vec2]) -> Stroke {
        let samples = points
            .iter()
            .enumerate()
            .map(|(i, &position)| StylusSample {
                position,
                pressure: 0.5,
                timestamp: i as f64 * 0.01,
            })
            .collect();
        Stroke::new(id, samples, Vec::new(), BLACK)
    }

    /// A page with `text` written at `origin`, scaled by `scale`
    fn page_with(lines: &[(&str, Vec2)], scale: f32) -> Page {
        let mut page = Page::new(7, crate::document::PageBackground::Blank);
        let mut id = 1;
        for (seed, (text, origin)) in lines.iter().enumerate() {
            for points in write_line(text, &mut Jitter(seed as u64 + 3)) {
                let points: Vec<Vec2> = points.iter().map(|p| *origin + *p * scale).collect();
                page.add_stroke(stroke(id, &points));
                id += 1;
            }
        }
        page
    }

    fn run(page: &Page) -> Option<Suggestion> {
        let mut assist = MathAssist {
            enabled: true,
            ..Default::default()
        };
        let start = Instant::now();
        assist.stroke_committed(page, start);
        assert!(!assist.update(page, start, false, true), "too early");
        assist.update(page, start + TRIGGER_DELAY, false, true);
        assist.suggestion_for(page).cloned()
    }

    #[test]
    fn suggests_result_after_equals() {
        let page = page_with(&[("(x+3)^2=", Vec2::new(100.0, 200.0))], 3.0);
        let suggestion = run(&page).unwrap();
        assert_eq!(suggestion.expression, "(x + 3)²");
        assert_eq!(suggestion.result, "x² + 6x + 9");
        let equals_right = page
            .strokes
            .iter()
            .rev()
            .take(2)
            .map(|s| s.aabb.max.x)
            .fold(f32::MIN, f32::max);
        assert!(suggestion.anchor.x > equals_right);
        // The handwriting is 14 units high, scaled by 3
        assert!(
            (suggestion.symbol_height - 42.0).abs() < 6.0,
            "{suggestion:?}"
        );
    }

    #[test]
    fn ignores_writing_far_to_the_left_and_on_other_lines() {
        let page = page_with(
            &[
                ("7", Vec2::new(0.0, 200.0)),
                ("5·5", Vec2::new(0.0, 300.0)),
                ("2^4=", Vec2::new(150.0, 200.0)),
            ],
            2.0,
        );
        let suggestion = run(&page).unwrap();
        assert_eq!(suggestion.expression, "2⁴");
        assert_eq!(suggestion.result, "16");
    }

    #[test]
    fn computes_the_last_part_of_a_chain() {
        let page = page_with(&[("x=2·3=", Vec2::new(0.0, 0.0))], 2.0);
        assert_eq!(run(&page).unwrap().result, "6");
    }

    #[test]
    fn nothing_without_equals_or_when_disabled() {
        let page = page_with(&[("2+3", Vec2::ZERO)], 2.0);
        assert!(run(&page).is_none());
        let page = page_with(&[("2+3=", Vec2::ZERO)], 2.0);
        let mut assist = MathAssist::default();
        assist.stroke_committed(&page, Instant::now());
        assert!(assist.deadline().is_none());
    }

    #[test]
    fn new_stroke_or_removed_equals_drops_the_suggestion() {
        let mut page = page_with(&[("1+1=", Vec2::ZERO)], 2.0);
        let mut assist = MathAssist {
            enabled: true,
            ..Default::default()
        };
        let start = Instant::now();
        assist.stroke_committed(&page, start);
        // A stroke still being drawn when the time is up: nothing yet
        assert!(!assist.update(&page, start + TRIGGER_DELAY, true, true));
        assert!(assist.update(&page, start + TRIGGER_DELAY, false, true));
        assert_eq!(assist.suggestion_for(&page).unwrap().result, "2");
        let last = page.strokes.last().unwrap().id;
        page.remove_strokes_by_ids(&[last]);
        assert!(assist.suggestion_for(&page).is_none());
        assist.stroke_committed(&page, start);
        assert!(assist.suggestion.is_none());
    }

    #[test]
    fn hit_takes_the_suggestion() {
        let page = page_with(&[("1+1=", Vec2::ZERO)], 2.0);
        let mut assist = MathAssist {
            enabled: true,
            ..Default::default()
        };
        let start = Instant::now();
        assist.stroke_committed(&page, start);
        assist.update(&page, start + TRIGGER_DELAY, false, true);
        assist.chip_rect = Some(egui::Rect::from_min_size(
            egui::pos2(10.0, 10.0),
            egui::vec2(50.0, 20.0),
        ));
        assert!(assist.take_hit(egui::pos2(5.0, 5.0)).is_none());
        assert_eq!(assist.take_hit(egui::pos2(20.0, 20.0)).unwrap().result, "2");
        assert!(assist.take_hit(egui::pos2(20.0, 20.0)).is_none());
    }

    #[test]
    fn result_text_sits_on_the_baseline() {
        let suggestion = Suggestion {
            page_id: 1,
            equals: [1, 2],
            expression: "1 + 1".into(),
            result: "2".into(),
            anchor: Vec2::new(100.0, 50.0),
            symbol_height: 14.0,
            color: BLACK,
        };
        let (top_left, size) = suggestion.text_placement(0.9);
        assert!((size - 20.0).abs() < 0.01);
        // Baseline (top + ascent) at the bottom of digits centered on the `=`
        assert!((top_left.y + 0.9 * size - 57.0).abs() < 0.01);
        assert_eq!(top_left.x, 100.0);
    }
}
