//! Geometrie- und Objekterkennung (Shape Recognition) für Freihandstriche.
//!
//! Erkennt Geraden, Rechtecke, Kreise und Koordinatensysteme (Achsen)
//! und erzeugt saubere Vektor-Primitiven und GPU-Triangle-Meshes.

use std::f32::consts::PI;
use glam::Vec2;
use serde::{Deserialize, Serialize};

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
            Self::CoordinateSystem { origin, x_axis_end, y_axis_end, .. } => {
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
            Self::CoordinateSystem { origin: o, x_axis_end, y_axis_end, .. } => {
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
        let sc = |pt: Vec2| -> Vec2 {
            origin + (pt - origin) * factors
        };

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
            Self::CoordinateSystem { origin: o, x_axis_end, y_axis_end, .. } => {
                *o = sc(*o);
                *x_axis_end = sc(*x_axis_end);
                *y_axis_end = sc(*y_axis_end);
            }
            Self::Polygon { vertices } => {
                for v in vertices {
                    *v = sc(*v);
                }
            }
            Self::Octahedron { center, radius_x, radius_y, depth } => {
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

/// 3. Rechteckerkennung: Geschlossener Pfad mit 4 dominanten Ecken (~90° +/- 18°).
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

    // RDP mit ca. 5% Pfadlänge als Toleranz
    let epsilon = (total_len * 0.05).clamp(8.0, 45.0);
    let simplified = ramer_douglas_peucker(points, epsilon);

    let corners: Vec<Vec2> = if simplified.len() == 5
        && (simplified[0] - simplified[4]).length() < epsilon * 2.5
    {
        simplified[0..4].to_vec()
    } else if simplified.len() == 4 {
        simplified
    } else if simplified.len() > 4 {
        // Suche die 4 markantesten Ecken (stärkste Richtungsänderung)
        extract_k_corners(&simplified, 4)?
    } else {
        return None;
    };

    if corners.len() != 4 {
        return None;
    }

    // Innenwinkel prüfen: 4 Kanten müssen jeweils ~90° (+/- 18°) zueinander stehen
    let c = [corners[0], corners[1], corners[2], corners[3]];
    for i in 0..4 {
        let prev = c[(i + 3) % 4];
        let curr = c[i];
        let next = c[(i + 1) % 4];
        let v1 = (prev - curr).normalize_or_zero();
        let v2 = (next - curr).normalize_or_zero();
        let dot = v1.dot(v2).clamp(-1.0, 1.0);
        let angle_deg = dot.acos().to_degrees();
        if !(72.0..=108.0).contains(&angle_deg) {
            return None;
        }
    }

    // Saubere rechtwinklige Ausrichtung
    let aabb = Aabb::from_points(&c);
    let center = (c[0] + c[1] + c[2] + c[3]) * 0.25;

    let e0 = c[1] - c[0];
    let angle_deg = e0.y.atan2(e0.x).to_degrees().abs();

    // Nahe an Achsenparallelität (< 12°)?
    let snapped_corners = if angle_deg < 12.0
        || (angle_deg - 90.0).abs() < 12.0
        || (angle_deg - 180.0).abs() < 12.0
    {
        [
            Vec2::new(aabb.min.x, aabb.min.y),
            Vec2::new(aabb.max.x, aabb.min.y),
            Vec2::new(aabb.max.x, aabb.max.y),
            Vec2::new(aabb.min.x, aabb.max.y),
        ]
    } else {
        // Rotiertes Rechteck mit exakt 90° Kanten
        let dir = e0.normalize_or_zero();
        let norm = Vec2::new(-dir.y, dir.x);
        let w = 0.5 * ((c[1] - c[0]).length() + (c[2] - c[3]).length());
        let h = 0.5 * ((c[2] - c[1]).length() + (c[3] - c[0]).length());
        let hw = w * 0.5;
        let hh = h * 0.5;
        [
            center - dir * hw - norm * hh,
            center + dir * hw - norm * hh,
            center + dir * hw + norm * hh,
            center - dir * hw + norm * hh,
        ]
    };

    Some(RecognizedShape::Rectangle {
        corners: snapped_corners,
    })
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
        // Dot product: je kleiner, desto spitzer/schärfer die Richtungsänderung
        let dot = v1.dot(v2).clamp(-1.0, 1.0);
        scored.push((i, dot));
    }

    scored.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
    let mut selected_indices: Vec<usize> = scored.iter().take(k).map(|&(idx, _)| idx).collect();
    selected_indices.sort_unstable();

    Some(selected_indices.into_iter().map(|idx| points[idx]).collect())
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

    let corners: Vec<Vec2> = if (simplified.len() == 4 && (simplified[0] - simplified[3]).length() < epsilon * 2.5)
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
        let c1 = center + Vec2::new((snapped_angle + 2.0 * PI / 3.0).cos(), (snapped_angle + 2.0 * PI / 3.0).sin()) * r;
        let c2 = center + Vec2::new((snapped_angle + 4.0 * PI / 3.0).cos(), (snapped_angle + 4.0 * PI / 3.0).sin()) * r;
        return Some(RecognizedShape::Triangle { corners: [c0, c1, c2] });
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
        let sign = if (v1.x * v2.y - v1.y * v2.x) >= 0.0 { 1.0 } else { -1.0 };
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
        return Some(RecognizedShape::Triangle { corners: [new_a, b, c] });
    } else if (l_bc - l_ca).abs() / l_bc.max(l_ca) < 0.12 {
        let mid_ab = (a + b) * 0.5;
        let axis = (c - mid_ab).normalize_or_zero();
        let perp = Vec2::new(-axis.y, axis.x);
        let half_base = ((a - mid_ab).length() + (b - mid_ab).length()) * 0.5;
        let new_a = mid_ab + perp * half_base;
        b = mid_ab - perp * half_base;
        return Some(RecognizedShape::Triangle { corners: [new_a, b, c] });
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

    let corners: Vec<Vec2> = if simplified.len() == 5 && (simplified[0] - simplified[4]).length() < epsilon * 2.5 {
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

    let sign = if (dir1.x * d2.y - dir1.y * d2.x) >= 0.0 { 1.0 } else { -1.0 };
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
    let center = corners.iter().copied().sum::<Vec2>() / (n as f32);

    let radii: Vec<f32> = corners.iter().map(|&pt| (pt - center).length()).collect();
    let mean_r = radii.iter().sum::<f32>() / (n as f32);
    if mean_r < 15.0 {
        return None;
    }

    let max_r_diff = radii.iter().map(|r| (r - mean_r).abs()).fold(0.0f32, f32::max);

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

fn ramer_douucker_closed(points: &[Vec2], epsilon: f32) -> Vec<Vec2> {
    let simplified = ramer_douglas_peucker(points, epsilon);
    if simplified.len() >= 2 && (simplified[0] - *simplified.last().unwrap()).length() < epsilon * 2.0 {
        simplified[0..simplified.len() - 1].to_vec()
    } else {
        simplified
    }
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

    // 1. Zuerst 3D-Kantenmodelle prüfen (Oktaeder / Doppelpyramide)
    if let Some(octa) = detect_octahedron(&pts) {
        return Some(octa);
    }

    // 2. Geschlossene Formen prüfen (Kreis, Dreieck, Rechteck, Raute, Polygon/Oktagon)
    if let Some(circle) = detect_circle(&pts) {
        return Some(circle);
    }
    if let Some(triangle) = detect_triangle(&pts) {
        return Some(triangle);
    }
    if let Some(rect) = detect_rectangle(&pts) {
        return Some(rect);
    }
    if let Some(rhombus) = detect_rhombus(&pts) {
        return Some(rhombus);
    }
    if let Some(polygon) = detect_polygon(&pts) {
        return Some(polygon);
    }

    // 3. Offene Formen: Koordinatensystem oder Gerade
    if let Some(coord) = detect_coordinate_system(&pts) {
        return Some(coord);
    }
    if let Some(line) = detect_line(&pts) {
        return Some(line);
    }

    None
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

        vertices.push(InkVertex::new(curr.x + corner_norm.x, curr.y + corner_norm.y, color));
        vertices.push(InkVertex::new(curr.x - corner_norm.x, curr.y - corner_norm.y, color));

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
pub fn octahedron_edges(center: Vec2, radius_x: f32, radius_y: f32, depth: f32) -> Vec<(Vec2, Vec2)> {
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

                vertices.push(InkVertex::new(center.x + cos * r_out, center.y + sin * r_out, color));
                vertices.push(InkVertex::new(center.x + cos * r_in, center.y + sin * r_in, color));

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

        RecognizedShape::Rectangle { corners } => {
            closed_polygon_to_mesh(corners, half_w, color)
        }

        RecognizedShape::Triangle { corners } => {
            closed_polygon_to_mesh(corners, half_w, color)
        }

        RecognizedShape::Rhombus { corners } => {
            closed_polygon_to_mesh(corners, half_w, color)
        }

        RecognizedShape::Polygon { vertices } => {
            closed_polygon_to_mesh(vertices, half_w, color)
        }

        RecognizedShape::Octahedron { center, radius_x, radius_y, depth } => {
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
            vertices.push(InkVertex::new(origin.x + y_norm.x, origin.y + y_norm.y, color));
            vertices.push(InkVertex::new(origin.x - y_norm.x, origin.y - y_norm.y, color));
            vertices.push(InkVertex::new(y_axis_end.x + y_norm.x, y_axis_end.y + y_norm.y, color));
            vertices.push(InkVertex::new(y_axis_end.x - y_norm.x, y_axis_end.y - y_norm.y, color));

            // Degenerate Dreiecke für Übergang zur X-Achse
            let x_dir = (*x_axis_end - *origin).normalize_or_zero();
            let x_norm = Vec2::new(-x_dir.y, x_dir.x) * half_w;

            let last_v = *vertices.last().unwrap();
            let next_v = InkVertex::new(origin.x + x_norm.x, origin.y + x_norm.y, color);
            vertices.push(last_v);
            vertices.push(next_v);

            // 2. X-Linienband
            vertices.push(next_v);
            vertices.push(InkVertex::new(origin.x - x_norm.x, origin.y - x_norm.y, color));
            vertices.push(InkVertex::new(x_axis_end.x + x_norm.x, x_axis_end.y + x_norm.y, color));
            vertices.push(InkVertex::new(x_axis_end.x - x_norm.x, x_axis_end.y - x_norm.y, color));

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
            points.push(Vec2::new(100.0 + t * 400.0 + jitter, 100.0 + t * 300.0 - jitter));
        }

        let detected = detect_line(&points);
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
                assert!(dot < 1e-4, "Winkel an Ecke {} ist nicht rechtwinklig: dot={}", i, dot);
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
        assert!(detected.is_some(), "Kreis sollte erkannt werden");

        if let Some(RecognizedShape::Circle { center: c, radius: r }) = detected {
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
        assert!(detected.is_some(), "Koordinatensystem sollte erkannt werden");

        if let Some(RecognizedShape::CoordinateSystem { origin: o, x_axis_end, y_axis_end, .. }) = detected {
            assert!((o - origin).length() < 2.0, "Ursprung weicht ab");
            let v_x = (x_axis_end - o).normalize();
            let v_y = (y_axis_end - o).normalize();
            let dot = v_x.dot(v_y).abs();
            assert!(dot < 1e-4, "Achsen sind nach Orthogonalitäts-Snap nicht exakt 90°: dot={}", dot);
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
        assert!(detected.is_some(), "Dreieck sollte erkannt werden");

        if let Some(RecognizedShape::Triangle { corners }) = detected {
            assert_eq!(corners.len(), 3);
            let ab = (corners[1] - corners[0]).length();
            let bc = (corners[2] - corners[1]).length();
            let ca = (corners[0] - corners[2]).length();
            assert!((ab - bc).abs() < 5.0, "Gleichseitiges Dreieck sollte gesnappt sein");
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
        let c3 = Vec2::new(80.0, 250.0);  // Links

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
        assert!(detected.is_some(), "Raute sollte erkannt werden");

        if let Some(RecognizedShape::Rhombus { corners }) = detected {
            assert_eq!(corners.len(), 4);
            // Gegenüberliegende Kanten müssen parallel und gleich lang sein
            let l0 = (corners[1] - corners[0]).length();
            let l1 = (corners[2] - corners[1]).length();
            let l2 = (corners[3] - corners[2]).length();
            let l3 = (corners[0] - corners[3]).length();
            assert!((l0 - l1).abs() < 1e-3, "Alle 4 Seiten einer Raute müssen exakt gleich lang sein");
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
        assert!(detected.is_some(), "Oktagon / Achteck sollte erkannt werden");

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
        assert!(detected.is_some(), "Oktaeder sollte erkannt werden");

        if let Some(RecognizedShape::Octahedron { center, radius_x, radius_y, depth }) = detected {
            assert!((center.x - 200.0).abs() < 5.0);
            assert!((center.y - 200.0).abs() < 5.0);
            assert!(radius_x > 100.0);
            assert!(radius_y > 100.0);
            assert!(depth > 10.0);
        } else {
            panic!("Falscher Formtyp erkannt");
        }
    }
}
