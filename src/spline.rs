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
}
