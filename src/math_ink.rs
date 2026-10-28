//! Recognition of handwritten math (Issue #46, stage 2).
//!
//! Turns the strokes of a handwritten expression into text for [`crate::math::solve`], e.g.
//! the strokes of `(x+3)²=` into `(x+3)^(2)=`. It works on the stroke points, not on an
//! image, in three steps:
//!
//! 1. **Grouping**: consecutive strokes that overlap form one symbol (the two strokes of `+`,
//!    `x` or `4`). Straight horizontal strokes and dots are kept apart, as their meaning
//!    depends on their neighbours.
//! 2. **Classifying** each symbol with the $P point-cloud recognizer (Vatavu, Anthony,
//!    Wobbrock 2012): its points are compared with templates of each symbol. It does not
//!    depend on stroke order or direction and needs no trained model; besides the built-in
//!    templates, one's own handwriting can be added as templates.
//! 3. **Structure** from the positions: two stacked lines are `=`, two stacked dots `:`, a
//!    line with symbols above and below is a fraction bar, a dot low between digits a
//!    decimal comma, and smaller raised symbols an exponent.

use glam::{Vec2, Vec3};

/// Points of a symbol after resampling, for $P
const CLOUD_POINTS: usize = 32;
/// Symbols whose best template is further away are not recognized ($P distance)
const MAX_DISTANCE: f32 = 3.0;
/// Factor on the distance per stroke more or less than the template has
const STROKE_PENALTY: f32 = 0.15;
/// Slants of the built-in templates (horizontal shift of the top against the bottom)
const TEMPLATE_SLANTS: [f32; 2] = [0.0, 0.25];
/// Weight of the turning angle against the position of a point ($P+)
const ANGLE_WEIGHT: f32 = 1.0;

/// A recognized symbol with the area it covers and the strokes it consists of
#[derive(Debug, Clone, PartialEq)]
pub struct Symbol {
    /// `0`–`9`, `x`, `y`, `a`, `b`, `+`, `-`, `·`, `:`, `/`, `(`, `)`, `=`, `,` or `?`
    pub ch: char,
    pub min: Vec2,
    pub max: Vec2,
    /// Indices into the strokes passed to [`Recognizer::recognize`]
    pub strokes: Vec<usize>,
}

/// Result of [`Recognizer::recognize`]
#[derive(Debug, Clone, PartialEq)]
pub struct Recognition {
    /// The expression as text, e.g. `(x+3)^(2)=` or `((1)/(2))+3=`
    pub text: String,
    /// All symbols, from left to right
    pub symbols: Vec<Symbol>,
}

struct Template {
    symbol: char,
    strokes: usize,
    cloud: Vec<Vec3>,
}

/// $P recognizer with templates for the symbols of simple math
pub struct Recognizer {
    templates: Vec<Template>,
}

impl Default for Recognizer {
    fn default() -> Self {
        Self::new()
    }
}

impl Recognizer {
    /// Recognizer with the built-in templates
    pub fn new() -> Self {
        let mut recognizer = Recognizer {
            templates: Vec::new(),
        };
        // Upright and slanted, as handwriting usually leans to the right
        for (symbol, strokes) in builtin_templates() {
            for slant in TEMPLATE_SLANTS {
                let width = template_width(symbol);
                let slanted: Vec<Vec<Vec2>> = strokes
                    .iter()
                    .map(|s| {
                        s.iter()
                            .map(|p| Vec2::new(p.x * width + slant * (1.0 - p.y), p.y))
                            .collect()
                    })
                    .collect();
                recognizer.add_template(symbol, &slanted);
            }
        }
        recognizer
    }

    /// Adds a template, e.g. a symbol in one's own handwriting
    pub fn add_template(&mut self, symbol: char, strokes: &[Vec<Vec2>]) {
        let strokes: Vec<&[Vec2]> = strokes.iter().map(Vec::as_slice).collect();
        if let Some(cloud) = normalize(&strokes) {
            self.templates.push(Template {
                symbol,
                strokes: strokes.len(),
                cloud,
            });
        }
    }

    /// The symbol whose template is closest, with its $P distance (smaller is better)
    pub fn classify(&self, strokes: &[&[Vec2]]) -> Option<(char, f32)> {
        let cloud = normalize(strokes)?;
        let mut best: Option<(char, f32)> = None;
        for template in &self.templates {
            // A different number of strokes makes a template less likely (`y` in two strokes
            // against `7` in one), but does not rule it out
            let penalty = 1.0 + STROKE_PENALTY * template.strokes.abs_diff(strokes.len()) as f32;
            let limit = best.map_or(f32::MAX, |b| b.1);
            let distance = greedy_cloud_match(&cloud, &template.cloud, limit / penalty) * penalty;
            if distance < limit {
                best = Some((template.symbol, distance));
            }
        }
        best
    }

    /// Recognizes the expression written with `strokes` (page coordinates, y pointing down,
    /// in the order they were written). `None` if there are no strokes.
    pub fn recognize(&self, strokes: &[Vec<Vec2>]) -> Option<Recognition> {
        let infos: Vec<StrokeInfo> = strokes
            .iter()
            .filter(|s| !s.is_empty())
            .map(|s| StrokeInfo::new(s))
            .collect();
        if infos.len() != strokes.len() || infos.is_empty() {
            return None;
        }
        let line_height = line_height(&infos);
        let kinds: Vec<Kind> = infos.iter().map(|i| i.kind(line_height)).collect();
        let groups = group_strokes(strokes, &infos, &kinds, line_height);

        let mut elements = Vec::new();
        for group in groups {
            let (min, max) = bounds(group.iter().map(|&i| &infos[i]));
            let ch = match (group.as_slice(), kinds[group[0]]) {
                ([_], Kind::Line) => LINE,
                ([_], Kind::Dot) => DOT,
                _ => {
                    let parts: Vec<&[Vec2]> =
                        group.iter().map(|&i| strokes[i].as_slice()).collect();
                    match self.classify(&parts) {
                        Some((ch, distance)) if distance <= MAX_DISTANCE => ch,
                        _ => '?',
                    }
                }
            };
            elements.push(Element {
                symbol: Symbol {
                    ch,
                    min,
                    max,
                    strokes: group,
                },
                text: None,
            });
        }

        let elements = pair_stacked(elements, line_height);
        let elements = build_fractions(elements, line_height);
        let text = layout(&elements, line_height);
        let mut symbols = Vec::new();
        for element in elements {
            element.collect_symbols(&mut symbols);
        }
        symbols.sort_by(|a, b| a.min.x.total_cmp(&b.min.x));
        Some(Recognition { text, symbols })
    }
}

