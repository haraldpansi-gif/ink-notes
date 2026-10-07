//! Geometrie- und Objekterkennung (Shape Recognition) für Freihandstriche.
//!
//! Erkennt Geraden, Rechtecke, Kreise und Koordinatensysteme (Achsen)
//! und erzeugt saubere Vektor-Primitiven und GPU-Triangle-Meshes.

use glam::Vec2;
use serde::{Deserialize, Serialize};
use std::f32::consts::PI;

use crate::canvas::InkVertex;
use crate::document::{Aabb, Stroke, StylusSample};

/// Arten der geometrischen Primitiven.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ShapeKind {
    Line,
    Circle,
    Rectangle,
    CoordinateSystem,
    Triangle,
    Rhombus,
    Polygon,
    Octahedron,
}

/// Erkannte, mathematisch bereinigte Form.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum RecognizedShape {
    Line {
        start: Vec2,
        end: Vec2,
    },
    Circle {
        center: Vec2,
        radius: f32,
    },
    Rectangle {
        /// 4 Eckpunkte in umlaufender Reihenfolge (strikt rechtwinklig).
        corners: [Vec2; 4],
    },
    CoordinateSystem {
        origin: Vec2,
        x_axis_end: Vec2,
        y_axis_end: Vec2,
        with_arrows: bool,
    },
    Triangle {
        /// 3 Eckpunkte in umlaufender Reihenfolge.
        corners: [Vec2; 3],
    },
    Rhombus {
        /// 4 Eckpunkte in umlaufender Reihenfolge (Raute/Karo mit gleich langen Kanten).
        corners: [Vec2; 4],
    },
    Polygon {
        /// N Eckpunkte in umlaufender Reihenfolge (z. B. 5-Eck, 6-Eck, 8-Eck / Oktagon).
        vertices: Vec<Vec2>,
    },
    Octahedron {
        /// 3D-Kantenmodell (Doppelpyramide) mit 6 Eckpunkten und 12 Kanten im Schrägbild.
        center: Vec2,
        radius_x: f32,
        radius_y: f32,
        depth: f32,
    },
}

impl RecognizedShape {
    pub fn kind(&self) -> ShapeKind {
        match self {
            Self::Line { .. } => ShapeKind::Line,
            Self::Circle { .. } => ShapeKind::Circle,
            Self::Rectangle { .. } => ShapeKind::Rectangle,
            Self::CoordinateSystem { .. } => ShapeKind::CoordinateSystem,
            Self::Triangle { .. } => ShapeKind::Triangle,
            Self::Rhombus { .. } => ShapeKind::Rhombus,
            Self::Polygon { .. } => ShapeKind::Polygon,
            Self::Octahedron { .. } => ShapeKind::Octahedron,
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::Line { .. } => "Gerade",
            Self::Circle { .. } => "Kreis",
            Self::Rectangle { .. } => "Rechteck",
            Self::CoordinateSystem { .. } => "Koordinatensystem",
            Self::Triangle { .. } => "Dreieck",
            Self::Rhombus { .. } => "Raute (Rhombus)",
            Self::Polygon { vertices } => match vertices.len() {
                5 => "Fünfeck (Pentagon)",
                6 => "Sechseck (Hexagon)",
                8 => "Achteck (Oktagon)",
                _ => "Polygon",
            },
            Self::Octahedron { .. } => "Oktaeder (3D-Kantenmodell)",
        }
    }

    /// Verschiebt die geometrische Form um `delta`.
    pub fn translate(&mut self, delta: Vec2) {
        match self {
            Self::Line { start, end } => {
                *start += delta;
                *end += delta;
            }
            Self::Circle { center, .. } => {
                *center += delta;
            }
            Self::Rectangle { corners } | Self::Rhombus { corners } => {
                for c in corners {
                    *c += delta;
                }
            }
            Self::Triangle { corners } => {
                for c in corners {
                    *c += delta;
                }
            }
            Self::CoordinateSystem {
                origin,
                x_axis_end,
                y_axis_end,
                ..
            } => {
                *origin += delta;
                *x_axis_end += delta;
                *y_axis_end += delta;
            }
            Self::Polygon { vertices } => {
                for v in vertices {
                    *v += delta;
                }
            }
            Self::Octahedron { center, .. } => {
                *center += delta;
            }
        }
    }

    /// Dreht die geometrische Form um `origin` mit dem Winkel `angle_rad`.
    pub fn rotate(&mut self, origin: Vec2, angle_rad: f32) {
        let rot = |pt: Vec2| -> Vec2 {
            let diff = pt - origin;
            let (sin, cos) = angle_rad.sin_cos();
            origin + Vec2::new(diff.x * cos - diff.y * sin, diff.x * sin + diff.y * cos)
        };

        match self {
            Self::Line { start, end } => {
                *start = rot(*start);
                *end = rot(*end);
            }
            Self::Circle { center, .. } => {
                *center = rot(*center);
            }
            Self::Rectangle { corners } | Self::Rhombus { corners } => {
                for c in corners {
                    *c = rot(*c);
                }
            }
            Self::Triangle { corners } => {
                for c in corners {
                    *c = rot(*c);
                }
            }
            Self::CoordinateSystem {
                origin: o,
                x_axis_end,
                y_axis_end,
                ..
            } => {
                *o = rot(*o);
                *x_axis_end = rot(*x_axis_end);
                *y_axis_end = rot(*y_axis_end);
            }
            Self::Polygon { vertices } => {
                for v in vertices {
                    *v = rot(*v);
                }
            }
            Self::Octahedron { center, .. } => {
                *center = rot(*center);
            }
        }
    }

    /// Skaliert die geometrische Form bezüglich `origin` um den Faktor `factors`.
    pub fn scale(&mut self, origin: Vec2, factors: Vec2) {
        let sc = |pt: Vec2| -> Vec2 { origin + (pt - origin) * factors };

        match self {
            Self::Line { start, end } => {
                *start = sc(*start);
                *end = sc(*end);
            }
            Self::Circle { center, radius } => {
                *center = sc(*center);
                *radius *= (factors.x.abs() + factors.y.abs()) * 0.5;
            }
            Self::Rectangle { corners } | Self::Rhombus { corners } => {
                for c in corners {
                    *c = sc(*c);
                }
            }
            Self::Triangle { corners } => {
                for c in corners {
                    *c = sc(*c);
                }
            }
            Self::CoordinateSystem {
                origin: o,
                x_axis_end,
                y_axis_end,
                ..
            } => {
                *o = sc(*o);
                *x_axis_end = sc(*x_axis_end);
                *y_axis_end = sc(*y_axis_end);
            }
            Self::Polygon { vertices } => {
                for v in vertices {
                    *v = sc(*v);
                }
            }
            Self::Octahedron {
                center,
                radius_x,
                radius_y,
                depth,
            } => {
                *center = sc(*center);
                *radius_x *= factors.x.abs();
                *radius_y *= factors.y.abs();
                *depth *= (factors.x.abs() + factors.y.abs()) * 0.5;
            }
        }
    }
}

/// Vereinfacht einen Polygonzug mit dem Ramer-Douglas-Peucker (RDP) Algorithmus.
pub fn ramer_douglas_peucker(points: &[Vec2], epsilon: f32) -> Vec<Vec2> {
    if points.len() <= 2 {
        return points.to_vec();
    }
    rdp_recursive(points, epsilon)
}

fn rdp_recursive(points: &[Vec2], epsilon: f32) -> Vec<Vec2> {
    if points.len() <= 2 {
        return points.to_vec();
    }

    let first = points[0];
    let last = *points.last().unwrap();
    let mut max_dist = 0.0f32;
    let mut max_idx = 0;

    for (i, &pt) in points.iter().enumerate().skip(1).take(points.len() - 2) {
        let dist = point_to_line_dist(pt, first, last);
        if dist > max_dist {
            max_dist = dist;
            max_idx = i;
        }
    }

    if max_dist > epsilon {
        let mut left = rdp_recursive(&points[..=max_idx], epsilon);
        let right = rdp_recursive(&points[max_idx..], epsilon);
        left.pop();
        left.extend(right);
        left
    } else {
        vec![first, last]
    }
}

/// Abstand eines Punktes `p` zur Geraden durch `a` und `b`.
#[inline]
pub fn point_to_line_dist(p: Vec2, a: Vec2, b: Vec2) -> f32 {
    let ab = b - a;
    let len = ab.length();
    if len < 1e-6 {
        return (p - a).length();
    }
    let cross = (p.x - a.x) * ab.y - (p.y - a.y) * ab.x;
    cross.abs() / len
}

/// Berechnet die aufsummierte Pfadlänge einer Punktfolge.
pub fn path_length(points: &[Vec2]) -> f32 {
    points.windows(2).map(|w| (w[1] - w[0]).length()).sum()
}

/// 1. Geradenerkennung: Verhältnis Sehnenlänge zu Pfadlänge > 0.92.
pub fn detect_line(points: &[Vec2]) -> Option<RecognizedShape> {
    if points.len() < 2 {
        return None;
    }
    let start = points[0];
    let end = *points.last().unwrap();
    let chord_len = (end - start).length();
    let total_len = path_length(points);

    if total_len < 10.0 {
        return None;
    }

    let ratio = chord_len / total_len;
    if ratio > 0.92 {
        // Bei Winkeln nahe an horizontal/vertikal (innerhalb 5°) einrasten
        let dir = end - start;
        let angle_deg = dir.y.atan2(dir.x).to_degrees().abs();
        let snapped_end = if angle_deg < 5.0 || (angle_deg - 180.0).abs() < 5.0 {
            Vec2::new(end.x, start.y)
        } else if (angle_deg - 90.0).abs() < 5.0 {
            Vec2::new(start.x, end.y)
        } else {
            end
        };

        Some(RecognizedShape::Line {
            start,
            end: snapped_end,
        })
    } else {
        None
    }
}

