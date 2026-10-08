//! Zentripetale Catmull-Rom-Spline-Interpolation (alpha = 0.5) über De-Boor / Barry-Goldman.
//!
//! Berechnet für jeden Schritt entlang der Kurve:
//! - Position C(t)
//! - Tangente T(t) (analytische Ableitung)
//! - Normale N(t) (senkrecht auf Tangente)
//! - Druckabhängigen Radius r(t)
//! - Linke und rechte Hüllpunkte
//!
//! Generiert daraus Triangle-Strips für das GPU-Rendering mit `wgpu`.

use crate::canvas::InkVertex;
use crate::document::{point_to_segment_distance, Stroke, StylusSample};

/// Standardtoleranz für Ramer-Douglas-Peucker (ca. 0.075 mm in DIN-A4-Einheiten).
pub const RDP_TOLERANCE_UNITS: f32 = 0.75;

/// Schwellenwert für Ecken-Erkennung (Richtungsänderung > 65 Grad).
pub const CORNER_ANGLE_THRESHOLD_DEG: f32 = 65.0;
/// Ecken nur zwischen Abschnitten, die nach der Ausdünnung mindestens so lang sind (0,6 mm).
/// Kleine Schleifen („o“, „a“, „e“) bestehen aus kurzen Abschnitten mit großem Knickwinkel;
/// würden sie als Ecken getrennt, würden sie zu spitzen Zacken statt Rundungen.
pub const CORNER_MIN_SEGMENT_UNITS: f32 = 6.0;

/// GPU-Vertex für Triangle-Strips (Memory-Layout kompatibel mit GPU-Buffern).
#[repr(C)]
#[derive(Copy, Clone, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Vertex {
    pub position: [f32; 2],
    pub pressure: f32,
    pub side: f32, // -1.0 für linken Hüllpunkt, +1.0 für rechten Hüllpunkt
}

impl Vertex {
    pub fn new(x: f32, y: f32, pressure: f32, side: f32) -> Self {
        Self {
            position: [x, y],
            pressure,
            side,
        }
    }

    pub fn from_vec2(pos: glam::Vec2, pressure: f32, side: f32) -> Self {
        Self {
            position: [pos.x, pos.y],
            pressure,
            side,
        }
    }

    pub fn pos(&self) -> glam::Vec2 {
        glam::Vec2::new(self.position[0], self.position[1])
    }
}

/// Kontrollpunkt für die Spline-Interpolation mit Position und Druck p ∈ [0.0, 1.0].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ControlPoint {
    pub position: glam::Vec2,
    pub pressure: f32,
}

impl ControlPoint {
    pub fn new(x: f32, y: f32, pressure: f32) -> Self {
        Self {
            position: glam::Vec2::new(x, y),
            pressure: pressure.clamp(0.0, 1.0),
        }
    }

    pub fn from_vec2(position: glam::Vec2, pressure: f32) -> Self {
        Self {
            position,
            pressure: pressure.clamp(0.0, 1.0),
        }
    }
}

/// Ein berechneter Abtastschritt entlang des Splines mit Tangente, Normale und Hüllpunkten.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SplineSample {
    pub position: glam::Vec2,
    pub tangent: glam::Vec2,
    pub normal: glam::Vec2,
    pub pressure: f32,
    pub radius: f32,
    pub left: glam::Vec2,
    pub right: glam::Vec2,
}

/// Konfiguration für die Spline-Interpolation und das Meshing.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SplineConfig {
    /// Minimaler Pinselradius bei Druck p = 0.0
    pub min_radius: f32,
    /// Maximaler Pinselradius bei Druck p = 1.0
    pub max_radius: f32,
    /// Anzahl an Diskretisierungsschritten pro Spline-Segment
    pub samples_per_segment: usize,
}

impl Default for SplineConfig {
    fn default() -> Self {
        Self {
            min_radius: 1.5,
            max_radius: 4.5,
            samples_per_segment: 16,
        }
    }
}

impl SplineConfig {
    /// Erzeugt eine Spline-Konfiguration aus der Strichbreite in internen Einheiten (10 Einheiten = 1 mm).
    pub fn from_width_units(width_units: f32) -> Self {
        let base_radius = (width_units / 2.0).max(0.25);
        Self {
            min_radius: (base_radius * 0.5).max(0.15),
            max_radius: (base_radius * 1.5).max(0.35),
            samples_per_segment: 16,
        }
    }
}

/// Berechnet den druckabhängigen Radius r(p) = min_r + p * (max_r - min_r).
pub fn calculate_radius(pressure: f32, min_radius: f32, max_radius: f32) -> f32 {
    let p = pressure.clamp(0.0, 1.0);
    min_radius + p * (max_radius - min_radius)
}