// --- $P point-cloud recognizer --------------------------------------------------------------

/// Resamples all strokes to [`CLOUD_POINTS`] points, scales them to a unit box (keeping the
/// aspect ratio) and moves their centroid to the origin.
fn normalize(strokes: &[&[Vec2]]) -> Option<Vec<Vec3>> {
    let lengths: Vec<f32> = strokes.iter().map(|s| path_length(s)).collect();
    let total: f32 = lengths.iter().sum();
    if strokes.is_empty() || strokes.len() > CLOUD_POINTS {
        return None;
    }
    // Points per stroke in proportion to its length, at least one each (largest remainder)
    let mut counts = vec![1usize; strokes.len()];
    let spare = CLOUD_POINTS - strokes.len();
    let shares: Vec<f32> = lengths
        .iter()
        .map(|l| {
            if total > 0.0 {
                l / total * spare as f32
            } else {
                spare as f32 / strokes.len() as f32
            }
        })
        .collect();
    let mut given = 0;
    for (count, share) in counts.iter_mut().zip(&shares) {
        *count += share.floor() as usize;
        given += share.floor() as usize;
    }
    let mut order: Vec<usize> = (0..strokes.len()).collect();
    order.sort_by(|&a, &b| {
        (shares[b] - shares[b].floor()).total_cmp(&(shares[a] - shares[a].floor()))
    });
    for &i in order.iter().cycle().take(spare - given) {
        counts[i] += 1;
    }

    let mut points = Vec::with_capacity(CLOUD_POINTS);
    let mut angles = Vec::with_capacity(CLOUD_POINTS);
    for (stroke, count) in strokes.iter().zip(counts) {
        let resampled = resample(stroke, count);
        angles.extend(turning_angles(&resampled));
        points.extend(resampled);
    }
    let (min, max) = points.iter().fold(
        (Vec2::splat(f32::MAX), Vec2::splat(f32::MIN)),
        |(lo, hi), p| (lo.min(*p), hi.max(*p)),
    );
    let size = (max - min).max_element().max(1e-6);
    let centroid = points.iter().copied().sum::<Vec2>() / points.len() as f32;
    Some(
        points
            .iter()
            .zip(angles)
            .map(|(p, angle)| ((*p - centroid) / size).extend(angle * ANGLE_WEIGHT))
            .collect(),
    )
}

/// How sharply the stroke turns at each point, from 0 (straight on, or an end) to 1 (back)
fn turning_angles(points: &[Vec2]) -> Vec<f32> {
    (0..points.len())
        .map(|i| {
            if i == 0 || i + 1 >= points.len() {
                return 0.0;
            }
            let a = points[i] - points[i - 1];
            let b = points[i + 1] - points[i];
            if a.length_squared() == 0.0 || b.length_squared() == 0.0 {
                return 0.0;
            }
            a.angle_between(b).abs() / std::f32::consts::PI
        })
        .collect()
}

fn path_length(points: &[Vec2]) -> f32 {
    points.windows(2).map(|w| w[0].distance(w[1])).sum()
}

/// `count` points evenly spaced along the polyline (for one point: its middle)
fn resample(points: &[Vec2], count: usize) -> Vec<Vec2> {
    let length = path_length(points);
    if count == 0 {
        return Vec::new();
    }
    if count == 1 || length <= 0.0 {
        let at = if count == 1 { length / 2.0 } else { 0.0 };
        return vec![point_at(points, at); count];
    }
    let step = length / (count - 1) as f32;
    (0..count)
        .map(|i| point_at(points, step * i as f32))
        .collect()
}

/// The point at distance `at` along the polyline
fn point_at(points: &[Vec2], at: f32) -> Vec2 {
    let mut remaining = at;
    for w in points.windows(2) {
        let d = w[0].distance(w[1]);
        if remaining <= d && d > 0.0 {
            return w[0].lerp(w[1], remaining / d);
        }
        remaining -= d;
    }
    *points.last().unwrap_or(&Vec2::ZERO)
}

/// $P distance between two clouds, or infinity if it is not below `limit` (then the
/// matching stops early)
fn greedy_cloud_match(points: &[Vec3], template: &[Vec3], limit: f32) -> f32 {
    let n = points.len();
    let step = (n as f32).sqrt().floor().max(1.0) as usize;
    let mut best = f32::INFINITY;
    for start in (0..n).step_by(step) {
        best = best.min(cloud_distance(points, template, start, best.min(limit)));
        best = best.min(cloud_distance(template, points, start, best.min(limit)));
    }
    if best < limit {
        best
    } else {
        f32::INFINITY
    }
}

