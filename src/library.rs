//! Ordner für Notizbücher (`<Basisordner>/library.json`).
//!
//! Ordner sind rein logisch: Die Notizbücher bleiben in ihren Verzeichnissen
//! `<Basisordner>/<id>/`, `library.json` hält nur fest, welcher Ordner welches Notizbuch
//! enthält. Umbenennen und Verschieben ändern so nie Notizbuch-Dateien. Fehlt die Datei oder
//! ist sie unlesbar, liegen alle Notizbücher auf der obersten Ebene – verloren geht nichts.
//!
//! Ordner werden als Pfade mit `/` als Trenner gespeichert (z. B. `3. Semester/Mathe`);
//! die oberste Ebene ist der leere Pfad.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::storage::{self, StorageError};
use crate::t;

/// Oberste Ebene.
pub const ROOT: &str = "";

/// Längster erlaubter Ordnername (Zeichen).
const MAX_NAME_CHARS: usize = 80;

/// Ordnerbaum und Zuordnung der Notizbücher.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Library {
    /// Alle Ordner als Pfade
    pub folders: BTreeSet<String>,
    /// Ordner je Notizbuch-Kennung; Notizbücher ohne Eintrag liegen auf oberster Ebene
    pub notebooks: BTreeMap<String, String>,
}

/// Letzter Teil eines Ordnerpfads (der angezeigte Name).
pub fn folder_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Übergeordneter Ordner (oberste Ebene für Ordner der obersten Ebene).
pub fn parent_folder(path: &str) -> &str {
    path.rsplit_once('/').map_or(ROOT, |(parent, _)| parent)
}

/// Pfad des Unterordners `name` in `parent`.
fn join(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        name.to_string()
    } else {
        format!("{parent}/{name}")
    }
}

/// `path` liegt in `folder` oder ist `folder` selbst.
fn is_within(path: &str, folder: &str) -> bool {
    path == folder
        || path
            .strip_prefix(folder)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// Bereinigt einen Ordnernamen; Fehlermeldung für die Oberfläche bei ungültigen Namen.
pub fn validate_name(name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err(t!("Der Ordnername ist leer").to_string());
    }
    if name.contains('/') {
        return Err(t!("Ordnernamen dürfen kein „/“ enthalten").to_string());
    }
    if name.chars().count() > MAX_NAME_CHARS {
        return Err(t!(
            "Der Ordnername ist zu lang (höchstens {max} Zeichen)",
            max = MAX_NAME_CHARS
        ));
    }
    Ok(name.to_string())
}

impl Library {
    /// Ort von `library.json` im Basisordner der Notizbücher.
    pub fn file_path(base_dir: &Path) -> PathBuf {
        base_dir.join("library.json")
    }

    /// Lädt die Ordner; fehlt die Datei oder ist sie beschädigt, gibt es keine Ordner.
    pub fn load(base_dir: &Path) -> Self {
        let path = Self::file_path(base_dir);
        let mut library: Self = match std::fs::read_to_string(&path) {
            Ok(content) => serde_json::from_str(&content).unwrap_or_else(|e| {
                eprintln!(
                    "[Ordner] '{}' ist ungültig ({e}), alle Notizbücher auf oberster Ebene.",
                    path.display()
                );
                Self::default()
            }),
            Err(_) => Self::default(),
        };
        library.repair();
        library
    }

    /// Speichert die Ordner atomar.
    pub fn save(&self, base_dir: &Path) -> Result<(), StorageError> {
        storage::atomic_write_json(&Self::file_path(base_dir), self)
    }

    /// Macht eine (evtl. von Hand bearbeitete) Datei stimmig: leere oder ungültige Namen
    /// fallen weg, jeder benutzte Ordner und seine übergeordneten Ordner existieren.
    fn repair(&mut self) {
        let valid = |path: &str| {
            !path.is_empty()
                && path
                    .split('/')
                    .all(|name| validate_name(name).as_deref() == Ok(name))
        };
        self.folders.retain(|f| valid(f));
        self.notebooks.retain(|_, f| f.is_empty() || valid(f));
        let used: Vec<String> = self
            .folders
            .iter()
            .chain(self.notebooks.values())
            .cloned()
            .collect();
        for mut folder in used {
            while !folder.is_empty() {
                let parent = parent_folder(&folder).to_string();
                self.folders.insert(folder);
                folder = parent;
            }
        }
    }

