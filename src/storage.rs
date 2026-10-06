//! Robuste lokale Notizbuch-Persistenz mit atomaren Writes (Crash-Resistenz)
//! und semantischer Datenspeicherung (ohne gecachte GPU-Vertizes).

use std::error::Error;
use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::canvas::InkVertex;
use crate::document::{Notebook, Page, PageBackground, Stroke, StylusSample};
use crate::filter::OneEuroFilter;
use crate::spline::{self, ControlPoint, SplineConfig};

/// Serialisierbarer Stylus-Abtastpunkt in DIN-A4-Koordinaten.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SampleData {
    pub x: f32,
    pub y: f32,
    pub p: f32,
    pub t: f64,
}

impl From<&StylusSample> for SampleData {
    fn from(s: &StylusSample) -> Self {
        Self {
            x: s.position.x,
            y: s.position.y,
            p: s.pressure,
            t: s.timestamp,
        }
    }
}

impl From<SampleData> for StylusSample {
    fn from(d: SampleData) -> Self {
        Self {
            position: glam::Vec2::new(d.x, d.y),
            pressure: d.p,
            timestamp: d.t,
        }
    }
}

/// Serialisierbare semantische Strichdaten (ohne GPU-Vertizes).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StrokeData {
    pub id: u64,
    pub color: [f32; 4],
    pub samples: Vec<SampleData>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shape: Option<crate::geometry::RecognizedShape>,
}

impl From<&Stroke> for StrokeData {
    fn from(stroke: &Stroke) -> Self {
        Self {
            id: stroke.id,
            color: stroke.color,
            samples: stroke.points.iter().map(SampleData::from).collect(),
            shape: stroke.shape.clone(),
        }
    }
}

impl StrokeData {
    /// Wandelt die semantischen Daten deterministisch zurück in einen `Stroke` um,
    /// inklusive Wiederherstellung des Triangle-Strip-Vertexpuffers und der AABB.
    pub fn to_stroke(&self) -> Stroke {
        let stylus_samples: Vec<StylusSample> = self
            .samples
            .iter()
            .copied()
            .map(StylusSample::from)
            .collect();
        let vertices = if let Some(ref shape) = self.shape {
            let (_, shape_verts) = crate::geometry::shape_to_mesh(shape, 20.0, self.color);
            shape_verts
        } else {
            reconstruct_stroke_vertices(&stylus_samples, self.color)
        };
        let mut stroke = Stroke::new(self.id, stylus_samples, vertices, self.color);
        stroke.shape = self.shape.clone();
        stroke
    }
}

/// Serialisierbare semantische Seitendaten.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PageData {
    pub id: u64,
    pub background: PageBackground,
    pub strokes: Vec<StrokeData>,
}

impl From<&Page> for PageData {
    fn from(page: &Page) -> Self {
        Self {
            id: page.id,
            background: page.background,
            strokes: page.strokes.iter().map(StrokeData::from).collect(),
        }
    }
}

impl PageData {
    /// Wandelt die Seitendaten in eine runtime-aktive `Page` mit initialisiertem Cache um.
    pub fn to_page(&self) -> Page {
        let mut page = Page::new(self.id, self.background);
        for stroke_data in &self.strokes {
            page.strokes.push(stroke_data.to_stroke());
        }
        page.is_dirty = true;
        page.rebuild_cache_if_dirty();
        page.is_loaded = true;
        page
    }
}

/// Metadaten des gesamten Notizbuchs (`manifest.json`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NotebookManifest {
    pub id: String,
    pub title: String,
    pub created_at: u64,
    pub modified_at: u64,
    pub current_page_index: usize,
    pub page_ids: Vec<u64>,
    pub next_stroke_id: u64,
    pub next_page_id: u64,
}

impl NotebookManifest {
    pub fn new(id: impl Into<String>, title: impl Into<String>) -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        Self {
            id: id.into(),
            title: title.into(),
            created_at: now,
            modified_at: now,
            current_page_index: 0,
            page_ids: vec![1],
            next_stroke_id: 1,
            next_page_id: 2,
        }
    }

    /// Übernimmt Seitenliste, aktuelle Seite und ID-Zähler aus dem Notizbuch,
    /// damit neu angelegte Seiten im Manifest referenziert werden.
    pub fn sync_from_notebook(&mut self, notebook: &Notebook) {
        self.page_ids = notebook.pages.iter().map(|p| p.id).collect();
        self.current_page_index = notebook.current_page_index;
        self.next_stroke_id = notebook.next_stroke_id;
        self.next_page_id = notebook.next_page_id;
        self.modified_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
    }
}