fn cloud_distance(a: &[Vec3], b: &[Vec3], start: usize, limit: f32) -> f32 {
    let n = a.len();
    let mut matched = [false; CLOUD_POINTS];
    let mut sum = 0.0;
    for k in 0..n {
        let i = (start + k) % n;
        let mut best = f32::MAX;
        let mut best_j = 0;
        for (j, q) in b.iter().enumerate() {
            if !matched[j] {
                let d = a[i].distance_squared(*q);
                if d < best {
                    best = d;
                    best_j = j;
                }
            }
        }
        if best == f32::MAX {
            break;
        }
        matched[best_j] = true;
        sum += (1.0 - k as f32 / n as f32) * best.sqrt();
        if sum >= limit {
            return sum;
        }
    }
    sum
}

// --- Built-in templates ---------------------------------------------------------------------

/// Points on an elliptic arc in a unit box (y down); angles in degrees, 0° = right,
/// 90° = bottom
fn arc(cx: f32, cy: f32, rx: f32, ry: f32, from: f32, to: f32) -> Vec<Vec2> {
    const STEPS: usize = 24;
    (0..=STEPS)
        .map(|i| {
            let a = (from + (to - from) * i as f32 / STEPS as f32).to_radians();
            Vec2::new(cx + rx * a.cos(), cy + ry * a.sin())
        })
        .collect()
}

/// Usual width of a symbol relative to its height; the templates below are drawn in a
/// square and narrowed by this
fn template_width(symbol: char) -> f32 {
    match symbol {
        '0'..='9' | 'b' | 'y' => 0.65,
        '/' => 0.8,
        _ => 1.0,
    }
}

fn polyline(points: &[(f32, f32)]) -> Vec<Vec2> {
    points.iter().map(|&(x, y)| Vec2::new(x, y)).collect()
}

fn chain(parts: &[Vec<Vec2>]) -> Vec<Vec2> {
    parts.concat()
}

/// Several ways to write each symbol; strokes in a unit box with y pointing down
fn builtin_templates() -> Vec<(char, Vec<Vec<Vec2>>)> {
    vec![
        ('0', vec![arc(0.5, 0.5, 0.35, 0.5, -90.0, 270.0)]),
        ('0', vec![arc(0.5, 0.5, 0.3, 0.5, -100.0, -460.0)]),
        ('1', vec![polyline(&[(0.5, 0.0), (0.5, 1.0)])]),
        ('1', vec![polyline(&[(0.3, 0.2), (0.55, 0.0), (0.55, 1.0)])]),
        (
            '2',
            vec![chain(&[
                arc(0.5, 0.3, 0.35, 0.3, 190.0, 400.0),
                polyline(&[(0.1, 1.0), (0.9, 1.0)]),
            ])],
        ),
        (
            '2',
            vec![chain(&[
                arc(0.5, 0.3, 0.35, 0.3, 200.0, 380.0),
                polyline(&[(0.15, 0.95)]),
                arc(0.5, 1.15, 0.4, 0.25, 200.0, 330.0),
            ])],
        ),
        (
            '3',
            vec![chain(&[
                arc(0.5, 0.25, 0.35, 0.25, 200.0, 450.0),
                arc(0.5, 0.75, 0.38, 0.25, 270.0, 520.0),
            ])],
        ),
        (
            '4',
            vec![
                polyline(&[(0.6, 0.0), (0.1, 0.65), (0.9, 0.65)]),
                polyline(&[(0.65, 0.3), (0.65, 1.0)]),
            ],
        ),
        (
            '4',
            vec![
                polyline(&[(0.2, 0.0), (0.15, 0.6), (0.9, 0.6)]),
                polyline(&[(0.7, 0.05), (0.7, 1.0)]),
            ],
        ),
        (
            '4',
            vec![polyline(&[
                (0.6, 0.0),
                (0.1, 0.65),
                (0.9, 0.65),
                (0.65, 0.65),
                (0.65, 0.3),
                (0.65, 1.0),
            ])],
        ),
        (
            '5',
            vec![chain(&[
                polyline(&[(0.8, 0.0), (0.25, 0.0), (0.2, 0.45)]),
                arc(0.5, 0.7, 0.35, 0.3, 225.0, 510.0),
            ])],
        ),
        (
            '5',
            vec![
                chain(&[
                    polyline(&[(0.25, 0.0), (0.2, 0.45)]),
                    arc(0.5, 0.7, 0.35, 0.3, 225.0, 510.0),
                ]),
                polyline(&[(0.25, 0.0), (0.85, 0.0)]),
            ],
        ),
        (
            '6',
            vec![chain(&[
                polyline(&[(0.75, 0.0), (0.45, 0.12), (0.25, 0.35), (0.16, 0.65)]),
                arc(0.5, 0.72, 0.34, 0.28, 180.0, -180.0),
            ])],
        ),
        ('7', vec![polyline(&[(0.1, 0.0), (0.9, 0.0), (0.35, 1.0)])]),
        (
            '7',
            vec![
                polyline(&[(0.1, 0.0), (0.9, 0.0), (0.35, 1.0)]),
                polyline(&[(0.35, 0.5), (0.85, 0.5)]),
            ],
        ),
        (
            '8',
            vec![chain(&[
                arc(0.5, 0.25, 0.28, 0.25, 0.0, 360.0),
                arc(0.5, 0.75, 0.35, 0.25, 0.0, -360.0),
            ])],
        ),
        (
            '8',
            vec![chain(&[
                arc(0.5, 0.25, 0.3, 0.25, 90.0, -270.0),
                arc(0.5, 0.75, 0.36, 0.25, -90.0, 270.0),
            ])],
        ),
        (
            '9',
            vec![chain(&[
                arc(0.5, 0.28, 0.32, 0.28, 0.0, -360.0),
                polyline(&[(0.82, 0.28), (0.75, 1.0)]),
            ])],
        ),
        (
            '9',
            vec![chain(&[
                arc(0.5, 0.28, 0.32, 0.28, 0.0, -360.0),
                arc(0.42, 0.28, 0.4, 0.72, 0.0, 100.0),
            ])],
        ),
        (
            '+',
            vec![
                polyline(&[(0.0, 0.5), (1.0, 0.5)]),
                polyline(&[(0.5, 0.0), (0.5, 1.0)]),
            ],
        ),
        ('(', vec![arc(1.0, 0.5, 0.7, 0.5, 240.0, 120.0)]),
        ('(', vec![arc(1.2, 0.5, 0.9, 0.55, 235.0, 125.0)]),
        (')', vec![arc(0.0, 0.5, 0.7, 0.5, -60.0, 60.0)]),
        (')', vec![arc(-0.2, 0.5, 0.9, 0.55, -55.0, 55.0)]),
        ('/', vec![polyline(&[(0.75, 0.0), (0.25, 1.0)])]),
        (
            'x',
            vec![
                polyline(&[(0.0, 0.0), (1.0, 1.0)]),
                polyline(&[(1.0, 0.0), (0.0, 1.0)]),
            ],
        ),
        (
            'x',
            vec![
                arc(0.0, 0.5, 0.45, 0.5, -90.0, 90.0),
                arc(1.0, 0.5, 0.45, 0.5, 270.0, 90.0),
            ],
        ),
        (
            'y',
            vec![
                polyline(&[(0.0, 0.0), (0.5, 0.5)]),
                polyline(&[(1.0, 0.0), (0.2, 1.0)]),
            ],
        ),
        (
            'y',
            vec![chain(&[
                polyline(&[(0.05, 0.0), (0.1, 0.35)]),
                arc(0.45, 0.35, 0.35, 0.15, 180.0, 90.0),
                polyline(&[(0.8, 0.0), (0.8, 0.8)]),
                arc(0.45, 0.8, 0.35, 0.2, 0.0, 150.0),
            ])],
        ),
        (
            'a',
            vec![chain(&[
                arc(0.45, 0.5, 0.4, 0.45, -20.0, -380.0),
                polyline(&[(0.85, 0.05), (0.85, 1.0)]),
            ])],
        ),
        (
            'a',
            vec![chain(&[
                arc(0.45, 0.5, 0.4, 0.45, -20.0, -380.0),
                polyline(&[(0.85, 0.05), (0.85, 0.9), (1.0, 1.0)]),
            ])],
        ),
        (
            'b',
            vec![chain(&[
                polyline(&[(0.2, 0.0), (0.2, 1.0)]),
                arc(0.5, 0.72, 0.3, 0.28, 180.0, -180.0),
            ])],
        ),
        (
            'b',
            vec![chain(&[
                polyline(&[(0.2, 0.0), (0.2, 0.95)]),
                arc(0.5, 0.72, 0.3, 0.28, 160.0, 520.0),
            ])],
        ),
    ]
}