/// 2. Kreis- & Ellipsenerkennung: Geschlossener Pfad und geringe Standardabweichung der Radien.
pub fn detect_circle(points: &[Vec2]) -> Option<RecognizedShape> {
    if points.len() < 8 {
        return None;
    }

    let total_len = path_length(points);
    if total_len < 30.0 {
        return None;
    }

    let start = points[0];
    let end = *points.last().unwrap();
    let chord_len = (end - start).length();

    // Pfad muss annähernd geschlossen sein
    if chord_len > 0.35 * total_len {
        return None;
    }

    // Schwerpunkt als Startpunkt
    let n = points.len() as f32;
    let mean_x = points.iter().map(|p| p.x).sum::<f32>() / n;
    let mean_y = points.iter().map(|p| p.y).sum::<f32>() / n;

    // Algebraic Least-Squares Circle Fit (Kåsa-Verfahren)
    let mut suu = 0.0;
    let mut suv = 0.0;
    let mut svv = 0.0;
    let mut suuu = 0.0;
    let mut svvv = 0.0;
    let mut suvv = 0.0;
    let mut svuu = 0.0;

    for p in points {
        let u = p.x - mean_x;
        let v = p.y - mean_y;
        let u2 = u * u;
        let v2 = v * v;
        suu += u2;
        suv += u * v;
        svv += v2;
        suuu += u2 * u;
        svvv += v2 * v;
        suvv += u * v2;
        svuu += v * u2;
    }

    let det = suu * svv - suv * suv;
    let center = if det.abs() > 1e-5 {
        let rhs_u = 0.5 * (suuu + suvv);
        let rhs_v = 0.5 * (svvv + svuu);
        let uc = (rhs_u * svv - rhs_v * suv) / det;
        let vc = (suu * rhs_v - suv * rhs_u) / det;
        Vec2::new(mean_x + uc, mean_y + vc)
    } else {
        Vec2::new(mean_x, mean_y)
    };

    // Radien vom berechneten Mittelpunkt ermitteln
    let radii: Vec<f32> = points.iter().map(|p| (*p - center).length()).collect();
    let mean_r = radii.iter().sum::<f32>() / n;
    if mean_r < 10.0 {
        return None;
    }

    let variance = radii.iter().map(|r| (r - mean_r).powi(2)).sum::<f32>() / n;
    let std_dev = variance.sqrt();
    let cv = std_dev / mean_r; // Variationskoeffizient

    // Geringe Standardabweichung der Radien (für Freihandkreise/Ovale < 0.22)
    if cv < 0.22 {
        Some(RecognizedShape::Circle {
            center,
            radius: mean_r,
        })
    } else {
        None
    }
}

/// 3. Rechteckerkennung: kleinstes umschließendes Rechteck eines geschlossenen Pfads.
///
/// Statt Ecken über eine grobe Linienvereinfachung zu suchen (das schnitt bei kleinen oder
/// zittrigen Strichen die Ecken ab), wird über alle Drehwinkel das Rechteck mit der
/// kleinsten Fläche gesucht. Einzelne Ausreißer (z. B. ein Überstrich am Schluss) zählen
/// nicht: Die Ausdehnung wird über das 2.–98. Perzentil der Punkte bestimmt. Angenommen
/// wird das Rechteck nur, wenn es den Strich eng beschreibt.
pub fn detect_rectangle(points: &[Vec2]) -> Option<RecognizedShape> {
    if points.len() < 8 {
        return None;
    }

    let total_len = path_length(points);
    if total_len < 40.0 {
        return None;
    }

    let start = points[0];
    let end = *points.last().unwrap();
    let chord_len = (end - start).length();

    // Pfad muss annähernd geschlossen sein
    if chord_len > 0.35 * total_len {
        return None;
    }

    // Ausdehnung entlang einer Achse (Winkel in Grad), robust gegen Ausreißer
    let extent = |angle_deg: f32| -> (f32, f32, f32, f32) {
        let (sin, cos) = angle_deg.to_radians().sin_cos();
        let mut along: Vec<f32> = points.iter().map(|p| p.x * cos + p.y * sin).collect();
        let mut across: Vec<f32> = points.iter().map(|p| -p.x * sin + p.y * cos).collect();
        along.sort_by(f32::total_cmp);
        across.sort_by(f32::total_cmp);
        let lo = points.len() * 2 / 100;
        let hi = points.len() - 1 - lo;
        (along[lo], along[hi], across[lo], across[hi])
    };
    let area = |angle_deg: f32| {
        let (a0, a1, b0, b1) = extent(angle_deg);
        (a1 - a0) * (b1 - b0)
    };

    // Grobsuche in 1°-Schritten, dann Feinsuche in 0,1°-Schritten um das Optimum
    let mut best_angle = (0..90)
        .map(|d| d as f32)
        .min_by(|a, b| area(*a).total_cmp(&area(*b)))
        .unwrap_or(0.0);
    best_angle = (-10..=10)
        .map(|d| best_angle + d as f32 * 0.1)
        .min_by(|a, b| area(*a).total_cmp(&area(*b)))
        .unwrap_or(best_angle);

    // Nahe an der Achsenparallelität (≤ 10°) gerade ausrichten
    let rem = best_angle.rem_euclid(90.0);
    if rem <= RECTANGLE_AXIS_SNAP_DEG || rem >= 90.0 - RECTANGLE_AXIS_SNAP_DEG {
        best_angle = 0.0;
    }

    let (mut a0, mut a1, mut b0, mut b1) = extent(best_angle);
    let (sin, cos) = best_angle.to_radians().sin_cos();
    let dir = Vec2::new(cos, sin);
    let norm = Vec2::new(-sin, cos);

    // Kanten in die Mitte des Zitterns legen: Jeder Punkt gehört zur nächsten Kante, die Kante
    // wandert auf den Median ihrer Punkte (das umschließende Rechteck liegt außen an den Spitzen)
    let mut sides: [Vec<f32>; 4] = Default::default();
    for p in points {
        let (u, v) = (p.dot(dir), p.dot(norm));
        let d = [
            (u - a0).abs(),
            (u - a1).abs(),
            (v - b0).abs(),
            (v - b1).abs(),
        ];
        let side = (0..4).min_by(|&i, &j| d[i].total_cmp(&d[j])).unwrap_or(0);
        sides[side].push(if side < 2 { u } else { v });
    }
    let median = |values: &mut Vec<f32>, fallback: f32| {
        if values.is_empty() {
            return fallback;
        }
        values.sort_by(f32::total_cmp);
        values[values.len() / 2]
    };
    a0 = median(&mut sides[0], a0);
    a1 = median(&mut sides[1], a1);
    b0 = median(&mut sides[2], b0);
    b1 = median(&mut sides[3], b1);

    let corners = [
        dir * a0 + norm * b0,
        dir * a1 + norm * b0,
        dir * a1 + norm * b1,
        dir * a0 + norm * b1,
    ];

    // Alle vier Ecken müssen gezeichnet sein: Ovale und Kreise schneiden die Ecken ab
    // (Kreis: ~20 % der Seitenlänge), sonst würden flache Ellipsen zu Rechtecken
    let shorter_side = (a1 - a0).abs().min((b1 - b0).abs());
    let corner_reached = |corner: Vec2| {
        points
            .iter()
            .map(|p| (*p - corner).length())
            .fold(f32::INFINITY, f32::min)
            <= RECTANGLE_CORNER_REACH * shorter_side
    };
    if !corners.iter().all(|c| corner_reached(*c)) {
        return None;
    }

    // Nur annehmen, wenn das Rechteck den Strich eng beschreibt (sonst wären beliebige
    // geschlossene Kritzeleien „Rechtecke“)
    let rect = RecognizedShape::Rectangle { corners };
    let err = contour_rms(points, &rect)?;
    let diagonal = (corners[2] - corners[0]).length();
    if err > RECTANGLE_MAX_RMS * diagonal {
        return None;
    }
    Some(rect)
}

/// Rastet einen Winkel in Radiant an horizontalen, vertikalen oder diagonalen Achsen ein.
pub fn snap_angle(angle_rad: f32, tolerance_deg: f32) -> f32 {
    let tol = tolerance_deg.to_radians();
    let cardinal = [
        0.0,
        PI * 0.5,
        PI,
        -PI,
        -PI * 0.5,
        PI * 0.25,
        PI * 0.75,
        -PI * 0.25,
        -PI * 0.75,
    ];
    for &target in &cardinal {
        let diff = (angle_rad - target).abs();
        let diff_wrapped = diff
            .min((angle_rad - (target + 2.0 * PI)).abs())
            .min((angle_rad - (target - 2.0 * PI)).abs());
        if diff_wrapped < tol {
            return target;
        }
    }
    angle_rad
}

/// Extrahiert die k markantesten Ecken (stärkste Richtungsänderung) aus einem vereinfachten Polygonzug.
pub fn extract_k_corners(points: &[Vec2], k: usize) -> Option<Vec<Vec2>> {
    let n = points.len();
    if n < k {
        return None;
    }

    let mut scored: Vec<(usize, f32)> = Vec::with_capacity(n);
    for i in 0..n {
        let prev = points[(i + n - 1) % n];
        let curr = points[i];
        let next = points[(i + 1) % n];
        let v1 = (prev - curr).normalize_or_zero();
        let v2 = (next - curr).normalize_or_zero();
        // Skalarprodukt der Richtungen zu den Nachbarn: −1 = gerader Verlauf (180°),
        // 0 = rechter Winkel, gegen +1 = spitze Ecke. Je größer, desto markanter die Ecke.
        let dot = v1.dot(v2).clamp(-1.0, 1.0);
        scored.push((i, dot));
    }

    // Markanteste Ecken zuerst (größtes Skalarprodukt). Vorher war die Sortierung
    // umgekehrt und wählte die geradesten Punkte.
    scored.sort_by(|a, b| b.1.total_cmp(&a.1));
    let mut selected_indices: Vec<usize> = scored.iter().take(k).map(|&(idx, _)| idx).collect();
    selected_indices.sort_unstable();

    Some(
        selected_indices
            .into_iter()
            .map(|idx| points[idx])
            .collect(),
    )
}

