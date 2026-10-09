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
use crate::t;

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

/// Standardwert für die Strichbreite bei Migration älterer Dateien (0.5 mm in Einheiten).
pub fn default_stroke_width() -> f32 {
    5.0
}

/// Standardwert für den Finalisierungsstatus bei Migration älterer Dateien.
pub fn default_finalized() -> bool {
    true
}

/// Serialisierbare semantische Strichdaten (ohne GPU-Vertizes).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StrokeData {
    pub id: u64,
    pub color: [f32; 4],
    pub samples: Vec<SampleData>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shape: Option<crate::geometry::RecognizedShape>,
    #[serde(default)]
    pub tool: crate::document::StrokeTool,
    #[serde(default = "default_stroke_width")]
    pub width: f32,
    #[serde(default = "default_finalized")]
    pub finalized: bool,
    /// Eingefügtes Bild (Name im Asset-Ordner); die Samples sind dann die vier Bildecken
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    /// Textfeld; die Samples sind dann die vier Ecken des Felds
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<crate::text::TextBox>,
}

impl From<&Stroke> for StrokeData {
    fn from(stroke: &Stroke) -> Self {
        Self {
            id: stroke.id,
            color: stroke.color,
            samples: stroke.points.iter().map(SampleData::from).collect(),
            shape: stroke.shape.clone(),
            tool: stroke.tool,
            width: stroke.width,
            finalized: stroke.finalized,
            image: stroke.image.clone(),
            text: stroke.text.clone(),
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
        if let (Some(text), [a, b, c, d, ..]) = (&self.text, stylus_samples.as_slice()) {
            return Stroke::new_text(
                self.id,
                text.clone(),
                [a.position, b.position, c.position, d.position],
                self.color,
            );
        }
        if let Some(asset) = &self.image {
            if let [a, b, c, d, ..] = stylus_samples.as_slice() {
                let mut stroke = Stroke::new_image(
                    self.id,
                    asset.clone(),
                    [a.position, b.position, c.position, d.position],
                );
                stroke.color = self.color;
                return stroke;
            }
        }
        let width = if self.width > 0.0 {
            self.width
        } else if self.shape.is_some() {
            20.0
        } else {
            5.0
        };
        let vertices = if let Some(ref shape) = self.shape {
            let (_, shape_verts) = crate::geometry::shape_to_mesh(shape, width, self.color);
            shape_verts
        } else {
            reconstruct_stroke_vertices(
                &stylus_samples,
                self.color,
                width,
                self.tool,
                self.finalized,
            )
        };
        let mut stroke = Stroke::new(self.id, stylus_samples, vertices, self.color);
        stroke.shape = self.shape.clone();
        stroke.tool = crate::document::StrokeTool::resolve_stored(
            self.tool,
            self.color,
            self.shape.is_some(),
        );
        stroke.width = width;
        stroke.finalized = self.finalized;
        stroke
    }
}

/// Aktuelle Dateiformat-Version von ink-notes.
/// - Version 2: Stroke-Treue mit tool, width, finalized
/// - Version 3: neue Formen (Ellipse, Pfeil). Ältere App-Versionen kennen sie nicht und würden
///   betroffene Seiten für beschädigt halten; mit Version 3 lehnen sie das Notizbuch stattdessen
///   sauber als „neuere Version“ ab.
/// - Version 4: eingefügte Bilder (`image` am Strich, Ordner `assets/`). Ältere Versionen würden
///   das unbekannte Feld ignorieren, Bilder als Rechteckstrich anzeigen und beim Speichern
///   verlieren.
/// - Version 5: importierte PDF-Seiten als Seitenhintergrund (`pdf_background`).
/// - Version 6: Seitentitel (`title` in Seite und Manifest).
/// - Version 7: Textfelder (`text` am Strich).
/// - Version 8: Seitengröße je Seite (`size` in Seite und Manifest), Querformat für Folien.
pub const CURRENT_FORMAT_VERSION: u32 = 8;

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
    /// Importierte PDF-Seite als Hintergrund
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pdf_background: Option<crate::document::PdfBackground>,
    /// Seitentitel (steht zusätzlich im Manifest)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Seitengröße in Seiteneinheiten, wenn nicht DIN A4 hoch (steht zusätzlich im Manifest)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<[f32; 2]>,
    pub strokes: Vec<StrokeData>,
}

/// Gespeicherte Seitengröße: `None` für DIN A4 hoch.
fn stored_size(size: glam::Vec2) -> Option<[f32; 2]> {
    (size != crate::document::A4_SIZE).then(|| size.to_array())
}

/// Seitengröße aus einer (evtl. von Hand bearbeiteten) Datei; unbrauchbare Werte ergeben A4.
pub fn page_size_from(stored: Option<[f32; 2]>) -> glam::Vec2 {
    match stored {
        Some([w, h]) if w.is_finite() && h.is_finite() && w >= 100.0 && h >= 100.0 => {
            glam::Vec2::new(w.min(20_000.0), h.min(20_000.0))
        }
        _ => crate::document::A4_SIZE,
    }
}

impl From<&Page> for PageData {
    fn from(page: &Page) -> Self {
        Self {
            format_version: CURRENT_FORMAT_VERSION,
            id: page.id,
            background: page.background,
            pdf_background: page.pdf_background.clone(),
            title: page.title.clone(),
            size: stored_size(page.size),
            strokes: page.strokes.iter().map(StrokeData::from).collect(),
        }
    }
}

impl PageData {
    /// Wandelt die Seitendaten in eine runtime-aktive `Page` mit initialisiertem Cache um.
    pub fn to_page(&self) -> Page {
        let mut page = Page::new(self.id, self.background).with_size(page_size_from(self.size));
        page.pdf_background = self.pdf_background.clone();
        page.title = self.title.clone();
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
    /// Seitentitel (Lesezeichen), damit die Seitenübersicht ihn ohne Laden der Seite kennt
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Seitengröße, wenn nicht DIN A4 hoch (für Lage und Maßstab ungeladener Seiten)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<[f32; 2]>,
}

impl PageMeta {
    pub fn new(id: u64, background: Option<PageBackground>) -> Self {
        Self {
            id,
            background,
            title: None,
            size: None,
        }
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

    pub fn title_for_page(&self, page_id: u64) -> Option<String> {
        self.pages
            .iter()
            .find(|p| p.id == page_id)
            .and_then(|p| p.title.clone())
    }

    pub fn size_for_page(&self, page_id: u64) -> glam::Vec2 {
        page_size_from(
            self.pages
                .iter()
                .find(|p| p.id == page_id)
                .and_then(|p| p.size),
        )
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
                title: p.title.clone(),
                size: stored_size(p.size),
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
    AlreadyLocked(String),
    PathTraversal(String),
}

impl fmt::Display for StorageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StorageError::Io(e) => f.write_str(&t!("I/O-Fehler: {e}", e = e)),
            StorageError::Serialization(e) => {
                f.write_str(&t!("Serialisierungsfehler: {e}", e = e))
            }
            StorageError::InvalidManifest(msg) => {
                f.write_str(&t!("Ungültiges Manifest: {msg}", msg = msg))
            }
            StorageError::CorruptedPage(id, msg) => {
                f.write_str(&t!("Beschädigte Seite {id}: {msg}", id = id, msg = msg))
            }
            StorageError::PageNotFound(id) => {
                f.write_str(&t!("Seite {id} nicht gefunden", id = id))
            }
            StorageError::UnsupportedVersion { found, supported } => f.write_str(&t!(
                "Notizbuch wurde mit neuerer Version erstellt (Formatversion {found}, unterstützt wird bis {supported})",
                found = found,
                supported = supported
            )),
            StorageError::AlreadyLocked(msg) => {
                f.write_str(&t!("Notizbuch ist gesperrt: {msg}", msg = msg))
            }
            StorageError::PathTraversal(msg) => {
                f.write_str(&t!("Ungültiger Pfad (Path Traversal): {msg}", msg = msg))
            }
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
pub fn reconstruct_stroke_vertices(
    samples: &[StylusSample],
    color: [f32; 4],
    width: f32,
    _tool: crate::document::StrokeTool,
    finalized: bool,
) -> Vec<InkVertex> {
    if samples.is_empty() {
        return Vec::new();
    }

    let config = SplineConfig::from_width_units(width);

    if finalized {
        let corners = spline::detect_corners(samples, spline::CORNER_ANGLE_THRESHOLD_DEG);
        let strip = spline::generate_finalized_triangle_strip(samples, &corners, &config);
        strip
            .into_iter()
            .map(|v| InkVertex::new(v.position[0], v.position[1], color))
            .collect()
    } else {
        if samples.len() == 1 {
            let pt = samples[0];
            let radius =
                spline::calculate_radius(pt.pressure, config.min_radius, config.max_radius);
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

        let raw_strip = spline::generate_triangle_strip(&control_points, &config);
        raw_strip
            .into_iter()
            .map(|v| InkVertex::new(v.position[0], v.position[1], color))
            .collect()
    }
}

/// Erstellt ein Verzeichnis mit gesicherten Rechten (0700 unter Unix).
pub fn create_secure_dir_all(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o700));
    }
    Ok(())
}

/// Schreibt Inhalt atomar auf die Festplatte (über temporäre Datei + `std::fs::rename`).
/// Setzt Berechtigung 0600 unter Unix und synchronisiert das übergeordnete Verzeichnis nach dem Rename.
pub fn atomic_write_file(path: &Path, content: &[u8]) -> Result<(), io::Error> {
    let dir = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            t!("Übergeordnetes Verzeichnis nicht gefunden"),
        )
    })?;
    create_secure_dir_all(dir)?;

    let filename = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    let random_suffix: u64 = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;
    let temp_filename = format!("{filename}.tmp.{}.{random_suffix}", std::process::id());
    let temp_path = dir.join(temp_filename);

    // In temporäre Datei schreiben, Rechte 0600 setzen und auf Datenträger flushen
    {
        let mut file = File::create(&temp_path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&temp_path, fs::Permissions::from_mode(0o600));
        }
        file.write_all(content)?;
        file.sync_all()?;
    }

    // Atomares Rename
    if let Err(e) = fs::rename(&temp_path, path) {
        let _ = fs::remove_file(&temp_path);
        return Err(e);
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
        if let Ok(dir_file) = File::open(dir) {
            let _ = dir_file.sync_all();
        }
    }

    Ok(())
}

