//! Adaptiver 1€-Filter (One Euro Filter) für rauschfreie und latenzoptimierte Stylus-Eingaben.
//!
//! Filtert x, y und Druck (pressure p ∈ [0.0, 1.0]).
//! Implementiert numerische Härtung:
//! - dt-Clamping: dt <= 0 wird mit dt = 1e-4 s abgefangen, dt > 1.0 s löst ein reset() aus.
//! - Glättungsfaktor alpha wird strikt auf den Bereich [1e-5, 1.0] begrenzt.
//! - NaN/Inf-Schutz gegen Filtervergiftung.

use std::f32::consts::PI;

/// Standard-Minimal-Grenzfrequenz in Hz (reduziert Jitter bei niedrigen Geschwindigkeiten)
pub const DEFAULT_FC_MIN: f32 = 4.5;
/// Standard-Geschwindigkeitskoeffizient (Hz je Seiteneinheit/s): Bei 400 Einheiten/s
/// (4 cm/s, zügige Handschrift) liegt die Grenzfrequenz bei rund 65 Hz, kleine Schleifen
/// werden kaum noch verkleinert
pub const DEFAULT_BETA: f32 = 0.15;
/// Standard-Grenzfrequenz für die Geschwindigkeitsschätzung in Hz (reagiert nach wenigen
/// Millisekunden auf schnellere Bewegung)
pub const DEFAULT_D_CUTOFF: f32 = 8.0;

/// Berechnet den Glättungsfaktor alpha = 1 / (1 + tau / dt) = 2*pi*fc*dt / (2*pi*fc*dt + 1).
///
/// Numerisch gehärtet: Garantiert Werte im Bereich [1e-5, 1.0] und schützt vor NaN/Inf.
pub fn smoothing_factor(dt: f32, cutoff: f32) -> f32 {
    let r = 2.0 * PI * cutoff * dt;
    if !r.is_finite() || r <= 0.0 {
        1e-5
    } else {
        let alpha = r / (r + 1.0);
        if !alpha.is_finite() {
            1e-5
        } else {
            alpha.clamp(1e-5, 1.0)
        }
    }
}

/// 1D One-Euro-Filter für skalare Signale.
#[derive(Debug, Clone)]
pub struct OneEuroFilter1D {
    pub fc_min: f32,
    pub beta: f32,
    pub d_cutoff: f32,
    x_prev: Option<f32>,
    dx_prev: f32,
    t_prev: Option<f64>,
}

impl OneEuroFilter1D {
    /// Erstellt einen neuen 1D-Filter mit den angegebenen Parametern.
    pub fn new(fc_min: f32, beta: f32, d_cutoff: f32) -> Self {
        Self {
            fc_min,
            beta,
            d_cutoff,
            x_prev: None,
            dx_prev: 0.0,
            t_prev: None,
        }
    }

    /// Setzt den Filterzustand vollständig zurück.
    pub fn reset(&mut self) {
        self.x_prev = None;
        self.dx_prev = 0.0;
        self.t_prev = None;
    }

    /// Gibt zurück, ob der Filter bereits mindestens einen Messwert empfangen hat.
    pub fn is_initialized(&self) -> bool {
        self.x_prev.is_some()
    }

    /// Letzter gefilterter Wert, falls vorhanden.
    pub fn last_value(&self) -> Option<f32> {
        self.x_prev
    }

    /// Filtert einen Wert `x` zum Zeitstempel `t` (in Sekunden).
    pub fn filter(&mut self, x: f32, t: f64) -> f32 {
        // Schutz vor NaN- und Inf-Eingaben
        if !x.is_finite() || !t.is_finite() {
            return self.x_prev.unwrap_or(0.0);
        }

        let Some(prev_t) = self.t_prev else {
            // Erster Messwert: Initialisierung
            self.x_prev = Some(x);
            self.dx_prev = 0.0;
            self.t_prev = Some(t);
            return x;
        };

        let raw_dt = t - prev_t;

        // Numerische Härtung: dt > 1.0 s führt zu reset()
        if raw_dt > 1.0 {
            self.reset();
            self.x_prev = Some(x);
            self.dx_prev = 0.0;
            self.t_prev = Some(t);
            return x;
        }

        // Numerische Härtung: dt <= 0 wird mit dt = 1e-4 s abgefangen
        let dt = if raw_dt <= 0.0 || !raw_dt.is_finite() {
            1e-4_f32
        } else {
            raw_dt as f32
        };

        self.t_prev = Some(t);

        let prev_x = self.x_prev.unwrap_or(x);

        // 1. Ableitung (Geschwindigkeit) filtern
        let mut dx = (x - prev_x) / dt;
        if !dx.is_finite() {
            dx = 0.0;
        }

        let a_d = smoothing_factor(dt, self.d_cutoff);
        let mut dx_hat = a_d * dx + (1.0 - a_d) * self.dx_prev;
        if !dx_hat.is_finite() {
            dx_hat = self.dx_prev;
        }

        // 2. Adaptive Grenzfrequenz fc berechnen
        let mut fc = self.fc_min + self.beta * dx_hat.abs();
        if !fc.is_finite() || fc < self.fc_min {
            fc = self.fc_min;
        }

        // 3. Signal mit adaptivem fc filtern
        let a = smoothing_factor(dt, fc);
        let mut x_hat = a * x + (1.0 - a) * prev_x;
        if !x_hat.is_finite() {
            x_hat = prev_x;
        }

        self.x_prev = Some(x_hat);
        self.dx_prev = dx_hat;

        x_hat
    }
}