/// Zentripetale Catmull-Rom-Interpolation (alpha = 0.5) über De-Boor / Barry-Goldman.
///
/// Berechnet Position, Tangente, Normale, Radius und linke/rechte Hüllpunkte
/// für den Parameter `u` ∈ [0.0, 1.0] im Segment zwischen `p1` und `p2`.
pub fn evaluate_catmull_rom_segment(
    p0: ControlPoint,
    p1: ControlPoint,
    p2: ControlPoint,
    p3: ControlPoint,
    u: f32,
    config: &SplineConfig,
) -> SplineSample {
    let u = u.clamp(0.0, 1.0);

    // Knotensequenz mit alpha = 0.5 (zentripetal: sqrt(Distanz))
    let d01 = (p1.position - p0.position).length();
    let d12 = (p2.position - p1.position).length();
    let d23 = (p3.position - p2.position).length();

    let dt01 = if d01 > 1e-6 { d01.sqrt() } else { 1e-4 };
    let dt12 = if d12 > 1e-6 { d12.sqrt() } else { 1e-4 };
    let dt23 = if d23 > 1e-6 { d23.sqrt() } else { 1e-4 };

    let t0 = 0.0_f32;
    let t1 = t0 + dt01;
    let t2 = t1 + dt12;
    let t3 = t2 + dt23;

    let t = t1 + u * (t2 - t1);

    // --- De-Boor / Barry-Goldman Pyramide ---

    // Ebene 1:
    let a1 = ((t1 - t) * p0.position + (t - t0) * p1.position) / (t1 - t0);
    let a2 = ((t2 - t) * p1.position + (t - t1) * p2.position) / (t2 - t1);
    let a3 = ((t3 - t) * p2.position + (t - t2) * p3.position) / (t3 - t2);

    let pa1 = ((t1 - t) * p0.pressure + (t - t0) * p1.pressure) / (t1 - t0);
    let pa2 = ((t2 - t) * p1.pressure + (t - t1) * p2.pressure) / (t2 - t1);
    let pa3 = ((t3 - t) * p2.pressure + (t - t2) * p3.pressure) / (t3 - t2);

    // Ebene 1 Ableitungen:
    let da1 = (p1.position - p0.position) / (t1 - t0);
    let da2 = (p2.position - p1.position) / (t2 - t1);
    let da3 = (p3.position - p2.position) / (t3 - t2);

    // Ebene 2:
    let b1 = ((t2 - t) * a1 + (t - t0) * a2) / (t2 - t0);
    let b2 = ((t3 - t) * a2 + (t - t1) * a3) / (t3 - t1);

    let pb1 = ((t2 - t) * pa1 + (t - t0) * pa2) / (t2 - t0);
    let pb2 = ((t3 - t) * pa2 + (t - t1) * pa3) / (t3 - t1);

    // Ebene 2 Ableitungen:
    let db1 = (a2 - a1) / (t2 - t0) + ((t2 - t) * da1 + (t - t0) * da2) / (t2 - t0);
    let db2 = (a3 - a2) / (t3 - t1) + ((t3 - t) * da2 + (t - t1) * da3) / (t3 - t1);

    // Ebene 3 (Ergebnis):
    let pos = ((t2 - t) * b1 + (t - t1) * b2) / (t2 - t1);
    let pressure = (((t2 - t) * pb1 + (t - t1) * pb2) / (t2 - t1)).clamp(0.0, 1.0);

    // Ebene 3 Ableitung (analytische Tangente C'(t)):
    let dc = (b2 - b1) / (t2 - t1) + ((t2 - t) * db1 + (t - t1) * db2) / (t2 - t1);

    // Tangente normieren (oder Fallback bei Stillstand)
    let tangent = if dc.length_squared() > 1e-10 {
        dc.normalize()
    } else {
        let fallback = p2.position - p1.position;
        if fallback.length_squared() > 1e-10 {
            fallback.normalize()
        } else {
            glam::Vec2::new(1.0, 0.0)
        }
    };

    // Normale senkrecht zur Tangente: (-Ty, Tx)
    let normal = glam::Vec2::new(-tangent.y, tangent.x);

    // Druckabhängiger Radius r(t)
    let radius = calculate_radius(pressure, config.min_radius, config.max_radius);

    // Linke und rechte Hüllpunkte
    let left = pos + normal * radius;
    let right = pos - normal * radius;

    SplineSample {
        position: pos,
        tangent,
        normal,
        pressure,
        radius,
        left,
        right,
    }
}

/// Tastet ein einzelnes Segment zwischen `p1` und `p2` ab.
pub fn sample_segment(
    p0: ControlPoint,
    p1: ControlPoint,
    p2: ControlPoint,
    p3: ControlPoint,
    config: &SplineConfig,
) -> Vec<SplineSample> {
    let samples = config.samples_per_segment.max(1);
    let mut result = Vec::with_capacity(samples + 1);

    for i in 0..=samples {
        let u = i as f32 / samples as f32;
        result.push(evaluate_catmull_rom_segment(p0, p1, p2, p3, u, config));
    }

    result
}