    /// Ordner, in dem das Notizbuch liegt.
    pub fn folder_of(&self, notebook_id: &str) -> &str {
        self.notebooks.get(notebook_id).map_or(ROOT, String::as_str)
    }

    /// Direkte Unterordner von `parent` (vollständige Pfade, nach Name sortiert).
    pub fn subfolders(&self, parent: &str) -> Vec<&str> {
        let mut list: Vec<&str> = self
            .folders
            .iter()
            .map(String::as_str)
            .filter(|f| parent_folder(f) == parent)
            .collect();
        list.sort_by_key(|f| folder_name(f).to_lowercase());
        list
    }

    /// Alle Ordner in Baumreihenfolge (Eltern vor Kindern, Geschwister nach Name).
    pub fn all_folders(&self) -> Vec<&str> {
        let mut out = Vec::with_capacity(self.folders.len());
        let mut stack: Vec<&str> = self.subfolders(ROOT).into_iter().rev().collect();
        while let Some(folder) = stack.pop() {
            out.push(folder);
            stack.extend(self.subfolders(folder).into_iter().rev());
        }
        out
    }

    /// Legt in `parent` den Ordner `name` an und gibt seinen Pfad zurück.
    pub fn create_folder(&mut self, parent: &str, name: &str) -> Result<String, String> {
        let name = validate_name(name)?;
        if !parent.is_empty() && !self.folders.contains(parent) {
            return Err(t!("Der übergeordnete Ordner existiert nicht mehr").to_string());
        }
        let path = join(parent, &name);
        if !self.folders.insert(path.clone()) {
            return Err(t!("Den Ordner „{name}“ gibt es hier schon", name = name));
        }
        Ok(path)
    }

    /// Benennt einen Ordner um (mitsamt Unterordnern und Zuordnungen); gibt den neuen Pfad
    /// zurück.
    pub fn rename_folder(&mut self, path: &str, new_name: &str) -> Result<String, String> {
        let new_name = validate_name(new_name)?;
        if !self.folders.contains(path) {
            return Err(t!("Den Ordner gibt es nicht mehr").to_string());
        }
        let new_path = join(parent_folder(path), &new_name);
        if new_path == path {
            return Ok(new_path);
        }
        if self.folders.contains(&new_path) {
            return Err(t!(
                "Den Ordner „{name}“ gibt es hier schon",
                name = new_name
            ));
        }
        self.move_contents(path, &new_path);
        Ok(new_path)
    }

    /// Löscht einen Ordner. Seine Notizbücher und Unterordner rücken eine Ebene nach oben
    /// (gleichnamige Unterordner werden zusammengelegt) – gelöscht wird kein Notizbuch.
    pub fn delete_folder(&mut self, path: &str) {
        if path.is_empty() || !self.folders.remove(path) {
            return;
        }
        let parent = parent_folder(path).to_string();
        let prefix = format!("{path}/");
        let moved: Vec<String> = self
            .folders
            .iter()
            .filter(|f| f.starts_with(&prefix))
            .cloned()
            .collect();
        for folder in moved {
            self.folders.remove(&folder);
            self.folders.insert(join(&parent, &folder[prefix.len()..]));
        }
        for folder in self.notebooks.values_mut() {
            if folder == path {
                *folder = parent.clone();
            } else if let Some(rest) = folder.strip_prefix(&prefix) {
                *folder = join(&parent, rest);
            }
        }
        self.notebooks.retain(|_, f| !f.is_empty());
    }

    /// Verschiebt einen Ordner samt Inhalt nach `new_path`.
    fn move_contents(&mut self, path: &str, new_path: &str) {
        let renamed = |f: &str| format!("{new_path}{}", &f[path.len()..]);
        let moved: Vec<String> = self
            .folders
            .iter()
            .filter(|f| is_within(f, path))
            .cloned()
            .collect();
        for folder in moved {
            self.folders.remove(&folder);
            self.folders.insert(renamed(&folder));
        }
        for folder in self.notebooks.values_mut() {
            if is_within(folder, path) {
                *folder = renamed(folder);
            }
        }
    }