impl Default for OneEuroFilter1D {
    fn default() -> Self {
        Self::new(DEFAULT_FC_MIN, DEFAULT_BETA, DEFAULT_D_CUTOFF)
    }
}

/// Gefilterter Eingabepunkt mit 2D-Position, Druck und Zeitstempel.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FilteredPoint {
    pub position: glam::Vec2,
    pub pressure: f32,
    pub timestamp: f64,
}

/// Adaptiver 1€-Filter für 2D-Position (x, y) und Stylus-Druck p ∈ [0.0, 1.0].
///
/// Die Position wird mit der Geschwindigkeit in der Ebene gefiltert (nicht je Achse):
/// Auf einer Kreisbahn ist eine Achse an den Umkehrpunkten in Ruhe, eine Filterung je
/// Achse würde dort stark glätten und Schleifen zusammenziehen.
#[derive(Debug, Clone)]
pub struct OneEuroFilter {
    pub fc_min: f32,
    pub beta: f32,
    pub d_cutoff: f32,
    pos_prev: Option<glam::Vec2>,
    speed_prev: f32,
    t_prev: Option<f64>,
    p_filter: OneEuroFilter1D,
    last_output: Option<(f32, f32, f32)>,
}

impl OneEuroFilter {
    /// Erstellt einen neuen OneEuroFilter mit den angegebenen Parametern.
    pub fn new(fc_min: f32, beta: f32, d_cutoff: f32) -> Self {
        Self {
            fc_min,
            beta,
            d_cutoff,
            pos_prev: None,
            speed_prev: 0.0,
            t_prev: None,
            p_filter: OneEuroFilter1D::new(fc_min, beta, d_cutoff),
            last_output: None,
        }
    }

    /// Setzt den Filter vollständig zurück (z. B. bei neuem Stroke).
    pub fn reset(&mut self) {
        self.pos_prev = None;
        self.speed_prev = 0.0;
        self.t_prev = None;
        self.p_filter.reset();
        self.last_output = None;
    }

    /// Filtert x, y und Druck p ∈ [0.0, 1.0] zum Zeitstempel `t` (in Sekunden).
    ///
    /// Gibt das gefilterte Tupel `(x, y, pressure)` zurück.
    pub fn filter(&mut self, x: f32, y: f32, pressure: f32, t: f64) -> (f32, f32, f32) {
        // Schutz vor NaN- und Inf-Eingaben
        if !x.is_finite() || !y.is_finite() || !pressure.is_finite() || !t.is_finite() {
            return self.last_output.unwrap_or((0.0, 0.0, 0.0));
        }

        // Druck immer auf [0.0, 1.0] klemmen
        let p_in = pressure.clamp(0.0, 1.0);

        let pos = self.filter_position(glam::Vec2::new(x, y), t);
        let (x_out, y_out) = (pos.x, pos.y);
        let p_out = self.p_filter.filter(p_in, t).clamp(0.0, 1.0);

        let out = (x_out, y_out, p_out);
        self.last_output = Some(out);
        out
    }

    /// 1€-Filter der Position mit der Geschwindigkeit in der Ebene.
    fn filter_position(&mut self, x: glam::Vec2, t: f64) -> glam::Vec2 {
        let (Some(prev), Some(prev_t)) = (self.pos_prev, self.t_prev) else {
            self.pos_prev = Some(x);
            self.speed_prev = 0.0;
            self.t_prev = Some(t);
            return x;
        };
        let raw_dt = t - prev_t;
        // Lange Pause: neu beginnen statt über die Pause hinweg zu glätten
        if raw_dt > 1.0 {
            self.pos_prev = Some(x);
            self.speed_prev = 0.0;
            self.t_prev = Some(t);
            return x;
        }
        let dt = if raw_dt <= 0.0 || !raw_dt.is_finite() {
            1e-4_f32
        } else {
            raw_dt as f32
        };
        self.t_prev = Some(t);

        let mut speed = (x - prev).length() / dt;
        if !speed.is_finite() {
            speed = 0.0;
        }
        let a_d = smoothing_factor(dt, self.d_cutoff);
        let speed_hat = a_d * speed + (1.0 - a_d) * self.speed_prev;

        let fc = (self.fc_min + self.beta * speed_hat).max(self.fc_min);
        let a = smoothing_factor(dt, fc);
        let mut out = prev + (x - prev) * a;
        if !out.is_finite() {
            out = prev;
        }
        self.pos_prev = Some(out);
        self.speed_prev = if speed_hat.is_finite() {
            speed_hat
        } else {
            0.0
        };
        out
    }

