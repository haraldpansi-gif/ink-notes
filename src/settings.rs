//! Persistente UI-Einstellungen (`$XDG_CONFIG_HOME/ink-notes/ui.json`).
//!
//! Fehlende oder unlesbare Dateien führen zu Standardwerten, damit die App immer startet.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::background::{PaperTheme, PatternColor};
use crate::i18n::Language;
use crate::storage::{self, StorageError};
use crate::t;

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

/// Stift-Vorlage: Farbe und Stärke eines der Stifte in der Werkzeugleiste. Ändert man Farbe
/// oder Stärke, während ein Stift gewählt ist, merkt sich die Vorlage die neuen Werte.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PenPreset {
    pub color: [f32; 4],
    pub width_mm: f32,
}

impl PenPreset {
    pub const WIDTH_RANGE: std::ops::RangeInclusive<f32> = 0.15..=5.0;

    /// Begrenzt Werte aus einer (evtl. von Hand bearbeiteten) Einstellungsdatei; Stifte sind
    /// immer deckend.
    pub fn sanitized(&self, fallback: PenPreset) -> Self {
        let color = sanitized_color(self.color, [0.0, 0.0, 0.0, 1.0]);
        let width_mm = if self.width_mm.is_finite() {
            self.width_mm
                .clamp(*Self::WIDTH_RANGE.start(), *Self::WIDTH_RANGE.end())
        } else {
            fallback.width_mm
        };
        Self { color, width_mm }
    }
}

/// Anzahl der Stifte in der Werkzeugleiste.
pub const PEN_PRESET_COUNT: usize = 4;

/// Standardstifte: Schwarz, Blau und Rot fein, Grün etwas kräftiger.
pub const DEFAULT_PEN_PRESETS: [PenPreset; PEN_PRESET_COUNT] = [
    PenPreset {
        color: [0.12, 0.12, 0.14, 1.0],
        width_mm: 0.5,
    },
    PenPreset {
        color: [0.15, 0.35, 0.85, 1.0],
        width_mm: 0.5,
    },
    PenPreset {
        color: [0.85, 0.20, 0.20, 1.0],
        width_mm: 0.5,
    },
    PenPreset {
        color: [0.18, 0.65, 0.35, 1.0],
        width_mm: 0.8,
    },
];

/// Farben des Textmarkers in der Werkzeugleiste (halbtransparent).
pub const DEFAULT_HIGHLIGHTER_COLORS: [[f32; 4]; 4] = [
    [1.0, 0.95, 0.1, 0.35],
    [0.2, 0.9, 0.3, 0.35],
    [0.25, 0.7, 1.0, 0.35],
    [1.0, 0.3, 0.65, 0.35],
];

/// Farben des Text-Werkzeugs in der Werkzeugleiste.
pub const DEFAULT_TEXT_COLORS: [[f32; 4]; 5] = [
    [0.12, 0.12, 0.14, 1.0],
    [0.15, 0.35, 0.85, 1.0],
    [0.85, 0.20, 0.20, 1.0],
    [0.18, 0.65, 0.35, 1.0],
    [0.45, 0.45, 0.48, 1.0],
];

/// Begrenzt eine Farbe aus der Einstellungsdatei auf 0–1; die Deckkraft kommt immer aus
/// `fallback` (Stift und Text deckend, Textmarker halbtransparent).
fn sanitized_color(color: [f32; 4], fallback: [f32; 4]) -> [f32; 4] {
    let mut out = fallback;
    if color.iter().all(|c| c.is_finite()) {
        for (o, c) in out.iter_mut().zip(color).take(3) {
            *o = c.clamp(0.0, 1.0);
        }
    }
    out
}

/// Was eine Taste am Stift bewirkt, solange sie gedrückt ist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum StylusAction {
    /// Radieren (Standard, wie bisher)
    #[default]
    Erase,
    /// Auswählen und verschieben
    Select,
    /// Mit dem Textmarker schreiben
    Highlighter,
    /// Mit dem gewählten Stift schreiben (z. B. während der Textmarker aktiv ist)
    Pen,
    /// Taste nicht verwenden
    Nothing,
}

impl StylusAction {
    pub const ALL: [StylusAction; 5] = [
        StylusAction::Erase,
        StylusAction::Select,
        StylusAction::Highlighter,
        StylusAction::Pen,
        StylusAction::Nothing,
    ];

    pub fn label(self) -> &'static str {
        match self {
            StylusAction::Erase => t!("Radieren"),
            StylusAction::Select => t!("Auswählen"),
            StylusAction::Highlighter => t!("Textmarker"),
            StylusAction::Pen => t!("Stift"),
            StylusAction::Nothing => t!("Nichts"),
        }
    }
}

/// Eine Taste am Stift: Kennung (aus der Tablet-Schnittstelle, beim ersten Drücken gelernt)
/// und ihre Belegung.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StylusButton {
    pub id: u64,
    pub action: StylusAction,
}

/// Höchstens so viele Stifttasten werden gemerkt.
pub const MAX_STYLUS_BUTTONS: usize = 4;

/// Benutzereinstellungen der Oberfläche.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UiSettings {
    pub sidebar_side: SidebarSide,
    /// Papierfarbe der Anzeige
    pub paper_theme: PaperTheme,
    /// Dunkle Stiftfarben auf dunklem Papier aufhellen
    pub adapt_ink_colors: bool,
    /// Farbe von Karos, Linien und Punkten (Anzeige und Export)
    pub pattern_color: PatternColor,
    /// Druckkennlinie des Stifts
    pub pressure_curve: PressureCurve,
    /// Seiten durchgehend untereinander anzeigen (sonst einzeln blättern)
    pub continuous_scroll: bool,
    /// Stifte der Werkzeugleiste und der gewählte davon
    pub pen_presets: [PenPreset; PEN_PRESET_COUNT],
    pub active_pen: usize,
    /// Eigene Farben von Textmarker und Text-Werkzeug und die gewählte davon
    pub highlighter_colors: [[f32; 4]; 4],
    pub active_highlighter: usize,
    pub text_colors: [[f32; 4]; 5],
    pub active_text_color: usize,
    /// Tasten am Stift in der Reihenfolge, in der sie zuerst gedrückt wurden
    pub stylus_buttons: Vec<StylusButton>,
    /// Sprache der Oberfläche; ohne Angabe die des Systems
    pub language: Option<Language>,
}

