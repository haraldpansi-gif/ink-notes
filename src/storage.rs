//! Robuste lokale Notizbuch-Persistenz mit atomaren Writes (Crash-Resistenz)
//! und semantischer Datenspeicherung (ohne gecachte GPU-Vertizes).

use std::error::Error;
use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::canvas::InkVertex;
use crate::document::{
    Notebook, Page, PageBackground, PageLoadWarning, PersistState, Stroke, StylusSample,
};
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

/// Aktuelle Dateiformat-Version von ink-notes.
pub const CURRENT_FORMAT_VERSION: u32 = 1;

/// Standardwert für ältere Dateien ohne explizite Formatversion (Legacy v0).
pub fn legacy_format_version() -> u32 {
    0
}

/// Serialisierbare semantische Seitendaten.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PageData {
    #[serde(default = "legacy_format_version")]
    pub format_version: u32,
    pub id: u64,
    pub background: PageBackground,
    pub strokes: Vec<StrokeData>,
}

impl From<&Page> for PageData {
    fn from(page: &Page) -> Self {
        Self {
            format_version: CURRENT_FORMAT_VERSION,
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

/// Metadaten einer einzelnen Seite im Manifest.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PageMeta {
    pub id: u64,
    #[serde(default)]
    pub background: Option<PageBackground>,
}

impl PageMeta {
    pub fn new(id: u64, background: Option<PageBackground>) -> Self {
        Self { id, background }
    }
}

/// Metadaten des gesamten Notizbuchs (`manifest.json`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NotebookManifest {
    #[serde(default = "legacy_format_version")]
    pub format_version: u32,
    pub id: String,
    pub title: String,
    pub created_at: u64,
    pub modified_at: u64,
    pub current_page_index: usize,
    #[serde(default)]
    pub pages: Vec<PageMeta>,
    #[serde(default)]
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
            format_version: CURRENT_FORMAT_VERSION,
            id: id.into(),
            title: title.into(),
            created_at: now,
            modified_at: now,
            current_page_index: 0,
            pages: vec![PageMeta::new(1, Some(PageBackground::Grid5mm))],
            page_ids: vec![1],
            next_stroke_id: 1,
            next_page_id: 2,
        }
    }

    pub fn page_ids(&self) -> Vec<u64> {
        if !self.pages.is_empty() {
            self.pages.iter().map(|p| p.id).collect()
        } else {
            self.page_ids.clone()
        }
    }

    pub fn background_for_page(&self, page_id: u64) -> Option<PageBackground> {
        self.pages
            .iter()
            .find(|p| p.id == page_id)
            .and_then(|p| p.background)
    }

    pub fn sync_from_notebook(&mut self, notebook: &Notebook) {
        self.format_version = CURRENT_FORMAT_VERSION;
        self.pages = notebook
            .pages
            .iter()
            .map(|p| PageMeta {
                id: p.id,
                background: Some(p.background),
            })
            .collect();
        self.page_ids = self.pages.iter().map(|p| p.id).collect();
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
    UnsupportedVersion { found: u32, supported: u32 },
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
            StorageError::UnsupportedVersion { found, supported } => write!(
                f,
                "Notizbuch wurde mit neuerer Version erstellt (Formatversion {found}, unterstützt wird bis {supported})"
            ),
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
    if path.exists() {
        let bak_path = notebook_dir.join("manifest.json.v0.bak");
        if !bak_path.exists() {
            if let Ok(content) = fs::read_to_string(&path) {
                if let Ok(old_m) = serde_json::from_str::<NotebookManifest>(&content) {
                    if old_m.format_version == 0 {
                        let _ = fs::copy(&path, &bak_path);
                    }
                }
            }
        }
    }
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
    let mut manifest: NotebookManifest = serde_json::from_str(&json_str)?;

    if manifest.format_version > CURRENT_FORMAT_VERSION {
        return Err(StorageError::UnsupportedVersion {
            found: manifest.format_version,
            supported: CURRENT_FORMAT_VERSION,
        });
    }

    if manifest.pages.is_empty() && !manifest.page_ids.is_empty() {
        manifest.pages = manifest
            .page_ids
            .iter()
            .map(|&id| PageMeta::new(id, None))
            .collect();
    }
    Ok(manifest)
}