/// Mögliche Fehler bei Speicher- und Ladeoperationen.
#[derive(Debug)]
pub enum StorageError {
    Io(io::Error),
    Serialization(serde_json::Error),
    InvalidManifest(String),
    CorruptedPage(u64, String),
    PageNotFound(u64),
}

impl fmt::Display for StorageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StorageError::Io(e) => write!(f, "I/O-Fehler: {e}"),
            StorageError::Serialization(e) => write!(f, "Serialisierungsfehler: {e}"),
            StorageError::InvalidManifest(msg) => write!(f, "Ungültiges Manifest: {msg}"),
            StorageError::CorruptedPage(id, msg) => {
                write!(f, "Beschädigte Seite {id}: {msg}")
            }
            StorageError::PageNotFound(id) => write!(f, "Seite {id} nicht gefunden"),
        }
    }
}

impl Error for StorageError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            StorageError::Io(e) => Some(e),
            StorageError::Serialization(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for StorageError {
    fn from(e: io::Error) -> Self {
        StorageError::Io(e)
    }
}

impl From<serde_json::Error> for StorageError {
    fn from(e: serde_json::Error) -> Self {
        StorageError::Serialization(e)
    }
}

/// Rekonstruiert deterministisch die GPU-Triangle-Strip-Vertizes aus Stylus-Samples.
pub fn reconstruct_stroke_vertices(samples: &[StylusSample], color: [f32; 4]) -> Vec<InkVertex> {
    if samples.is_empty() {
        return Vec::new();
    }

    if samples.len() == 1 {
        let pt = samples[0];
        let radius = spline::calculate_radius(pt.pressure, 2.5, 10.0);
        let segments = 8;
        let mut vertices = Vec::with_capacity((segments + 1) * 2);
        for i in 0..=segments {
            let angle = (i as f32 / segments as f32) * std::f32::consts::TAU;
            let offset = glam::Vec2::new(angle.cos() * radius, angle.sin() * radius);
            vertices.push(InkVertex::new(pt.position.x, pt.position.y, color));
            vertices.push(InkVertex::new(
                pt.position.x + offset.x,
                pt.position.y + offset.y,
                color,
            ));
        }
        return vertices;
    }

    let mut filter = OneEuroFilter::default();
    let mut control_points: Vec<ControlPoint> = Vec::with_capacity(samples.len());

    for s in samples {
        let (fx, fy, fp) = filter.filter(s.position.x, s.position.y, s.pressure, s.timestamp);
        let pt = ControlPoint::new(fx, fy, fp);
        if let Some(last) = control_points.last() {
            if (pt.position - last.position).length_squared() < 1.0 {
                continue;
            }
        }
        control_points.push(pt);
    }

    let config = SplineConfig {
        min_radius: 2.5,
        max_radius: 10.0,
        samples_per_segment: 16,
    };
    let raw_strip = spline::generate_triangle_strip(&control_points, &config);
    raw_strip
        .into_iter()
        .map(|v| InkVertex::new(v.position[0], v.position[1], color))
        .collect()
}

/// Schreibt Inhalt atomar auf die Festplatte (über temporäre Datei + `std::fs::rename`).
pub fn atomic_write_file(path: &Path, content: &[u8]) -> Result<(), io::Error> {
    let dir = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "Übergeordnetes Verzeichnis nicht gefunden",
        )
    })?;
    fs::create_dir_all(dir)?;

    let filename = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    let random_suffix: u64 = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;
    let temp_filename = format!("{filename}.tmp.{}.{random_suffix}", std::process::id());
    let temp_path = dir.join(temp_filename);

    // In temporäre Datei schreiben und auf Datenträger flushen
    {
        let mut file = File::create(&temp_path)?;
        file.write_all(content)?;
        file.sync_all()?;
    }

    // Atomares Rename
    if let Err(e) = fs::rename(&temp_path, path) {
        let _ = fs::remove_file(&temp_path);
        return Err(e);
    }

    Ok(())
}

/// Serialisiert einen Wert als formatiertes JSON und schreibt ihn atomar.
pub fn atomic_write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), StorageError> {
    let bytes = serde_json::to_vec_pretty(value)?;
    atomic_write_file(path, &bytes)?;
    Ok(())
}

/// Speichert das Manifest atomar in `manifest.json`.
pub fn save_manifest(notebook_dir: &Path, manifest: &NotebookManifest) -> Result<(), StorageError> {
    let path = notebook_dir.join("manifest.json");
    atomic_write_json(&path, manifest)
}