/// 4. Dreieck-Erkennung: Geschlossener Pfad mit 3 dominanten Ecken.
pub fn detect_triangle(points: &[Vec2]) -> Option<RecognizedShape> {
    if points.len() < 4 {
        return None;
    }

    let total_len = path_length(points);
    if total_len < 35.0 {
        return None;
    }

    let start = points[0];
    let end = *points.last().unwrap();
    let chord_len = (end - start).length();

    // Pfad muss annähernd geschlossen sein
    if chord_len > 0.40 * total_len {
        return None;
    }

    let epsilon = (total_len * 0.07).clamp(8.0, 50.0);
    let simplified = ramer_douglas_peucker(points, epsilon);

    let corners: Vec<Vec2> = if (simplified.len() == 4
        && (simplified[0] - simplified[3]).length() < epsilon * 2.5)
        || simplified.len() == 3
    {
        simplified[0..3].to_vec()
    } else if simplified.len() > 4 {
        extract_k_corners(&simplified, 3)?
    } else {
        return None;
    };

    if corners.len() != 3 {
        return None;
    }

    let a = corners[0];
    let mut b = corners[1];
    let mut c = corners[2];

    let l_ab = (b - a).length();
    let l_bc = (c - b).length();
    let l_ca = (a - c).length();

    if l_ab < 15.0 || l_bc < 15.0 || l_ca < 15.0 {
        return None;
    }

    // Kreuzprodukt für Fläche
    let cross = ((b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x)).abs();
    if cross < 80.0 {
        return None;
    }

    let angle_at = |prev: Vec2, curr: Vec2, next: Vec2| -> f32 {
        let v1 = (prev - curr).normalize_or_zero();
        let v2 = (next - curr).normalize_or_zero();
        v1.dot(v2).clamp(-1.0, 1.0).acos().to_degrees()
    };

    let ang_a = angle_at(c, a, b);
    let ang_b = angle_at(a, b, c);
    let ang_c = angle_at(b, c, a);

    if ang_a < 18.0 || ang_b < 18.0 || ang_c < 18.0 {
        return None;
    }

    // Geometrisches Einrasten (Snapping):
    // 1. Gleichseitig? (Alle Winkel nahe 60°)
    if (ang_a - 60.0).abs() < 14.0 && (ang_b - 60.0).abs() < 14.0 && (ang_c - 60.0).abs() < 14.0 {
        let center = (a + b + c) / 3.0;
        let r = ((a - center).length() + (b - center).length() + (c - center).length()) / 3.0;
        let base_dir = (a - center).normalize_or_zero();
        let base_angle = base_dir.y.atan2(base_dir.x);
        let snapped_angle = snap_angle(base_angle, 15.0);

        let c0 = center + Vec2::new(snapped_angle.cos(), snapped_angle.sin()) * r;
        let c1 = center
            + Vec2::new(
                (snapped_angle + 2.0 * PI / 3.0).cos(),
                (snapped_angle + 2.0 * PI / 3.0).sin(),
            ) * r;
        let c2 = center
            + Vec2::new(
                (snapped_angle + 4.0 * PI / 3.0).cos(),
                (snapped_angle + 4.0 * PI / 3.0).sin(),
            ) * r;
        return Some(RecognizedShape::Triangle {
            corners: [c0, c1, c2],
        });
    }

    // 2. Rechtwinklig? (Ein Winkel nahe 90°)
    let angles = [(0, ang_a), (1, ang_b), (2, ang_c)];
    if let Some(&(idx, _)) = angles.iter().find(|(_, ang)| (*ang - 90.0).abs() < 14.0) {
        let (corner_v, p1, p2) = match idx {
            0 => (a, b, c),
            1 => (b, c, a),
            _ => (c, a, b),
        };
        let mut v1 = p1 - corner_v;
        let mut v2 = p2 - corner_v;

        let a1 = v1.y.atan2(v1.x);
        let a1_snapped = snap_angle(a1, 15.0);
        let len1 = v1.length();
        let len2 = v2.length();

        v1 = Vec2::new(a1_snapped.cos(), a1_snapped.sin()) * len1;
        let sign = if (v1.x * v2.y - v1.y * v2.x) >= 0.0 {
            1.0
        } else {
            -1.0
        };
        v2 = Vec2::new(-v1.y * sign, v1.x * sign).normalize_or_zero() * len2;

        let new_p1 = corner_v + v1;
        let new_p2 = corner_v + v2;
        let snapped = match idx {
            0 => [corner_v, new_p1, new_p2],
            1 => [new_p2, corner_v, new_p1],
            _ => [new_p1, new_p2, corner_v],
        };
        return Some(RecognizedShape::Triangle { corners: snapped });
    }

    // 3. Gleichschenklig?
    if (l_ab - l_ca).abs() / l_ab.max(l_ca) < 0.12 {
        let mid_bc = (b + c) * 0.5;
        let axis = (a - mid_bc).normalize_or_zero();
        let perp = Vec2::new(-axis.y, axis.x);
        let half_base = ((b - mid_bc).length() + (c - mid_bc).length()) * 0.5;
        b = mid_bc + perp * half_base;
        c = mid_bc - perp * half_base;
    } else if (l_ab - l_bc).abs() / l_ab.max(l_bc) < 0.12 {
        let mid_ac = (a + c) * 0.5;
        let axis = (b - mid_ac).normalize_or_zero();
        let perp = Vec2::new(-axis.y, axis.x);
        let half_base = ((a - mid_ac).length() + (c - mid_ac).length()) * 0.5;
        let new_a = mid_ac + perp * half_base;
        c = mid_ac - perp * half_base;
        return Some(RecognizedShape::Triangle {
            corners: [new_a, b, c],
        });
    } else if (l_bc - l_ca).abs() / l_bc.max(l_ca) < 0.12 {
        let mid_ab = (a + b) * 0.5;
        let axis = (c - mid_ab).normalize_or_zero();
        let perp = Vec2::new(-axis.y, axis.x);
        let half_base = ((a - mid_ab).length() + (b - mid_ab).length()) * 0.5;
        let new_a = mid_ab + perp * half_base;
        b = mid_ab - perp * half_base;
        return Some(RecognizedShape::Triangle {
            corners: [new_a, b, c],
        });
    }

    Some(RecognizedShape::Triangle { corners: [a, b, c] })
}

/// 5. Rauten-Erkennung (Rhombus / Karo): Geschlossener Pfad mit 4 gleich langen Kanten und orthogonalen Diagonalen.
pub fn detect_rhombus(points: &[Vec2]) -> Option<RecognizedShape> {
    if points.len() < 4 {
        return None;
    }

    let total_len = path_length(points);
    if total_len < 40.0 {
        return None;
    }

    let start = points[0];
    let end = *points.last().unwrap();
    if (end - start).length() > 0.35 * total_len {
        return None;
    }

    let epsilon = (total_len * 0.05).clamp(8.0, 45.0);
    let simplified = ramer_douglas_peucker(points, epsilon);

    let corners: Vec<Vec2> =
        if simplified.len() == 5 && (simplified[0] - simplified[4]).length() < epsilon * 2.5 {
            simplified[0..4].to_vec()
        } else if simplified.len() == 4 {
            simplified
        } else if simplified.len() > 4 {
            extract_k_corners(&simplified, 4)?
        } else {
            return None;
        };

    if corners.len() != 4 {
        return None;
    }

    let c = [corners[0], corners[1], corners[2], corners[3]];

    let l0 = (c[1] - c[0]).length();
    let l1 = (c[2] - c[1]).length();
    let l2 = (c[3] - c[2]).length();
    let l3 = (c[0] - c[3]).length();
    let mean_l = (l0 + l1 + l2 + l3) * 0.25;

    if mean_l < 15.0 {
        return None;
    }

    let max_dev = ((l0 - mean_l).abs())
        .max((l1 - mean_l).abs())
        .max((l2 - mean_l).abs())
        .max((l3 - mean_l).abs());
    if max_dev / mean_l > 0.28 {
        return None;
    }

    let center = (c[0] + c[1] + c[2] + c[3]) * 0.25;
    let d1 = c[2] - c[0];
    let d2 = c[3] - c[1];

    let len1 = d1.length();
    let len2 = d2.length();
    if len1 < 10.0 || len2 < 10.0 {
        return None;
    }

    let dot = (d1.normalize_or_zero().dot(d2.normalize_or_zero())).clamp(-1.0, 1.0);
    let diag_angle_deg = dot.acos().to_degrees();
    if !(65.0..=115.0).contains(&diag_angle_deg) {
        return None;
    }

    let mut dir1 = d1.normalize_or_zero();
    let angle1 = dir1.y.atan2(dir1.x);
    let snapped_a1 = snap_angle(angle1, 15.0);
    dir1 = Vec2::new(snapped_a1.cos(), snapped_a1.sin());

    let sign = if (dir1.x * d2.y - dir1.y * d2.x) >= 0.0 {
        1.0
    } else {
        -1.0
    };
    let dir2 = Vec2::new(-dir1.y * sign, dir1.x * sign);

    let h1 = len1 * 0.5;
    let h2 = len2 * 0.5;

    let c0 = center - dir1 * h1;
    let c1 = center - dir2 * h2;
    let c2 = center + dir1 * h1;
    let c3 = center + dir2 * h2;

    Some(RecognizedShape::Rhombus {
        corners: [c0, c1, c2, c3],
    })
}

/// 6. Polygon-Erkennung (z. B. 5-Eck, 6-Eck, 8-Eck / Oktagon).
pub fn detect_polygon(points: &[Vec2]) -> Option<RecognizedShape> {
    if points.len() < 6 {
        return None;
    }

    let total_len = path_length(points);
    if total_len < 50.0 {
        return None;
    }

    let start = points[0];
    let end = *points.last().unwrap();
    if (end - start).length() > 0.35 * total_len {
        return None;
    }

    let epsilon = (total_len * 0.04).clamp(6.0, 30.0);
    let simplified = ramer_douucker_closed(points, epsilon);

    let n = simplified.len();
    if n != 5 && n != 6 && n != 8 {
        return None;
    }

    let corners = simplified;
    // Echte Ecken verlangen: Bei einem glatten Oval verteilt sich die Richtungsänderung
    // gleichmäßig, die Vereinfachung liefert aber trotzdem 6 oder 8 Punkte
    if !turning_concentrated_at_corners(points, &corners) {
        return None;
    }
    let center = corners.iter().copied().sum::<Vec2>() / (n as f32);

    let radii: Vec<f32> = corners.iter().map(|&pt| (pt - center).length()).collect();
    let mean_r = radii.iter().sum::<f32>() / (n as f32);
    if mean_r < 15.0 {
        return None;
    }

    let max_r_diff = radii
        .iter()
        .map(|r| (r - mean_r).abs())
        .fold(0.0f32, f32::max);

    let snapped_vertices = if max_r_diff / mean_r < 0.25 {
        let base_dir = (corners[0] - center).normalize_or_zero();
        let base_angle = snap_angle(base_dir.y.atan2(base_dir.x), 15.0);
        let step = 2.0 * PI / (n as f32);

        (0..n)
            .map(|i| {
                let ang = base_angle + i as f32 * step;
                center + Vec2::new(ang.cos(), ang.sin()) * mean_r
            })
            .collect()
    } else {
        corners
    };

    Some(RecognizedShape::Polygon {
        vertices: snapped_vertices,
    })
}

/// Anteil der gesamten Richtungsänderung, der bei einem Vieleck in der Nähe der Ecken liegen muss.
const POLYGON_TURNING_AT_CORNERS: f32 = 0.7;
/// Umgebung einer Ecke als Anteil des Umfangs.
const POLYGON_CORNER_NEIGHBORHOOD: f32 = 0.03;

/// Liegt die Richtungsänderung des Strichs überwiegend an den Ecken (Vieleck) und nicht
/// gleichmäßig verteilt (Oval)? Gemessen wird die vorzeichenbehaftete Drehung entlang des
/// gleichmäßig neu abgetasteten Strichs; Zittern hebt sich dabei weitgehend auf.
fn turning_concentrated_at_corners(points: &[Vec2], corners: &[Vec2]) -> bool {
    corner_turning_ratio(points, corners).is_some_and(|r| r >= POLYGON_TURNING_AT_CORNERS)
}