    /// Verschiebt einen Ordner samt Inhalt nach `new_parent` (oberste Ebene: [`ROOT`]) und
    /// gibt seinen neuen Pfad zurück. Nicht in sich selbst oder einen eigenen Unterordner.
    pub fn move_folder(&mut self, path: &str, new_parent: &str) -> Result<String, String> {
        if !self.folders.contains(path) {
            return Err(t!("Den Ordner gibt es nicht mehr").to_string());
        }
        if !new_parent.is_empty() && !self.folders.contains(new_parent) {
            return Err(t!("Den Zielordner gibt es nicht mehr").to_string());
        }
        if is_within(new_parent, path) {
            return Err(t!("Ein Ordner kann nicht in sich selbst verschoben werden").to_string());
        }
        let name = folder_name(path);
        let new_path = join(new_parent, name);
        if new_path == path {
            return Ok(new_path);
        }
        if self.folders.contains(&new_path) {
            return Err(t!(
                "Im Zielordner gibt es schon einen Ordner „{name}“",
                name = name
            ));
        }
        self.move_contents(path, &new_path);
        Ok(new_path)
    }

    /// Mögliche Ziele für das Verschieben des Ordners `path`: alle Ordner außer ihm selbst und
    /// seinen Unterordnern (die oberste Ebene ist immer ein Ziel).
    pub fn folder_move_targets(&self, path: &str) -> Vec<&str> {
        self.all_folders()
            .into_iter()
            .filter(|f| !is_within(f, path))
            .collect()
    }

    /// Legt das Notizbuch in `folder` (oberste Ebene: [`ROOT`]).
    pub fn move_notebook(&mut self, notebook_id: &str, folder: &str) -> Result<(), String> {
        if folder.is_empty() {
            self.notebooks.remove(notebook_id);
            return Ok(());
        }
        if !self.folders.contains(folder) {
            return Err(t!("Den Ordner gibt es nicht mehr").to_string());
        }
        self.notebooks
            .insert(notebook_id.to_string(), folder.to_string());
        Ok(())
    }