/// Serialisiert einen Wert als kompaktes JSON (`serde_json::to_vec`) und schreibt ihn atomar.
pub fn atomic_write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), StorageError> {
    let bytes = serde_json::to_vec(value)?;
    atomic_write_file(path, &bytes)?;
    Ok(())
}

/// Speichert das Manifest atomar in `manifest.json`.
/// Sichert das bisherige Manifest als `manifest.json.bak` vor dem Überschreiben.
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
        // Rolling Backup: Vor Überschreiben sichern
        let rolling_bak = notebook_dir.join("manifest.json.bak");
        let _ = fs::copy(&path, &rolling_bak);
    }
    atomic_write_json(&path, manifest)
}

fn load_manifest_from_file(path: &Path) -> Result<NotebookManifest, StorageError> {
    let mut file = File::open(path)?;
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

/// Lädt das Manifest aus `manifest.json`.
/// Fällt bei Fehlern/Beschädigung automatisch auf `manifest.json.bak` zurück.
pub fn load_manifest(notebook_dir: &Path) -> Result<NotebookManifest, StorageError> {
    let path = notebook_dir.join("manifest.json");
    if !path.exists() {
        let bak_path = notebook_dir.join("manifest.json.bak");
        if bak_path.exists() {
            eprintln!("[Storage-Warnung] manifest.json fehlt, lade aus manifest.json.bak");
            return load_manifest_from_file(&bak_path);
        }
        return Err(StorageError::InvalidManifest(t!(
            "Manifestdatei nicht gefunden: {path}",
            path = path.display()
        )));
    }

    match load_manifest_from_file(&path) {
        Ok(manifest) => Ok(manifest),
        Err(e) => {
            let bak_path = notebook_dir.join("manifest.json.bak");
            if bak_path.exists() {
                eprintln!(
                    "[Storage-Warnung] manifest.json ist korrupt ({e}). Versuche Wiederherstellung aus manifest.json.bak"
                );
                if let Ok(bak_manifest) = load_manifest_from_file(&bak_path) {
                    return Ok(bak_manifest);
                }
            }
            Err(e)
        }
    }
}

/// Verschiebt eine defekte Seitendatei nach `page_<id>.json.corrupt-<unix_ts>`.
/// Nur bei Parse-/Schemafehlern aufrufen, nicht bei I/O-Fehlern!
pub fn quarantine_corrupted_page(notebook_dir: &Path, page_id: u64) -> Result<PathBuf, io::Error> {
    let source_path = notebook_dir.join(format!("page_{page_id}.json"));
    if !source_path.exists() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            t!("Datei existiert nicht"),
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
    create_secure_dir_all(notebook_dir)?;

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
    let _ = cleanup_orphaned_temp_files(notebook_dir, std::time::Duration::from_secs(3600));
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
            let mut page = Page::new(id, first_bg).with_size(manifest.size_for_page(id));
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
                    message: t!("I/O-Fehler: {e}", e = e),
                    quarantine_path: None,
                },
            );
            let mut page =
                Page::new(first_id, first_bg).with_size(manifest.size_for_page(first_id));
            page.persist = PersistState::ReadOnlyAfterLoadError;
            page
        }
        Err(e) => {
            eprintln!("[Storage] Konnte erste Seite {first_id} nicht laden: {e}. Erstelle leere Ersatzseite.");
            let mut page =
                Page::new(first_id, first_bg).with_size(manifest.size_for_page(first_id));
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
        pages.push(Page::new_unloaded(page_id, bg).with_size(manifest.size_for_page(page_id)));
    }

    // Titel aus dem Manifest (Platzhalter und Ersatzseiten kennen sie sonst nicht)
    for page in &mut pages {
        if page.title.is_none() {
            page.title = manifest.title_for_page(page.id);
        }
    }

    let mut notebook = Notebook::from_pages(
        pages,
        manifest.current_page_index,
        manifest.next_stroke_id,
        manifest.next_page_id,
    );
    notebook.load_warnings = load_warnings;

    // Die zuletzt geöffnete Seite sofort laden: Sie ist nach dem Start die aktuelle Seite,
    // und ein leerer Platzhalter würde beim Weiterschreiben ihren Inhalt überschreiben.
    let current = notebook.current_page_index;
    if !notebook.pages[current].is_loaded {
        if let Err(e) = ensure_page_loaded(notebook_dir, &mut notebook, current) {
            eprintln!("[Storage] Aktuelle Seite {current} konnte nicht geladen werden: {e}");
        }
    }

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
            let title = notebook.pages[page_idx].title.take();
            let mut page = data.to_page();
            page.title = page.title.or(title);
            notebook.pages[page_idx] = page;
            Ok(())
        }
        // Angehängt, aber nie gespeichert (noch leer): leere Seite statt Ladefehler
        Err(StorageError::PageNotFound(_)) => {
            let placeholder = &mut notebook.pages[page_idx];
            let mut page = Page::new(page_id, placeholder.background).with_size(placeholder.size);
            page.title = placeholder.title.take();
            *placeholder = page;
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
                    message: t!("I/O-Fehler: {e}", e = err_str),
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

/// Status eines Notizbuchs in der Dokumentenliste.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum NotebookStatus {
    #[default]
    Normal,
    Damaged,
}

/// Zusammenfassende Metadaten eines Notizbuchs für die Dokumentenliste.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NotebookSummary {
    pub id: String,
    pub title: String,
    pub page_count: usize,
    pub modified_at: u64,
    #[serde(default)]
    pub status: NotebookStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Dateibasierter Lock für ein geöffnetes Notizbuch, um parallele Schreibvorgänge mehrerer Instanzen zu verhindern.
#[derive(Debug)]
pub struct NotebookLock {
    _file: File,
    pub path: PathBuf,
}