/// Verschiebt eine defekte Seitendatei nach `page_<id>.json.corrupt-<unix_ts>`.
/// Nur bei Parse-/Schemafehlern aufrufen, nicht bei I/O-Fehlern!
pub fn quarantine_corrupted_page(notebook_dir: &Path, page_id: u64) -> Result<PathBuf, io::Error> {
    let source_path = notebook_dir.join(format!("page_{page_id}.json"));
    if !source_path.exists() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "Datei existiert nicht",
        ));
    }
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let mut target_path = notebook_dir.join(format!("page_{page_id}.json.corrupt-{ts}"));
    if target_path.exists() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos();
        target_path = notebook_dir.join(format!("page_{page_id}.json.corrupt-{ts}-{nanos}"));
    }
    fs::rename(&source_path, &target_path)?;
    Ok(target_path)
}

/// Speichert eine Seite atomar in `page_<id>.json`.
pub fn save_page(notebook_dir: &Path, page: &PageData) -> Result<(), StorageError> {
    let path = notebook_dir.join(format!("page_{}.json", page.id));
    if path.exists() {
        let bak_path = notebook_dir.join(format!("page_{}.json.v0.bak", page.id));
        if !bak_path.exists() {
            if let Ok(content) = fs::read_to_string(&path) {
                if let Ok(old_p) = serde_json::from_str::<PageData>(&content) {
                    if old_p.format_version == 0 {
                        let _ = fs::copy(&path, &bak_path);
                    }
                }
            }
        }
    }
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

    let page_data = match serde_json::from_str::<PageData>(&json_str) {
        Ok(data) => data,
        Err(e) => {
            eprintln!(
                "[Storage-Warnung] Seite {page_id} in '{}' ist beschädigt: {e}",
                path.display()
            );
            return Err(StorageError::CorruptedPage(page_id, e.to_string()));
        }
    };

    if page_data.format_version > CURRENT_FORMAT_VERSION {
        return Err(StorageError::UnsupportedVersion {
            found: page_data.format_version,
            supported: CURRENT_FORMAT_VERSION,
        });
    }

    Ok(page_data)
}