/// Lädt das Manifest aus `manifest.json`.
pub fn load_manifest(notebook_dir: &Path) -> Result<NotebookManifest, StorageError> {
    let path = notebook_dir.join("manifest.json");
    if !path.exists() {
        return Err(StorageError::InvalidManifest(format!(
            "Manifestdatei nicht gefunden: {}",
            path.display()
        )));
    }
    let mut file = File::open(&path)?;
    let mut json_str = String::new();
    file.read_to_string(&mut json_str)?;
    let manifest: NotebookManifest = serde_json::from_str(&json_str)?;
    Ok(manifest)
}

/// Speichert eine Seite atomar in `page_<id>.json`.
pub fn save_page(notebook_dir: &Path, page: &PageData) -> Result<(), StorageError> {
    let path = notebook_dir.join(format!("page_{}.json", page.id));
    atomic_write_json(&path, page)
}

/// Lädt eine Seite aus `page_<id>.json`.
pub fn load_page(notebook_dir: &Path, page_id: u64) -> Result<PageData, StorageError> {
    let path = notebook_dir.join(format!("page_{page_id}.json"));
    if !path.exists() {
        return Err(StorageError::PageNotFound(page_id));
    }

    let mut file = File::open(&path)?;
    let mut json_str = String::new();
    file.read_to_string(&mut json_str)?;

    match serde_json::from_str::<PageData>(&json_str) {
        Ok(data) => Ok(data),
        Err(e) => {
            eprintln!(
                "[Storage-Warnung] Seite {page_id} in '{}' ist beschädigt: {e}",
                path.display()
            );
            Err(StorageError::CorruptedPage(page_id, e.to_string()))
        }
    }
}

/// Speichert ein gesamtes Notebook synchron auf die Festplatte (z. B. initial oder beim Flush).
pub fn save_notebook(
    notebook_dir: &Path,
    notebook: &Notebook,
    manifest: &mut NotebookManifest,
) -> Result<(), StorageError> {
    fs::create_dir_all(notebook_dir)?;

    manifest.sync_from_notebook(notebook);

    // Alle bereits geladenen Seiten speichern (fehlgeschlagene Ersatzseiten niemals)
    for page in &notebook.pages {
        if page.is_loaded && !page.load_failed {
            let page_data = PageData::from(page);
            save_page(notebook_dir, &page_data)?;
        }
    }

    save_manifest(notebook_dir, manifest)?;
    Ok(())
}

/// Lädt ein Notizbuch aus einem Ordner (erste Seite sofort, Folgeseiten als Platzhalter für Lazy Loading).
pub fn load_notebook(notebook_dir: &Path) -> Result<(Notebook, NotebookManifest), StorageError> {
    let manifest = load_manifest(notebook_dir)?;
    if manifest.page_ids.is_empty() {
        let nb = Notebook::new();
        return Ok((nb, manifest));
    }

    let mut pages = Vec::with_capacity(manifest.page_ids.len());

    // Erste Seite sofort laden und initialisieren
    let first_id = manifest.page_ids[0];
    let first_page = match load_page(notebook_dir, first_id) {
        Ok(data) => data.to_page(),
        Err(e) => {
            eprintln!("[Storage] Konnte erste Seite {first_id} nicht laden: {e}.");
            replacement_page_after_load_error(notebook_dir, first_id, &e)
        }
    };
    pages.push(first_page);

    // Folgeseiten als ungeladene Platzhalter anlegen (Lazy Loading)
    for &page_id in &manifest.page_ids[1..] {
        pages.push(Page::new_unloaded(page_id, PageBackground::Grid5mm));
    }

    let notebook = Notebook::from_pages(
        pages,
        manifest.current_page_index,
        manifest.next_stroke_id,
        manifest.next_page_id,
    );

    Ok((notebook, manifest))
}

/// Stellt sicher, dass die Seite am Index `page_idx` geladen ist (Lazy Loading beim Seitenwechsel).
pub fn ensure_page_loaded(
    notebook_dir: &Path,
    notebook: &mut Notebook,
    page_idx: usize,
) -> Result<(), StorageError> {
    if page_idx >= notebook.pages.len() {
        return Err(StorageError::PageNotFound(page_idx as u64));
    }

    if notebook.pages[page_idx].is_loaded {
        return Ok(());
    }

    let page_id = notebook.pages[page_idx].id;
    match load_page(notebook_dir, page_id) {
        Ok(data) => {
            notebook.pages[page_idx] = data.to_page();
            Ok(())
        }
        Err(e) => {
            eprintln!("[Storage] Fehler beim Lazy Loading von Seite {page_id}: {e}");
            // Ersatzseite einsetzen, damit die App nicht hängt
            let background = notebook.pages[page_idx].background;
            let mut page = replacement_page_after_load_error(notebook_dir, page_id, &e);
            page.background = background;
            page.rebuild_cache_if_dirty();
            notebook.pages[page_idx] = page;
            Err(e)
        }
    }
}