impl NotebookLock {
    /// Versucht, einen exklusiven, nicht-blockierenden Lock auf `notebook_dir/.lock` zu erwerben.
    pub fn acquire(notebook_dir: &Path) -> Result<Self, StorageError> {
        create_secure_dir_all(notebook_dir)?;
        let lock_path = notebook_dir.join(".lock");
        let file = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(StorageError::Io)?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&lock_path, fs::Permissions::from_mode(0o600));
        }

        use fs2::FileExt;
        match file.try_lock_exclusive() {
            Ok(()) => Ok(Self {
                _file: file,
                path: lock_path,
            }),
            Err(e) => {
                let kind = e.kind();
                if kind == io::ErrorKind::WouldBlock || e.raw_os_error() == Some(11) {
                    Err(StorageError::AlreadyLocked(t!(
                        "Notizbuch '{path}' wird bereits von einer anderen Instanz verwendet",
                        path = notebook_dir.display()
                    )))
                } else {
                    Err(StorageError::Io(e))
                }
            }
        }
    }
}

/// Räumt verwaiste temporäre Dateien (*.tmp.*) auf, die älter als `max_age` sind.
pub fn cleanup_orphaned_temp_files(
    notebook_dir: &Path,
    max_age: std::time::Duration,
) -> io::Result<usize> {
    let mut cleaned = 0;
    if let Ok(entries) = fs::read_dir(notebook_dir) {
        let now = std::time::SystemTime::now();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    if name.contains(".tmp.") {
                        if let Ok(metadata) = entry.metadata() {
                            if let Ok(modified) = metadata.modified() {
                                if let Ok(age) = now.duration_since(modified) {
                                    if age >= max_age && fs::remove_file(&path).is_ok() {
                                        cleaned += 1;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(cleaned)
}

/// Durchsucht das Basis-Verzeichnis nach allen vorhandenen Notizbüchern.
/// Beschädigte Notizbücher werden als `NotebookStatus::Damaged` aufgeführt. `.trash` wird ignoriert.
pub fn list_notebooks(base_dir: &Path) -> Vec<NotebookSummary> {
    let mut list = Vec::new();
    let Ok(entries) = fs::read_dir(base_dir) else {
        return list;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let dir_name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default();

            // Versteckte Ordner (wie .trash, .recovery) ignorieren
            if dir_name.starts_with('.') {
                continue;
            }

            let manifest_path = path.join("manifest.json");
            if manifest_path.exists() {
                match load_manifest(&path) {
                    Ok(manifest) => {
                        let page_count = manifest.page_ids().len();
                        list.push(NotebookSummary {
                            id: manifest.id,
                            title: manifest.title,
                            page_count,
                            modified_at: manifest.modified_at,
                            status: NotebookStatus::Normal,
                            error: None,
                        });
                    }
                    Err(err) => {
                        let modified_at = entry
                            .metadata()
                            .and_then(|m| m.modified())
                            .ok()
                            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                            .map(|d| d.as_secs())
                            .unwrap_or(0);

                        list.push(NotebookSummary {
                            id: dir_name.to_string(),
                            title: t!("⚠️ {name} (Beschädigt)", name = dir_name),
                            page_count: 0,
                            modified_at,
                            status: NotebookStatus::Damaged,
                            error: Some(err.to_string()),
                        });
                    }
                }
            } else {
                let modified_at = entry
                    .metadata()
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs())
                    .unwrap_or(0);

                list.push(NotebookSummary {
                    id: dir_name.to_string(),
                    title: t!("⚠️ {name} (Beschädigt - kein Manifest)", name = dir_name),
                    page_count: 0,
                    modified_at,
                    status: NotebookStatus::Damaged,
                    error: Some(t!("Keine manifest.json vorhanden").to_string()),
                });
            }
        }
    }

    // Nach letztem Änderungsdatum absteigend sortieren (neueste zuerst)
    list.sort_by_key(|a| std::cmp::Reverse(a.modified_at));
    list
}

/// Erstellt ein neues Notizbuch mit dem angegebenen Titel und initialisiert Seite 1.
/// Verhindert Kollisionen durch automatischen Suffix-Inkrement.
pub fn create_new_notebook(
    base_dir: &Path,
    title: &str,
) -> Result<(Notebook, NotebookManifest, PathBuf), StorageError> {
    create_secure_dir_all(base_dir)?;

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

    let base_id = if clean_slug.is_empty() {
        format!("nb_{now_ts}")
    } else {
        format!("{clean_slug}_{now_ts}")
    };

    let mut id = base_id.clone();
    let mut notebook_dir = base_dir.join(&id);
    let mut counter = 1;
    while notebook_dir.exists() {
        id = format!("{base_id}_{counter}");
        notebook_dir = base_dir.join(&id);
        counter += 1;
        if counter > 100 {
            return Err(StorageError::Io(io::Error::new(
                io::ErrorKind::AlreadyExists,
                t!("Konnte nach 100 Versuchen kein eindeutiges Notizbuch-Verzeichnis finden"),
            )));
        }
    }

    create_secure_dir_all(&notebook_dir)?;

    let notebook = Notebook::new();
    let mut manifest = NotebookManifest::new(&id, title);

    save_notebook(&notebook_dir, &notebook, &mut manifest)?;

    Ok((notebook, manifest, notebook_dir))
}

/// Legt ein neues Notizbuch aus einem PDF an: eine Seite je PDF-Seite, die PDF-Seite als
/// Hintergrund. Das PDF wird unverändert im Asset-Ordner abgelegt; alle Seiten werden sofort
/// gespeichert.
pub fn create_notebook_from_pdf(
    base_dir: &Path,
    title: &str,
    pdf_bytes: Vec<u8>,
) -> Result<(Notebook, NotebookManifest, PathBuf), String> {
    let sizes = crate::pdf_doc::page_sizes(pdf_bytes.clone())?;
    let (_, _, dir) = create_new_notebook(base_dir, title).map_err(|e| e.to_string())?;
    let result = (|| {
        let asset = crate::assets::AssetStore::new(&dir)
            .import_pdf(&pdf_bytes)
            .map_err(|e| e.to_string())?;
        let pages: Vec<Page> = sizes
            .iter()
            .enumerate()
            .map(|(i, &(width_pt, height_pt))| {
                let mut page = Page::new(i as u64 + 1, PageBackground::Blank)
                    .with_size(crate::document::pdf_page_size(width_pt, height_pt));
                page.pdf_background = Some(crate::document::PdfBackground {
                    asset: asset.clone(),
                    page: i as u32,
                    width_pt,
                    height_pt,
                });
                page
            })
            .collect();
        let page_count = pages.len() as u64;
        let notebook = Notebook::from_pages(pages, 0, 1, page_count + 1);
        let mut manifest = load_manifest(&dir).map_err(|e| e.to_string())?;
        save_notebook(&dir, &notebook, &mut manifest).map_err(|e| e.to_string())?;
        Ok((notebook, manifest))
    })();
    match result {
        Ok((notebook, manifest)) => Ok((notebook, manifest, dir)),
        Err(err) => {
            // Halbfertiges Notizbuch nicht liegen lassen
            let _ = fs::remove_dir_all(&dir);
            Err(err)
        }
    }
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

/// Verschiebt ein Notizbuch sicher in den Papierkorb (`<base_dir>/.trash/<id>_<ts>`).
/// Prüft, dass der Pfad direkt unter `base_dir` liegt, kein Symlink ist und `manifest.json` enthält.
pub fn delete_notebook(notebook_dir: &Path, base_dir: &Path) -> Result<PathBuf, StorageError> {
    if !notebook_dir.exists() {
        return Err(StorageError::InvalidManifest(t!(
            "Verzeichnis '{path}' existiert nicht",
            path = notebook_dir.display()
        )));
    }

    // Prüfen auf Symlink
    let symlink_meta = fs::symlink_metadata(notebook_dir).map_err(StorageError::Io)?;
    if symlink_meta.file_type().is_symlink() {
        return Err(StorageError::InvalidManifest(t!(
            "Pfad '{path}' ist ein Symlink und darf nicht gelöscht werden",
            path = notebook_dir.display()
        )));
    }

    let canon_base = base_dir.canonicalize().map_err(StorageError::Io)?;
    let canon_target = notebook_dir.canonicalize().map_err(StorageError::Io)?;

    // Prüfen, dass der Pfad direkt unter base_dir liegt
    if canon_target.parent() != Some(&canon_base) {
        return Err(StorageError::PathTraversal(t!(
            "Pfad '{path}' liegt nicht direkt unter Basis-Verzeichnis '{base}'",
            path = notebook_dir.display(),
            base = base_dir.display()
        )));
    }

    // Prüfen, dass manifest.json vorhanden ist
    if !canon_target.join("manifest.json").exists() {
        return Err(StorageError::InvalidManifest(t!(
            "Verzeichnis '{path}' enthält kein manifest.json",
            path = notebook_dir.display()
        )));
    }

    let notebook_id = canon_target
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown");

    let trash_dir = canon_base.join(".trash");
    create_secure_dir_all(&trash_dir)?;

    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let trash_target = trash_dir.join(format!("{notebook_id}_{ts}"));

    fs::rename(&canon_target, &trash_target)?;

    Ok(trash_target)
}

/// Stellt ein Notizbuch aus dem Papierkorb wieder her (`<base_dir>/.trash/<eintrag>` -> `<base_dir>/<id>`).
pub fn restore_notebook_from_trash(
    base_dir: &Path,
    trash_target: &Path,
) -> Result<PathBuf, StorageError> {
    if !trash_target.exists() {
        return Err(StorageError::Io(io::Error::new(
            io::ErrorKind::NotFound,
            t!(
                "Papierkorb-Eintrag '{path}' existiert nicht",
                path = trash_target.display()
            ),
        )));
    }

    let canon_base = base_dir.canonicalize().map_err(StorageError::Io)?;
    let canon_trash = trash_target.canonicalize().map_err(StorageError::Io)?;

    let expected_trash_dir = canon_base.join(".trash");
    if !expected_trash_dir.exists() {
        return Err(StorageError::InvalidManifest(
            t!("Papierkorb-Verzeichnis existiert nicht").to_string(),
        ));
    }
    let canon_trash_base = expected_trash_dir
        .canonicalize()
        .map_err(StorageError::Io)?;

    if canon_trash.parent() != Some(&canon_trash_base) {
        return Err(StorageError::PathTraversal(t!(
            "Papierkorb-Pfad '{path}' liegt nicht im Papierkorb",
            path = trash_target.display()
        )));
    }

    let manifest_path = canon_trash.join("manifest.json");
    let target_id = if manifest_path.exists() {
        if let Ok(content) = fs::read_to_string(&manifest_path) {
            serde_json::from_str::<NotebookManifest>(&content)
                .map(|m| m.id)
                .unwrap_or_else(|_| {
                    canon_trash
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("restored")
                        .to_string()
                })
        } else {
            "restored".to_string()
        }
    } else {
        "restored".to_string()
    };

    let mut restored_dir = canon_base.join(&target_id);
    if restored_dir.exists() {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        restored_dir = canon_base.join(format!("{target_id}_restored_{ts}"));
    }

    fs::rename(&canon_trash, &restored_dir)?;
    Ok(restored_dir)
}

/// Stellt ein beschädigtes Manifest aus der Rolling-Backup-Datei (`manifest.json.bak`) wieder her.
pub fn restore_manifest_from_backup(notebook_dir: &Path) -> Result<(), StorageError> {
    let bak_path = notebook_dir.join("manifest.json.bak");
    let manifest_path = notebook_dir.join("manifest.json");
    if bak_path.exists() {
        fs::copy(&bak_path, &manifest_path).map_err(StorageError::Io)?;
        Ok(())
    } else {
        Err(StorageError::InvalidManifest(
            t!("Keine Sicherungskopie (manifest.json.bak) vorhanden").to_string(),
        ))
    }
}

/// Entfernt Einträge aus `.trash`, die älter als `max_age` sind.
pub fn cleanup_trash(base_dir: &Path, max_age: std::time::Duration) -> io::Result<usize> {
    let trash_dir = base_dir.join(".trash");
    if !trash_dir.exists() {
        return Ok(0);
    }
    let mut removed = 0;
    let now = std::time::SystemTime::now();
    if let Ok(entries) = fs::read_dir(&trash_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if let Ok(metadata) = entry.metadata() {
                if let Ok(modified) = metadata.modified() {
                    if let Ok(age) = now.duration_since(modified) {
                        if age >= max_age {
                            if path.is_dir() {
                                if fs::remove_dir_all(&path).is_ok() {
                                    removed += 1;
                                }
                            } else if fs::remove_file(&path).is_ok() {
                                removed += 1;
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(removed)
}

/// Lädt alle noch nicht geladenen Seiten ohne Ersatzseiten (z. B. für den PDF-Export).
/// Schlägt fehl, sobald eine Seite nicht vollständig verfügbar ist.
pub fn load_all_pages(notebook_dir: &Path, notebook: &mut Notebook) -> Result<(), StorageError> {
    for page in &mut notebook.pages {
        if page.persist == crate::document::PersistState::ReadOnlyAfterLoadError {
            return Err(StorageError::CorruptedPage(
                page.id,
                t!("Seite konnte nicht geladen werden").to_string(),
            ));
        }
        if !page.is_loaded {
            let title = page.title.take();
            *page = match load_page(notebook_dir, page.id) {
                Ok(data) => data.to_page(),
                // Wie beim Lazy Loading: nie gespeicherte Seite = leere Seite
                Err(StorageError::PageNotFound(_)) => {
                    Page::new(page.id, page.background).with_size(page.size)
                }
                Err(e) => return Err(e),
            };
            page.title = page.title.take().or(title);
        }
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
        delete_notebook(&dir2, base_dir).expect("Löschen muss klappen");
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

        let temp_dir = tempfile::tempdir().expect("tempdir");
        let path = temp_dir.path();

        let manifest = NotebookManifest::new("nb_io", "Test IO");
        save_manifest(path, &manifest).unwrap();

        // I/O-Fehler simulieren, der auch als root auftritt (CI-Runner laufen oft als root,
        // dort greift ein chmod 000 nicht): page_1.json ist ein Verzeichnis -> EISDIR beim Lesen
        let original_content = br#"{"id": 1, "background": "Grid5mm", "strokes": []}"#;
        let page_path = path.join("page_1.json");
        fs::create_dir(&page_path).unwrap();
        let marker = page_path.join("original");
        fs::write(&marker, original_content).unwrap();

        let (nb, mut m) =
            load_notebook(path).expect("load_notebook should succeed with fallback page");

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

        assert!(
            page_path.is_dir(),
            "Original page path must not be replaced"
        );
        assert_eq!(
            fs::read(&marker).unwrap(),
            original_content,
            "Original content must not be overwritten"
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

        // Reload to verify format_version is now CURRENT_FORMAT_VERSION (2)
        let (_nb_upgraded, m_upgraded) = load_notebook(path).expect("Upgraded notebook must load");
        assert_eq!(m_upgraded.format_version, CURRENT_FORMAT_VERSION);
        let p1_upgraded = load_page(path, 1).expect("Upgraded page must load");
        assert_eq!(p1_upgraded.format_version, CURRENT_FORMAT_VERSION);
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

    #[test]
    #[cfg(unix)]
    fn test_permissions_0600_0700() {
        use std::os::unix::fs::PermissionsExt;
        let temp_dir = tempfile::tempdir().unwrap();
        let base_dir = temp_dir.path();

        let (_nb, _m, nb_dir) = create_new_notebook(base_dir, "PermTest").unwrap();
        let dir_mode = fs::metadata(&nb_dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            dir_mode, 0o700,
            "Notizbuch-Verzeichnis muss 0700 Rechte haben"
        );

        let manifest_path = nb_dir.join("manifest.json");
        let manifest_mode = fs::metadata(&manifest_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(manifest_mode, 0o600, "manifest.json muss 0600 Rechte haben");

        let page_path = nb_dir.join("page_1.json");
        let page_mode = fs::metadata(&page_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(page_mode, 0o600, "page_1.json muss 0600 Rechte haben");
    }

    #[test]
    fn test_cleanup_orphaned_temp_files() {
        let temp_dir = tempfile::tempdir().unwrap();
        let nb_dir = temp_dir.path();

        let old_temp = nb_dir.join("manifest.json.tmp.111.222");
        let new_temp = nb_dir.join("manifest.json.tmp.333.444");
        fs::write(&old_temp, b"old").unwrap();
        fs::write(&new_temp, b"new").unwrap();

        // 1 Stunde alt simulieren
        let cleaned = cleanup_orphaned_temp_files(nb_dir, Duration::ZERO).unwrap();
        assert_eq!(
            cleaned, 2,
            "Beide temporären Dateien müssen aufgeräumt werden"
        );
        assert!(!old_temp.exists());
        assert!(!new_temp.exists());
    }

    #[test]
    fn test_corrupted_manifest_fallback_to_bak() {
        let temp_dir = tempfile::tempdir().unwrap();
        let nb_dir = temp_dir.path();

        let valid_manifest = NotebookManifest::new("nb_test", "Backup Test");
        let bak_path = nb_dir.join("manifest.json.bak");
        atomic_write_json(&bak_path, &valid_manifest).unwrap();

        let broken_manifest_path = nb_dir.join("manifest.json");
        fs::write(&broken_manifest_path, b"{ broken json ... ").unwrap();

        let loaded = load_manifest(nb_dir).expect("Muss aus .bak geladen werden");
        assert_eq!(loaded.id, "nb_test");
        assert_eq!(loaded.title, "Backup Test");
    }

    #[test]
    fn test_create_notebook_collision_generates_new_id() {
        let temp_dir = tempfile::tempdir().unwrap();
        let base_dir = temp_dir.path();

        let (_nb1, m1, dir1) = create_new_notebook(base_dir, "Notiz").unwrap();
        assert!(dir1.exists());

        // Erzwinge absichtlich existierendes Verzeichnis mit demselben ID-Schema
        let (_nb2, m2, dir2) = create_new_notebook(base_dir, "Notiz").unwrap();
        assert!(dir2.exists());
        assert_ne!(m1.id, m2.id, "IDs müssen bei Kollision eindeutig sein");
        assert_ne!(dir1, dir2, "Verzeichnisse dürfen sich nicht überschreiben");
    }

    #[test]
    fn test_delete_notebook_path_traversal_rejected() {
        let temp_dir = tempfile::tempdir().unwrap();
        let base_dir = temp_dir.path().join("storage");
        let outside_dir = temp_dir.path().join("secret");
        fs::create_dir_all(&base_dir).unwrap();
        fs::create_dir_all(&outside_dir).unwrap();

        let malicious_path = base_dir.join("../secret");
        let res = delete_notebook(&malicious_path, &base_dir);
        assert!(res.is_err(), "Path-Traversal muss abgewehrt werden");
        match res.unwrap_err() {
            StorageError::PathTraversal(_) => {}
            other => panic!("Erwartet PathTraversal Fehler, erhalten: {other:?}"),
        }
        assert!(
            outside_dir.exists(),
            "Verzeichnis außerhalb darf nicht berührt werden"
        );
    }

    #[test]
    fn test_delete_notebook_moves_to_trash_and_can_be_restored() {
        let temp_dir = tempfile::tempdir().unwrap();
        let base_dir = temp_dir.path();
        let (_nb, m, nb_dir) = create_new_notebook(base_dir, "TrashTest").unwrap();
        assert!(nb_dir.exists());

        let trash_target =
            delete_notebook(&nb_dir, base_dir).expect("Löschen muss erfolgreich sein");
        assert!(
            !nb_dir.exists(),
            "Originalverzeichnis muss verschwunden sein"
        );
        assert!(trash_target.exists(), "Papierkorb-Eintrag muss existieren");

        let trash_dir = base_dir.join(".trash");
        assert!(trash_dir.exists(), ".trash Ordner muss existieren");
        let entries: Vec<_> = fs::read_dir(&trash_dir).unwrap().flatten().collect();
        assert_eq!(
            entries.len(),
            1,
            "Gelöschtes Notizbuch muss im Papierkorb liegen"
        );
        assert!(entries[0].file_name().to_string_lossy().starts_with(&m.id));

        // Wiederherstellung prüfen
        let restored_dir = restore_notebook_from_trash(base_dir, &trash_target)
            .expect("Wiederherstellung aus Papierkorb muss erfolgreich sein");
        assert!(
            restored_dir.exists(),
            "Wiederhergestelltes Verzeichnis muss existieren"
        );
        assert!(restored_dir.join("manifest.json").exists());
        assert!(
            !trash_target.exists(),
            "Eintrag darf nicht mehr im Papierkorb liegen"
        );
    }

    #[test]
    fn test_delete_notebook_missing_manifest_rejected() {
        let temp_dir = tempfile::tempdir().unwrap();
        let base_dir = temp_dir.path();
        let no_manifest_dir = base_dir.join("no_manifest");
        fs::create_dir_all(&no_manifest_dir).unwrap();

        let res = delete_notebook(&no_manifest_dir, base_dir);
        assert!(
            res.is_err(),
            "Löschen ohne manifest.json muss abgewiesen werden"
        );
        match res.unwrap_err() {
            StorageError::InvalidManifest(_) => {}
            other => panic!("Erwartet InvalidManifest Fehler, erhalten: {other:?}"),
        }
        assert!(
            no_manifest_dir.exists(),
            "Verzeichnis ohne manifest.json darf nicht gelöscht werden"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_delete_notebook_symlink_rejected() {
        let temp_dir = tempfile::tempdir().unwrap();
        let base_dir = temp_dir.path();
        let (_nb, _m, real_dir) = create_new_notebook(base_dir, "RealNB").unwrap();

        let symlink_path = base_dir.join("symlink_nb");
        std::os::unix::fs::symlink(&real_dir, &symlink_path).unwrap();

        let res = delete_notebook(&symlink_path, base_dir);
        assert!(res.is_err(), "Löschen von Symlinks muss abgewiesen werden");
        match res.unwrap_err() {
            StorageError::InvalidManifest(_) => {}
            other => panic!("Erwartet InvalidManifest Fehler, erhalten: {other:?}"),
        }
        assert!(
            real_dir.exists(),
            "Reales Notizbuch darf nicht angetastet werden"
        );
        assert!(
            symlink_path.exists(),
            "Symlink darf nicht angetastet werden"
        );
    }

    #[test]
    fn test_list_notebooks_includes_damaged_and_ignores_trash() {
        let temp_dir = tempfile::tempdir().unwrap();
        let base_dir = temp_dir.path();

        // 1. Intaktes Notizbuch
        let (_nb, _m, _dir) = create_new_notebook(base_dir, "Intakt").unwrap();

        // 2. Beschädigtes Notizbuch (defektes manifest.json)
        let damaged_dir = base_dir.join("damaged_nb");
        fs::create_dir_all(&damaged_dir).unwrap();
        fs::write(damaged_dir.join("manifest.json"), b"corrupt").unwrap();

        // 3. .trash Ordner
        let trash_dir = base_dir.join(".trash");
        fs::create_dir_all(&trash_dir).unwrap();

        let summaries = list_notebooks(base_dir);
        // .trash darf nicht auftauchen!
        assert!(!summaries.iter().any(|s| s.id == ".trash"));

        // damaged_nb MUSS auftauchen mit Status Damaged
        let damaged_summary = summaries.iter().find(|s| s.id == "damaged_nb");
        assert!(
            damaged_summary.is_some(),
            "Beschädigtes Notizbuch muss aufgelistet werden"
        );
        assert_eq!(damaged_summary.unwrap().status, NotebookStatus::Damaged);
    }

    #[test]
    fn test_notebook_instance_lock_prevents_concurrent_open() {
        let temp_dir = tempfile::tempdir().unwrap();
        let nb_dir = temp_dir.path();

        let lock1 = NotebookLock::acquire(nb_dir).expect("Erster Lock muss gelingen");
        let lock2 = NotebookLock::acquire(nb_dir);
        assert!(
            lock2.is_err(),
            "Zweiter Lock auf selbes Verzeichnis muss scheitern"
        );
        match lock2.unwrap_err() {
            StorageError::AlreadyLocked(_) => {}
            other => panic!("Erwartet AlreadyLocked, erhalten: {other:?}"),
        }

        // Lock 1 freigeben
        drop(lock1);
        let lock3 = NotebookLock::acquire(nb_dir);
        assert!(
            lock3.is_ok(),
            "Nach Drop muss Lock wieder erworben werden können"
        );
    }

    #[test]
    fn test_round_trip_fidelity() {
        use crate::geometry::RecognizedShape;
        use crate::spline::{finalize_stroke_with_config, SplineConfig};

        let mut page = Page::new(1, PageBackground::Blank);

        // 1. Dünner Stift (0.3 mm = 3.0 Einheiten)
        let samples1 = vec![
            StylusSample {
                position: glam::Vec2::new(100.0, 100.0),
                pressure: 0.5,
                timestamp: 0.0,
            },
            StylusSample {
                position: glam::Vec2::new(150.0, 100.0),
                pressure: 0.6,
                timestamp: 0.1,
            },
            StylusSample {
                position: glam::Vec2::new(200.0, 150.0),
                pressure: 0.7,
                timestamp: 0.2,
            },
        ];
        let cfg_thin = SplineConfig {
            min_radius: 0.75,
            max_radius: 2.25,
            samples_per_segment: 16,
        };
        let stroke_thin =
            finalize_stroke_with_config(1, &samples1, [0.0, 0.0, 0.0, 1.0], &cfg_thin);
        page.add_stroke(stroke_thin);

        // 2. Breiter Stift (1.5 mm = 15.0 Einheiten)
        let samples2 = vec![
            StylusSample {
                position: glam::Vec2::new(200.0, 200.0),
                pressure: 0.5,
                timestamp: 0.0,
            },
            StylusSample {
                position: glam::Vec2::new(250.0, 200.0),
                pressure: 0.5,
                timestamp: 0.1,
            },
            StylusSample {
                position: glam::Vec2::new(300.0, 250.0),
                pressure: 0.5,
                timestamp: 0.2,
            },
        ];
        let cfg_thick = SplineConfig {
            min_radius: 3.75,
            max_radius: 11.25,
            samples_per_segment: 16,
        };
        let stroke_thick =
            finalize_stroke_with_config(2, &samples2, [0.1, 0.2, 0.8, 1.0], &cfg_thick);
        page.add_stroke(stroke_thick);

        // 3. Textmarker (6.0 mm = 60.0 Einheiten)
        let samples3 = vec![
            StylusSample {
                position: glam::Vec2::new(100.0, 300.0),
                pressure: 0.5,
                timestamp: 0.0,
            },
            StylusSample {
                position: glam::Vec2::new(200.0, 300.0),
                pressure: 0.5,
                timestamp: 0.1,
            },
            StylusSample {
                position: glam::Vec2::new(300.0, 300.0),
                pressure: 0.5,
                timestamp: 0.2,
            },
        ];
        let cfg_highlighter = SplineConfig {
            min_radius: 15.0,
            max_radius: 45.0,
            samples_per_segment: 16,
        };
        let stroke_highlighter =
            finalize_stroke_with_config(3, &samples3, [1.0, 1.0, 0.0, 0.4], &cfg_highlighter);
        page.add_stroke(stroke_highlighter);

        // 4. Erkannte Form (Kreis mit Breite 10.0 Einheiten)
        let circle = RecognizedShape::Circle {
            center: glam::Vec2::new(500.0, 500.0),
            radius: 50.0,
        };
        let circle_stroke = crate::geometry::shape_to_stroke(circle, 4, [0.0, 0.5, 0.0, 1.0], 10.0);
        page.add_stroke(circle_stroke);

        // Serialisieren & Deserialisieren
        let page_data = PageData::from(&page);
        let restored_page = page_data.to_page();

        assert_eq!(page.strokes.len(), restored_page.strokes.len());
        for (i, (orig, restored)) in page
            .strokes
            .iter()
            .zip(restored_page.strokes.iter())
            .enumerate()
        {
            assert_eq!(
                orig.vertices.len(),
                restored.vertices.len(),
                "Strich {i} Vertices-Anzahl weicht ab: orig {}, restored {}",
                orig.vertices.len(),
                restored.vertices.len()
            );
            for (v_idx, (v1, v2)) in orig
                .vertices
                .iter()
                .zip(restored.vertices.iter())
                .enumerate()
            {
                assert!(
                    (v1.position[0] - v2.position[0]).abs() < 1e-3
                        && (v1.position[1] - v2.position[1]).abs() < 1e-3,
                    "Strich {i} Vertex {v_idx} Position weicht ab: orig {:?}, restored {:?}",
                    v1.position,
                    v2.position
                );
            }
        }
    }

    #[test]
    fn test_compact_json_size_reduction() {
        use crate::spline::{finalize_stroke_with_config, SplineConfig};
        let mut page = Page::new(1, PageBackground::Blank);
        let samples: Vec<StylusSample> = (0..500)
            .map(|i| StylusSample {
                position: glam::Vec2::new(i as f32, (i * 2) as f32),
                pressure: 0.5,
                timestamp: i as f64 * 0.01,
            })
            .collect();
        let stroke = finalize_stroke_with_config(
            1,
            &samples,
            [0.0, 0.0, 0.0, 1.0],
            &SplineConfig::default(),
        );
        page.add_stroke(stroke);

        let page_data = PageData::from(&page);
        let pretty_bytes = serde_json::to_vec_pretty(&page_data).unwrap();
        let compact_bytes = serde_json::to_vec(&page_data).unwrap();

        println!(
            "\n[Dateigrößen-Vergleich] Pretty: {} Bytes | Kompakt: {} Bytes | Ersparnis: {:.1}%",
            pretty_bytes.len(),
            compact_bytes.len(),
            (1.0 - compact_bytes.len() as f64 / pretty_bytes.len() as f64) * 100.0
        );

        assert!(
            compact_bytes.len() < pretty_bytes.len(),
            "Kompaktes JSON muss kleiner sein"
        );
        assert!(
            compact_bytes.len() * 2 <= pretty_bytes.len(),
            "Kompaktes JSON spart mind. 50% Whitespace"
        );
    }

    /// Hilfsfunktion: Seite mit einem einzelnen Strich.
    fn page_with_one_stroke(page_id: u64, background: PageBackground) -> Page {
        let mut page = Page::new(page_id, background);
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
        let vertices = vec![
            InkVertex::new(100.0, 100.0, [0.0, 0.0, 0.0, 1.0]),
            InkVertex::new(200.0, 150.0, [0.0, 0.0, 0.0, 1.0]),
        ];
        page.add_stroke(Stroke::new(1, samples, vertices, [0.0, 0.0, 0.0, 1.0]));
        page
    }

    #[test]
    fn test_keyboard_new_page_is_recorded_in_manifest() {
        let temp_dir = tempfile::tempdir().unwrap();
        let (mut notebook, mut manifest, dir) =
            create_new_notebook(temp_dir.path(), "Neue Seiten").unwrap();
        let mut autosave = AutosaveController::new(dir.clone(), Duration::from_secs(60));

        // Wie Taste N in main.rs: neue Seite anhängen, Strich darauf, speichern
        notebook.next_page();
        let idx = notebook.current_page_index;
        notebook.pages[idx] = page_with_one_stroke(notebook.pages[idx].id, PageBackground::Lined);
        autosave.mark_dirty(&notebook, &mut manifest);
        autosave.flush().unwrap();
        autosave.shutdown().unwrap();

        let (mut reloaded, reloaded_manifest) = load_notebook(&dir).unwrap();
        assert_eq!(reloaded_manifest.page_ids(), vec![1, 2]);
        assert_eq!(
            reloaded.pages.len(),
            2,
            "Neue Seite muss einen Neustart überleben"
        );
        ensure_page_loaded(&dir, &mut reloaded, 1).unwrap();
        assert_eq!(reloaded.pages[1].strokes.len(), 1);
    }

    #[test]
    fn test_load_all_pages_for_export() {
        let temp_dir = tempfile::tempdir().unwrap();
        let (mut notebook, mut manifest, dir) =
            create_new_notebook(temp_dir.path(), "Export").unwrap();
        notebook.add_page(PageBackground::Lined);
        notebook.pages[1] = page_with_one_stroke(notebook.pages[1].id, PageBackground::Lined);
        save_notebook(&dir, &notebook, &mut manifest).unwrap();

        let (mut loaded, _) = load_notebook(&dir).unwrap();
        assert!(!loaded.pages[1].is_loaded);
        // Ohne Nachladen verweigert der Export, statt leere Seiten zu erzeugen
        assert!(crate::export::generate_pdf_document(
            &loaded,
            Default::default(),
            None,
            Default::default()
        )
        .is_err());

        load_all_pages(&dir, &mut loaded).unwrap();
        assert_eq!(loaded.pages[1].strokes.len(), 1);
        assert_eq!(loaded.pages[1].background, PageBackground::Lined);
        assert!(crate::export::generate_pdf_document(
            &loaded,
            Default::default(),
            None,
            Default::default()
        )
        .is_ok());
    }

    #[test]
    fn test_restart_on_later_page_keeps_its_content() {
        // Regression: Beim Start wurde nur Seite 1 geladen; war zuletzt eine spätere Seite
        // offen, war sie ein leerer Platzhalter, und Weiterschreiben überschrieb ihren Inhalt.
        let temp_dir = tempfile::tempdir().unwrap();
        let (mut notebook, mut manifest, dir) =
            create_new_notebook(temp_dir.path(), "Neustart").unwrap();
        notebook.add_page(PageBackground::Lined);
        notebook.pages[1] = page_with_one_stroke(notebook.pages[1].id, PageBackground::Lined);
        notebook.current_page_index = 1;
        save_notebook(&dir, &notebook, &mut manifest).unwrap();

        let (mut loaded, mut loaded_manifest) = load_notebook(&dir).unwrap();
        assert_eq!(loaded.current_page_index, 1);
        assert!(
            loaded.current_page().is_loaded,
            "aktuelle Seite muss geladen sein"
        );
        assert_eq!(loaded.current_page().strokes.len(), 1);

        // Weiterschreiben und speichern darf den bisherigen Inhalt nicht verlieren
        let extra = page_with_one_stroke(99, PageBackground::Lined).strokes[0].clone();
        loaded.current_page_mut().strokes.push(extra);
        let mut autosave = AutosaveController::new(dir.clone(), Duration::from_secs(60));
        autosave.mark_dirty(&loaded, &mut loaded_manifest);
        autosave.flush().unwrap();
        autosave.shutdown().unwrap();

        let (reloaded, _) = load_notebook(&dir).unwrap();
        assert_eq!(reloaded.current_page().strokes.len(), 2);
    }

    #[test]
    fn test_never_saved_page_loads_as_empty_page() {
        // Angehängte, nie beschriebene Seite: steht im Manifest, hat aber keine Datei
        let temp_dir = tempfile::tempdir().unwrap();
        let (mut notebook, mut manifest, dir) =
            create_new_notebook(temp_dir.path(), "Angehängt").unwrap();
        notebook.add_page(PageBackground::Lined);
        save_notebook(&dir, &notebook, &mut manifest).unwrap();
        let _ = fs::remove_file(dir.join(format!("page_{}.json", notebook.pages[1].id)));

        let (mut loaded, _) = load_notebook(&dir).unwrap();
        assert!(!loaded.pages[1].is_loaded);
        ensure_page_loaded(&dir, &mut loaded, 1).unwrap();
        let page = &loaded.pages[1];
        assert!(page.is_loaded && !page.is_readonly());
        assert_eq!(page.background, PageBackground::Lined);
        assert!(page.strokes.is_empty());
        assert!(loaded.load_warnings.is_empty());
    }

    #[test]
    fn test_autosave_never_writes_unloaded_placeholder() {
        let temp_dir = tempfile::tempdir().unwrap();
        let (mut notebook, mut manifest, dir) =
            create_new_notebook(temp_dir.path(), "Platzhalter").unwrap();
        notebook.add_page(PageBackground::Lined);
        notebook.pages[1] = page_with_one_stroke(notebook.pages[1].id, PageBackground::Lined);
        save_notebook(&dir, &notebook, &mut manifest).unwrap();
        let page_file = dir.join(format!("page_{}.json", notebook.pages[1].id));
        let before = fs::read(&page_file).unwrap();

        // Platzhalter (nicht geladen) als aktuelle Seite markieren und speichern
        let placeholder = Page::new_unloaded(notebook.pages[1].id, PageBackground::Lined);
        let mut autosave = AutosaveController::new(dir.clone(), Duration::from_secs(60));
        autosave.mark_page_dirty(&placeholder, &manifest);
        autosave.flush().unwrap();
        autosave.shutdown().unwrap();

        assert_eq!(
            fs::read(&page_file).unwrap(),
            before,
            "Seitendatei wurde überschrieben"
        );
    }

    #[test]
    fn test_new_shapes_round_trip_through_page_file() {
        let temp_dir = tempfile::tempdir().unwrap();
        let (mut notebook, mut manifest, dir) =
            create_new_notebook(temp_dir.path(), "Neue Formen").unwrap();
        let shapes = [
            crate::geometry::RecognizedShape::Ellipse {
                center: glam::Vec2::new(500.0, 600.0),
                radius_x: 250.0,
                radius_y: 120.0,
                rotation: 0.3,
            },
            crate::geometry::RecognizedShape::Arrow {
                start: glam::Vec2::new(100.0, 100.0),
                end: glam::Vec2::new(700.0, 300.0),
                double: true,
            },
        ];
        for (i, shape) in shapes.iter().enumerate() {
            notebook
                .current_page_mut()
                .strokes
                .push(crate::geometry::shape_to_stroke(
                    shape.clone(),
                    i as u64 + 1,
                    [0.0, 0.0, 0.0, 1.0],
                    5.0,
                ));
        }
        save_notebook(&dir, &notebook, &mut manifest).unwrap();
        assert_eq!(
            load_manifest(&dir).unwrap().format_version,
            CURRENT_FORMAT_VERSION
        );

        let (reloaded, _) = load_notebook(&dir).unwrap();
        let loaded: Vec<_> = reloaded
            .current_page()
            .strokes
            .iter()
            .map(|s| s.shape.clone())
            .collect();
        assert_eq!(loaded, shapes.iter().cloned().map(Some).collect::<Vec<_>>());
    }

    #[test]
    fn test_stroke_tool_of_old_files_is_recovered() {
        // Bis v0.3.7 wurde jeder Strich als "pen" gespeichert
        let json = r#"{"id": 1, "color": [1.0, 0.95, 0.1, 0.35], "tool": "pen",
            "samples": [{"x": 10.0, "y": 10.0, "p": 0.5, "t": 0.0},
                        {"x": 90.0, "y": 10.0, "p": 0.5, "t": 0.1}]}"#;
        let data: StrokeData = serde_json::from_str(json).unwrap();
        let highlighter = data.to_stroke();
        assert_eq!(highlighter.tool, crate::document::StrokeTool::Highlighter);

        let mut pen = data.clone();
        pen.color = [0.12, 0.12, 0.14, 1.0];
        assert_eq!(pen.to_stroke().tool, crate::document::StrokeTool::Pen);

        let mut shape = pen.clone();
        shape.shape = Some(crate::geometry::RecognizedShape::Line {
            start: glam::Vec2::new(10.0, 10.0),
            end: glam::Vec2::new(90.0, 10.0),
        });
        assert_eq!(shape.to_stroke().tool, crate::document::StrokeTool::Shape);

        // Ausdrücklich gespeicherte Werkzeuge bleiben unverändert
        let mut explicit = data.clone();
        explicit.tool = crate::document::StrokeTool::Shape;
        assert_eq!(
            explicit.to_stroke().tool,
            crate::document::StrokeTool::Shape
        );
    }

    #[test]
    fn test_image_objects_round_trip_through_page_file() {
        let temp_dir = tempfile::tempdir().unwrap();
        let (mut notebook, mut manifest, dir) =
            create_new_notebook(temp_dir.path(), "Mit Bild").unwrap();
        let assets = crate::assets::AssetStore::new(&dir);
        let imported = assets.import_rgba(4, 2, vec![200; 32]).unwrap();
        let corners = [
            glam::Vec2::new(100.0, 100.0),
            glam::Vec2::new(500.0, 120.0),
            glam::Vec2::new(480.0, 320.0),
            glam::Vec2::new(80.0, 300.0),
        ];
        notebook.current_page_mut().strokes.push(Stroke::new_image(
            7,
            imported.asset.clone(),
            corners,
        ));
        save_notebook(&dir, &notebook, &mut manifest).unwrap();
        assert_eq!(
            load_manifest(&dir).unwrap().format_version,
            CURRENT_FORMAT_VERSION
        );

        let (reloaded, _) = load_notebook(&dir).unwrap();
        let image = &reloaded.current_page().strokes[0];
        assert_eq!(image.image.as_deref(), Some(imported.asset.as_str()));
        assert_eq!(image.image_corners(), Some(corners));
        assert!(image.vertices.is_empty());
        // Bilddatei liegt im Notizbuchordner
        assert!(dir.join("assets").join(&imported.asset).exists());
    }

    #[test]
    fn test_text_boxes_round_trip_through_page_file() {
        let temp_dir = tempfile::tempdir().unwrap();
        let (mut notebook, mut manifest, dir) =
            create_new_notebook(temp_dir.path(), "Mit Text").unwrap();
        let text = crate::text::TextBox::new(
            "Überschrift\n\tfn main() {}".into(),
            42.0,
            crate::text::TextFont::Mono,
            900.0,
        );
        let corners = [
            glam::Vec2::new(100.0, 100.0),
            glam::Vec2::new(1000.0, 100.0),
            glam::Vec2::new(1000.0, 220.0),
            glam::Vec2::new(100.0, 220.0),
        ];
        notebook.current_page_mut().strokes.push(Stroke::new_text(
            8,
            text.clone(),
            corners,
            [0.15, 0.35, 0.85, 1.0],
        ));
        save_notebook(&dir, &notebook, &mut manifest).unwrap();

        let (reloaded, _) = load_notebook(&dir).unwrap();
        let stroke = &reloaded.current_page().strokes[0];
        assert_eq!(stroke.text.as_ref(), Some(&text));
        assert_eq!(stroke.object_corners(), Some(corners));
        assert_eq!(stroke.color, [0.15, 0.35, 0.85, 1.0]);
        assert!(stroke.is_text() && !stroke.is_image() && stroke.vertices.is_empty());
    }

    #[test]
    fn test_page_size_round_trips_and_is_known_before_loading() {
        let temp_dir = tempfile::tempdir().unwrap();
        let (mut notebook, mut manifest, dir) =
            create_new_notebook(temp_dir.path(), "Folien").unwrap();
        let slide = glam::Vec2::new(2970.0, 1671.0);
        notebook.pages[0].size = slide;
        notebook.add_page(PageBackground::Blank);
        notebook.add_page(PageBackground::Blank);
        notebook.pages[2].size = crate::document::A4_SIZE;
        save_notebook(&dir, &notebook, &mut manifest).unwrap();

        let (mut reloaded, _) = load_notebook(&dir).unwrap();
        let sizes: Vec<glam::Vec2> = reloaded.pages.iter().map(|p| p.size).collect();
        assert_eq!(sizes, vec![slide, slide, crate::document::A4_SIZE]);
        // Ungeladene Platzhalter kennen ihre Größe aus dem Manifest
        assert!(!reloaded.pages[1].is_loaded);
        ensure_page_loaded(&dir, &mut reloaded, 1).unwrap();
        assert_eq!(reloaded.pages[1].size, slide);
        // A4-Seiten speichern keine Größe (Dateien bleiben wie bisher)
        let data = load_page(&dir, reloaded.pages[2].id).unwrap();
        assert_eq!(data.size, None);
        // Unsinnige Werte aus einer bearbeiteten Datei ergeben A4
        assert_eq!(
            page_size_from(Some([f32::NAN, 5.0])),
            crate::document::A4_SIZE
        );
    }

    #[test]
    fn test_create_notebook_from_pdf() {
        let temp_dir = tempfile::tempdir().unwrap();
        let pdf = crate::pdf_doc::tests::sample_pdf(&[(842.0, 595.0, 0), (595.0, 842.0, 0)]);
        let (notebook, manifest, dir) =
            create_notebook_from_pdf(temp_dir.path(), "Vorlesung 3", pdf.clone()).unwrap();
        assert_eq!(manifest.title, "Vorlesung 3");
        assert_eq!(notebook.pages.len(), 2);
        assert_eq!(notebook.next_page_id, 3);

        // Nach dem Neuladen: jede Seite verweist auf ihre PDF-Seite, das PDF liegt unverändert
        // im Asset-Ordner
        let (mut reloaded, _) = load_notebook(&dir).unwrap();
        load_all_pages(&dir, &mut reloaded).unwrap();
        for (i, page) in reloaded.pages.iter().enumerate() {
            let bg = page.pdf_background.as_ref().expect("PDF-Hintergrund fehlt");
            assert_eq!(bg.page, i as u32);
            assert_eq!(page.background, PageBackground::Blank);
        }
        let first = reloaded.pages[0].pdf_background.clone().unwrap();
        assert_eq!((first.width_pt, first.height_pt), (842.0, 595.0));
        let assets = crate::assets::AssetStore::new(&dir);
        assert_eq!(assets.read_bytes(&first.asset).unwrap(), pdf);

        // Kein PDF: Fehler und kein halbfertiges Notizbuch
        let before = std::fs::read_dir(temp_dir.path()).unwrap().count();
        assert!(
            create_notebook_from_pdf(temp_dir.path(), "Kaputt", b"%PDF-kaputt".to_vec()).is_err()
        );
        assert_eq!(std::fs::read_dir(temp_dir.path()).unwrap().count(), before);
    }

    #[test]
    fn test_page_titles_survive_restart_and_lazy_loading() {
        let temp_dir = tempfile::tempdir().unwrap();
        let (mut notebook, mut manifest, dir) =
            create_new_notebook(temp_dir.path(), "Mit Titeln").unwrap();
        notebook.add_page(PageBackground::Lined);
        notebook.add_page(PageBackground::Lined);
        let ids: Vec<u64> = notebook.pages.iter().map(|p| p.id).collect();
        assert!(notebook.set_page_title(ids[0], "  Einleitung \n"));
        assert!(notebook.set_page_title(ids[2], "Kapitel 2 – Integrale"));
        assert!(!notebook.set_page_title(ids[2], "Kapitel 2 – Integrale"));
        save_notebook(&dir, &notebook, &mut manifest).unwrap();
        assert_eq!(manifest.pages[0].title.as_deref(), Some("Einleitung"));

        // Neustart: Folgeseiten sind Platzhalter, kennen ihren Titel aber aus dem Manifest
        let (mut reloaded, _) = load_notebook(&dir).unwrap();
        assert!(!reloaded.pages[2].is_loaded);
        assert_eq!(
            reloaded.pages[2].title.as_deref(),
            Some("Kapitel 2 – Integrale")
        );
        ensure_page_loaded(&dir, &mut reloaded, 2).unwrap();
        assert_eq!(
            reloaded.pages[2].title.as_deref(),
            Some("Kapitel 2 – Integrale")
        );
        assert_eq!(reloaded.pages[1].title, None);

        // Titel entfernen
        assert!(reloaded.set_page_title(ids[0], "   "));
        assert_eq!(reloaded.pages[0].title, None);
    }
}