/// Speichert ein gesamtes Notebook synchron auf die Festplatte (z. B. initial oder beim Flush).
pub fn save_notebook(
    notebook_dir: &Path,
    notebook: &Notebook,
    manifest: &mut NotebookManifest,
) -> Result<(), StorageError> {
    fs::create_dir_all(notebook_dir)?;

    manifest.sync_from_notebook(notebook);

    // Alle bereits geladenen Seiten speichern, schreibgeschützte Seiten nach Ladefehler überspringen!
    for page in &notebook.pages {
        if page.is_loaded && page.persist == PersistState::Normal {
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
    let page_ids = manifest.page_ids();
    if page_ids.is_empty() {
        let nb = Notebook::new();
        return Ok((nb, manifest));
    }

    let mut pages = Vec::with_capacity(page_ids.len());
    let mut load_warnings = std::collections::HashMap::new();

    // Erste Seite sofort laden und initialisieren
    let first_id = page_ids[0];
    let first_bg = manifest
        .background_for_page(first_id)
        .unwrap_or(PageBackground::Grid5mm);

    let first_page = match load_page(notebook_dir, first_id) {
        Ok(data) => data.to_page(),
        Err(StorageError::CorruptedPage(id, err_msg)) => {
            let q_res = quarantine_corrupted_page(notebook_dir, id);
            let q_path = q_res.ok();
            eprintln!(
                "[Storage] Erste Seite {id} ist beschädigt ({err_msg}). Quarantänisiert nach: {:?}",
                q_path
            );
            load_warnings.insert(
                id,
                PageLoadWarning {
                    page_id: id,
                    message: err_msg,
                    quarantine_path: q_path,
                },
            );
            let mut page = Page::new(id, first_bg);
            page.persist = PersistState::ReadOnlyAfterLoadError;
            page
        }
        Err(StorageError::Io(e)) => {
            eprintln!(
                "[Storage] I/O-Fehler beim Laden von Seite {first_id}: {e}. Seite wird schreibgeschützt."
            );
            load_warnings.insert(
                first_id,
                PageLoadWarning {
                    page_id: first_id,
                    message: format!("I/O-Fehler: {e}"),
                    quarantine_path: None,
                },
            );
            let mut page = Page::new(first_id, first_bg);
            page.persist = PersistState::ReadOnlyAfterLoadError;
            page
        }
        Err(e) => {
            eprintln!("[Storage] Konnte erste Seite {first_id} nicht laden: {e}. Erstelle leere Ersatzseite.");
            let mut page = Page::new(first_id, first_bg);
            page.persist = PersistState::ReadOnlyAfterLoadError;
            page
        }
    };
    pages.push(first_page);

    // Folgeseiten als ungeladene Platzhalter anlegen (Lazy Loading)
    for &page_id in &page_ids[1..] {
        let bg = manifest
            .background_for_page(page_id)
            .unwrap_or(PageBackground::Grid5mm);
        pages.push(Page::new_unloaded(page_id, bg));
    }

    let mut notebook = Notebook::from_pages(
        pages,
        manifest.current_page_index,
        manifest.next_stroke_id,
        manifest.next_page_id,
    );
    notebook.load_warnings = load_warnings;

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
        Err(StorageError::CorruptedPage(id, err_msg)) => {
            let q_res = quarantine_corrupted_page(notebook_dir, id);
            let q_path = q_res.ok();
            eprintln!(
                "[Storage] Lazy Loading von Seite {id} fehlgeschlagen ({err_msg}). Quarantäne: {:?}",
                q_path
            );
            notebook.load_warnings.insert(
                id,
                PageLoadWarning {
                    page_id: id,
                    message: err_msg.clone(),
                    quarantine_path: q_path,
                },
            );
            notebook.pages[page_idx].is_loaded = true;
            notebook.pages[page_idx].persist = PersistState::ReadOnlyAfterLoadError;
            notebook.pages[page_idx].rebuild_cache_if_dirty();
            Err(StorageError::CorruptedPage(id, err_msg))
        }
        Err(StorageError::Io(e)) => {
            eprintln!("[Storage] I/O-Fehler beim Lazy Loading von Seite {page_id}: {e}");
            let err_str = e.to_string();
            notebook.load_warnings.insert(
                page_id,
                PageLoadWarning {
                    page_id,
                    message: format!("I/O-Fehler: {err_str}"),
                    quarantine_path: None,
                },
            );
            notebook.pages[page_idx].is_loaded = true;
            notebook.pages[page_idx].persist = PersistState::ReadOnlyAfterLoadError;
            notebook.pages[page_idx].rebuild_cache_if_dirty();
            Err(StorageError::Io(e))
        }
        Err(e) => {
            eprintln!("[Storage] Fehler beim Lazy Loading von Seite {page_id}: {e}");
            notebook.pages[page_idx].is_loaded = true;
            notebook.pages[page_idx].persist = PersistState::ReadOnlyAfterLoadError;
            notebook.pages[page_idx].rebuild_cache_if_dirty();
            Err(e)
        }
    }
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
    use std::sync::Arc;
    use std::time::{Duration, Instant};

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
        use crate::autosave::tests::MockClock;

        let temp_dir = tempfile::tempdir().expect("Konnte temporäres Verzeichnis nicht anlegen");
        let notebook_dir = temp_dir.path().to_path_buf();

        let clock = Arc::new(MockClock::new(Instant::now()));
        let debounce_duration = Duration::from_millis(150);
        let max_wait = Duration::from_secs(10);
        let store = Arc::new(crate::autosave::DiskPageStore);

        let mut controller = AutosaveController::with_options(
            notebook_dir.clone(),
            debounce_duration,
            max_wait,
            store,
            Arc::clone(&clock) as Arc<dyn crate::autosave::Clock>,
        );
        let manifest = NotebookManifest::new("test_nb", "Test Notizbuch");

        let mut page = Page::new(1, PageBackground::Grid5mm);

        // 10 schnelle aufeinanderfolgende Änderungen (alle 5 ms mit MockClock, keine Wartezeit)
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
            clock.advance(Duration::from_millis(5));
            controller.update();
        }

        // Während der schnellen Änderungen darf noch KEIN Schreibvorgang erfolgt sein!
        assert_eq!(
            controller.total_page_writes(),
            0,
            "Vor Ablauf des Debounce-Fensters darf noch kein Disk-Write stattgefunden haben"
        );

        // Jetzt drehen wir die Mock-Uhr um 180 ms vor (> 150 ms Debounce)
        clock.advance(Duration::from_millis(180));
        controller.update();

        // Flush abwarten
        let flush_res = controller.flush();
        assert!(flush_res.is_ok(), "Flush muss erfolgreich sein");

        // Es darf genau EIN tatsächlicher Schreibvorgang für die Seite stattgefunden haben
        assert_eq!(
            controller.total_page_writes(),
            1,
            "10 schnelle Änderungen müssen in genau 1 gebündelten Schreibvorgang münden"
        );

        // Verifiziere, dass die Datei existiert und alle 10 Striche enthält
        let loaded = load_page(&notebook_dir, 1).expect("Seite muss lesbar sein");
        assert_eq!(loaded.strokes.len(), 10);

        let shutdown_res = controller.shutdown();
        assert!(shutdown_res.is_ok());
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

    #[test]
    fn test_load_corrupted_page_quarantines_and_does_not_overwrite() {
        use crate::document::PersistState;

        let temp_dir = tempfile::tempdir().expect("tempdir");
        let path = temp_dir.path();

        let manifest = NotebookManifest::new("nb_corrupt", "Test Corrupt");
        save_manifest(path, &manifest).unwrap();

        let corrupted_content = br#"{"id": 1, "background": "Grid5mm", "strokes": INVALID_JSON"#;
        let page_file = path.join("page_1.json");
        fs::write(&page_file, corrupted_content).unwrap();

        // Load notebook
        let (nb, mut m) =
            load_notebook(path).expect("load_notebook should succeed with fallback page");

        // (1) Check that page_1.json was quarantined
        let dir_entries: Vec<String> = fs::read_dir(path)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        let corrupt_entry = dir_entries
            .iter()
            .find(|name| name.starts_with("page_1.json.corrupt-"));
        assert!(
            corrupt_entry.is_some(),
            "Corrupt file must be renamed to page_1.json.corrupt-<ts>, found entries: {dir_entries:?}"
        );
        let corrupt_file = path.join(corrupt_entry.unwrap());
        assert_eq!(
            fs::read(&corrupt_file).unwrap(),
            corrupted_content,
            "Quarantine file must contain original corrupt content"
        );

        // (2) The replacement page must be in ReadOnlyAfterLoadError state
        assert_eq!(
            nb.pages[0].persist,
            PersistState::ReadOnlyAfterLoadError,
            "Page must be marked ReadOnlyAfterLoadError"
        );

        // (3) Calling save_notebook must NOT overwrite or create a blank page_1.json!
        save_notebook(path, &nb, &mut m).unwrap();

        assert!(
            !page_file.exists(),
            "save_notebook must NOT recreate page_1.json from an unedited/locked replacement page"
        );
        assert!(
            corrupt_file.exists(),
            "Quarantined file must remain intact after save_notebook"
        );
    }

    #[test]
    #[cfg(unix)]
    fn test_io_error_does_not_quarantine_and_does_not_overwrite() {
        use crate::document::PersistState;
        use std::os::unix::fs::PermissionsExt;

        // Skip test if running as root
        if std::env::var("USER").unwrap_or_default() == "root" {
            return;
        }

        let temp_dir = tempfile::tempdir().expect("tempdir");
        let path = temp_dir.path();

        let manifest = NotebookManifest::new("nb_io", "Test IO");
        save_manifest(path, &manifest).unwrap();

        let original_content = br#"{"id": 1, "background": "Grid5mm", "strokes": []}"#;
        let page_file = path.join("page_1.json");
        fs::write(&page_file, original_content).unwrap();

        // Make file unreadable
        fs::set_permissions(&page_file, fs::Permissions::from_mode(0o000)).unwrap();

        let (nb, mut m) =
            load_notebook(path).expect("load_notebook should succeed with fallback page");

        // Restore permissions for inspection
        fs::set_permissions(&page_file, fs::Permissions::from_mode(0o600)).unwrap();

        let dir_entries: Vec<String> = fs::read_dir(path)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        let corrupt_entry = dir_entries.iter().find(|name| name.contains("corrupt"));
        assert!(
            corrupt_entry.is_none(),
            "I/O error must NOT trigger quarantine, found: {corrupt_entry:?}"
        );

        assert_eq!(
            nb.pages[0].persist,
            PersistState::ReadOnlyAfterLoadError,
            "Page must be marked ReadOnlyAfterLoadError on I/O error"
        );

        save_notebook(path, &nb, &mut m).unwrap();
        assert_eq!(
            fs::read(&page_file).unwrap(),
            original_content,
            "Original file content must not be overwritten"
        );
    }

    #[test]
    fn test_edit_replacement_saves_only_after_unlock() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let path = temp_dir.path();

        let manifest = NotebookManifest::new("nb_unlock", "Test Unlock");
        save_manifest(path, &manifest).unwrap();

        let corrupted_content = br#"CORRUPTED_JSON_DATA"#;
        let page_file = path.join("page_1.json");
        fs::write(&page_file, corrupted_content).unwrap();

        let (mut nb, mut m) = load_notebook(path).expect("load_notebook");

        save_notebook(path, &nb, &mut m).unwrap();
        assert!(!page_file.exists());

        // Now user explicitly acknowledges and unlocks
        nb.pages[0].unlock_after_load_error();
        nb.pages[0].add_stroke(Stroke::new(1, vec![], vec![], [1.0, 0.0, 0.0, 1.0]));

        save_notebook(path, &nb, &mut m).unwrap();
        assert!(
            page_file.exists(),
            "Page must be saved after explicit unlock and edit"
        );

        let dir_entries: Vec<String> = fs::read_dir(path)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        assert!(dir_entries
            .iter()
            .any(|name| name.starts_with("page_1.json.corrupt-")));
    }

    #[test]
    fn test_fixture_legacy_v0_loads_and_migrates_with_backup() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let path = temp_dir.path();

        let root_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let fixture_manifest = root_dir.join("tests/fixtures/legacy_v0_manifest.json");
        let fixture_page = root_dir.join("tests/fixtures/legacy_v0_page_1.json");

        fs::copy(&fixture_manifest, path.join("manifest.json")).unwrap();
        fs::copy(&fixture_page, path.join("page_1.json")).unwrap();

        let (nb, mut m) = load_notebook(path).expect("Legacy v0 notebook must load");
        assert_eq!(m.format_version, 0, "Legacy manifest has format_version 0");
        assert_eq!(nb.pages.len(), 1);
        assert_eq!(nb.pages[0].background, PageBackground::DotGrid);
        assert_eq!(nb.pages[0].strokes.len(), 1);

        // Saving must upgrade to format_version 1 and create .v0.bak files
        save_notebook(path, &nb, &mut m).expect("Save must succeed");

        assert!(
            path.join("manifest.json.v0.bak").exists(),
            "manifest.json.v0.bak must be created before upgrading legacy format"
        );
        assert!(
            path.join("page_1.json.v0.bak").exists(),
            "page_1.json.v0.bak must be created before upgrading legacy format"
        );

        // Reload to verify format_version is now 1
        let (_nb_upgraded, m_upgraded) = load_notebook(path).expect("Upgraded notebook must load");
        assert_eq!(m_upgraded.format_version, 1);
        let p1_upgraded = load_page(path, 1).expect("Upgraded page must load");
        assert_eq!(p1_upgraded.format_version, 1);
    }

    #[test]
    fn test_fixture_unknown_fields_load_successfully() {
        let root_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let fixture_page = root_dir.join("tests/fixtures/unknown_fields_page_1.json");
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let path = temp_dir.path();
        fs::copy(&fixture_page, path.join("page_1.json")).unwrap();

        let page_data = load_page(path, 1).expect("Unknown fields must be tolerated");
        assert_eq!(page_data.background, PageBackground::Lined);
        assert_eq!(page_data.format_version, 1);
    }

    #[test]
    fn test_fixture_future_version_is_rejected() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let path = temp_dir.path();

        let root_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let fixture_manifest = root_dir.join("tests/fixtures/future_v99_manifest.json");
        let fixture_page = root_dir.join("tests/fixtures/future_v99_page_1.json");

        let dest_manifest = path.join("manifest.json");
        let dest_page = path.join("page_1.json");
        fs::copy(&fixture_manifest, &dest_manifest).unwrap();
        fs::copy(&fixture_page, &dest_page).unwrap();

        let orig_manifest_content = fs::read(&dest_manifest).unwrap();
        let orig_page_content = fs::read(&dest_page).unwrap();

        // Loading must fail with UnsupportedVersion
        let load_res = load_notebook(path);
        assert!(load_res.is_err(), "Future version must be rejected");
        match load_res.unwrap_err() {
            StorageError::UnsupportedVersion { found, supported } => {
                assert_eq!(found, 99);
                assert_eq!(supported, CURRENT_FORMAT_VERSION);
            }
            other => panic!("Expected UnsupportedVersion, got: {other:?}"),
        }

        // Files on disk must remain completely untouched
        assert_eq!(fs::read(&dest_manifest).unwrap(), orig_manifest_content);
        assert_eq!(fs::read(&dest_page).unwrap(), orig_page_content);
    }
}