/// Anteil der vorzeichenbehafteten Richtungsänderung, der in der Nähe der Ecken liegt.
fn corner_turning_ratio(points: &[Vec2], corners: &[Vec2]) -> Option<f32> {
    let total_len = path_length(points);
    if total_len < 1e-3 || corners.is_empty() {
        return None;
    }
    // Gleichmäßig neu abtasten (120 Punkte entlang der Bogenlänge)
    const SAMPLES: usize = 120;
    let step = total_len / SAMPLES as f32;
    let mut resampled = vec![points[0]];
    let mut carried = 0.0;
    for w in points.windows(2) {
        let (a, b) = (w[0], w[1]);
        let seg = (b - a).length();
        let mut pos = step - carried;
        while pos <= seg {
            resampled.push(a + (b - a) * (pos / seg));
            pos += step;
        }
        carried = seg - (pos - step);
    }
    // Richtungen über einen Abstand von 3 Punkten, um das Zittern zu glätten
    const W: usize = 3;
    if resampled.len() < 2 * W + 2 {
        return None;
    }
    let near = POLYGON_CORNER_NEIGHBORHOOD * total_len;
    let (mut turn_total, mut turn_near) = (0.0_f32, 0.0_f32);
    for i in W..resampled.len() - W {
        let d_in = resampled[i] - resampled[i - W];
        let d_out = resampled[i + W] - resampled[i];
        let turn = d_in.perp_dot(d_out).atan2(d_in.dot(d_out)) / W as f32;
        turn_total += turn;
        if corners.iter().any(|c| (*c - resampled[i]).length() <= near) {
            turn_near += turn;
        }
    }
    (turn_total.abs() > 1e-3).then(|| turn_near / turn_total)
}

fn ramer_douucker_closed(points: &[Vec2], epsilon: f32) -> Vec<Vec2> {
    let simplified = ramer_douglas_peucker(points, epsilon);
    let mut vertices = if simplified.len() >= 2
        && (simplified[0] - *simplified.last().unwrap()).length() < epsilon * 2.0
    {
        simplified[0..simplified.len() - 1].to_vec()
    } else {
        simplified
    };
    // Überzählige Punkte entfernen: zu dicht am Nachbarn (z. B. das Strichende kurz vor dem
    // Start) oder fast auf einer Geraden mit den Nachbarn (< 20° Richtungsänderung)
    loop {
        let n = vertices.len();
        if n <= 3 {
            break;
        }
        let redundant = (0..n).find(|&i| {
            let (prev, curr, next) = (
                vertices[(i + n - 1) % n],
                vertices[i],
                vertices[(i + 1) % n],
            );
            let d_in = curr - prev;
            let d_out = next - curr;
            let turn = d_in.perp_dot(d_out).atan2(d_in.dot(d_out)).abs();
            (next - curr).length() < epsilon || turn < 20f32.to_radians()
        });
        match redundant {
            Some(i) => {
                vertices.remove(i);
            }
            None => break,
        }
    }
    vertices
}

/// 7. Oktaeder-Erkennung (3D-Kantenmodell / Doppelpyramide).
pub fn detect_octahedron(points: &[Vec2]) -> Option<RecognizedShape> {
    if points.len() < 8 {
        return None;
    }

    let total_len = path_length(points);
    if total_len < 70.0 {
        return None;
    }

    let aabb = Aabb::from_points(points);
    let w = (aabb.max.x - aabb.min.x) * 0.5;
    let h = (aabb.max.y - aabb.min.y) * 0.5;

    if w < 20.0 || h < 20.0 {
        return None;
    }

    let aspect = w / h;
    if !(0.3..=3.0).contains(&aspect) {
        return None;
    }

    let center = (aabb.min + aabb.max) * 0.5;

    let mut has_top = false;
    let mut has_bottom = false;
    let mut has_left = false;
    let mut has_right = false;
    let mut has_center_equator = false;

    for &pt in points {
        if pt.y <= aabb.min.y + 0.18 * h && (pt.x - center.x).abs() < 0.40 * w {
            has_top = true;
        }
        if pt.y >= aabb.max.y - 0.18 * h && (pt.x - center.x).abs() < 0.40 * w {
            has_bottom = true;
        }
        if pt.x <= aabb.min.x + 0.18 * w && (pt.y - center.y).abs() < 0.40 * h {
            has_left = true;
        }
        if pt.x >= aabb.max.x - 0.18 * w && (pt.y - center.y).abs() < 0.40 * h {
            has_right = true;
        }
        if (pt.x - center.x).abs() < 0.45 * w && (pt.y - center.y).abs() < 0.22 * h {
            has_center_equator = true;
        }
    }

    if has_top && has_bottom && has_left && has_right && has_center_equator {
        let depth = (w * 0.35).clamp(10.0, 50.0);
        return Some(RecognizedShape::Octahedron {
            center,
            radius_x: w,
            radius_y: h,
            depth,
        });
    }

    None
}

/// 4. Koordinatensystem-Erkennung: Zwei aufeinander stehende Linien (80° bis 100°), gesnappt auf exakt 90°.
pub fn detect_coordinate_system(points: &[Vec2]) -> Option<RecognizedShape> {
    if points.len() < 5 {
        return None;
    }

    let total_len = path_length(points);
    if total_len < 30.0 {
        return None;
    }

    let start = points[0];
    let end = *points.last().unwrap();
    let chord_len = (end - start).length();

    // Darf NICHT geschlossen sein
    if chord_len < 0.3 * total_len {
        return None;
    }

    // RDP zur Findung des Eckpunktes (Ursprungs)
    let epsilon = (total_len * 0.08).clamp(12.0, 50.0);
    let simplified = ramer_douglas_peucker(points, epsilon);

    if simplified.len() != 3 {
        return None;
    }

    let p0 = simplified[0];
    let origin = simplified[1];
    let p2 = simplified[2];

    let v0 = p0 - origin;
    let v2 = p2 - origin;
    let len0 = v0.length();
    let len2 = v2.length();

    if len0 < 15.0 || len2 < 15.0 {
        return None;
    }

    let dot = (v0.normalize().dot(v2.normalize())).clamp(-1.0, 1.0);
    let angle_deg = dot.acos().to_degrees();

    // Orthogonalitäts-Bedingung: 80° bis 100°
    if !(80.0..=100.0).contains(&angle_deg) {
        return None;
    }

    // Orthogonalitäts-Snap: Auf exakt 90° ausrichten
    // Bestimme, welche Achse näher an der Vertikalen (Y) und welche an der Horizontalen (X) liegt
    let (x_end, y_end) = if v0.x.abs() > v0.y.abs() {
        // v0 ist eher horizontal, v2 eher vertikal
        let sign_x = if v0.x >= 0.0 { 1.0 } else { -1.0 };
        let sign_y = if v2.y >= 0.0 { 1.0 } else { -1.0 };
        (
            origin + Vec2::new(sign_x * len0, 0.0),
            origin + Vec2::new(0.0, sign_y * len2),
        )
    } else {
        // v2 ist eher horizontal, v0 eher vertikal
        let sign_y = if v0.y >= 0.0 { 1.0 } else { -1.0 };
        let sign_x = if v2.x >= 0.0 { 1.0 } else { -1.0 };
        (
            origin + Vec2::new(sign_x * len2, 0.0),
            origin + Vec2::new(0.0, sign_y * len0),
        )
    };

    Some(RecognizedShape::CoordinateSystem {
        origin,
        x_axis_end: x_end,
        y_axis_end: y_end,
        with_arrows: true,
    })
}

/// Führt die automatische Erkennung für einen Strich aus.
pub fn detect_shape(points: &[StylusSample]) -> Option<RecognizedShape> {
    if points.len() < 2 {
        return None;
    }
    let pts: Vec<Vec2> = points.iter().map(|s| s.position).collect();

    // 1. Geschlossene Formen: Kreis, Dreieck, Rechteck und Raute konkurrieren; gewählt wird
    //    die Form, deren Kontur am besten zu den gezeichneten Punkten passt. Eine feste
    //    Reihenfolge genügt nicht: Auch Quadrate schwanken nur um ~10 % im Abstand zum
    //    Mittelpunkt und bestünden den Kreistest, und die Dreieckserkennung akzeptiert
    //    gestreckte Rechtecke.
    let best_closed = best_closed_shape(&pts);

    // 2. 3D-Kantenmodelle (Oktaeder / Doppelpyramide) nur, wenn keine geschlossene Form den
    //    Strich gut beschreibt: Ein Oktaeder enthält innere Kanten, die weit von jedem Umriss
    //    entfernt liegen. Ein sauber passender Umriss ist daher kein Oktaeder.
    let closed_fits_well = best_closed.as_ref().is_some_and(|(err, _)| {
        let aabb = Aabb::from_points(&pts);
        *err <= CLOSED_SHAPE_GOOD_FIT * (aabb.max - aabb.min).length()
    });
    let best_closed = best_closed.map(|(_, shape)| shape);

    // Kandidaten in Prioritätsreihenfolge; 3. offene Formen (Koordinatensystem, Gerade) zuletzt.
    // Übernommen wird der erste, der den Strich wirklich beschreibt. Passt keiner, bleibt der
    // Strich unverändert (vorher wurde fast jede geschlossene Kritzelei zu einer Form).
    let candidates: [&dyn Fn() -> Option<RecognizedShape>; 6] = [
        &|| {
            if closed_fits_well {
                None
            } else {
                detect_octahedron(&pts)
            }
        },
        &|| best_closed.clone(),
        &|| detect_polygon(&pts),
        &|| detect_coordinate_system(&pts),
        &|| detect_line(&pts),
        &|| None,
    ];
    candidates
        .iter()
        .filter_map(|candidate| candidate())
        .find(|shape| shape_describes_stroke(&pts, shape))
}

/// Größte zulässige mittlere Abweichung (RMS) des Strichs von der erkannten Form, als Anteil
/// der Strichdiagonale. Echte Formen liegen bei 0,5–3 %, Freiformen (Herz, Stern, Wolke,
/// Spirale) und flache Ellipsen bei 5–6,5 %.
const SHAPE_MAX_FIT_ERROR: f32 = 0.04;

/// Mittlere Abweichung (RMS) des Strichs von einer Form, relativ zur Strichdiagonale.
/// Gemessen wird gegen den Linienzug der Punkte, aus denen die Form gezeichnet wird; das
/// funktioniert für alle Formtypen gleich (auch Linien, Koordinatensysteme und Oktaeder).
pub fn shape_fit_error(points: &[Vec2], shape: &RecognizedShape) -> Option<f32> {
    if points.is_empty() {
        return None;
    }
    let (model, _) = shape_to_mesh(shape, 1.0, [0.0; 4]);
    if model.is_empty() {
        return None;
    }
    let aabb = Aabb::from_points(points);
    let diagonal = (aabb.max - aabb.min).length();
    if diagonal < 1e-3 {
        return None;
    }
    // Abstand zum Linienzug der Modellpunkte (nicht zu den Punkten selbst: manche Formen,
    // z. B. das Koordinatensystem, liefern nur ihre Eckpunkte)
    let model: Vec<Vec2> = model.iter().map(|m| m.position).collect();
    let sum_sq: f32 = points
        .iter()
        .map(|p| {
            let d = if model.len() == 1 {
                (model[0] - *p).length()
            } else {
                model
                    .windows(2)
                    .map(|w| segment_distance(*p, w[0], w[1]))
                    .fold(f32::INFINITY, f32::min)
            };
            d * d
        })
        .sum();
    Some((sum_sq / points.len() as f32).sqrt() / diagonal)
}