/// Wandelt eine Folge von `SplineSample`s in ein abwechselndes Triangle-Strip-Vertex-Array um:
/// [left_0, right_0, left_1, right_1, left_2, right_2, ...]
pub fn samples_to_triangle_strip(samples: &[SplineSample]) -> Vec<Vertex> {
    if samples.is_empty() {
        return Vec::new();
    }

    let mut vertices = Vec::with_capacity(samples.len() * 2);

    for s in samples {
        vertices.push(Vertex::new(s.left.x, s.left.y, s.pressure, -1.0));
        vertices.push(Vertex::new(s.right.x, s.right.y, s.pressure, 1.0));
    }

    vertices
}

/// Generiert einen zusammenhängenden Triangle-Strip aus einer Liste von Eingabe-Kontrollpunkten.
///
/// Handhabt automatisch Randpunkte (Geisterpunkte / Spiegelung an den Enden),
/// interpoliert mit zentripetalen Catmull-Rom-Splines und erzeugt ein flüssiges Ribbon-Mesh.
pub fn generate_triangle_strip(
    control_points: &[ControlPoint],
    config: &SplineConfig,
) -> Vec<Vertex> {
    match control_points.len() {
        0 => Vec::new(),
        1 => {
            // Einzelner Punkt (Stift-Tippen / Dot): Erzeuge kreisförmiges Pad
            let pt = control_points[0];
            let radius = calculate_radius(pt.pressure, config.min_radius, config.max_radius);
            let segments = 8;
            let mut vertices = Vec::with_capacity((segments + 1) * 2);

            for i in 0..=segments {
                let angle = (i as f32 / segments as f32) * std::f32::consts::TAU;
                let offset = glam::Vec2::new(angle.cos() * radius, angle.sin() * radius);
                vertices.push(Vertex::from_vec2(pt.position, pt.pressure, 0.0));
                vertices.push(Vertex::from_vec2(pt.position + offset, pt.pressure, 1.0));
            }

            vertices
        }
        2 => {
            let p0 = control_points[0];
            let p1 = control_points[1];

            // Geisterpunkte durch lineare Extrapolation
            let ghost_before = ControlPoint {
                position: 2.0 * p0.position - p1.position,
                pressure: p0.pressure,
            };
            let ghost_after = ControlPoint {
                position: 2.0 * p1.position - p0.position,
                pressure: p1.pressure,
            };

            let samples = sample_segment(ghost_before, p0, p1, ghost_after, config);
            samples_to_triangle_strip(&samples)
        }
        n => {
            let steps = config.samples_per_segment.max(1);
            let mut samples = Vec::new();

            for i in 0..n - 1 {
                let p_curr = control_points[i];
                let p_next = control_points[i + 1];

                let p_prev = if i == 0 {
                    ControlPoint {
                        position: 2.0 * p_curr.position - p_next.position,
                        pressure: p_curr.pressure,
                    }
                } else {
                    control_points[i - 1]
                };

                let p_after = if i + 2 < n {
                    control_points[i + 2]
                } else {
                    ControlPoint {
                        position: 2.0 * p_next.position - p_curr.position,
                        pressure: p_next.pressure,
                    }
                };

                // Für das erste Segment sample von u = 0.0 bis 1.0
                // Für nachfolgende Segmente bei u > 0 beginnen, um doppelte Vertices zu vermeiden
                let start_step = if i == 0 { 0 } else { 1 };

                for s in start_step..=steps {
                    let u = s as f32 / steps as f32;
                    samples.push(evaluate_catmull_rom_segment(
                        p_prev, p_curr, p_next, p_after, u, config,
                    ));
                }
            }

            samples_to_triangle_strip(&samples)
        }
    }
}

/// Glättet den Druckwertverlauf p über ein einfaches gewichtetes 3-Punkt-Fenster [0.25, 0.5, 0.25].
pub fn smooth_pressure(samples: &[StylusSample]) -> Vec<StylusSample> {
    if samples.len() < 3 {
        return samples.to_vec();
    }
    let mut smoothed = samples.to_vec();
    for i in 1..samples.len() - 1 {
        let p = 0.25 * samples[i - 1].pressure
            + 0.50 * samples[i].pressure
            + 0.25 * samples[i + 1].pressure;
        smoothed[i].pressure = p.clamp(0.0, 1.0);
    }
    smoothed
}