    /// Wie viele der Notizbücher `ids` in `folder` liegen, Unterordner eingeschlossen.
    pub fn count_within<'a>(&self, folder: &str, ids: impl IntoIterator<Item = &'a str>) -> usize {
        ids.into_iter()
            .filter(|id| is_within(self.folder_of(id), folder))
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn library() -> Library {
        let mut lib = Library::default();
        let sem3 = lib.create_folder(ROOT, "3. Semester").unwrap();
        let mathe = lib.create_folder(&sem3, "Mathe").unwrap();
        lib.create_folder(&sem3, "Physik").unwrap();
        lib.move_notebook("analysis", &mathe).unwrap();
        lib.move_notebook("uebung", &sem3).unwrap();
        lib
    }

    #[test]
    fn test_paths_and_names() {
        assert_eq!(folder_name("3. Semester/Mathe"), "Mathe");
        assert_eq!(folder_name("Mathe"), "Mathe");
        assert_eq!(parent_folder("3. Semester/Mathe"), "3. Semester");
        assert_eq!(parent_folder("Mathe"), ROOT);
        assert_eq!(validate_name("  Mathe  ").unwrap(), "Mathe");
        assert!(validate_name("   ").is_err());
        assert!(validate_name("a/b").is_err());
        assert!(validate_name(&"x".repeat(81)).is_err());
    }

    #[test]
    fn test_create_and_list_folders() {
        let mut lib = library();
        assert_eq!(lib.subfolders(ROOT), ["3. Semester"]);
        assert_eq!(
            lib.subfolders("3. Semester"),
            ["3. Semester/Mathe", "3. Semester/Physik"]
        );
        assert_eq!(
            lib.all_folders(),
            ["3. Semester", "3. Semester/Mathe", "3. Semester/Physik"]
        );
        assert_eq!(lib.folder_of("analysis"), "3. Semester/Mathe");
        assert_eq!(lib.folder_of("unbekannt"), ROOT);
        // Doppelte Namen und fehlende Eltern werden abgelehnt
        assert!(lib.create_folder("3. Semester", " Mathe ").is_err());
        assert!(lib.create_folder("Gibt es nicht", "X").is_err());
        // Gleicher Name in einem anderen Ordner ist erlaubt
        lib.create_folder(ROOT, "4. Semester").unwrap();
        assert!(lib.create_folder("4. Semester", "Mathe").is_ok());
        assert_eq!(
            lib.count_within("3. Semester", ["analysis", "uebung", "lose"]),
            2
        );
    }

    #[test]
    fn test_rename_folder_keeps_contents() {
        let mut lib = library();
        let new = lib.rename_folder("3. Semester", "WS 25-26").unwrap();
        assert_eq!(new, "WS 25-26");
        assert_eq!(lib.folder_of("analysis"), "WS 25-26/Mathe");
        assert_eq!(lib.folder_of("uebung"), "WS 25-26");
        assert!(lib.folders.contains("WS 25-26/Physik"));
        assert!(!lib.folders.iter().any(|f| f.starts_with("3. Semester")));
        // Ein Ordner mit ähnlichem Namen bleibt unberührt
        let mut lib = library();
        lib.create_folder(ROOT, "3. Semester alt").unwrap();
        lib.rename_folder("3. Semester", "Drei").unwrap();
        assert!(lib.folders.contains("3. Semester alt"));
        // Kollision
        assert!(lib.rename_folder("Drei", "3. Semester alt").is_err());
    }

    #[test]
    fn test_delete_folder_moves_contents_up() {
        let mut lib = library();
        lib.delete_folder("3. Semester");
        assert_eq!(lib.folder_of("analysis"), "Mathe");
        assert_eq!(lib.folder_of("uebung"), ROOT);
        assert!(!lib.notebooks.contains_key("uebung"));
        assert_eq!(lib.subfolders(ROOT), ["Mathe", "Physik"]);
    }

    #[test]
    fn test_move_folder() {
        let mut lib = library();
        lib.create_folder(ROOT, "Mathe-Sammlung").unwrap();
        // Ordner samt Inhalt in einen anderen Ordner schieben
        let new = lib
            .move_folder("3. Semester/Mathe", "Mathe-Sammlung")
            .unwrap();
        assert_eq!(new, "Mathe-Sammlung/Mathe");
        assert_eq!(lib.folder_of("analysis"), "Mathe-Sammlung/Mathe");
        assert!(!lib.folders.contains("3. Semester/Mathe"));
        // Auf die oberste Ebene und zurück
        assert_eq!(
            lib.move_folder("Mathe-Sammlung/Mathe", ROOT).unwrap(),
            "Mathe"
        );
        assert_eq!(
            lib.move_folder("Mathe", "3. Semester").unwrap(),
            "3. Semester/Mathe"
        );
        // Nicht in sich selbst oder in einen eigenen Unterordner
        assert!(lib.move_folder("3. Semester", "3. Semester").is_err());
        assert!(lib.move_folder("3. Semester", "3. Semester/Mathe").is_err());
        assert_eq!(lib.folder_move_targets("3. Semester"), ["Mathe-Sammlung"]);
        // Namenskollision am Ziel
        lib.create_folder("Mathe-Sammlung", "Physik").unwrap();
        assert!(lib
            .move_folder("3. Semester/Physik", "Mathe-Sammlung")
            .is_err());
        assert_eq!(lib.folder_of("uebung"), "3. Semester");
    }

    #[test]
    fn test_move_notebook() {
        let mut lib = library();
        lib.move_notebook("uebung", "3. Semester/Physik").unwrap();
        assert_eq!(lib.folder_of("uebung"), "3. Semester/Physik");
        lib.move_notebook("uebung", ROOT).unwrap();
        assert_eq!(lib.folder_of("uebung"), ROOT);
        assert!(lib.move_notebook("uebung", "Gibt es nicht").is_err());
    }

    #[test]
    fn test_load_save_and_repair() {
        let dir = tempfile::tempdir().unwrap();
        // Fehlende Datei: keine Ordner
        assert_eq!(Library::load(dir.path()), Library::default());

        let lib = library();
        lib.save(dir.path()).unwrap();
        assert_eq!(Library::load(dir.path()), lib);

        // Beschädigt: alles auf oberster Ebene statt Absturz
        std::fs::write(Library::file_path(dir.path()), b"{ kaputt").unwrap();
        assert_eq!(Library::load(dir.path()), Library::default());

        // Von Hand bearbeitet: fehlende Elternordner werden ergänzt, Unsinn fällt weg
        std::fs::write(
            Library::file_path(dir.path()),
            br#"{"folders": ["A/B", "", "C//D"], "notebooks": {"x": "E/F", "y": "/kaputt"}}"#,
        )
        .unwrap();
        let lib = Library::load(dir.path());
        assert_eq!(lib.all_folders(), ["A", "A/B", "E", "E/F"]);
        assert_eq!(lib.folder_of("x"), "E/F");
        assert_eq!(lib.folder_of("y"), ROOT);
    }
}