/// Beschreibt die Form den Strich gut genug, um ihn zu ersetzen?
fn shape_describes_stroke(points: &[Vec2], shape: &RecognizedShape) -> bool {
    shape_fit_error(points, shape).is_some_and(|err| err <= SHAPE_MAX_FIT_ERROR)
}

/// Größte zulässige Abweichung (RMS) eines Rechtecks vom Strich, als Anteil der Diagonale.
const RECTANGLE_MAX_RMS: f32 = 0.05;
/// Jede Rechteckecke muss bis auf diesen Anteil der kürzeren Seite gezeichnet sein.
const RECTANGLE_CORNER_REACH: f32 = 0.15;
/// Rechtecke, die höchstens so viele Grad schief sind, werden achsenparallel ausgerichtet.
const RECTANGLE_AXIS_SNAP_DEG: f32 = 10.0;

/// Anteil der Diagonale, bis zu dem die Abweichung einer geschlossenen Form als „passt gut“ gilt.
const CLOSED_SHAPE_GOOD_FIT: f32 = 0.04;
/// Gewichte der Abweichung im Formvergleich (kleiner = bevorzugt). Ein Rechteck gewinnt gegen
/// Raute und Vieleck bis zur 1,5-fachen Abweichung (sonst würden Quadrate oft zur Raute);
/// ein Vieleck gewinnt entsprechend gegen den Kreis.
const RECTANGLE_WEIGHT: f32 = 1.0 / 1.5;
const CIRCLE_WEIGHT: f32 = 1.5;

/// Wählt unter Kreis, Dreieck, Rechteck, Raute und Vieleck die Form, deren Kontur am besten passt,
/// und liefert sie mit ihrer Abweichung (RMS).
fn best_closed_shape(pts: &[Vec2]) -> Option<(f32, RecognizedShape)> {
    // Gewichtete Abweichung: Bei fast gleicher Passung gewinnt die wahrscheinlichere Absicht.
    // Rechte Winkel schlagen die freiere Raute bzw. ein freies Vieleck; ein Vieleck mit
    // nachgewiesenen Ecken schlägt den Kreis (ein Achteck ist fast ein Kreis).
    let fit = |shape: Option<RecognizedShape>, weight: f32| {
        shape.and_then(|s| contour_rms(pts, &s).map(|err| (err * weight, s)))
    };
    [
        fit(detect_circle(pts), CIRCLE_WEIGHT),
        fit(detect_triangle(pts), 1.0),
        fit(detect_rectangle(pts), RECTANGLE_WEIGHT),
        fit(detect_rhombus(pts), 1.0),
        fit(detect_polygon(pts), 1.0),
    ]
    .into_iter()
    .flatten()
    .min_by(|a, b| a.0.total_cmp(&b.0))
}

/// Abstand eines Punktes zur Strecke `a`–`b`.
fn segment_distance(p: Vec2, a: Vec2, b: Vec2) -> f32 {
    let ab = b - a;
    let len_sq = ab.length_squared();
    if len_sq < 1e-9 {
        return (p - a).length();
    }
    let t = ((p - a).dot(ab) / len_sq).clamp(0.0, 1.0);
    (p - (a + ab * t)).length()
}

/// Abstand eines Punktes zum Rand eines geschlossenen Polygons.
fn polygon_distance(p: Vec2, corners: &[Vec2]) -> f32 {
    (0..corners.len())
        .map(|i| segment_distance(p, corners[i], corners[(i + 1) % corners.len()]))
        .fold(f32::INFINITY, f32::min)
}

/// Mittlere quadratische Abweichung (RMS) der Strichpunkte von der Kontur einer erkannten
/// geschlossenen Form. Je kleiner, desto besser beschreibt die Form den Strich.
/// Für offene Formen (Linie, Koordinatensystem, Oktaeder) gibt es keinen Wert.
pub fn contour_rms(points: &[Vec2], shape: &RecognizedShape) -> Option<f32> {
    if points.is_empty() {
        return None;
    }
    let distance = |p: Vec2| -> Option<f32> {
        match shape {
            RecognizedShape::Circle { center, radius } => {
                Some(((p - *center).length() - radius).abs())
            }
            RecognizedShape::Triangle { corners } => Some(polygon_distance(p, corners)),
            RecognizedShape::Rectangle { corners } | RecognizedShape::Rhombus { corners } => {
                Some(polygon_distance(p, corners))
            }
            RecognizedShape::Polygon { vertices } => Some(polygon_distance(p, vertices)),
            _ => None,
        }
    };
    let mut sum_sq = 0.0;
    for &p in points {
        let d = distance(p)?;
        sum_sq += d * d;
    }
    Some((sum_sq / points.len() as f32).sqrt())
}

/// Erzeugt ein gleichmäßiges Triangle-Strip-Mesh für ein geschlossenes Polygon (Dreieck, Rechteck, Raute, N-Eck).
pub fn closed_polygon_to_mesh(
    corners: &[Vec2],
    half_w: f32,
    color: [f32; 4],
) -> (Vec<StylusSample>, Vec<InkVertex>) {
    let n = corners.len();
    if n < 3 {
        return (Vec::new(), Vec::new());
    }

    let mut vertices = Vec::with_capacity((n + 1) * 2);
    let mut samples = Vec::with_capacity(n * 8);

    for i in 0..n {
        let prev = corners[(i + n - 1) % n];
        let curr = corners[i];
        let next = corners[(i + 1) % n];

        let d_prev = (curr - prev).normalize_or_zero();
        let d_next = (next - curr).normalize_or_zero();

        let n_prev = Vec2::new(-d_prev.y, d_prev.x);
        let n_next = Vec2::new(-d_next.y, d_next.x);
        let corner_norm = (n_prev + n_next).normalize_or_zero() * half_w;

        vertices.push(InkVertex::new(
            curr.x + corner_norm.x,
            curr.y + corner_norm.y,
            color,
        ));
        vertices.push(InkVertex::new(
            curr.x - corner_norm.x,
            curr.y - corner_norm.y,
            color,
        ));

        let edge_len = (next - curr).length();
        let steps = ((edge_len / 10.0).ceil() as usize).max(2);
        for s in 0..steps {
            let t = s as f32 / steps as f32;
            samples.push(StylusSample {
                position: curr.lerp(next, t),
                pressure: 0.5,
                timestamp: 0.0,
            });
        }
    }

    if vertices.len() >= 2 {
        let v0 = vertices[0];
        let v1 = vertices[1];
        vertices.push(v0);
        vertices.push(v1);
    }

    (samples, vertices)
}

/// Berechnet die 12 Kanten der 3D-Schrägbild-Projektion eines Oktaeders (Doppelpyramide).
pub fn octahedron_edges(
    center: Vec2,
    radius_x: f32,
    radius_y: f32,
    depth: f32,
) -> Vec<(Vec2, Vec2)> {
    let top = center + Vec2::new(0.0, -radius_y);
    let bottom = center + Vec2::new(0.0, radius_y);
    let left = center + Vec2::new(-radius_x, 0.0);
    let right = center + Vec2::new(radius_x, 0.0);
    let front = center + Vec2::new(-0.2 * radius_x, depth);
    let back = center + Vec2::new(0.2 * radius_x, -depth);

    vec![
        // Äquator-Ring (4 Kanten)
        (left, front),
        (front, right),
        (right, back),
        (back, left),
        // Obere Pyramide (4 Kanten)
        (top, left),
        (top, front),
        (top, right),
        (top, back),
        // Untere Pyramide (4 Kanten)
        (bottom, left),
        (bottom, front),
        (bottom, right),
        (bottom, back),
    ]
}