/// Glättet die Positionen mit dem Fenster [0.25, 0.5, 0.25] gegen Digitizer-Rauschen.
/// Symmetrisch (ohne Verzögerung), daher bleiben auch kleine Schleifen rund. Anfang, Ende
/// und Punkte mit scharfem Knick (über [`CORNER_ANGLE_THRESHOLD_DEG`]) bleiben unverändert,
/// damit Ecken spitz bleiben.
pub fn smooth_positions(samples: &[StylusSample]) -> Vec<StylusSample> {
    if samples.len() < 3 {
        return samples.to_vec();
    }
    let cos_limit = CORNER_ANGLE_THRESHOLD_DEG.to_radians().cos();
    let mut smoothed = samples.to_vec();
    for i in 1..samples.len() - 1 {
        let (a, b, c) = (
            samples[i - 1].position,
            samples[i].position,
            samples[i + 1].position,
        );
        let (d_in, d_out) = (b - a, c - b);
        if d_in.length_squared() > 1e-8 && d_out.length_squared() > 1e-8 {
            let cos = d_in.normalize().dot(d_out.normalize());
            if cos < cos_limit {
                continue;
            }
        }
        smoothed[i].position = 0.25 * a + 0.5 * b + 0.25 * c;
    }
    smoothed
}

/// Rekursives RDP-Markieren zur Ausdünnung kollinearer Zwischenpunkte.
fn rdp_mark(positions: &[glam::Vec2], start: usize, end: usize, epsilon: f32, keep: &mut [bool]) {
    if end <= start + 1 {
        return;
    }
    let a = positions[start];
    let b = positions[end];
    let mut max_dist = 0.0_f32;
    let mut max_idx = start;

    for (offset, &pos) in positions[(start + 1)..end].iter().enumerate() {
        let dist = point_to_segment_distance(pos, a, b);
        if dist > max_dist {
            max_dist = dist;
            max_idx = start + 1 + offset;
        }
    }

    if max_dist > epsilon {
        keep[max_idx] = true;
        rdp_mark(positions, start, max_idx, epsilon, keep);
        rdp_mark(positions, max_idx, end, epsilon, keep);
    }
}

/// Dezimiert kollineare Zwischenpunkte mit Ramer-Douglas-Peucker (Toleranz ca. 0.05 bis 0.1 mm).
pub fn rdp_filter_samples(samples: &[StylusSample], epsilon: f32) -> Vec<StylusSample> {
    if samples.len() <= 2 {
        return samples.to_vec();
    }
    let positions: Vec<glam::Vec2> = samples.iter().map(|s| s.position).collect();
    let mut keep = vec![false; samples.len()];
    keep[0] = true;
    keep[samples.len() - 1] = true;

    rdp_mark(&positions, 0, samples.len() - 1, epsilon, &mut keep);

    samples
        .iter()
        .enumerate()
        .filter(|(i, _)| keep[*i])
        .map(|(_, s)| *s)
        .collect()
}

/// Berechnet die Richtungsänderung zwischen benachbarten Segmenten und findet Ecken mit Winkel > `threshold_deg`
/// (nur zwischen Segmenten von mindestens [`CORNER_MIN_SEGMENT_UNITS`] Länge).
pub fn detect_corners(samples: &[StylusSample], threshold_deg: f32) -> Vec<usize> {
    if samples.len() < 3 {
        return Vec::new();
    }
    let mut corners = Vec::new();

    for i in 1..samples.len() - 1 {
        let v_in = samples[i].position - samples[i - 1].position;
        let v_out = samples[i + 1].position - samples[i].position;
        let l_in = v_in.length();
        let l_out = v_out.length();

        if l_in >= CORNER_MIN_SEGMENT_UNITS && l_out >= CORNER_MIN_SEGMENT_UNITS {
            let d_in = v_in / l_in;
            let d_out = v_out / l_out;
            let dot = d_in.dot(d_out).clamp(-1.0, 1.0);
            let angle_deg = dot.acos() * (180.0 / std::f32::consts::PI);
            if angle_deg > threshold_deg {
                corners.push(i);
            }
        }
    }

    corners
}