/// Erzeugt eine leere Ersatzseite für eine Seite, deren Datei nicht geladen werden konnte.
///
/// - Fehlende Datei: normale leere Seite (z. B. neu angelegt, aber noch nie gespeichert).
/// - Beschädigtes JSON: Die Datei wird als `page_<id>.json.corrupt-<zeit>` beiseite gelegt,
///   danach ist die leere Seite normal beschreibbar.
/// - Sonstige Fehler (I/O) oder fehlgeschlagenes Beiseitelegen: Die Ersatzseite wird als
///   `load_failed` markiert und nie gespeichert, damit die Originaldatei erhalten bleibt.
fn replacement_page_after_load_error(
    notebook_dir: &Path,
    page_id: u64,
    err: &StorageError,
) -> Page {
    let mut page = Page::new(page_id, PageBackground::Grid5mm);
    match err {
        StorageError::PageNotFound(_) => {}
        StorageError::CorruptedPage(..) => {
            let path = notebook_dir.join(format!("page_{page_id}.json"));
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let backup = notebook_dir.join(format!("page_{page_id}.json.corrupt-{now}"));
            match fs::rename(&path, &backup) {
                Ok(()) => eprintln!(
                    "[Storage] Beschädigte Seite gesichert als '{}'.",
                    backup.display()
                ),
                Err(e) => {
                    eprintln!("[Storage] Konnte beschädigte Seite nicht sichern ({e}). Seite wird nicht gespeichert.");
                    page.load_failed = true;
                }
            }
        }
        _ => page.load_failed = true,
    }
    page
}

/// Lädt alle noch nicht geladenen Seiten ohne Ersatzseiten (z. B. für den PDF-Export).
/// Schlägt fehl, sobald eine Seite nicht vollständig verfügbar ist.
pub fn load_all_pages(notebook_dir: &Path, notebook: &mut Notebook) -> Result<(), StorageError> {
    for page in &mut notebook.pages {
        if page.load_failed {
            return Err(StorageError::CorruptedPage(
                page.id,
                "Seite konnte nicht geladen werden".to_string(),
            ));
        }
        if !page.is_loaded {
            *page = match load_page(notebook_dir, page.id) {
                Ok(data) => data.to_page(),
                // Wie beim Lazy Loading: nie gespeicherte Seite = leere Seite
                Err(StorageError::PageNotFound(_)) => Page::new(page.id, page.background),
                Err(e) => return Err(e),
            };
        }
    }
    Ok(())
}

/// Gibt das Standard-Verzeichnis für Notizbücher zurück (gemäß XDG-Spezifikation).
pub fn default_storage_dir() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_DATA_HOME") {
        if !xdg.is_empty() {
            return PathBuf::from(xdg).join("ink-notes").join("notebooks");
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() {
            return PathBuf::from(home)
                .join(".local")
                .join("share")
                .join("ink-notes")
                .join("notebooks");
        }
    }
    PathBuf::from("./data/notebooks")
}

/// Zusammenfassende Metadaten eines Notizbuchs für die Dokumentenliste.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NotebookSummary {
    pub id: String,
    pub title: String,
    pub page_count: usize,
    pub modified_at: u64,
}

/// Durchsucht das Basis-Verzeichnis nach allen vorhandenen Notizbüchern.
pub fn list_notebooks(base_dir: &Path) -> Vec<NotebookSummary> {
    let mut list = Vec::new();
    let Ok(entries) = fs::read_dir(base_dir) else {
        return list;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let manifest_path = path.join("manifest.json");
            if manifest_path.exists() {
                if let Ok(manifest) = load_manifest(&path) {
                    list.push(NotebookSummary {
                        id: manifest.id,
                        title: manifest.title,
                        page_count: manifest.page_ids.len(),
                        modified_at: manifest.modified_at,
                    });
                }
            }
        }
    }

    // Nach letztem Änderungsdatum absteigend sortieren (neueste zuerst)
    list.sort_by_key(|a| std::cmp::Reverse(a.modified_at));
    list
}