/// Erzeugt dichte Abtastpunkte und ein GPU-kompatibles Triangle-Strip-Mesh für eine erkannte Form.
pub fn shape_to_mesh(
    shape: &RecognizedShape,
    width_units: f32,
    color: [f32; 4],
) -> (Vec<StylusSample>, Vec<InkVertex>) {
    let half_w = (width_units * 0.5).max(1.0);

    match shape {
        RecognizedShape::Line { start, end } => {
            let dir = (*end - *start).normalize_or_zero();
            let norm = Vec2::new(-dir.y, dir.x) * half_w;

            let vertices = vec![
                InkVertex::new(start.x + norm.x, start.y + norm.y, color),
                InkVertex::new(start.x - norm.x, start.y - norm.y, color),
                InkVertex::new(end.x + norm.x, end.y + norm.y, color),
                InkVertex::new(end.x - norm.x, end.y - norm.y, color),
            ];

            let len = (*end - *start).length();
            let steps = ((len / 5.0).ceil() as usize).max(2);
            let mut samples = Vec::with_capacity(steps);
            for i in 0..steps {
                let t = i as f32 / (steps - 1) as f32;
                samples.push(StylusSample {
                    position: start.lerp(*end, t),
                    pressure: 0.5,
                    timestamp: 0.0,
                });
            }

            (samples, vertices)
        }

        RecognizedShape::Circle { center, radius } => {
            let segments = 64;
            let r_in = (radius - half_w).max(0.1);
            let r_out = radius + half_w;

            let mut vertices = Vec::with_capacity((segments + 1) * 2);
            let mut samples = Vec::with_capacity(segments);

            for i in 0..=segments {
                let angle = (i as f32 / segments as f32) * 2.0 * PI;
                let (sin, cos) = angle.sin_cos();

                vertices.push(InkVertex::new(
                    center.x + cos * r_out,
                    center.y + sin * r_out,
                    color,
                ));
                vertices.push(InkVertex::new(
                    center.x + cos * r_in,
                    center.y + sin * r_in,
                    color,
                ));

                if i < segments {
                    samples.push(StylusSample {
                        position: *center + Vec2::new(cos * radius, sin * radius),
                        pressure: 0.5,
                        timestamp: 0.0,
                    });
                }
            }

            (samples, vertices)
        }

        RecognizedShape::Rectangle { corners } => closed_polygon_to_mesh(corners, half_w, color),

        RecognizedShape::Triangle { corners } => closed_polygon_to_mesh(corners, half_w, color),

        RecognizedShape::Rhombus { corners } => closed_polygon_to_mesh(corners, half_w, color),

        RecognizedShape::Polygon { vertices } => closed_polygon_to_mesh(vertices, half_w, color),

        RecognizedShape::Octahedron {
            center,
            radius_x,
            radius_y,
            depth,
        } => {
            let edges = octahedron_edges(*center, *radius_x, *radius_y, *depth);
            let mut vertices = Vec::new();
            let mut samples = Vec::new();

            for (i, &(p1, p2)) in edges.iter().enumerate() {
                let dir = (p2 - p1).normalize_or_zero();
                let norm = Vec2::new(-dir.y, dir.x) * half_w;

                let v0 = InkVertex::new(p1.x + norm.x, p1.y + norm.y, color);
                let v1 = InkVertex::new(p1.x - norm.x, p1.y - norm.y, color);
                let v2 = InkVertex::new(p2.x + norm.x, p2.y + norm.y, color);
                let v3 = InkVertex::new(p2.x - norm.x, p2.y - norm.y, color);

                if i > 0 {
                    let last = *vertices.last().unwrap();
                    vertices.push(last);
                    vertices.push(v0);
                }

                vertices.push(v0);
                vertices.push(v1);
                vertices.push(v2);
                vertices.push(v3);

                let len = (p2 - p1).length();
                let steps = ((len / 15.0).ceil() as usize).max(2);
                for s in 0..steps {
                    let t = s as f32 / steps as f32;
                    samples.push(StylusSample {
                        position: p1.lerp(p2, t),
                        pressure: 0.5,
                        timestamp: 0.0,
                    });
                }
            }

            (samples, vertices)
        }

        RecognizedShape::CoordinateSystem {
            origin,
            x_axis_end,
            y_axis_end,
            with_arrows,
        } => {
            let mut vertices = Vec::new();
            let mut samples = Vec::new();

            // 1. Y-Achse (von Pfeilspitze oben zum Ursprung)
            let y_dir = (*y_axis_end - *origin).normalize_or_zero();
            let y_norm = Vec2::new(-y_dir.y, y_dir.x) * half_w;

            // Y-Linienband
            vertices.push(InkVertex::new(
                origin.x + y_norm.x,
                origin.y + y_norm.y,
                color,
            ));
            vertices.push(InkVertex::new(
                origin.x - y_norm.x,
                origin.y - y_norm.y,
                color,
            ));
            vertices.push(InkVertex::new(
                y_axis_end.x + y_norm.x,
                y_axis_end.y + y_norm.y,
                color,
            ));
            vertices.push(InkVertex::new(
                y_axis_end.x - y_norm.x,
                y_axis_end.y - y_norm.y,
                color,
            ));

            // Degenerate Dreiecke für Übergang zur X-Achse
            let x_dir = (*x_axis_end - *origin).normalize_or_zero();
            let x_norm = Vec2::new(-x_dir.y, x_dir.x) * half_w;

            let last_v = *vertices.last().unwrap();
            let next_v = InkVertex::new(origin.x + x_norm.x, origin.y + x_norm.y, color);
            vertices.push(last_v);
            vertices.push(next_v);

            // 2. X-Linienband
            vertices.push(next_v);
            vertices.push(InkVertex::new(
                origin.x - x_norm.x,
                origin.y - x_norm.y,
                color,
            ));
            vertices.push(InkVertex::new(
                x_axis_end.x + x_norm.x,
                x_axis_end.y + x_norm.y,
                color,
            ));
            vertices.push(InkVertex::new(
                x_axis_end.x - x_norm.x,
                x_axis_end.y - x_norm.y,
                color,
            ));

            // 3. Optionale Pfeilspitzen
            if *with_arrows {
                let arrow_len = (half_w * 5.0).max(18.0);
                let arrow_w = (half_w * 3.5).max(12.0);

                // Pfeilspitze Y
                let y_base = *y_axis_end - y_dir * arrow_len;
                let y_left = y_base + Vec2::new(-y_dir.y, y_dir.x) * arrow_w;
                let y_right = y_base - Vec2::new(-y_dir.y, y_dir.x) * arrow_w;

                let last_v = *vertices.last().unwrap();
                let tip_v = InkVertex::new(y_axis_end.x, y_axis_end.y, color);
                vertices.push(last_v);
                vertices.push(tip_v);

                vertices.push(tip_v);
                vertices.push(InkVertex::new(y_left.x, y_left.y, color));
                vertices.push(InkVertex::new(y_right.x, y_right.y, color));

                // Pfeilspitze X
                let x_base = *x_axis_end - x_dir * arrow_len;
                let x_left = x_base + Vec2::new(-x_dir.y, x_dir.x) * arrow_w;
                let x_right = x_base - Vec2::new(-x_dir.y, x_dir.x) * arrow_w;

                let last_v = *vertices.last().unwrap();
                let tip_x = InkVertex::new(x_axis_end.x, x_axis_end.y, color);
                vertices.push(last_v);
                vertices.push(tip_x);

                vertices.push(tip_x);
                vertices.push(InkVertex::new(x_left.x, x_left.y, color));
                vertices.push(InkVertex::new(x_right.x, x_right.y, color));
            }

            // Abtastpunkte
            samples.push(StylusSample {
                position: *y_axis_end,
                pressure: 0.5,
                timestamp: 0.0,
            });
            samples.push(StylusSample {
                position: *origin,
                pressure: 0.5,
                timestamp: 0.0,
            });
            samples.push(StylusSample {
                position: *x_axis_end,
                pressure: 0.5,
                timestamp: 0.0,
            });

            (samples, vertices)
        }
    }
}