// --- Grouping strokes into symbols ----------------------------------------------------------

/// Placeholder symbols before the structure is known
const LINE: char = '—';
const DOT: char = '•';

#[derive(Debug, Clone, Copy, PartialEq)]
enum Kind {
    /// Straight, roughly horizontal: minus, fraction bar or half of `=`
    Line,
    /// Small: multiplication dot, half of `:` or decimal comma
    Dot,
    Other,
}

struct StrokeInfo {
    min: Vec2,
    max: Vec2,
    straight_horizontal: bool,
}

impl StrokeInfo {
    fn new(points: &[Vec2]) -> StrokeInfo {
        let (min, max) = points.iter().fold(
            (Vec2::splat(f32::MAX), Vec2::splat(f32::MIN)),
            |(lo, hi), p| (lo.min(*p), hi.max(*p)),
        );
        let size = max - min;
        let straight_horizontal =
            size.x > 0.0 && size.x >= 2.5 * size.y && path_length(points) <= 1.3 * size.x;
        StrokeInfo {
            min,
            max,
            straight_horizontal,
        }
    }

    fn size(&self) -> Vec2 {
        self.max - self.min
    }

    fn kind(&self, line_height: f32) -> Kind {
        if self.size().max_element() < 0.2 * line_height {
            Kind::Dot
        } else if self.straight_horizontal {
            Kind::Line
        } else {
            Kind::Other
        }
    }
}

/// Typical symbol height: the median height of the strokes that are not lines
fn line_height(infos: &[StrokeInfo]) -> f32 {
    let mut heights: Vec<f32> = infos
        .iter()
        .filter(|i| !i.straight_horizontal)
        .map(|i| i.size().y)
        .collect();
    if heights.is_empty() {
        heights = infos.iter().map(|i| i.size().max_element()).collect();
    }
    heights.sort_by(f32::total_cmp);
    // The upper median, so that small symbols (dots, exponents) do not dominate
    heights[heights.len() * 2 / 3].max(1e-3)
}

fn bounds<'a>(infos: impl Iterator<Item = &'a StrokeInfo>) -> (Vec2, Vec2) {
    infos.fold(
        (Vec2::splat(f32::MAX), Vec2::splat(f32::MIN)),
        |(lo, hi), i| (lo.min(i.min), hi.max(i.max)),
    )
}