/// Erstellt ein neues Notizbuch mit dem angegebenen Titel und initialisiert Seite 1.
pub fn create_new_notebook(
    base_dir: &Path,
    title: &str,
) -> Result<(Notebook, NotebookManifest, PathBuf), StorageError> {
    fs::create_dir_all(base_dir)?;

    let now_ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();

    let clean_slug: String = title
        .chars()
        .filter_map(|c| {
            if c.is_alphanumeric() {
                Some(c.to_ascii_lowercase())
            } else if c == ' ' || c == '-' || c == '_' {
                Some('_')
            } else {
                None
            }
        })
        .collect();
    let clean_slug = clean_slug.trim_matches('_');

    let id = if clean_slug.is_empty() {
        format!("nb_{now_ts}")
    } else {
        format!("{clean_slug}_{now_ts}")
    };

    let notebook_dir = base_dir.join(&id);
    fs::create_dir_all(&notebook_dir)?;

    let notebook = Notebook::new();
    let mut manifest = NotebookManifest::new(&id, title);

    save_notebook(&notebook_dir, &notebook, &mut manifest)?;

    Ok((notebook, manifest, notebook_dir))
}

/// Benennt ein Notizbuch um und aktualisiert das Manifest atomar auf der Festplatte.
pub fn rename_notebook(
    notebook_dir: &Path,
    manifest: &mut NotebookManifest,
    new_title: &str,
) -> Result<(), StorageError> {
    manifest.title = new_title.to_string();
    manifest.modified_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    save_manifest(notebook_dir, manifest)?;
    Ok(())
}