impl Default for UiSettings {
    fn default() -> Self {
        Self {
            sidebar_side: SidebarSide::default(),
            paper_theme: PaperTheme::default(),
            adapt_ink_colors: true,
            pattern_color: PatternColor::default(),
            pressure_curve: PressureCurve::default(),
            continuous_scroll: true,
            pen_presets: DEFAULT_PEN_PRESETS,
            active_pen: 0,
            highlighter_colors: DEFAULT_HIGHLIGHTER_COLORS,
            active_highlighter: 0,
            text_colors: DEFAULT_TEXT_COLORS,
            active_text_color: 0,
            stylus_buttons: Vec::new(),
            language: None,
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
        for (preset, fallback) in settings.pen_presets.iter_mut().zip(DEFAULT_PEN_PRESETS) {
            *preset = preset.sanitized(fallback);
        }
        settings.active_pen = settings.active_pen.min(PEN_PRESET_COUNT - 1);
        for (color, fallback) in settings
            .highlighter_colors
            .iter_mut()
            .zip(DEFAULT_HIGHLIGHTER_COLORS)
        {
            *color = sanitized_color(*color, fallback);
        }
        settings.active_highlighter = settings
            .active_highlighter
            .min(settings.highlighter_colors.len() - 1);
        for (color, fallback) in settings.text_colors.iter_mut().zip(DEFAULT_TEXT_COLORS) {
            *color = sanitized_color(*color, fallback);
        }
        settings.active_text_color = settings
            .active_text_color
            .min(settings.text_colors.len() - 1);
        settings.stylus_buttons.truncate(MAX_STYLUS_BUTTONS);
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
            pattern_color: PatternColor::Strong,
            pressure_curve: PressureCurve {
                exponent: 1.8,
                threshold: 0.1,
            },
            continuous_scroll: false,
            pen_presets: [PenPreset {
                color: [0.5, 0.2, 0.7, 1.0],
                width_mm: 1.5,
            }; PEN_PRESET_COUNT],
            active_pen: 2,
            highlighter_colors: [[0.9, 0.5, 0.1, 0.35]; 4],
            active_highlighter: 3,
            text_colors: [[0.3, 0.1, 0.6, 1.0]; 5],
            active_text_color: 4,
            stylus_buttons: vec![
                StylusButton {
                    id: 17,
                    action: StylusAction::Select,
                },
                StylusButton {
                    id: 42,
                    action: StylusAction::Nothing,
                },
            ],
            language: Some(Language::English),
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
        assert_eq!(settings.pattern_color, PatternColor::Light);

        // Unsinnige Werte aus einer von Hand bearbeiteten Datei werden begrenzt
        std::fs::write(
            &path,
            br#"{"pressure_curve": {"exponent": 50.0, "threshold": -3.0}}"#,
        )
        .unwrap();
        let curve = UiSettings::load_from(&path).pressure_curve;
        assert_eq!(curve.exponent, 3.0);
        assert_eq!(curve.threshold, 0.0);

        // Ohne Stifte: die Standardstifte; unsinnige Stifte werden begrenzt
        assert_eq!(settings.pen_presets, DEFAULT_PEN_PRESETS);
        assert_eq!(settings.active_pen, 0);
        std::fs::write(
            &path,
            br#"{"active_pen": 9, "pen_presets": [
                {"color": [2.0, -1.0, 0.5, 0.2], "width_mm": 99.0},
                {"color": [0.1, 0.2, 0.3, 1.0], "width_mm": 0.0},
                {"color": [0.1, 0.2, 0.3, 1.0], "width_mm": 1.0},
                {"color": [0.1, 0.2, 0.3, 1.0], "width_mm": 1.0}
            ]}"#,
        )
        .unwrap();
        let settings = UiSettings::load_from(&path);
        assert_eq!(settings.active_pen, PEN_PRESET_COUNT - 1);
        assert_eq!(settings.pen_presets[0].color, [1.0, 0.0, 0.5, 1.0]);
        assert_eq!(settings.pen_presets[0].width_mm, 5.0);
        assert_eq!(settings.pen_presets[1].width_mm, 0.15);

        // Eigene Farben: Textmarker bleibt halbtransparent, Text deckend
        std::fs::write(
            &path,
            br#"{"highlighter_colors": [[0.1, 0.2, 0.3, 1.0], [0.1, 0.2, 0.3, 1.0],
                [0.1, 0.2, 0.3, 1.0], [0.1, 0.2, 0.3, 1.0]], "active_highlighter": 7,
                "text_colors": [[5.0, 0.2, 0.3, 0.1], [0.1, 0.2, 0.3, 1.0], [0.1, 0.2, 0.3, 1.0],
                [0.1, 0.2, 0.3, 1.0], [0.1, 0.2, 0.3, 1.0]]}"#,
        )
        .unwrap();
        let settings = UiSettings::load_from(&path);
        assert_eq!(settings.highlighter_colors[0], [0.1, 0.2, 0.3, 0.35]);
        assert_eq!(settings.active_highlighter, 3);
        assert_eq!(settings.text_colors[0], [1.0, 0.2, 0.3, 1.0]);
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