/// Joins each stroke with the symbol written just before it if they belong together
fn group_strokes(
    strokes: &[Vec<Vec2>],
    infos: &[StrokeInfo],
    kinds: &[Kind],
    line_height: f32,
) -> Vec<Vec<usize>> {
    let mut groups: Vec<Vec<usize>> = Vec::new();
    for (i, info) in infos.iter().enumerate() {
        let joins = groups.last().is_some_and(|group| {
            let (gmin, gmax) = bounds(group.iter().map(|&j| &infos[j]));
            let gsize = gmax - gmin;
            let group_kinds: Vec<Kind> = group.iter().map(|&j| kinds[j]).collect();
            if kinds[i] == Kind::Dot || group_kinds.contains(&Kind::Dot) {
                return false;
            }
            let touches = || {
                group
                    .iter()
                    .any(|&j| polyline_distance(&strokes[i], &strokes[j]) < 0.12 * line_height)
            };
            let only_lines = group_kinds.iter().all(|k| *k == Kind::Line);
            match (kinds[i], only_lines) {
                // Two lines are `=` or a fraction bar and a minus; decided later
                (Kind::Line, true) => false,
                // The bar of `+`, `7` or `5`: it crosses or touches the symbol and is not much
                // wider (a fraction bar is wider than the numerator and lies below it)
                (Kind::Line, false) => info.size().x <= 1.5 * gsize.max_element() && touches(),
                (_, true) => gsize.x <= 1.5 * info.size().max_element() && touches(),
                _ => {
                    let margin = 0.1 * line_height;
                    let overlap_x =
                        info.max.x.min(gmax.x + margin) - info.min.x.max(gmin.x - margin);
                    let overlap_y =
                        info.max.y.min(gmax.y + margin) - info.min.y.max(gmin.y - margin);
                    overlap_x >= 0.3 * info.size().x.min(gsize.x)
                        && overlap_x > 0.0
                        && overlap_y > 0.0
                }
            }
        });
        match groups.last_mut() {
            Some(group) if joins => group.push(i),
            _ => groups.push(vec![i]),
        }
    }
    groups
}

fn point_segment_distance(p: Vec2, a: Vec2, b: Vec2) -> f32 {
    let ab = b - a;
    let t = if ab.length_squared() > 0.0 {
        ((p - a).dot(ab) / ab.length_squared()).clamp(0.0, 1.0)
    } else {
        0.0
    };
    p.distance(a + ab * t)
}

fn segment_distance(a0: Vec2, a1: Vec2, b0: Vec2, b1: Vec2) -> f32 {
    let cross = |o: Vec2, p: Vec2, q: Vec2| (p - o).perp_dot(q - o);
    let (d1, d2) = (cross(b0, b1, a0), cross(b0, b1, a1));
    let (d3, d4) = (cross(a0, a1, b0), cross(a0, a1, b1));
    if d1 * d2 < 0.0 && d3 * d4 < 0.0 {
        return 0.0;
    }
    point_segment_distance(a0, b0, b1)
        .min(point_segment_distance(a1, b0, b1))
        .min(point_segment_distance(b0, a0, a1))
        .min(point_segment_distance(b1, a0, a1))
}

/// Smallest distance between two polylines (0 if they cross)
fn polyline_distance(a: &[Vec2], b: &[Vec2]) -> f32 {
    let segments = |p: &[Vec2]| -> Vec<(Vec2, Vec2)> {
        match p {
            [only] => vec![(*only, *only)],
            _ => p.windows(2).map(|w| (w[0], w[1])).collect(),
        }
    };
    let (sa, sb) = (segments(a), segments(b));
    sa.iter()
        .flat_map(|&(a0, a1)| {
            sb.iter()
                .map(move |&(b0, b1)| segment_distance(a0, a1, b0, b1))
        })
        .fold(f32::MAX, f32::min)
}

// --- Structure ------------------------------------------------------------------------------

/// A symbol, or a fraction made of several (then `text` holds its text)
#[derive(Debug, Clone)]
struct Element {
    symbol: Symbol,
    text: Option<(String, Vec<Element>)>,
}

impl Element {
    fn center(&self) -> Vec2 {
        (self.symbol.min + self.symbol.max) / 2.0
    }

    fn height(&self) -> f32 {
        self.symbol.max.y - self.symbol.min.y
    }

    fn width(&self) -> f32 {
        self.symbol.max.x - self.symbol.min.x
    }

    fn is_operand(&self) -> bool {
        self.text.is_some() || self.symbol.ch.is_ascii_alphanumeric() || self.symbol.ch == ')'
    }

    fn collect_symbols(self, out: &mut Vec<Symbol>) {
        match self.text {
            Some((_, parts)) => {
                for part in parts {
                    part.collect_symbols(out);
                }
            }
            None => out.push(self.symbol),
        }
    }
}

fn merged(a: &Element, b: &Element, ch: char) -> Element {
    let mut strokes = a.symbol.strokes.clone();
    strokes.extend(&b.symbol.strokes);
    Element {
        symbol: Symbol {
            ch,
            min: a.symbol.min.min(b.symbol.min),
            max: a.symbol.max.max(b.symbol.max),
            strokes,
        },
        text: None,
    }
}

/// Two stacked lines of similar length become `=`, two stacked dots `:`
fn pair_stacked(mut elements: Vec<Element>, line_height: f32) -> Vec<Element> {
    let mut i = 0;
    while i < elements.len() {
        let partner = (0..elements.len()).find(|&j| {
            if j == i {
                return false;
            }
            let (a, b) = (&elements[i], &elements[j]);
            let dy = (a.center().y - b.center().y).abs();
            match (a.symbol.ch, b.symbol.ch) {
                (LINE, LINE) => {
                    let overlap =
                        a.symbol.max.x.min(b.symbol.max.x) - a.symbol.min.x.max(b.symbol.min.x);
                    let (short, long) = (a.width().min(b.width()), a.width().max(b.width()));
                    overlap >= 0.5 * short && long <= 2.0 * short && dy < 0.7 * line_height
                }
                (DOT, DOT) => {
                    (a.center().x - b.center().x).abs() < 0.3 * line_height
                        && dy > 0.15 * line_height
                        && dy < line_height
                }
                _ => false,
            }
        });
        if let Some(j) = partner {
            let ch = if elements[i].symbol.ch == LINE {
                '='
            } else {
                ':'
            };
            let pair = merged(&elements[i], &elements[j], ch);
            let (first, second) = (i.min(j), i.max(j));
            elements.remove(second);
            elements[first] = pair;
        }
        i += 1;
    }
    elements
}