    /// Filtert einen 2D-Punkt `pos` und Druck `pressure` zum Zeitstempel `t`.
    pub fn filter_point(&mut self, pos: glam::Vec2, pressure: f32, t: f64) -> (glam::Vec2, f32) {
        let (x, y, p) = self.filter(pos.x, pos.y, pressure, t);
        (glam::Vec2::new(x, y), p)
    }

    /// Filtert einen `FilteredPoint` und gibt den geglätteten Punkt zurück.
    pub fn filter_stroke_point(&mut self, point: FilteredPoint) -> FilteredPoint {
        let (pos, p) = self.filter_point(point.position, point.pressure, point.timestamp);
        FilteredPoint {
            position: pos,
            pressure: p,
            timestamp: point.timestamp,
        }
    }
}

impl Default for OneEuroFilter {
    fn default() -> Self {
        Self::new(DEFAULT_FC_MIN, DEFAULT_BETA, DEFAULT_D_CUTOFF)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_zero_and_negative_dt() {
        let mut filter = OneEuroFilter::default();

        // 1. Erster gültiger Datenpunkt bei t = 1.0
        let (x1, y1, p1) = filter.filter(10.0, 20.0, 0.5, 1.0);
        assert_eq!(x1, 10.0);
        assert_eq!(y1, 20.0);
        assert_eq!(p1, 0.5);

        // 2. dt == 0 (gleicher Zeitstempel t = 1.0): Sollte dt = 1e-4 abfangen
        let (x2, y2, p2) = filter.filter(10.5, 20.5, 0.6, 1.0);
        assert!(x2.is_finite(), "x2 muss endlich sein");
        assert!(y2.is_finite(), "y2 muss endlich sein");
        assert!(p2.is_finite(), "p2 muss endlich sein");
        assert!((0.0..=1.0).contains(&p2), "Druck muss in [0, 1] liegen");

        // 3. dt < 0 (Zeitstempel springt zurück auf t = 0.5): Sollte mit dt = 1e-4 abgefangen werden
        let (x3, y3, p3) = filter.filter(11.0, 21.0, 0.7, 0.5);
        assert!(x3.is_finite(), "x3 muss nach negativem dt endlich sein");
        assert!(y3.is_finite(), "y3 muss nach negativem dt endlich sein");
        assert!(p3.is_finite(), "p3 muss nach negativem dt endlich sein");
        assert!((0.0..=1.0).contains(&p3));

        // 4. dt > 1.0 s (großer Zeitsprung z.B. auf t = 5.0 s): Muss reset() auslösen
        let (x4, y4, p4) = filter.filter(50.0, 100.0, 0.9, 5.0);
        // Nach dem Reset muss der Filter sofort exakt den neuen Punkt als Startpunkt annehmen
        assert_eq!(
            x4, 50.0,
            "Nach Reset muss der neue X-Wert übernommen werden"
        );
        assert_eq!(
            y4, 100.0,
            "Nach Reset muss der neue Y-Wert übernommen werden"
        );
        assert_eq!(
            p4, 0.9,
            "Nach Reset muss der neue Druckwert übernommen werden"
        );
    }

    #[test]
    fn test_small_loops_do_not_shrink() {
        // Kreis mit 0,8 mm Radius in 0,1 s: die Filterung in der Ebene folgt ihm
        let mut filter = OneEuroFilter::default();
        let (r, period, hz) = (8.0_f32, 0.1_f64, 200.0);
        let mut radii = Vec::new();
        for i in 0..=(period * hz) as usize * 2 {
            let t = i as f64 / hz;
            let a = std::f32::consts::TAU * (t / period) as f32;
            let p = glam::Vec2::new(500.0 + r * a.sin(), 500.0 - r * a.cos());
            let (out, _) = filter.filter_point(p, 0.5, t);
            if t > period {
                radii.push((out - glam::Vec2::new(500.0, 500.0)).length());
            }
        }
        let mean = radii.iter().sum::<f32>() / radii.len() as f32;
        assert!(mean > r * 0.9, "Schleife geschrumpft: {mean}");
    }

    #[test]
    fn test_high_frequency_jitter() {
        let mut filter = OneEuroFilter::default();

        let base_x = 100.0_f32;
        let base_y = 50.0_f32;
        let base_p = 0.5_f32;
        // Realistisches Digitizer-Rauschen (±0,05 mm) bei ruhendem Stift
        let jitter_amplitude = 0.5_f32;

        let num_samples = 120;
        let dt = 1.0 / 120.0; // 120 Hz Abtastrate

        let mut raw_x_values = Vec::with_capacity(num_samples);
        let mut filtered_x_values = Vec::with_capacity(num_samples);

        for i in 0..num_samples {
            let t = i as f64 * dt;
            // Hochfrequentes Rauschen mit Vorzeichenwechsel
            let sign = if i % 2 == 0 { 1.0 } else { -1.0 };
            let raw_x = base_x + sign * jitter_amplitude;
            let raw_y = base_y + sign * jitter_amplitude;
            let raw_p = base_p + sign * 0.1;

            raw_x_values.push(raw_x);

            let (fx, fy, fp) = filter.filter(raw_x, raw_y, raw_p, t);
            filtered_x_values.push(fx);

            assert!(fx.is_finite());
            assert!(fy.is_finite());
            assert!(fp.is_finite());
            assert!((0.0..=1.0).contains(&fp));
        }

        // Varianz nach der Einschwingphase (letzte 60 Samples) berechnen
        let eval_range = 60..num_samples;
        let count = eval_range.len() as f32;

        let raw_mean: f32 = raw_x_values[eval_range.clone()].iter().sum::<f32>() / count;
        let raw_variance: f32 = raw_x_values[eval_range.clone()]
            .iter()
            .map(|v| (v - raw_mean).powi(2))
            .sum::<f32>()
            / count;

        let filtered_mean: f32 = filtered_x_values[eval_range.clone()].iter().sum::<f32>() / count;
        let filtered_variance: f32 = filtered_x_values[eval_range]
            .iter()
            .map(|v| (v - filtered_mean).powi(2))
            .sum::<f32>()
            / count;

        // Der 1€-Filter muss das hochfrequente Jitter deutlich dämpfen (der fertige Strich
        // wird zusätzlich symmetrisch geglättet)
        assert!(
            filtered_variance < raw_variance * 0.2,
            "Gefilterte Varianz ({filtered_variance}) muss deutlich kleiner sein als Raw-Varianz ({raw_variance})"
        );
    }

    #[test]
    fn test_nan_inf_poisoning() {
        let mut filter = OneEuroFilter::default();

        // 1. Zuerst normale Werte einspeisen
        let (x0, y0, p0) = filter.filter(10.0, 20.0, 0.5, 0.0);
        assert_eq!((x0, y0, p0), (10.0, 20.0, 0.5));

        let (x1, y1, p1) = filter.filter(11.0, 21.0, 0.55, 0.01);
        assert!(x1.is_finite());
        assert!(y1.is_finite());
        assert!(p1.is_finite());

        // 2. Vergiftungsversuch mit NaN und Infinity
        let (x_nan, y_nan, p_nan) = filter.filter(f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 0.02);
        assert!(
            x_nan.is_finite(),
            "Filter darf kein NaN bei Vergiftung ausgeben"
        );
        assert!(
            y_nan.is_finite(),
            "Filter darf kein Inf bei Vergiftung ausgeben"
        );
        assert!(
            p_nan.is_finite(),
            "Filter darf kein -Inf bei Vergiftung ausgeben"
        );

        let (x_nan2, y_nan2, p_nan2) = filter.filter(12.0, 22.0, 0.6, f64::NAN);
        assert!(x_nan2.is_finite());
        assert!(y_nan2.is_finite());
        assert!(p_nan2.is_finite());

        // 3. Nach der Vergiftung wieder normale Werte einspeisen
        let (x2, y2, p2) = filter.filter(13.0, 23.0, 0.65, 0.03);
        assert!(x2.is_finite(), "x2 muss nach Vergiftung endlich sein");
        assert!(y2.is_finite(), "y2 muss nach Vergiftung endlich sein");
        assert!(p2.is_finite(), "p2 muss nach Vergiftung endlich sein");

        // Sicherstellen, dass der Filter nicht bei einem fixen Wert 'steckenbleibt'
        let (x3, y3, p3) = filter.filter(30.0, 40.0, 0.8, 0.04);
        assert!(x3.is_finite());
        assert!(y3.is_finite());
        assert!(p3.is_finite());
        assert!(
            x3 > x2,
            "Filter muss sich nach Vergiftung weiter normal anpassen"
        );
    }
}
