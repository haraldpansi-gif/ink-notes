//! Persistente UI-Einstellungen (`$XDG_CONFIG_HOME/ink-notes/ui.json`).
//!
//! Fehlende oder unlesbare Dateien führen zu Standardwerten, damit die App immer startet.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::background::PaperTheme;
use crate::storage::{self, StorageError};

/// Fensterseite, an der die Werkzeugleiste angedockt ist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum SidebarSide {
    /// Links (Standard: die Schreibhand von Rechtshändern verdeckt die Leiste nicht)
    #[default]
    Left,
    /// Rechts (für Linkshänder)
    Right,
}

/// Druckkennlinie des Stifts: passt die sehr unterschiedlichen Druckbereiche verschiedener
/// Digitizer (Wacom EMR, MPP, …) an das Schreibgefühl an.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PressureCurve {
    /// Exponent der Kennlinie: < 1 weich (schon leichter Druck ergibt kräftige Linien),
    /// > 1 hart (erst fester Druck ergibt kräftige Linien)
    pub exponent: f32,
    /// Rohdruck unterhalb dieser Schwelle zählt als kein Druck (dünnste Linie);
    /// unterdrückt das Grundrauschen empfindlicher Stifte
    pub threshold: f32,
}

impl PressureCurve {
    pub const EXPONENT_RANGE: std::ops::RangeInclusive<f32> = 0.3..=3.0;
    pub const THRESHOLD_RANGE: std::ops::RangeInclusive<f32> = 0.0..=0.5;

    /// Rechnet den Rohdruck des Stifts (0–1) in den Druck für die Strichbreite um.
    pub fn apply(&self, raw: f32) -> f32 {
        let curve = self.sanitized();
        if !raw.is_finite() {
            return 0.0;
        }
        let normalized = ((raw - curve.threshold) / (1.0 - curve.threshold)).clamp(0.0, 1.0);
        normalized.powf(curve.exponent)
    }

    /// Begrenzt Werte aus einer (evtl. von Hand bearbeiteten) Einstellungsdatei auf den
    /// gültigen Bereich.
    pub fn sanitized(&self) -> Self {
        let default = Self::default();
        let clamp = |v: f32, range: &std::ops::RangeInclusive<f32>, fallback: f32| {
            if v.is_finite() {
                v.clamp(*range.start(), *range.end())
            } else {
                fallback
            }
        };
        Self {
            exponent: clamp(self.exponent, &Self::EXPONENT_RANGE, default.exponent),
            threshold: clamp(self.threshold, &Self::THRESHOLD_RANGE, default.threshold),
        }
    }
}

impl Default for PressureCurve {
    fn default() -> Self {
        Self {
            exponent: 1.0,
            threshold: 0.0,
        }
    }
}

/// Benutzereinstellungen der Oberfläche.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UiSettings {
    pub sidebar_side: SidebarSide,
    /// Papierfarbe der Anzeige
    pub paper_theme: PaperTheme,
    /// Dunkle Stiftfarben auf dunklem Papier aufhellen
    pub adapt_ink_colors: bool,
    /// Druckkennlinie des Stifts
    pub pressure_curve: PressureCurve,
    /// Seiten durchgehend untereinander anzeigen (sonst einzeln blättern)
    pub continuous_scroll: bool,
}

impl Default for UiSettings {
    fn default() -> Self {
        Self {
            sidebar_side: SidebarSide::default(),
            paper_theme: PaperTheme::default(),
            adapt_ink_colors: true,
            pressure_curve: PressureCurve::default(),
            continuous_scroll: true,
        }
    }
}

impl UiSettings {
    /// Standardpfad gemäß XDG-Spezifikation.
    pub fn default_path() -> Option<PathBuf> {
        let base = match std::env::var("XDG_CONFIG_HOME") {
            Ok(dir) if !dir.trim().is_empty() => PathBuf::from(dir),
            _ => PathBuf::from(std::env::var("HOME").ok()?).join(".config"),
        };
        Some(base.join("ink-notes").join("ui.json"))
    }