/// Lines with symbols above and below become fractions, the narrowest first so that nested
/// fractions work
fn build_fractions(mut elements: Vec<Element>, line_height: f32) -> Vec<Element> {
    loop {
        let mut bars: Vec<usize> = (0..elements.len())
            .filter(|&i| elements[i].symbol.ch == LINE)
            .collect();
        bars.sort_by(|&a, &b| elements[a].width().total_cmp(&elements[b].width()));
        let found = bars.into_iter().find_map(|bar| {
            let b = &elements[bar];
            let y = b.center().y;
            let margin = 0.1 * line_height;
            let inside = |e: &Element| {
                let c = e.center().x;
                c >= b.symbol.min.x - margin && c <= b.symbol.max.x + margin
            };
            let mut above = Vec::new();
            let mut below = Vec::new();
            for (i, e) in elements.iter().enumerate() {
                if i == bar || !inside(e) {
                    continue;
                }
                if e.symbol.max.y <= y + margin && e.symbol.max.y > y - 1.5 * line_height {
                    above.push(i);
                } else if e.symbol.min.y >= y - margin && e.symbol.min.y < y + 1.5 * line_height {
                    below.push(i);
                }
            }
            (!above.is_empty() && !below.is_empty()).then_some((bar, above, below))
        });
        let Some((bar, above, below)) = found else {
            return elements;
        };
        let take = |indices: &[usize]| -> Vec<Element> {
            indices.iter().map(|&i| elements[i].clone()).collect()
        };
        let numerator = take(&above);
        let denominator = take(&below);
        let text = format!(
            "(({})/({}))",
            layout(&numerator, line_height),
            layout(&denominator, line_height)
        );
        let mut fraction = elements[bar].clone();
        // No longer a line, so that it is not taken as a bar again
        fraction.symbol.ch = '/';
        for part in numerator.iter().chain(&denominator) {
            fraction.symbol.min = fraction.symbol.min.min(part.symbol.min);
            fraction.symbol.max = fraction.symbol.max.max(part.symbol.max);
        }
        let mut parts = numerator;
        parts.extend(denominator);
        let mut bar_element = elements[bar].clone();
        bar_element.symbol.ch = '/';
        parts.push(bar_element);
        fraction.symbol.strokes = parts
            .iter()
            .flat_map(|p| p.symbol.strokes.clone())
            .collect();
        fraction.text = Some((text, parts));
        let mut removed: Vec<usize> = above.into_iter().chain(below).chain([bar]).collect();
        removed.sort_unstable();
        let at = removed[0];
        for &i in removed.iter().rev() {
            elements.remove(i);
        }
        elements.insert(at, fraction);
    }
}

/// Is `e` written raised next to `base`, i.e. an exponent? Its bottom is well above the
/// bottom of `base`, and its middle is in the upper part of `base`. A minus has to be at
/// about the top of `base`, so that a minus after a small letter such as `x` stays a minus.
fn is_raised(e: &Element, base: &Element, line_height: f32) -> bool {
    let (rise, upper_part) = if e.symbol.ch == LINE {
        (0.6, 0.1)
    } else {
        (0.45, 0.35)
    };
    e.symbol.max.y < base.symbol.max.y - rise * line_height
        && e.center().y < base.symbol.min.y + upper_part * base.height()
}