/// Tastet einen zusammenhängenden, glatten Teilabschnitt (Run) zwischen Ecken ab.
fn sample_smooth_run(control_points: &[ControlPoint], config: &SplineConfig) -> Vec<SplineSample> {
    match control_points.len() {
        0 => Vec::new(),
        1 => {
            let pt = control_points[0];
            let radius = calculate_radius(pt.pressure, config.min_radius, config.max_radius);
            vec![SplineSample {
                position: pt.position,
                tangent: glam::Vec2::new(1.0, 0.0),
                normal: glam::Vec2::new(0.0, 1.0),
                pressure: pt.pressure,
                radius,
                left: pt.position + glam::Vec2::new(0.0, radius),
                right: pt.position - glam::Vec2::new(0.0, radius),
            }]
        }
        2 => {
            let p0 = control_points[0];
            let p1 = control_points[1];
            let dir = p1.position - p0.position;
            let len = dir.length();
            let tangent = if len > 1e-5 {
                dir / len
            } else {
                glam::Vec2::new(1.0, 0.0)
            };
            let normal = glam::Vec2::new(-tangent.y, tangent.x);

            let steps = (len / 10.0).clamp(2.0, 8.0) as usize;
            let mut samples = Vec::with_capacity(steps + 1);

            for s in 0..=steps {
                let u = s as f32 / steps as f32;
                let pos = p0.position.lerp(p1.position, u);
                let pressure = p0.pressure + u * (p1.pressure - p0.pressure);
                let radius = calculate_radius(pressure, config.min_radius, config.max_radius);
                samples.push(SplineSample {
                    position: pos,
                    tangent,
                    normal,
                    pressure,
                    radius,
                    left: pos + normal * radius,
                    right: pos - normal * radius,
                });
            }

            samples
        }
        n => {
            let steps = config.samples_per_segment.max(1);
            let mut samples = Vec::new();

            for i in 0..n - 1 {
                let p_curr = control_points[i];
                let p_next = control_points[i + 1];

                // C0-Enden für den Run: Extrapolierte Geisterpunkte verhindern Ausbeulen an Ecken
                let p_prev = if i == 0 {
                    ControlPoint {
                        position: 2.0 * p_curr.position - p_next.position,
                        pressure: p_curr.pressure,
                    }
                } else {
                    control_points[i - 1]
                };

                let p_after = if i + 2 < n {
                    control_points[i + 2]
                } else {
                    ControlPoint {
                        position: 2.0 * p_next.position - p_curr.position,
                        pressure: p_next.pressure,
                    }
                };

                let start_step = if i == 0 { 0 } else { 1 };
                for s in start_step..=steps {
                    let u = s as f32 / steps as f32;
                    samples.push(evaluate_catmull_rom_segment(
                        p_prev, p_curr, p_next, p_after, u, config,
                    ));
                }
            }

            samples
        }
    }
}

/// Generiert das finale Triangle-Mesh aus den bereinigten Punkten unter Beachtung
/// von Ecken-Unterbrechungen (C0) sowie runden Kappen an Start und Ende.
pub fn generate_finalized_triangle_strip(
    samples: &[StylusSample],
    corners: &[usize],
    config: &SplineConfig,
) -> Vec<Vertex> {
    if samples.is_empty() {
        return Vec::new();
    }
    if samples.len() == 1 {
        let pt = ControlPoint::from_vec2(samples[0].position, samples[0].pressure);
        return generate_triangle_strip(&[pt], config);
    }

    // 1. Teile die Punkte an erkannten Ecken in glatte Runs auf
    let mut run_ranges = Vec::new();
    let mut current_start = 0;

    for &corner_idx in corners {
        if corner_idx > current_start && corner_idx < samples.len() {
            run_ranges.push(current_start..=corner_idx);
            current_start = corner_idx;
        }
    }
    if current_start < samples.len() {
        run_ranges.push(current_start..=(samples.len() - 1));
    }

    // 2. Taste alle Runs einzeln ab
    let mut runs: Vec<Vec<SplineSample>> = Vec::with_capacity(run_ranges.len());
    for range in run_ranges {
        let pts: Vec<ControlPoint> = samples[range]
            .iter()
            .map(|s| ControlPoint::from_vec2(s.position, s.pressure))
            .collect();
        let run_samples = sample_smooth_run(&pts, config);
        if !run_samples.is_empty() {
            runs.push(run_samples);
        }
    }

    if runs.is_empty() {
        return Vec::new();
    }

    const CAP_STEPS: usize = 4;
    let mut vertices = Vec::new();

    // 3. Runde Start-Kappe vor dem ersten Run
    let s_first = &runs[0][0];
    let p_start = s_first.position;
    let t_start = s_first.tangent;
    let n_start = s_first.normal;
    let r_start = s_first.radius;
    let pr_start = s_first.pressure;

    for k in 0..CAP_STEPS {
        let alpha = (k as f32 / CAP_STEPS as f32) * (std::f32::consts::PI * 0.5);
        let left = p_start - t_start * (r_start * alpha.cos()) + n_start * (r_start * alpha.sin());
        let right = p_start - t_start * (r_start * alpha.cos()) - n_start * (r_start * alpha.sin());
        vertices.push(Vertex::new(left.x, left.y, pr_start, -1.0));
        vertices.push(Vertex::new(right.x, right.y, pr_start, 1.0));
    }

    // 4. Runs und Ecken-Übergänge verbinden
    for (run_idx, run) in runs.iter().enumerate() {
        if run_idx > 0 {
            // Ecken-Verbindung: Nahtlose C0-Verbindung am Knick
            let s_corner = &run[0];
            let p_c = s_corner.position;
            let pr_c = s_corner.pressure;
            vertices.push(Vertex::new(p_c.x, p_c.y, pr_c, 0.0));
            vertices.push(Vertex::new(p_c.x, p_c.y, pr_c, 0.0));
            vertices.push(Vertex::new(s_corner.left.x, s_corner.left.y, pr_c, -1.0));
            vertices.push(Vertex::new(s_corner.right.x, s_corner.right.y, pr_c, 1.0));
        }

        let start_sample_idx = if run_idx > 0 { 1 } else { 0 };
        for s in &run[start_sample_idx..] {
            vertices.push(Vertex::new(s.left.x, s.left.y, s.pressure, -1.0));
            vertices.push(Vertex::new(s.right.x, s.right.y, s.pressure, 1.0));
        }
    }

    // 5. Runde End-Kappe nach dem letzten Run
    let s_last = runs.last().unwrap().last().unwrap();
    let p_end = s_last.position;
    let t_end = s_last.tangent;
    let n_end = s_last.normal;
    let r_end = s_last.radius;
    let pr_end = s_last.pressure;

    for k in 1..=CAP_STEPS {
        let beta = (std::f32::consts::PI * 0.5) * (1.0 - k as f32 / CAP_STEPS as f32);
        let left = p_end + t_end * (r_end * beta.cos()) + n_end * (r_end * beta.sin());
        let right = p_end + t_end * (r_end * beta.cos()) - n_end * (r_end * beta.sin());
        vertices.push(Vertex::new(left.x, left.y, pr_end, -1.0));
        vertices.push(Vertex::new(right.x, right.y, pr_end, 1.0));
    }

    vertices
}