/// Erzeugt einen neuen `Stroke` aus einer erkannten Form.
pub fn shape_to_stroke(
    shape: RecognizedShape,
    stroke_id: u64,
    color: [f32; 4],
    width_units: f32,
) -> Stroke {
    let (samples, vertices) = shape_to_mesh(&shape, width_units, color);
    let mut stroke = Stroke::new(stroke_id, samples, vertices, color);
    stroke.shape = Some(shape);
    stroke.width = width_units;
    stroke.tool = crate::document::StrokeTool::Shape;
    stroke.finalized = true;
    stroke
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_line() {
        // Leicht zittrige diagonale Punktfolge
        let mut points = Vec::new();
        for i in 0..=50 {
            let t = i as f32 / 50.0;
            let jitter = (i as f32 * 0.7).sin() * 1.5;
            points.push(Vec2::new(
                100.0 + t * 400.0 + jitter,
                100.0 + t * 300.0 - jitter,
            ));
        }

        let detected = detect_line(&points);
        assert_eq!(
            kind(&detect_shape(&to_samples(&points))),
            kind(&detected),
            "Die vollständige Erkennung (detect_shape) muss dieselbe Form liefern"
        );
        assert!(detected.is_some(), "Gerade sollte erkannt werden");

        if let Some(RecognizedShape::Line { start, end }) = detected {
            assert!((start - points[0]).length() < 5.0);
            assert!((end - *points.last().unwrap()).length() < 10.0);
        } else {
            panic!("Falscher Formtyp erkannt");
        }
    }

    #[test]
    fn test_detect_rectangle() {
        // Handgezeichnete 4-Punkt-Schleife mit leichtem Jitter
        let mut points = Vec::new();
        // Kante 1: Oben links nach oben rechts
        for i in 0..20 {
            let t = i as f32 / 20.0;
            points.push(Vec2::new(100.0 + t * 200.0, 100.0 + (i as f32).sin() * 1.2));
        }
        // Kante 2: Oben rechts nach unten rechts
        for i in 0..20 {
            let t = i as f32 / 20.0;
            points.push(Vec2::new(300.0 + (i as f32).cos() * 1.2, 100.0 + t * 150.0));
        }
        // Kante 3: Unten rechts nach unten links
        for i in 0..20 {
            let t = i as f32 / 20.0;
            points.push(Vec2::new(300.0 - t * 200.0, 250.0 - (i as f32).sin() * 1.2));
        }
        // Kante 4: Unten links nach oben links (Schließen)
        for i in 0..20 {
            let t = i as f32 / 20.0;
            points.push(Vec2::new(100.0 - (i as f32).cos() * 1.2, 250.0 - t * 150.0));
        }
        points.push(points[0]);

        let detected = detect_rectangle(&points);
        assert_eq!(
            kind(&detect_shape(&to_samples(&points))),
            kind(&detected),
            "Die vollständige Erkennung (detect_shape) muss dieselbe Form liefern"
        );
        assert!(detected.is_some(), "Rechteck sollte erkannt werden");

        if let Some(RecognizedShape::Rectangle { corners }) = detected {
            // Alle 4 Winkel müssen exakt 90° sein
            for i in 0..4 {
                let prev = corners[(i + 3) % 4];
                let curr = corners[i];
                let next = corners[(i + 1) % 4];
                let v1 = (prev - curr).normalize();
                let v2 = (next - curr).normalize();
                let dot = v1.dot(v2).abs();
                assert!(
                    dot < 1e-4,
                    "Winkel an Ecke {} ist nicht rechtwinklig: dot={}",
                    i,
                    dot
                );
            }
        } else {
            panic!("Falscher Formtyp erkannt");
        }
    }

    #[test]
    fn test_detect_circle() {
        // Unrundes Oval / Kreis mit leicht variierendem Radius (50 bis 58)
        let mut points = Vec::new();
        let center = Vec2::new(200.0, 200.0);
        let n = 40;
        for i in 0..=n {
            let angle = (i as f32 / n as f32) * 2.0 * PI;
            let r = 54.0 + (angle * 3.0).sin() * 3.5;
            points.push(center + Vec2::new(angle.cos() * r, angle.sin() * r));
        }

        let detected = detect_circle(&points);
        assert_eq!(
            kind(&detect_shape(&to_samples(&points))),
            kind(&detected),
            "Die vollständige Erkennung (detect_shape) muss dieselbe Form liefern"
        );
        assert!(detected.is_some(), "Kreis sollte erkannt werden");

        if let Some(RecognizedShape::Circle {
            center: c,
            radius: r,
        }) = detected
        {
            assert!((c - center).length() < 5.0, "Zentrum weicht ab: {:?}", c);
            assert!((r - 54.0).abs() < 5.0, "Radius weicht ab: {}", r);
        } else {
            panic!("Falscher Formtyp erkannt");
        }
    }

    #[test]
    fn test_detect_coordinate_system() {
        // Zwei zusammenhängende Linien im 85°-Winkel
        let mut points = Vec::new();
        let origin = Vec2::new(100.0, 300.0);

        // Vertikaler Schenkel (Y-Achse von oben (100, 100) hinunter zum Ursprung (100, 300))
        for i in 0..=20 {
            let t = i as f32 / 20.0;
            points.push(Vec2::new(100.0, 100.0 + t * 200.0));
        }

        // Schenkel im 85°-Winkel zur Y-Achse (5° nach oben geneigt statt rein horizontal)
        let angle_rad = 5.0f32.to_radians();
        for i in 1..=20 {
            let t = i as f32 / 20.0;
            let len = t * 200.0;
            points.push(origin + Vec2::new(len * angle_rad.cos(), -len * angle_rad.sin()));
        }

        let detected = detect_coordinate_system(&points);
        assert_eq!(
            kind(&detect_shape(&to_samples(&points))),
            kind(&detected),
            "Die vollständige Erkennung (detect_shape) muss dieselbe Form liefern"
        );
        assert!(
            detected.is_some(),
            "Koordinatensystem sollte erkannt werden"
        );

        if let Some(RecognizedShape::CoordinateSystem {
            origin: o,
            x_axis_end,
            y_axis_end,
            ..
        }) = detected
        {
            assert!((o - origin).length() < 2.0, "Ursprung weicht ab");
            let v_x = (x_axis_end - o).normalize();
            let v_y = (y_axis_end - o).normalize();
            let dot = v_x.dot(v_y).abs();
            assert!(
                dot < 1e-4,
                "Achsen sind nach Orthogonalitäts-Snap nicht exakt 90°: dot={}",
                dot
            );
        } else {
            panic!("Falscher Formtyp erkannt");
        }
    }

    #[test]
    fn test_detect_triangle() {
        let mut points = Vec::new();
        let a = Vec2::new(100.0, 300.0);
        let b = Vec2::new(300.0, 300.0);
        let c = Vec2::new(200.0, 126.8); // ~Gleichseitiges Dreieck

        // Kante A -> B
        for i in 0..20 {
            let t = i as f32 / 20.0;
            points.push(a.lerp(b, t) + Vec2::new(0.0, (i as f32).sin() * 1.0));
        }
        // Kante B -> C
        for i in 0..20 {
            let t = i as f32 / 20.0;
            points.push(b.lerp(c, t) + Vec2::new((i as f32).cos() * 1.0, 0.0));
        }
        // Kante C -> A
        for i in 0..20 {
            let t = i as f32 / 20.0;
            points.push(c.lerp(a, t));
        }
        points.push(a);

        let detected = detect_triangle(&points);
        assert_eq!(
            kind(&detect_shape(&to_samples(&points))),
            kind(&detected),
            "Die vollständige Erkennung (detect_shape) muss dieselbe Form liefern"
        );
        assert!(detected.is_some(), "Dreieck sollte erkannt werden");

        if let Some(RecognizedShape::Triangle { corners }) = detected {
            assert_eq!(corners.len(), 3);
            let ab = (corners[1] - corners[0]).length();
            let bc = (corners[2] - corners[1]).length();
            let ca = (corners[0] - corners[2]).length();
            assert!(
                (ab - bc).abs() < 5.0,
                "Gleichseitiges Dreieck sollte gesnappt sein"
            );
            assert!((bc - ca).abs() < 5.0);
        } else {
            panic!("Falscher Formtyp erkannt");
        }
    }

    #[test]
    fn test_detect_rhombus() {
        let mut points = Vec::new();
        // Raute mit Kantenlängen ~200, nicht rechtwinklig (Winkel 60° und 120°)
        let c0 = Vec2::new(200.0, 100.0); // Oben
        let c1 = Vec2::new(320.0, 250.0); // Rechts
        let c2 = Vec2::new(200.0, 400.0); // Unten
        let c3 = Vec2::new(80.0, 250.0); // Links

        for i in 0..20 {
            let t = i as f32 / 20.0;
            points.push(c0.lerp(c1, t) + Vec2::new((i as f32).sin() * 1.0, 0.0));
        }
        for i in 0..20 {
            let t = i as f32 / 20.0;
            points.push(c1.lerp(c2, t));
        }
        for i in 0..20 {
            let t = i as f32 / 20.0;
            points.push(c2.lerp(c3, t) - Vec2::new((i as f32).cos() * 1.0, 0.0));
        }
        for i in 0..20 {
            let t = i as f32 / 20.0;
            points.push(c3.lerp(c0, t));
        }
        points.push(c0);

        let detected = detect_rhombus(&points);
        assert_eq!(
            kind(&detect_shape(&to_samples(&points))),
            kind(&detected),
            "Die vollständige Erkennung (detect_shape) muss dieselbe Form liefern"
        );
        assert!(detected.is_some(), "Raute sollte erkannt werden");

        if let Some(RecognizedShape::Rhombus { corners }) = detected {
            assert_eq!(corners.len(), 4);
            // Gegenüberliegende Kanten müssen parallel und gleich lang sein
            let l0 = (corners[1] - corners[0]).length();
            let l1 = (corners[2] - corners[1]).length();
            let l2 = (corners[3] - corners[2]).length();
            let l3 = (corners[0] - corners[3]).length();
            assert!(
                (l0 - l1).abs() < 1e-3,
                "Alle 4 Seiten einer Raute müssen exakt gleich lang sein"
            );
            assert!((l1 - l2).abs() < 1e-3);
            assert!((l2 - l3).abs() < 1e-3);
        } else {
            panic!("Falscher Formtyp erkannt");
        }
    }

    #[test]
    fn test_detect_polygon_octagon() {
        let mut points = Vec::new();
        let center = Vec2::new(200.0, 200.0);
        let r = 80.0;
        let n = 8;

        for i in 0..n {
            let a1 = i as f32 * (2.0 * PI / n as f32);
            let a2 = (i + 1) as f32 * (2.0 * PI / n as f32);
            let p1 = center + Vec2::new(a1.cos() * r, a1.sin() * r);
            let p2 = center + Vec2::new(a2.cos() * r, a2.sin() * r);

            for step in 0..10 {
                let t = step as f32 / 10.0;
                points.push(p1.lerp(p2, t) + Vec2::new((step as f32 * 0.5).sin() * 0.8, 0.0));
            }
        }
        points.push(points[0]);

        let detected = detect_polygon(&points);
        assert_eq!(
            kind(&detect_shape(&to_samples(&points))),
            kind(&detected),
            "Die vollständige Erkennung (detect_shape) muss dieselbe Form liefern"
        );
        assert!(
            detected.is_some(),
            "Oktagon / Achteck sollte erkannt werden"
        );

        if let Some(RecognizedShape::Polygon { vertices }) = detected {
            assert_eq!(vertices.len(), 8, "Achteck muss 8 Vertizes haben");
        } else {
            panic!("Falscher Formtyp erkannt");
        }
    }

    #[test]
    fn test_detect_octahedron() {
        let mut points = Vec::new();
        let top = Vec2::new(200.0, 50.0);
        let right = Vec2::new(350.0, 200.0);
        let bottom = Vec2::new(200.0, 350.0);
        let left = Vec2::new(50.0, 200.0);

        // Äußere Raute
        for i in 0..15 {
            points.push(top.lerp(right, i as f32 / 15.0));
        }
        for i in 0..15 {
            points.push(right.lerp(bottom, i as f32 / 15.0));
        }
        for i in 0..15 {
            points.push(bottom.lerp(left, i as f32 / 15.0));
        }
        for i in 0..15 {
            points.push(left.lerp(top, i as f32 / 15.0));
        }
        // Interne Äquatorlinie von Links nach Rechts durch das Zentrum
        for i in 0..20 {
            points.push(left.lerp(right, i as f32 / 20.0));
        }

        let detected = detect_octahedron(&points);
        assert_eq!(
            kind(&detect_shape(&to_samples(&points))),
            kind(&detected),
            "Die vollständige Erkennung (detect_shape) muss dieselbe Form liefern"
        );
        assert!(detected.is_some(), "Oktaeder sollte erkannt werden");

        if let Some(RecognizedShape::Octahedron {
            center,
            radius_x,
            radius_y,
            depth,
        }) = detected
        {
            assert!((center.x - 200.0).abs() < 5.0);
            assert!((center.y - 200.0).abs() < 5.0);
            assert!(radius_x > 100.0);
            assert!(radius_y > 100.0);
            assert!(depth > 10.0);
        } else {
            panic!("Falscher Formtyp erkannt");
        }
    }

    /// Bildet einen Freihandstrich entlang eines Umrisses nach: Punkte alle ~6 Einheiten
    /// (0,6 mm), seitliches Zittern um ±4 Einheiten und ein nicht ganz exakter Schluss.
    fn freehand(outline: &[Vec2], closed: bool, seed: f32) -> Vec<StylusSample> {
        let mut path = outline.to_vec();
        if closed {
            path.push(outline[0]);
        }
        let mut samples = Vec::new();
        let mut i = 0usize;
        for w in path.windows(2) {
            let (a, b) = (w[0], w[1]);
            let len = (b - a).length();
            let dir = (b - a).normalize_or_zero();
            let normal = Vec2::new(-dir.y, dir.x);
            let steps = (len / 6.0).ceil().max(1.0) as usize;
            for k in 0..steps {
                let t = k as f32 / steps as f32;
                let jitter =
                    (i as f32 * 0.9 + seed).sin() * 3.0 + (i as f32 * 2.3 + seed).cos() * 1.0;
                samples.push(StylusSample {
                    position: a + (b - a) * t + normal * jitter,
                    pressure: 0.5,
                    timestamp: i as f64 * 0.008,
                });
                i += 1;
            }
        }
        if closed {
            // Strich endet knapp vor dem Startpunkt (2 % des Umfangs)
            let keep = samples.len() - samples.len() / 50;
            samples.truncate(keep);
        }
        samples
    }

    fn rect_outline(center: Vec2, w: f32, h: f32, angle_deg: f32) -> Vec<Vec2> {
        let (s, c) = angle_deg.to_radians().sin_cos();
        let rot = |v: Vec2| center + Vec2::new(v.x * c - v.y * s, v.x * s + v.y * c);
        vec![
            rot(Vec2::new(-w / 2.0, -h / 2.0)),
            rot(Vec2::new(w / 2.0, -h / 2.0)),
            rot(Vec2::new(w / 2.0, h / 2.0)),
            rot(Vec2::new(-w / 2.0, h / 2.0)),
        ]
    }

    fn circle_outline(center: Vec2, rx: f32, ry: f32) -> Vec<Vec2> {
        (0..72)
            .map(|i| {
                let a = i as f32 / 72.0 * std::f32::consts::TAU;
                center + Vec2::new(a.cos() * rx, a.sin() * ry)
            })
            .collect()
    }

    fn kind(shape: &Option<RecognizedShape>) -> &'static str {
        match shape {
            Some(RecognizedShape::Line { .. }) => "Linie",
            Some(RecognizedShape::Circle { .. }) => "Kreis",
            Some(RecognizedShape::Rectangle { .. }) => "Rechteck",
            Some(RecognizedShape::Triangle { .. }) => "Dreieck",
            Some(RecognizedShape::Rhombus { .. }) => "Raute",
            Some(RecognizedShape::Polygon { .. }) => "Polygon",
            Some(RecognizedShape::CoordinateSystem { .. }) => "Koordinatensystem",
            Some(RecognizedShape::Octahedron { .. }) => "Oktaeder",
            None => "nichts",
        }
    }

    #[test]
    fn test_detect_shape_freehand_classification() {
        let c = Vec2::new(1000.0, 1400.0);
        let cases: Vec<(&str, Vec<StylusSample>, &str)> = vec![
            (
                "Quadrat 40 mm",
                freehand(&rect_outline(c, 400.0, 400.0, 0.0), true, 0.3),
                "Rechteck",
            ),
            (
                "Quadrat 15 mm",
                freehand(&rect_outline(c, 150.0, 150.0, 0.0), true, 1.1),
                "Rechteck",
            ),
            (
                "Quadrat 60 mm",
                freehand(&rect_outline(c, 600.0, 600.0, 0.0), true, 2.0),
                "Rechteck",
            ),
            (
                "Rechteck 60×30 mm",
                freehand(&rect_outline(c, 600.0, 300.0, 0.0), true, 0.7),
                "Rechteck",
            ),
            (
                "Rechteck 40×30 mm",
                freehand(&rect_outline(c, 400.0, 300.0, 0.0), true, 1.7),
                "Rechteck",
            ),
            (
                "Quadrat gedreht 20°",
                freehand(&rect_outline(c, 400.0, 400.0, 20.0), true, 0.5),
                "Rechteck",
            ),
            (
                "Kreis r = 20 mm",
                freehand(&circle_outline(c, 200.0, 200.0), true, 0.9),
                "Kreis",
            ),
            (
                "Kreis r = 8 mm",
                freehand(&circle_outline(c, 80.0, 80.0), true, 1.3),
                "Kreis",
            ),
            (
                "Kreis r = 35 mm",
                freehand(&circle_outline(c, 350.0, 350.0), true, 2.4),
                "Kreis",
            ),
            (
                "Dreieck",
                freehand(
                    &[
                        c + Vec2::new(0.0, -250.0),
                        c + Vec2::new(230.0, 170.0),
                        c + Vec2::new(-230.0, 170.0),
                    ],
                    true,
                    0.4,
                ),
                "Dreieck",
            ),
            (
                "Linie",
                freehand(&[c, c + Vec2::new(500.0, 120.0)], false, 0.2),
                "Linie",
            ),
        ];
        let mut wrong = Vec::new();
        for (name, samples, expected) in &cases {
            let got = kind(&detect_shape(samples));
            if got != *expected {
                wrong.push(format!("{name}: erwartet {expected}, erkannt {got}"));
            }
        }
        assert!(wrong.is_empty(), "Falsch erkannt:\n{}", wrong.join("\n"));
    }

    /// Erkennt Rechtecke, Kreise, Dreiecke und Vielecke in vielen Größen, Seitenverhältnissen,
    /// Drehungen und Zittermustern; Freiformen bleiben unverändert, flache Ellipsen werden nie
    /// zu Rechtecken.
    #[test]
    fn test_detect_shape_robustness() {
        let c = Vec2::new(1000.0, 1400.0);
        let mut stats: std::collections::BTreeMap<String, std::collections::BTreeMap<&str, usize>> =
            Default::default();
        let mut add = |group: String, got: &'static str| {
            *stats.entry(group).or_default().entry(got).or_default() += 1;
        };
        let seeds = [0.1_f32, 0.6, 1.2, 1.9, 2.7, 3.4, 4.2, 5.0];
        for &size in &[150.0_f32, 250.0, 400.0, 600.0] {
            for &aspect in &[1.0_f32, 0.75, 0.5] {
                for &angle in &[0.0_f32, 15.0, 30.0, 45.0] {
                    for &seed in &seeds {
                        let pts =
                            freehand(&rect_outline(c, size, size * aspect, angle), true, seed);
                        add(format!("Rechteck {aspect}"), kind(&detect_shape(&pts)));
                    }
                }
            }
        }
        for &r in &[60.0_f32, 120.0, 200.0, 350.0] {
            for &ry in &[1.0_f32, 0.8, 0.6] {
                for &seed in &seeds {
                    let pts = freehand(&circle_outline(c, r, r * ry), true, seed);
                    add(format!("Kreis/Ellipse {ry}"), kind(&detect_shape(&pts)));
                }
            }
        }
        for &size in &[150.0_f32, 300.0, 500.0] {
            for &seed in &seeds {
                let tri = [
                    c + Vec2::new(0.0, -size),
                    c + Vec2::new(size * 0.9, size * 0.7),
                    c + Vec2::new(-size * 0.9, size * 0.7),
                ];
                add(
                    "Dreieck".to_string(),
                    kind(&detect_shape(&freehand(&tri, true, seed))),
                );
            }
        }
        // Realistischere Rechtecke: abgerundete Ecken, schiefe Kanten, Überstrich am Schluss
        for &size in &[150.0_f32, 300.0, 500.0] {
            for &aspect in &[1.0_f32, 0.6] {
                for &seed in &seeds {
                    let (w, h) = (size, size * aspect);
                    // Abgerundete Ecken (Radius 10 % der kürzeren Seite)
                    let r = 0.1 * w.min(h);
                    let mut rounded = Vec::new();
                    for (cx, cy, a0) in [
                        (w / 2.0 - r, -h / 2.0 + r, -90.0_f32),
                        (w / 2.0 - r, h / 2.0 - r, 0.0),
                        (-w / 2.0 + r, h / 2.0 - r, 90.0),
                        (-w / 2.0 + r, -h / 2.0 + r, 180.0),
                    ] {
                        for k in 0..=6 {
                            let a = (a0 + k as f32 * 15.0).to_radians();
                            rounded.push(c + Vec2::new(cx + a.cos() * r, cy + a.sin() * r));
                        }
                    }
                    add(
                        format!("Rechteck rund {aspect}"),
                        kind(&detect_shape(&freehand(&rounded, true, seed))),
                    );
                    // Schiefe Kanten: Ecken um bis zu 6 % verschoben
                    let mut skew = rect_outline(c, w, h, 0.0);
                    for (i, p) in skew.iter_mut().enumerate() {
                        let f = 0.06 * w.min(h);
                        *p += Vec2::new(
                            ((i as f32 + seed) * 2.1).sin() * f,
                            ((i as f32 + seed) * 1.3).cos() * f,
                        );
                    }
                    add(
                        format!("Rechteck schief {aspect}"),
                        kind(&detect_shape(&freehand(&skew, true, seed))),
                    );
                    // Überstrich: Strich läuft 8 % des Umfangs über den Start hinaus
                    let mut over = freehand(&rect_outline(c, w, h, 0.0), true, seed);
                    let extra: Vec<StylusSample> =
                        over.iter().take(over.len() * 8 / 100).copied().collect();
                    over.extend(extra);
                    add(
                        format!("Rechteck Überstrich {aspect}"),
                        kind(&detect_shape(&over)),
                    );
                }
            }
        }
        // Regelmäßige Vielecke (5, 6, 8 Ecken) und Ovale (0,6) – Ovale dürfen keine Vielecke werden
        for &n in &[5usize, 6, 8] {
            for &size in &[200.0_f32, 400.0] {
                for &seed in &seeds {
                    let outline: Vec<Vec2> = (0..n)
                        .map(|i| {
                            let a = i as f32 / n as f32 * std::f32::consts::TAU
                                - std::f32::consts::FRAC_PI_2;
                            c + Vec2::new(a.cos(), a.sin()) * size
                        })
                        .collect();
                    add(
                        format!("Vieleck {n}"),
                        kind(&detect_shape(&freehand(&outline, true, seed))),
                    );
                }
            }
        }
        for &r in &[120.0_f32, 250.0, 400.0] {
            for &seed in &seeds {
                let pts = freehand(&circle_outline(c, r, r * 0.6), true, seed);
                add("Oval 0.6".to_string(), kind(&detect_shape(&pts)));
            }
        }
        // Freiformen, die KEIN Rechteck/Kreis werden sollen
        for &seed in &seeds {
            let star: Vec<Vec2> = (0..10)
                .map(|i| {
                    let a = i as f32 / 10.0 * std::f32::consts::TAU;
                    let r = if i % 2 == 0 { 300.0 } else { 120.0 };
                    c + Vec2::new(a.cos() * r, a.sin() * r)
                })
                .collect();
            add(
                "Freiform Stern".into(),
                kind(&detect_shape(&freehand(&star, true, seed))),
            );
            let cloud: Vec<Vec2> = (0..72)
                .map(|i| {
                    let a = i as f32 / 72.0 * std::f32::consts::TAU;
                    let r = 250.0 * (1.0 + 0.3 * (3.0 * a).sin());
                    c + Vec2::new(a.cos() * r, a.sin() * r)
                })
                .collect();
            add(
                "Freiform Wolke".into(),
                kind(&detect_shape(&freehand(&cloud, true, seed))),
            );
            let heart: Vec<Vec2> = (0..72)
                .map(|i| {
                    let t = i as f32 / 72.0 * std::f32::consts::TAU;
                    let x = 16.0 * t.sin().powi(3);
                    let y = -(13.0 * t.cos()
                        - 5.0 * (2.0 * t).cos()
                        - 2.0 * (3.0 * t).cos()
                        - (4.0 * t).cos());
                    c + Vec2::new(x, y) * 18.0
                })
                .collect();
            add(
                "Freiform Herz".into(),
                kind(&detect_shape(&freehand(&heart, true, seed))),
            );
            let spiral: Vec<Vec2> = (0..120)
                .map(|i| {
                    let a = i as f32 / 30.0 * std::f32::consts::TAU;
                    let r = 40.0 + i as f32 * 2.5;
                    c + Vec2::new(a.cos() * r, a.sin() * r)
                })
                .collect();
            add(
                "Freiform Spirale".into(),
                kind(&detect_shape(&freehand(&spiral, false, seed))),
            );
        }
        let count = |group: &str, kind: &str| -> (usize, usize) {
            let m = stats
                .get(group)
                .unwrap_or_else(|| panic!("Gruppe {group} fehlt"));
            (m.get(kind).copied().unwrap_or(0), m.values().sum())
        };
        let mut problems = Vec::new();
        for group in stats.keys() {
            let expected = if group.starts_with("Rechteck") {
                Some("Rechteck")
            } else if group == "Kreis/Ellipse 1" || group == "Kreis/Ellipse 0.8" {
                Some("Kreis")
            } else if group == "Dreieck" {
                Some("Dreieck")
            } else if group.starts_with("Vieleck") {
                Some("Polygon")
            } else if group.starts_with("Freiform") {
                // Freiformen bleiben unverändert
                Some("nichts")
            } else {
                None
            };
            if let Some(kind) = expected {
                let (hits, total) = count(group, kind);
                // Achteck/Sechseck und leicht ovale Kreise sind von Hand kaum zu unterscheiden
                let min_share = if group.starts_with("Vieleck") || group == "Kreis/Ellipse 0.8" {
                    0.9
                } else {
                    1.0
                };
                if (hits as f32) < min_share * total as f32 {
                    problems.push(format!(
                        "{group}: {hits}/{total} als {kind} erkannt ({:?})",
                        stats[group]
                    ));
                }
            } else {
                // Flache Ellipsen und Freiformen: niemals Rechteck
                let (rects, _) = count(group, "Rechteck");
                if rects > 0 {
                    problems.push(format!(
                        "{group}: {rects}× fälschlich Rechteck ({:?})",
                        stats[group]
                    ));
                }
            }
        }
        assert!(problems.is_empty(), "{}", problems.join("\n"));
    }

    fn to_samples(points: &[Vec2]) -> Vec<StylusSample> {
        points
            .iter()
            .enumerate()
            .map(|(i, p)| StylusSample {
                position: *p,
                pressure: 0.5,
                timestamp: i as f64 * 0.008,
            })
            .collect()
    }
}
