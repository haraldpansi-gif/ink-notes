//! Eingebaute, stift- und fingertaugliche Dateiauswahl für Bilder.
//!
//! Bewusst ohne Systemdialog: der bräuchte GTK oder das Desktop-Portal, die unter WSL und auf
//! schlanken Systemen oft fehlen. Die Auswahl zeigt Ordner und unterstützte Bilder des
//! aktuellen Ordners; versteckte Dateien bleiben ausgeblendet.

use std::path::{Path, PathBuf};

use crate::assets::is_supported_image_path;

/// Ein Eintrag der Dateiliste.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickerEntry {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
}

/// Zustand der Dateiauswahl.
#[derive(Debug, Clone, PartialEq)]
pub struct FilePicker {
    pub dir: PathBuf,
    pub entries: Vec<PickerEntry>,
    /// Ordner nicht lesbar
    pub error: Option<String>,
}

impl FilePicker {
    /// Öffnet die Auswahl in `dir`, sonst im Bilder-Ordner, sonst im Home-Ordner.
    pub fn open(dir: Option<&Path>) -> Self {
        let start = dir
            .filter(|d| d.is_dir())
            .map(Path::to_path_buf)
            .or_else(|| shortcuts().into_iter().map(|(_, p)| p).find(|p| p.is_dir()))
            .unwrap_or_else(|| PathBuf::from("/"));
        let mut picker = Self {
            dir: start.clone(),
            entries: Vec::new(),
            error: None,
        };
        picker.navigate(&start);
        picker
    }

    /// Wechselt in `dir` und liest den Inhalt neu ein.
    pub fn navigate(&mut self, dir: &Path) {
        match list_dir(dir) {
            Ok(entries) => {
                self.dir = dir.to_path_buf();
                self.entries = entries;
                self.error = None;
            }
            Err(err) => {
                self.error = Some(format!(
                    "'{}' kann nicht geöffnet werden: {err}",
                    dir.display()
                ));
            }
        }
    }

    /// Eine Ebene nach oben.
    pub fn go_up(&mut self) {
        if let Some(parent) = self.dir.parent().map(Path::to_path_buf) {
            self.navigate(&parent);
        }
    }
}

/// Ordner und unterstützte Bilder in `dir`: erst Ordner, dann Dateien, jeweils alphabetisch
/// ohne Groß-/Kleinschreibung; versteckte Einträge (Punkt am Anfang) fehlen.
pub fn list_dir(dir: &Path) -> std::io::Result<Vec<PickerEntry>> {
    let mut entries: Vec<PickerEntry> = std::fs::read_dir(dir)?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                return None;
            }
            let path = entry.path();
            // Symlinks auflösen: ein Link auf einen Ordner ist ein Ordner
            let is_dir = path.is_dir();
            (is_dir || is_supported_image_path(&path)).then_some(PickerEntry { name, path, is_dir })
        })
        .collect();
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(entries)
}

/// Schnellzugriffe: Bilder, Downloads, Schreibtisch und Home-Ordner (soweit vorhanden),
/// mit den Namen aus `~/.config/user-dirs.dirs` (z. B. „Bilder“ statt „Pictures“).
pub fn shortcuts() -> Vec<(&'static str, PathBuf)> {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return Vec::new();
    };
    let user_dirs =
        std::fs::read_to_string(home.join(".config/user-dirs.dirs")).unwrap_or_default();
    let xdg = |key: &str, fallbacks: &[&str]| -> Option<PathBuf> {
        parse_user_dir(&user_dirs, key, &home)
            .into_iter()
            .chain(fallbacks.iter().map(|f| home.join(f)))
            .find(|p| p.is_dir() && *p != home)
    };
    let mut list = Vec::new();
    if let Some(p) = xdg("XDG_PICTURES_DIR", &["Bilder", "Pictures"]) {
        list.push(("🖼 Bilder", p));
    }
    if let Some(p) = xdg("XDG_DOWNLOAD_DIR", &["Downloads"]) {
        list.push(("⬇ Downloads", p));
    }
    if let Some(p) = xdg("XDG_DESKTOP_DIR", &["Schreibtisch", "Desktop"]) {
        list.push(("🖥 Schreibtisch", p));
    }
    list.push(("🏠 Persönlicher Ordner", home));
    list
}

/// Liest einen Eintrag wie `XDG_PICTURES_DIR="$HOME/Bilder"` aus `user-dirs.dirs`.
pub fn parse_user_dir(content: &str, key: &str, home: &Path) -> Option<PathBuf> {
    content.lines().find_map(|line| {
        let value = line
            .trim()
            .strip_prefix(key)?
            .trim_start()
            .strip_prefix('=')?;
        let value = value.trim().trim_matches('"');
        let path = match value.strip_prefix("$HOME") {
            Some(rest) => home.join(rest.trim_start_matches('/')),
            None => PathBuf::from(value),
        };
        path.is_absolute().then_some(path)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_list_dir_sorts_and_filters() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["b.png", "A.jpg", "notiz.txt", ".versteckt.png", "c.webp"] {
            std::fs::write(dir.path().join(name), b"x").unwrap();
        }
        std::fs::create_dir(dir.path().join("zOrdner")).unwrap();
        std::fs::create_dir(dir.path().join("aOrdner")).unwrap();
        std::fs::create_dir(dir.path().join(".cache")).unwrap();

        let names: Vec<String> = list_dir(dir.path())
            .unwrap()
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert_eq!(names, ["aOrdner", "zOrdner", "A.jpg", "b.png", "c.webp"]);
    }

    #[test]
    fn test_navigation_and_errors() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("Fotos");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("tafel.jpg"), b"x").unwrap();

        let mut picker = FilePicker::open(Some(&sub));
        assert_eq!(picker.dir, sub);
        assert_eq!(picker.entries.len(), 1);

        picker.go_up();
        assert_eq!(picker.dir, dir.path());
        assert!(picker.entries.iter().any(|e| e.is_dir && e.name == "Fotos"));

        // Nicht vorhandener Ordner: Fehlermeldung, aktueller Ordner bleibt
        picker.navigate(&dir.path().join("gibt-es-nicht"));
        assert!(picker.error.is_some());
        assert_eq!(picker.dir, dir.path());
    }

    #[test]
    fn test_parse_user_dirs() {
        let home = Path::new("/home/anna");
        let content = "# Kommentar\nXDG_DESKTOP_DIR=\"$HOME/Schreibtisch\"\nXDG_PICTURES_DIR=\"$HOME/Bilder\"\nXDG_MUSIC_DIR=\"/daten/Musik\"\n";
        assert_eq!(
            parse_user_dir(content, "XDG_PICTURES_DIR", home),
            Some(PathBuf::from("/home/anna/Bilder"))
        );
        assert_eq!(
            parse_user_dir(content, "XDG_MUSIC_DIR", home),
            Some(PathBuf::from("/daten/Musik"))
        );
        assert_eq!(parse_user_dir(content, "XDG_DOWNLOAD_DIR", home), None);
    }
}