/// Text of the elements from left to right, with exponents as `^(…)`
fn layout(elements: &[Element], line_height: f32) -> String {
    let mut sorted: Vec<&Element> = elements.iter().collect();
    sorted.sort_by(|a, b| a.symbol.min.x.total_cmp(&b.symbol.min.x));
    let mut out = String::new();
    let mut base: Option<&Element> = None;
    // The first symbol of the exponent being read
    let mut exponent: Option<&Element> = None;
    for (k, e) in sorted.iter().enumerate() {
        match exponent {
            None => {
                let starts_exponent = e.is_operand() || matches!(e.symbol.ch, '(' | LINE);
                if base.is_some_and(|b| starts_exponent && is_raised(e, b, line_height)) {
                    out.push_str("^(");
                    exponent = Some(e);
                }
            }
            // The exponent goes on as long as the symbols stay as high as its first one (or
            // are raised themselves, after a minus)
            Some(first) => {
                let high = e.center().y < first.symbol.max.y
                    || base.is_some_and(|b| is_raised(e, b, line_height));
                if e.symbol.ch == '=' || !high {
                    out.push(')');
                    exponent = None;
                }
            }
        }
        let previous_digit = out.chars().last().is_some_and(|c| c.is_ascii_digit());
        let next_digit = sorted
            .get(k + 1)
            .is_some_and(|n| n.text.is_none() && n.symbol.ch.is_ascii_digit());
        match (&e.text, e.symbol.ch) {
            (Some((text, _)), _) => out.push_str(text),
            (None, LINE) => out.push('-'),
            (None, DOT) => {
                // Low between digits it is a decimal comma, otherwise a multiplication dot
                let low = base.is_some_and(|b| e.center().y > b.symbol.min.y + 0.7 * b.height());
                out.push(if low && previous_digit && next_digit {
                    ','
                } else {
                    '·'
                });
            }
            // An x between two numbers is a multiplication sign
            (None, 'x') if previous_digit && next_digit => out.push('·'),
            (None, ch) => out.push(ch),
        }
        if exponent.is_none() && e.is_operand() {
            base = Some(e);
        }
    }
    if exponent.is_some() {
        out.push(')');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic pseudo-random numbers in [-1, 1]
    struct Jitter(u64);

    impl Jitter {
        fn next(&mut self) -> f32 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((self.0 >> 33) as f32 / (1u64 << 31) as f32) * 2.0 - 1.0
        }
    }

    /// Writes `symbol` in a box at `origin` with the given size, slanted, with jittered
    /// points and every stroke reversed if `reverse`, using the `variant`-th built-in template
    fn write(
        symbol: char,
        variant: usize,
        origin: Vec2,
        size: Vec2,
        jitter: &mut Jitter,
        reverse: bool,
    ) -> Vec<Vec<Vec2>> {
        let strokes = builtin_templates()
            .into_iter()
            .filter(|(s, _)| *s == symbol)
            .nth(variant)
            .unwrap_or_else(|| panic!("no template {variant} for {symbol}"))
            .1;
        // Slanted, slightly rotated and smoothly warped like real handwriting, plus a little
        // pen noise
        let slant = 0.15 + 0.1 * jitter.next();
        let rotation = Vec2::from_angle(0.08 * jitter.next());
        let (fx, fy) = (1.0 + 0.5 * jitter.next(), 1.0 + 0.5 * jitter.next());
        let (px, py) = (3.0 * jitter.next(), 3.0 * jitter.next());
        strokes
            .into_iter()
            .map(|stroke| {
                let mut points: Vec<Vec2> = stroke
                    .iter()
                    .map(|p| {
                        let warp =
                            Vec2::new((p.y * fx * 4.0 + px).sin(), (p.x * fy * 6.0 + py).sin())
                                * 0.04;
                        let noise = Vec2::new(jitter.next(), jitter.next()) * 0.005;
                        let p = Vec2::new(p.x + slant * (1.0 - p.y), p.y) + warp + noise;
                        origin + rotation.rotate(p - 0.5) * size + 0.5 * size
                    })
                    .collect();
                if reverse {
                    points.reverse();
                }
                points
            })
            .collect()
    }

    fn line(from: Vec2, to: Vec2) -> Vec<Vec2> {
        (0..=8).map(|i| from.lerp(to, i as f32 / 8.0)).collect()
    }

    fn dot(at: Vec2) -> Vec<Vec2> {
        vec![at, at + Vec2::new(0.5, 0.4), at + Vec2::new(0.2, 0.8)]
    }

    #[test]
    fn classifies_distorted_symbols() {
        // Synthetic handwriting is no substitute for real samples, but it keeps the templates
        // apart from each other: at most 1 in 20 distorted symbols may be misread
        let recognizer = Recognizer::new();
        let mut jitter = Jitter(7);
        let templates = builtin_templates();
        let mut total = 0;
        let mut wrong = Vec::new();
        for (index, (symbol, _)) in templates.iter().enumerate() {
            let variant = templates[..index]
                .iter()
                .filter(|(s, _)| s == symbol)
                .count();
            for round in 0..6 {
                let aspect = 1.0 + 0.2 * jitter.next();
                let strokes = write(
                    *symbol,
                    variant,
                    Vec2::ZERO,
                    Vec2::new(14.0 * template_width(*symbol) * aspect, 14.0),
                    &mut jitter,
                    round % 2 == 1,
                );
                let parts: Vec<&[Vec2]> = strokes.iter().map(Vec::as_slice).collect();
                let (found, distance) = recognizer.classify(&parts).unwrap();
                total += 1;
                if found != *symbol || distance > MAX_DISTANCE {
                    wrong.push((*symbol, variant, found, distance));
                }
            }
        }
        assert!(
            wrong.len() * 20 <= total,
            "misread {} of {total}: {wrong:?}",
            wrong.len()
        );
    }

    /// Writes the symbols of `text` from left to right; `^` raises the next symbol
    fn write_line(text: &str, jitter: &mut Jitter) -> Vec<Vec<Vec2>> {
        let mut strokes = Vec::new();
        let mut x = 0.0;
        let mut raised = false;
        for ch in text.chars() {
            let (y, h) = if raised { (-5.0, 8.0) } else { (0.0, 14.0) };
            let w = h * 0.7;
            match ch {
                '^' => {
                    raised = true;
                    continue;
                }
                ' ' => x += 4.0,
                '-' => {
                    strokes.push(line(
                        Vec2::new(x, y + h * 0.5),
                        Vec2::new(x + w, y + h * 0.5),
                    ));
                    x += w + 4.0;
                }
                '=' => {
                    strokes.push(line(Vec2::new(x, 4.0), Vec2::new(x + 10.0, 4.5)));
                    strokes.push(line(Vec2::new(x, 9.0), Vec2::new(x + 10.0, 9.5)));
                    x += 14.0;
                }
                '·' => {
                    strokes.push(dot(Vec2::new(x, 6.5)));
                    x += 5.0;
                }
                ',' => {
                    strokes.push(dot(Vec2::new(x, 13.0)));
                    x += 4.0;
                }
                ':' => {
                    strokes.push(dot(Vec2::new(x, 3.0)));
                    strokes.push(dot(Vec2::new(x, 10.0)));
                    x += 5.0;
                }
                _ => {
                    // Lowercase letters are smaller and sit on the baseline, `y` goes below it
                    let (top, size) = match ch {
                        'a' | 'x' => (y + 0.4 * h, Vec2::splat(0.6 * h)),
                        'y' => (y + 0.4 * h, Vec2::new(0.5 * h, 0.9 * h)),
                        '+' => (y + 0.15 * h, Vec2::splat(0.7 * h)),
                        _ => (y, Vec2::new(template_width(ch) * h, h)),
                    };
                    strokes.extend(write(ch, 0, Vec2::new(x, top), size, jitter, false));
                    x += size.x + 4.0;
                }
            }
            raised = false;
        }
        strokes
    }

    #[test]
    fn reads_expressions() {
        let cases = [
            ("2^4=", "2^(4)="),
            ("12+34=", "12+34="),
            ("(x+3)^2=", "(x+3)^(2)="),
            ("7-5=", "7-5="),
            ("2,5·3=", "2,5·3="),
            ("9:3=", "9:3="),
            ("2x3=", "2·3="),
            ("(a+b)^2=", "(a+b)^(2)="),
            ("x^2+2x=", "x^(2)+2x="),
            ("x^2-1=", "x^(2)-1="),
            ("y^1^0=", "y^(10)="),
            ("2^x^+^1=", "2^(x+1)="),
            ("2^-^1=", "2^(-1)="),
            ("56·78=", "56·78="),
            ("90-1=", "90-1="),
            ("3(x+1)=", "3(x+1)="),
            ("(x+1)(x-1)=", "(x+1)(x-1)="),
            ("b^2:4=", "b^(2):4="),
            ("y·y·y=", "y·y·y="),
        ];
        let recognizer = Recognizer::new();
        for seed in 1..=4 {
            for (written, expected) in cases {
                let strokes = write_line(written, &mut Jitter(seed));
                let text = recognizer.recognize(&strokes).unwrap().text;
                assert_eq!(text, expected, "{written} (seed {seed})");
            }
        }
    }

    fn read(text: &str) -> String {
        let strokes = write_line(text, &mut Jitter(11));
        Recognizer::new().recognize(&strokes).unwrap().text
    }

    #[test]
    fn reads_fractions() {
        let mut jitter = Jitter(3);
        let mut strokes = write(
            '1',
            0,
            Vec2::new(2.0, -16.0),
            Vec2::new(8.0, 13.0),
            &mut jitter,
            false,
        );
        strokes.push(line(Vec2::new(0.0, 0.0), Vec2::new(12.0, 0.3)));
        strokes.extend(write(
            '2',
            0,
            Vec2::new(2.0, 3.0),
            Vec2::new(8.0, 13.0),
            &mut jitter,
            false,
        ));
        strokes.extend(write(
            '+',
            0,
            Vec2::new(16.0, -5.0),
            Vec2::new(9.0, 9.0),
            &mut jitter,
            false,
        ));
        strokes.extend(write(
            '3',
            0,
            Vec2::new(29.0, -7.0),
            Vec2::new(9.0, 14.0),
            &mut jitter,
            false,
        ));
        strokes.push(line(Vec2::new(42.0, -3.0), Vec2::new(52.0, -3.0)));
        strokes.push(line(Vec2::new(42.0, 2.0), Vec2::new(52.0, 2.0)));
        let recognition = Recognizer::new().recognize(&strokes).unwrap();
        assert_eq!(recognition.text, "((1)/(2))+3=");
        assert_eq!(
            crate::math::solve(&recognition.text, true),
            Some("3,5".into())
        );
    }

    fn shifted(strokes: Vec<Vec<Vec2>>, offset: Vec2) -> Vec<Vec<Vec2>> {
        strokes
            .into_iter()
            .map(|s| s.into_iter().map(|p| p + offset).collect())
            .collect()
    }

    #[test]
    fn reads_fractions_with_expressions() {
        let mut jitter = Jitter(9);
        // (x+1)/2 + 1/(2·3) =, with the bars drawn after the numerators
        let mut strokes = shifted(write_line("x+1", &mut jitter), Vec2::new(2.0, -17.0));
        strokes.push(line(Vec2::new(0.0, 0.0), Vec2::new(40.0, 0.5)));
        strokes.extend(shifted(write_line("2", &mut jitter), Vec2::new(14.0, 3.0)));
        strokes.extend(shifted(write_line("+", &mut jitter), Vec2::new(46.0, -7.0)));
        strokes.extend(shifted(
            write_line("1", &mut jitter),
            Vec2::new(70.0, -17.0),
        ));
        strokes.push(line(Vec2::new(62.0, 0.0), Vec2::new(90.0, 0.0)));
        strokes.extend(shifted(
            write_line("2·3", &mut jitter),
            Vec2::new(64.0, 3.0),
        ));
        strokes.extend(shifted(write_line("=", &mut jitter), Vec2::new(96.0, -7.0)));
        let recognition = Recognizer::new().recognize(&strokes).unwrap();
        assert_eq!(recognition.text, "((x+1)/(2))+((1)/(2·3))=");
        assert_eq!(
            crate::math::solve(&recognition.text, true),
            Some("0,5x + 2/3".into())
        );
    }

    #[test]
    fn recognized_text_can_be_computed() {
        let solve = |text: &str| crate::math::solve(&read(text), true);
        assert_eq!(solve("2^4="), Some("16".into()));
        assert_eq!(solve("(x+3)^2="), Some("x² + 6x + 9".into()));
        assert_eq!(solve("2,5·3="), Some("7,5".into()));
        assert_eq!(solve("9:3="), Some("3".into()));
    }

    #[test]
    fn symbols_cover_their_strokes() {
        let strokes = write_line("4+7=", &mut Jitter(5));
        let recognition = Recognizer::new().recognize(&strokes).unwrap();
        let chars: String = recognition.symbols.iter().map(|s| s.ch).collect();
        assert_eq!(chars, "4+7=");
        let mut covered: Vec<usize> = recognition
            .symbols
            .iter()
            .flat_map(|s| s.strokes.iter().copied())
            .collect();
        covered.sort_unstable();
        assert_eq!(covered, (0..strokes.len()).collect::<Vec<_>>());
    }

    #[test]
    fn scribbles_are_not_recognized() {
        let recognizer = Recognizer::new();
        let zigzag: Vec<Vec2> = (0..40)
            .map(|i| Vec2::new(i as f32, if i % 2 == 0 { 0.0 } else { 10.0 }))
            .collect();
        let recognition = recognizer.recognize(&[zigzag]).unwrap();
        assert_eq!(recognition.text, "?");
        assert!(recognizer.recognize(&[]).is_none());
    }
}