    /// Lädt die Einstellungen; bei fehlender oder beschädigter Datei gelten Standardwerte.
    pub fn load_from(path: &Path) -> Self {
        let mut settings: Self = match std::fs::read_to_string(path) {
            Ok(content) => serde_json::from_str(&content).unwrap_or_else(|e| {
                eprintln!(
                    "[Einstellungen] '{}' ist ungültig ({e}), verwende Standardwerte.",
                    path.display()
                );
                Self::default()
            }),
            Err(_) => Self::default(),
        };
        settings.pressure_curve = settings.pressure_curve.sanitized();
        settings
    }

    /// Speichert die Einstellungen atomar.
    pub fn save_to(&self, path: &Path) -> Result<(), StorageError> {
        storage::atomic_write_json(path, self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_settings_round_trip_and_fallbacks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ink-notes").join("ui.json");

        // Fehlende Datei -> Standard (links)
        assert_eq!(UiSettings::load_from(&path).sidebar_side, SidebarSide::Left);

        let settings = UiSettings {
            sidebar_side: SidebarSide::Right,
            paper_theme: PaperTheme::Black,
            adapt_ink_colors: false,
            pressure_curve: PressureCurve {
                exponent: 1.8,
                threshold: 0.1,
            },
            continuous_scroll: false,
        };
        settings.save_to(&path).unwrap();
        assert_eq!(UiSettings::load_from(&path), settings);

        // Beschädigte Datei -> Standard statt Absturz
        std::fs::write(&path, b"{ kaputt").unwrap();
        assert_eq!(UiSettings::load_from(&path), UiSettings::default());

        // Unbekannte / fehlende Felder werden toleriert
        std::fs::write(&path, br#"{"zukunft": 1}"#).unwrap();
        assert_eq!(UiSettings::load_from(&path), UiSettings::default());
    }

    #[test]
    fn test_old_settings_file_keeps_new_defaults() {
        // ui.json aus v0.3.x kennt nur die Seite der Werkzeugleiste
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ui.json");
        std::fs::write(&path, br#"{"sidebar_side": "Right"}"#).unwrap();
        let settings = UiSettings::load_from(&path);
        assert_eq!(settings.sidebar_side, SidebarSide::Right);
        assert_eq!(settings.paper_theme, PaperTheme::White);
        assert!(settings.adapt_ink_colors);
        assert_eq!(settings.pressure_curve, PressureCurve::default());
        assert!(settings.continuous_scroll);

        // Unsinnige Werte aus einer von Hand bearbeiteten Datei werden begrenzt
        std::fs::write(
            &path,
            br#"{"pressure_curve": {"exponent": 50.0, "threshold": -3.0}}"#,
        )
        .unwrap();
        let curve = UiSettings::load_from(&path).pressure_curve;
        assert_eq!(curve.exponent, 3.0);
        assert_eq!(curve.threshold, 0.0);
    }

    #[test]
    fn test_pressure_curve() {
        // Standard: unverändert
        let linear = PressureCurve::default();
        for p in [0.0, 0.25, 0.5, 1.0] {
            assert!((linear.apply(p) - p).abs() < 1e-6);
        }
        // Weich: halber Druck ergibt mehr als halbe Breite, hart: weniger
        let soft = PressureCurve {
            exponent: 0.5,
            threshold: 0.0,
        };
        let hard = PressureCurve {
            exponent: 2.0,
            threshold: 0.0,
        };
        assert!(soft.apply(0.5) > 0.6);
        assert!(hard.apply(0.5) < 0.3);
        // Endpunkte bleiben fest
        for curve in [soft, hard] {
            assert_eq!(curve.apply(0.0), 0.0);
            assert!((curve.apply(1.0) - 1.0).abs() < 1e-6);
        }
        // Schwelle: darunter kein Druck, darüber auf den vollen Bereich gestreckt
        let gated = PressureCurve {
            exponent: 1.0,
            threshold: 0.2,
        };
        assert_eq!(gated.apply(0.15), 0.0);
        assert!((gated.apply(0.6) - 0.5).abs() < 1e-6);
        assert!((gated.apply(1.0) - 1.0).abs() < 1e-6);
        // Monoton steigend
        let mut last = -1.0;
        for i in 0..=100 {
            let v = hard.apply(i as f32 / 100.0);
            assert!(v >= last);
            last = v;
        }
        // Ungültige Eingaben ergeben nie NaN
        assert_eq!(linear.apply(f32::NAN), 0.0);
        assert_eq!(linear.apply(7.0), 1.0);
    }
}
