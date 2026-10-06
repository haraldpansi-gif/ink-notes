//! Persistente UI-Einstellungen (`$XDG_CONFIG_HOME/ink-notes/ui.json`).
//!
//! Fehlende oder unlesbare Dateien führen zu Standardwerten, damit die App immer startet.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

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

/// Benutzereinstellungen der Oberfläche.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct UiSettings {
    pub sidebar_side: SidebarSide,
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
        match std::fs::read_to_string(path) {
            Ok(content) => serde_json::from_str(&content).unwrap_or_else(|e| {
                eprintln!(
                    "[Einstellungen] '{}' ist ungültig ({e}), verwende Standardwerte.",
                    path.display()
                );
                Self::default()
            }),
            Err(_) => Self::default(),
        }
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
}