/// Löscht ein Notizbuch vollständig vom Datenträger.
pub fn delete_notebook(notebook_dir: &Path) -> Result<(), StorageError> {
    if notebook_dir.exists() {
        fs::remove_dir_all(notebook_dir)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::autosave::AutosaveController;
    use std::time::Duration;

    #[test]
    fn test_atomic_save_and_reload() {
        let temp_dir = tempfile::tempdir().expect("Konnte temporäres Verzeichnis nicht anlegen");
        let path = temp_dir.path();

        let mut page = Page::new(42, PageBackground::DotGrid);
        let sample1 = StylusSample {
            position: glam::Vec2::new(120.0, 340.0),
            pressure: 0.45,
            timestamp: 1.0,
        };
        let sample2 = StylusSample {
            position: glam::Vec2::new(135.0, 360.0),
            pressure: 0.85,
            timestamp: 1.02,
        };
        let sample3 = StylusSample {
            position: glam::Vec2::new(160.0, 390.0),
            pressure: 0.65,
            timestamp: 1.05,
        };

        let stroke = Stroke::new(
            101,
            vec![sample1, sample2, sample3],
            vec![
                InkVertex::new(118.0, 340.0, [0.1, 0.2, 0.8, 1.0]),
                InkVertex::new(122.0, 340.0, [0.1, 0.2, 0.8, 1.0]),
                InkVertex::new(132.0, 360.0, [0.1, 0.2, 0.8, 1.0]),
                InkVertex::new(138.0, 360.0, [0.1, 0.2, 0.8, 1.0]),
                InkVertex::new(158.0, 390.0, [0.1, 0.2, 0.8, 1.0]),
                InkVertex::new(162.0, 390.0, [0.1, 0.2, 0.8, 1.0]),
            ],
            [0.1, 0.2, 0.8, 1.0],
        );
        page.add_stroke(stroke);

        // Speichern
        let page_data = PageData::from(&page);
        let save_res = save_page(path, &page_data);
        assert!(save_res.is_ok(), "Speichern der Seite muss gelingen");

        // Datei existiert
        let file_path = path.join("page_42.json");
        assert!(file_path.exists());

        // Neu laden
        let load_res = load_page(path, 42);
        assert!(load_res.is_ok(), "Laden der Seite muss gelingen");
        let loaded_data = load_res.unwrap();
        let loaded_page = loaded_data.to_page();

        // Überprüfungen
        assert_eq!(loaded_page.id, 42);
        assert_eq!(loaded_page.background, PageBackground::DotGrid);
        assert_eq!(loaded_page.strokes.len(), 1);

        let loaded_stroke = &loaded_page.strokes[0];
        assert_eq!(loaded_stroke.id, 101);
        assert_eq!(loaded_stroke.color, [0.1, 0.2, 0.8, 1.0]);
        assert_eq!(loaded_stroke.points.len(), 3);

        for (orig, loaded) in page.strokes[0]
            .points
            .iter()
            .zip(loaded_stroke.points.iter())
        {
            assert!((orig.position.x - loaded.position.x).abs() < 1e-4);
            assert!((orig.position.y - loaded.position.y).abs() < 1e-4);
            assert!((orig.pressure - loaded.pressure).abs() < 1e-4);
            assert!((orig.timestamp - loaded.timestamp).abs() < 1e-4);
        }

        // Vertizes wurden deterministisch regeneriert
        assert!(!loaded_stroke.vertices.is_empty());
        assert!(!loaded_page.vertex_cache.is_empty());
        assert!(loaded_page.is_loaded);
    }

    #[test]
    fn test_corrupted_file_recovery() {
        let temp_dir = tempfile::tempdir().expect("Konnte temporäres Verzeichnis nicht anlegen");
        let path = temp_dir.path();

        // Beschädigtes, unvollständiges JSON manuell schreiben
        let corrupted_file = path.join("page_99.json");
        let truncated_json =
            br#"{"id": 99, "background": "Grid5mm", "strokes": [{"id": 1, "color": [0.1, 0.2"#;
        fs::write(&corrupted_file, truncated_json).expect("Konnte Datei nicht schreiben");

        // Laden muss einen abgefangenen CorruptedPage-Fehler liefern und darf nicht panicken
        let load_res = load_page(path, 99);
        assert!(
            load_res.is_err(),
            "Laden beschädigter Daten muss fehlschlagen"
        );

        match load_res.unwrap_err() {
            StorageError::CorruptedPage(id, msg) => {
                assert_eq!(id, 99);
                assert!(!msg.is_empty());
            }
            other => panic!("Erwartete CorruptedPage-Fehler, erhielt: {other:?}"),
        }
    }

    #[test]
    fn test_debounce_timing() {
        let temp_dir = tempfile::tempdir().expect("Konnte temporäres Verzeichnis nicht anlegen");
        let notebook_dir = temp_dir.path().to_path_buf();

        let debounce_duration = Duration::from_millis(150);
        let mut controller = AutosaveController::new(notebook_dir.clone(), debounce_duration);
        let manifest = NotebookManifest::new("test_nb", "Test Notizbuch");

        let mut page = Page::new(1, PageBackground::Grid5mm);

        // 10 schnelle aufeinanderfolgende Änderungen (alle 5 ms, deutlich kürzer als 150 ms Debounce)
        for i in 0..10 {
            let sample = StylusSample {
                position: glam::Vec2::new(10.0 + i as f32, 20.0 + i as f32),
                pressure: 0.5,
                timestamp: i as f64 * 0.01,
            };
            let stroke = Stroke::new(
                i + 1,
                vec![sample],
                vec![InkVertex::new(10.0, 20.0, [0.0, 0.0, 0.0, 1.0])],
                [0.0, 0.0, 0.0, 1.0],
            );
            page.add_stroke(stroke);

            controller.mark_page_dirty(&page, &manifest);
            controller.update();
            std::thread::sleep(Duration::from_millis(5));
        }

        // Während der schnellen Änderungen (ca. 50 ms vergangen) darf noch KEIN Schreibvorgang erfolgt sein!
        assert_eq!(
            controller.total_page_writes(),
            0,
            "Vor Ablauf des Debounce-Fensters darf noch kein Disk-Write stattgefunden haben"
        );

        // Jetzt warten wir, bis das Debounce-Fenster abgelaufen ist (> 150 ms)
        std::thread::sleep(Duration::from_millis(180));
        controller.update();

        // Flush abwarten
        controller.flush().expect("Flush muss gelingen");

        // Es darf genau EIN tatsächlicher Schreibvorgang für die Seite stattgefunden haben
        assert_eq!(
            controller.total_page_writes(),
            1,
            "10 schnelle Änderungen müssen in genau 1 gebündelten Schreibvorgang münden"
        );

        // Verifiziere, dass die Datei existiert und alle 10 Striche enthält
        let loaded = load_page(&notebook_dir, 1).expect("Seite muss lesbar sein");
        assert_eq!(loaded.strokes.len(), 10);

        controller.shutdown();
    }

    #[test]
    fn test_notebook_management() {
        let temp_dir = tempfile::tempdir().expect("Konnte temporäres Verzeichnis nicht anlegen");
        let base_dir = temp_dir.path();

        // 1. Erstes Notizbuch anlegen
        let (nb1, mut m1, dir1) = create_new_notebook(base_dir, "Mathe Vorlesung")
            .expect("Erstellen von Notizbuch 1 muss klappen");
        assert_eq!(m1.title, "Mathe Vorlesung");
        assert_eq!(nb1.pages.len(), 1);
        assert!(dir1.exists());

        // 2. Zweites Notizbuch anlegen
        let (_nb2, m2, dir2) = create_new_notebook(base_dir, "Skizzen & Entwürfe")
            .expect("Erstellen von Notizbuch 2 muss klappen");
        assert_eq!(m2.title, "Skizzen & Entwürfe");
        assert!(dir2.exists());

        // 3. Auflisten
        let summaries = list_notebooks(base_dir);
        assert_eq!(summaries.len(), 2);
        assert!(summaries
            .iter()
            .any(|s| s.id == m1.id && s.title == "Mathe Vorlesung"));
        assert!(summaries
            .iter()
            .any(|s| s.id == m2.id && s.title == "Skizzen & Entwürfe"));

        // 4. Umbenennen
        rename_notebook(&dir1, &mut m1, "Höhere Mathematik").expect("Umbenennen muss klappen");
        assert_eq!(m1.title, "Höhere Mathematik");

        let updated_summaries = list_notebooks(base_dir);
        assert!(updated_summaries
            .iter()
            .any(|s| s.id == m1.id && s.title == "Höhere Mathematik"));

        // 5. Löschen
        delete_notebook(&dir2).expect("Löschen muss klappen");
        assert!(!dir2.exists());

        let final_summaries = list_notebooks(base_dir);
        assert_eq!(final_summaries.len(), 1);
        assert_eq!(final_summaries[0].id, m1.id);
    }

    /// Hilfsfunktion: Seite mit einem einzelnen Strich.
    fn page_with_stroke(page_id: u64, stroke_id: u64) -> Page {
        let mut page = Page::new(page_id, PageBackground::Lined);
        let samples = vec![
            StylusSample {
                position: glam::Vec2::new(100.0, 100.0),
                pressure: 0.5,
                timestamp: 0.0,
            },
            StylusSample {
                position: glam::Vec2::new(200.0, 150.0),
                pressure: 0.5,
                timestamp: 0.05,
            },
        ];
        let vertices = reconstruct_stroke_vertices(&samples, [0.0, 0.0, 0.0, 1.0]);
        page.add_stroke(Stroke::new(
            stroke_id,
            samples,
            vertices,
            [0.0, 0.0, 0.0, 1.0],
        ));
        page
    }

    #[test]
    fn test_corrupted_page_is_quarantined_not_overwritten() {
        let temp_dir = tempfile::tempdir().unwrap();
        let dir = temp_dir.path();

        let mut notebook = Notebook::new();
        notebook.add_page(PageBackground::Grid5mm);
        let mut manifest = NotebookManifest::new("nb", "Test");
        save_notebook(dir, &notebook, &mut manifest).unwrap();

        let corrupted = br#"{"id": 2, "background": "Lined", "strokes": [{"id": 1"#;
        fs::write(dir.join("page_2.json"), corrupted).unwrap();

        let (mut loaded, loaded_manifest) = load_notebook(dir).unwrap();
        assert!(ensure_page_loaded(dir, &mut loaded, 1).is_err());

        // Original wurde beiseite gelegt und bleibt Byte für Byte erhalten
        let backups: Vec<_> = fs::read_dir(dir)
            .unwrap()
            .flatten()
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("page_2.json.corrupt-")
            })
            .collect();
        assert_eq!(backups.len(), 1);
        assert_eq!(fs::read(backups[0].path()).unwrap(), corrupted);

        // Ersatzseite ist normal beschreibbar
        assert!(!loaded.pages[1].load_failed);
        let mut autosave = AutosaveController::new(dir.to_path_buf(), Duration::from_secs(60));
        loaded.current_page_index = 1;
        autosave.mark_dirty(&loaded, &loaded_manifest);
        autosave.flush().unwrap();
        assert!(load_page(dir, 2).is_ok());
        autosave.shutdown();
    }

    #[test]
    fn test_unreadable_page_is_never_overwritten() {
        let temp_dir = tempfile::tempdir().unwrap();
        let dir = temp_dir.path();

        let mut notebook = Notebook::new();
        notebook.add_page(PageBackground::Grid5mm);
        let mut manifest = NotebookManifest::new("nb", "Test");
        save_notebook(dir, &notebook, &mut manifest).unwrap();

        // Unlesbare Seite simulieren (I/O-Fehler): Inhalt liegt in einer Datei, die wir
        // durch ein gleichnamiges Verzeichnis ersetzen
        fs::remove_file(dir.join("page_2.json")).unwrap();
        fs::create_dir(dir.join("page_2.json")).unwrap();
        fs::write(dir.join("page_2.json").join("marker"), b"original").unwrap();

        let (mut loaded, loaded_manifest) = load_notebook(dir).unwrap();
        assert!(ensure_page_loaded(dir, &mut loaded, 1).is_err());
        assert!(loaded.pages[1].load_failed);

        // Navigation + Autosave + Strg+S dürfen die Seite nicht anfassen
        loaded.current_page_index = 1;
        let mut autosave = AutosaveController::new(dir.to_path_buf(), Duration::from_secs(60));
        autosave.mark_dirty(&loaded, &loaded_manifest);
        autosave.save_now(&loaded, &loaded_manifest).unwrap();
        save_notebook(dir, &loaded, &mut loaded_manifest.clone()).unwrap();
        autosave.shutdown();

        assert_eq!(
            fs::read(dir.join("page_2.json").join("marker")).unwrap(),
            b"original"
        );
        // Die Seite bleibt im Manifest referenziert
        assert_eq!(load_manifest(dir).unwrap().page_ids, vec![1, 2]);
    }

    #[test]
    fn test_autosave_records_new_pages_in_manifest() {
        let temp_dir = tempfile::tempdir().unwrap();
        let dir = temp_dir.path();

        let mut notebook = Notebook::new();
        let mut manifest = NotebookManifest::new("nb", "Test");
        save_notebook(dir, &notebook, &mut manifest).unwrap();

        // Wie in main.rs: neue Seite per Navigation, Strich darauf, Autosave
        notebook.next_page();
        notebook.pages[1] = page_with_stroke(notebook.pages[1].id, 1);
        let mut autosave = AutosaveController::new(dir.to_path_buf(), Duration::from_secs(60));
        autosave.mark_dirty(&notebook, &manifest);
        autosave.shutdown();

        let (mut reloaded, reloaded_manifest) = load_notebook(dir).unwrap();
        assert_eq!(reloaded_manifest.page_ids, vec![1, 2]);
        assert_eq!(reloaded.pages.len(), 2);
        ensure_page_loaded(dir, &mut reloaded, 1).unwrap();
        assert_eq!(reloaded.pages[1].strokes.len(), 1);
    }

    #[test]
    fn test_flush_and_save_now_report_write_errors() {
        let temp_dir = tempfile::tempdir().unwrap();
        // Zielpfad ist eine Datei -> Schreiben ins "Notizbuch-Verzeichnis" muss fehlschlagen
        let blocked = temp_dir.path().join("blocked");
        fs::write(&blocked, b"").unwrap();

        let notebook = Notebook::new();
        let manifest = NotebookManifest::new("nb", "Test");
        let mut autosave = AutosaveController::new(blocked, Duration::from_secs(60));

        autosave.mark_dirty(&notebook, &manifest);
        assert!(autosave.flush().is_err());
        assert_eq!(autosave.status(), crate::autosave::AutosaveStatus::Error);

        assert!(autosave.save_now(&notebook, &manifest).is_err());
        assert_eq!(autosave.status(), crate::autosave::AutosaveStatus::Error);
        autosave.shutdown();
    }

    #[test]
    fn test_deleted_notebook_is_not_recreated_by_pending_autosave() {
        let temp_dir = tempfile::tempdir().unwrap();
        let base = temp_dir.path();
        let (notebook, manifest, dir) = create_new_notebook(base, "Weg damit").unwrap();
        let (_, _, other_dir) = create_new_notebook(base, "Bleibt").unwrap();

        let mut autosave = AutosaveController::new(dir.clone(), Duration::from_secs(60));
        autosave.mark_dirty(&notebook, &manifest);

        // Ablauf wie in main.rs beim Löschen des aktuellen Notizbuchs
        autosave.flush().unwrap();
        delete_notebook(&dir).unwrap();
        autosave.switch_notebook_dir(other_dir).unwrap();
        autosave.shutdown();

        assert!(
            !dir.exists(),
            "Gelöschtes Notizbuch darf nicht wieder auftauchen"
        );
    }

    #[test]
    fn test_load_all_pages_for_export() {
        let temp_dir = tempfile::tempdir().unwrap();
        let dir = temp_dir.path();

        let mut notebook = Notebook::new();
        notebook.add_page(PageBackground::Lined);
        notebook.pages[1] = page_with_stroke(2, 1);
        let mut manifest = NotebookManifest::new("nb", "Test");
        save_notebook(dir, &notebook, &mut manifest).unwrap();

        let (mut loaded, _) = load_notebook(dir).unwrap();
        assert!(!loaded.pages[1].is_loaded);
        assert!(crate::export::generate_pdf_document(&loaded, Default::default()).is_err());

        load_all_pages(dir, &mut loaded).unwrap();
        assert_eq!(loaded.pages[1].strokes.len(), 1);
        assert_eq!(loaded.pages[1].background, PageBackground::Lined);
        assert!(crate::export::generate_pdf_document(&loaded, Default::default()).is_ok());
    }
}