/// Finalisiert einen Strich nach dem Zwei-Stufen-Modell:
/// 1. Ecken-Erkennung (Corner Preservation bei Richtungsänderung > 65°)
/// 2. Druck- und Positionsglättung über gewichtetes 3-Punkt-Fenster [0.25, 0.5, 0.25]
/// 3. Ramer-Douglas-Peucker (RDP) Dezimierung (Toleranz ca. 0.05 bis 0.1 mm)
/// 4. Generierung der finalen Triangle-Strips mit runden Kappen an Start und Ende.
pub fn finalize_stroke(raw_samples: &[StylusSample]) -> Stroke {
    finalize_stroke_with_config(
        0,
        raw_samples,
        [0.0, 0.0, 0.0, 1.0],
        &SplineConfig::default(),
    )
}

/// Vollständige Finalisierung mit konfigurierbarer Stroke-ID, Farbe und SplineConfig.
pub fn finalize_stroke_with_config(
    stroke_id: u64,
    raw_samples: &[StylusSample],
    color: [f32; 4],
    config: &SplineConfig,
) -> Stroke {
    if raw_samples.is_empty() {
        return Stroke::new(stroke_id, Vec::new(), Vec::new(), color);
    }

    // 1. Druck- und Positionsglättung über 3-Punkt-Fenster [0.25, 0.5, 0.25]
    let smoothed = smooth_positions(&smooth_pressure(raw_samples));

    // 2. RDP-Dezimierung (ca. 0.075 mm = 0.75 Einheiten)
    let decimated = rdp_filter_samples(&smoothed, RDP_TOLERANCE_UNITS);

    // 3. Ecken-Erkennung (Winkel > 65 Grad)
    let corners = detect_corners(&decimated, CORNER_ANGLE_THRESHOLD_DEG);

    // 4. Finale Triangle-Strips mit C0-Ecken und runden Kappen generieren
    let strip_vertices = generate_finalized_triangle_strip(&decimated, &corners, config);
    let ink_vertices: Vec<InkVertex> = strip_vertices
        .into_iter()
        .map(|v| InkVertex::new(v.position[0], v.position[1], color))
        .collect();

    let mut stroke = Stroke::new(stroke_id, decimated, ink_vertices, color);
    stroke.width = config.min_radius + config.max_radius;
    stroke.finalized = true;
    stroke
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_centripetal_interpolation_endpoints() {
        let p0 = ControlPoint::new(0.0, 0.0, 0.2);
        let p1 = ControlPoint::new(10.0, 2.0, 0.4);
        let p2 = ControlPoint::new(20.0, 8.0, 0.8);
        let p3 = ControlPoint::new(30.0, 10.0, 1.0);

        let config = SplineConfig::default();

        // Am Start u = 0.0 muss der Spline exakt bei p1 sein
        let start_sample = evaluate_catmull_rom_segment(p0, p1, p2, p3, 0.0, &config);
        assert!((start_sample.position.x - p1.position.x).abs() < 1e-4);
        assert!((start_sample.position.y - p1.position.y).abs() < 1e-4);
        assert!((start_sample.pressure - p1.pressure).abs() < 1e-4);

        // Am Ende u = 1.0 muss der Spline exakt bei p2 sein
        let end_sample = evaluate_catmull_rom_segment(p0, p1, p2, p3, 1.0, &config);
        assert!((end_sample.position.x - p2.position.x).abs() < 1e-4);
        assert!((end_sample.position.y - p2.position.y).abs() < 1e-4);
        assert!((end_sample.pressure - p2.pressure).abs() < 1e-4);
    }

    #[test]
    fn test_tangent_and_normal_orthogonality() {
        let p0 = ControlPoint::new(0.0, 0.0, 0.5);
        let p1 = ControlPoint::new(10.0, 10.0, 0.5);
        let p2 = ControlPoint::new(20.0, 5.0, 0.5);
        let p3 = ControlPoint::new(30.0, 0.0, 0.5);

        let config = SplineConfig::default();
        let samples = sample_segment(p0, p1, p2, p3, &config);

        for s in samples {
            // Tangente und Normale müssen normiert sein (Länge ca. 1.0)
            assert!((s.tangent.length() - 1.0).abs() < 1e-4);
            assert!((s.normal.length() - 1.0).abs() < 1e-4);

            // Skalarprodukt aus Tangente und Normale muss 0 sein (orthogonal)
            let dot = s.tangent.dot(s.normal);
            assert!(
                dot.abs() < 1e-4,
                "Tangente ({:?}) und Normale ({:?}) müssen orthogonal sein, dot = {}",
                s.tangent,
                s.normal,
                dot
            );

            // Linker und rechter Punkt müssen im Abstand 'radius' von der Mittellinie liegen
            let dist_left = (s.left - s.position).length();
            let dist_right = (s.right - s.position).length();
            assert!((dist_left - s.radius).abs() < 1e-4);
            assert!((dist_right - s.radius).abs() < 1e-4);
        }
    }

    #[test]
    fn test_pressure_dependent_radius() {
        let min_r = 1.0;
        let max_r = 5.0;

        assert_eq!(calculate_radius(0.0, min_r, max_r), 1.0);
        assert_eq!(calculate_radius(1.0, min_r, max_r), 5.0);
        assert_eq!(calculate_radius(0.5, min_r, max_r), 3.0);
        // Außerhalb des Intervalls geklemmt
        assert_eq!(calculate_radius(-0.5, min_r, max_r), 1.0);
        assert_eq!(calculate_radius(1.5, min_r, max_r), 5.0);
    }

    #[test]
    fn test_triangle_strip_generation() {
        let points = vec![
            ControlPoint::new(0.0, 0.0, 0.2),
            ControlPoint::new(10.0, 10.0, 0.5),
            ControlPoint::new(20.0, 10.0, 0.8),
            ControlPoint::new(30.0, 0.0, 1.0),
        ];

        let config = SplineConfig {
            min_radius: 1.0,
            max_radius: 4.0,
            samples_per_segment: 10,
        };

        let strip = generate_triangle_strip(&points, &config);

        // 3 Segmente, erstes mit 11 Samples, nächste zwei je 10 Samples = 31 Samples
        // Jedes Sample liefert 2 Vertices (left, right) -> 62 Vertices
        assert_eq!(strip.len(), 31 * 2);

        // Vertices müssen abwechselnd side -1.0 und +1.0 haben
        for (i, v) in strip.iter().enumerate() {
            assert!(v.position[0].is_finite());
            assert!(v.position[1].is_finite());
            assert!((0.0..=1.0).contains(&v.pressure));

            if i % 2 == 0 {
                assert_eq!(v.side, -1.0, "Gerader Index muss linker Hüllpunkt sein");
            } else {
                assert_eq!(v.side, 1.0, "Ungerader Index muss rechter Hüllpunkt sein");
            }
        }
    }

    #[test]
    fn test_degenerate_points() {
        // Zwei identische Punkte dürfen keine Panik, keine Division durch Null und kein NaN erzeugen
        let points = vec![
            ControlPoint::new(5.0, 5.0, 0.5),
            ControlPoint::new(5.0, 5.0, 0.5),
        ];

        let config = SplineConfig::default();
        let strip = generate_triangle_strip(&points, &config);

        assert!(!strip.is_empty());
        for v in strip {
            assert!(v.position[0].is_finite());
            assert!(v.position[1].is_finite());
            assert!(v.pressure.is_finite());
            assert!(v.side.is_finite());
        }
    }

    #[test]
    fn test_corner_preservation() {
        let mut raw_samples = Vec::new();
        // Horizontaler Schenkel von (0, 0) bis (100, 0)
        for i in 0..=20 {
            raw_samples.push(StylusSample {
                position: glam::Vec2::new(i as f32 * 5.0, 0.0),
                pressure: 0.5,
                timestamp: i as f64 * 0.01,
            });
        }
        // Vertikaler Schenkel von (100, 0) bis (100, 100) (90°-Knick / L-Form)
        for i in 1..=20 {
            raw_samples.push(StylusSample {
                position: glam::Vec2::new(100.0, i as f32 * 5.0),
                pressure: 0.5,
                timestamp: (20 + i) as f64 * 0.01,
            });
        }

        let stroke = finalize_stroke(&raw_samples);

        // 1. Strich und Meshes müssen sauber aufgebaut sein
        assert!(!stroke.points.is_empty());
        assert!(!stroke.vertices.is_empty());

        // 2. Der Eckpunkt (100.0, 0.0) muss in den Strichpunkten erhalten bleiben
        let corner_found = stroke
            .points
            .iter()
            .any(|s| (s.position.x - 100.0).abs() < 1e-3 && (s.position.y - 0.0).abs() < 1e-3);
        assert!(
            corner_found,
            "Eckpunkt (100.0, 0.0) muss in den finalen Strichpunkten erhalten bleiben!"
        );

        // 3. Ecken-Erhaltung: Entlang des horizontalen Schenkels vor der Ecke (x < 99)
        // darf kein Rundungsausweichen in y-Richtung stattfinden
        for pt in &stroke.points {
            if pt.position.x < 99.0 {
                assert!(
                    pt.position.y.abs() < 1e-3,
                    "Horizontaler Schenkel darf vor der Ecke nicht abgerundet werden (y = {})",
                    pt.position.y
                );
            }
            if pt.position.y > 1.0 {
                assert!(
                    (pt.position.x - 100.0).abs() < 1e-3,
                    "Vertikaler Schenkel darf nach der Ecke nicht abgerundet werden (x = {})",
                    pt.position.x
                );
            }
        }

        // 4. Ecken-Erkennung prüft den 90°-Winkel (> 65 Grad)
        let corners = detect_corners(&stroke.points, CORNER_ANGLE_THRESHOLD_DEG);
        assert_eq!(
            corners.len(),
            1,
            "Exakt eine 90-Grad-Ecke muss erkannt werden"
        );
        let corner_idx = corners[0];
        let corner_pt = stroke.points[corner_idx];
        assert!((corner_pt.position.x - 100.0).abs() < 1e-3);
        assert!((corner_pt.position.y - 0.0).abs() < 1e-3);
    }

    #[test]
    fn test_small_loops_stay_round() {
        // Kleines „o“ (Radius 0,8 mm) in 0,1 s mit 200 Hz, mit Vorschub wie im Wort
        let (r, period, hz) = (8.0_f32, 0.1_f64, 200.0);
        let center = |t: f64| glam::Vec2::new(500.0 + t as f32 * 40.0, 500.0);
        let raw: Vec<StylusSample> = (0..=((period * hz) as usize))
            .map(|i| {
                let t = i as f64 / hz;
                let a = std::f32::consts::TAU * (t / period) as f32;
                StylusSample {
                    position: center(t) + glam::Vec2::new(r * a.sin(), -r * a.cos()),
                    pressure: 0.6,
                    timestamp: t,
                }
            })
            .collect();
        let stroke =
            finalize_stroke_with_config(1, &raw, [0.0; 4], &SplineConfig::from_width_units(5.0));
        for p in &stroke.points {
            let d = (p.position - center(p.timestamp)).length();
            assert!((d - r).abs() < 0.6, "Schleife verformt: Abstand {d}");
        }
        assert!(detect_corners(&stroke.points, CORNER_ANGLE_THRESHOLD_DEG).is_empty());
    }

    #[test]
    fn test_pressure_smoothing() {
        let samples = vec![
            StylusSample {
                position: glam::Vec2::new(0.0, 0.0),
                pressure: 0.2,
                timestamp: 0.0,
            },
            StylusSample {
                position: glam::Vec2::new(1.0, 0.0),
                pressure: 0.8,
                timestamp: 0.1,
            },
            StylusSample {
                position: glam::Vec2::new(2.0, 0.0),
                pressure: 0.2,
                timestamp: 0.2,
            },
        ];

        let smoothed = smooth_pressure(&samples);
        // Fenster: 0.25 * 0.2 + 0.5 * 0.8 + 0.25 * 0.2 = 0.05 + 0.40 + 0.05 = 0.50
        assert!((smoothed[1].pressure - 0.50).abs() < 1e-4);
    }

    #[test]
    fn test_rdp_decimation() {
        let mut samples = Vec::new();
        // 10 Punkte exakt auf einer Linie
        for i in 0..=10 {
            samples.push(StylusSample {
                position: glam::Vec2::new(i as f32 * 10.0, 0.0),
                pressure: 0.5,
                timestamp: i as f64 * 0.01,
            });
        }

        let decimated = rdp_filter_samples(&samples, RDP_TOLERANCE_UNITS);
        // Kollineare Zwischenpunkte müssen entfernt werden -> nur Start und Ende bleiben
        assert_eq!(decimated.len(), 2);
        assert_eq!(decimated[0].position, glam::Vec2::new(0.0, 0.0));
        assert_eq!(decimated[1].position, glam::Vec2::new(100.0, 0.0));
    }
}
