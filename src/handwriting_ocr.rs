//! 100 % lokale Offline-Handschrifterkennung mit `tract-onnx` (Pure Rust) und
//! optionalem PP-OCRv5-Latin-Sprachpaket (~8 MB, inkl. deutscher Umlaute ä, ö, ü, ß).
//!
//! Architektur:
//! 1. Deterministische Vektor-Zeilensegmentierung direkt aus den Strichen (kein schweres
//!    Detection-Netz nötig).
//! 2. Schneller CPU-Rasterer (48 px Höhe, anti-aliased dunkel auf weißem Grund, normiert auf [-1, 1]).
//! 3. ONNX-Inferenz via `tract-onnx` + CTC-Greedy-Decoding.
//! 4. Lokaler Cache `<notebook_dir>/.ocr_cache.json` (mit führendem Punkt → kein Cloud-Sync)
//!    für die Volltext-Suche (`Strg+F`) sowie asynchrone Umwandlung einer Auswahl in ein Textfeld.

use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

use glam::Vec2;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tract_onnx::prelude::*;

use crate::document::{Aabb, Stroke, StrokeTool};
use crate::paths;
use crate::storage::{self, StrokeData};
use crate::t;

/// Unterordner unter `paths::data_dir()` für das optionale Sprachpaket.
const MODEL_SUBDIR: &str = "models/ppocrv5-latin";
/// Standard-Download-Basis-URL auf dem eigenen Gitea-Server (Release `ocr-models-v1`).
pub const DEFAULT_MODEL_BASE_URL: &str =
    "https://gitea.pansi.eu/harald/ink-notes/releases/download/ocr-models-v1";
/// Optionale Umgebungsvariable für einen alternativen HTTPS-Mirror.
const MODEL_BASE_URL_ENV: &str = "INK_NOTES_OCR_BASE_URL";

pub const REC_MODEL_FILENAME: &str = "latin_ppocrv5_rec.onnx";
pub const REC_MODEL_SHA256: &str =
    "614ffc2d6d3902d360fad7f1b0dd455ee45e877069d14c4e51a99dc4ef144409";
pub const DICT_FILENAME: &str = "latin_ppocrv5_dict.txt";
pub const DICT_SHA256: &str = "3c0a8a79b612653c25f765271714f71281e4e955962c153e272b7b8c1d2b13ff";

/// Maximale Dateigröße für das ONNX-Modell beim Download (32 MiB).
const MAX_MODEL_BYTES: usize = 32 * 1024 * 1024;
/// Maximale Dateigröße für das Wörterbuch beim Download (1 MiB).
const MAX_DICT_BYTES: usize = 1024 * 1024;
/// Maximale Dateigröße eines lokalen `.ocr_cache.json` (16 MiB).
const MAX_OCR_CACHE_BYTES: u64 = 16 * 1024 * 1024;

/// Zielhöhe in Pixeln für `latin_PP-OCRv5_mobile_rec`.
pub const TARGET_HEIGHT: u32 = 48;
/// Weißer Rand rund um jede gerasterte Zeile in Pixeln.
const RASTER_PADDING: u32 = 4;
/// Maximale Breite einer gerasterten Zeile in Pixeln (Schutz vor extremen Breiten).
const MAX_LINE_WIDTH: u32 = 2048;

/// Name der lokalen OCR-Cache-Datei pro Notizbuch-Ordner (führender Punkt → kein Cloud-Sync).
pub const OCR_CACHE_FILENAME: &str = ".ocr_cache.json";
const OCR_CACHE_VERSION: u32 = 1;

/// Ordner für die lokalen Modell-Dateien (`$XDG_DATA_HOME/ink-notes/models/ppocrv5-latin`).
pub fn model_dir() -> Option<PathBuf> {
    paths::data_dir().map(|d| d.join(MODEL_SUBDIR))
}

/// Prüft, ob beide Modell-Dateien im angegebenen Ordner vorhanden sind.
pub fn is_model_installed_in(dir: &Path) -> bool {
    dir.join(REC_MODEL_FILENAME).is_file() && dir.join(DICT_FILENAME).is_file()
}

/// Prüft, ob das optionale Sprachpaket lokal installiert ist.
pub fn is_model_installed() -> bool {
    model_dir().is_some_and(|d| is_model_installed_in(&d))
}

/// Liefert die konfigurierte Basis-URL (Standard: Gitea, optional per Env überschreibbar).
fn model_base_url() -> String {
    std::env::var(MODEL_BASE_URL_ENV)
        .ok()
        .map(|s| s.trim().trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_MODEL_BASE_URL.to_string())
}

/// Prüft, ob ein Strich als Handschrift zählt (Stiftstrich, keine erkannte Form, kein Bild/Text).
#[inline]
pub fn is_handwriting_stroke(stroke: &Stroke) -> bool {
    !stroke.is_object()
        && stroke.tool == StrokeTool::Pen
        && stroke.shape.is_none()
        && !stroke.points.is_empty()
}

#[inline]
fn is_handwriting_stroke_data(stroke: &StrokeData) -> bool {
    stroke.image.is_none()
        && stroke.text.is_none()
        && stroke.tool == StrokeTool::Pen
        && stroke.shape.is_none()
        && !stroke.samples.is_empty()
}

/// Schneller, deterministischer FNV-1a-Hash (64 Bit) über die Handschrift-Striche einer Seite.
/// Gibt `0` zurück, wenn die Seite keine Handschrift-Striche enthält.
pub fn page_handwriting_fingerprint(strokes: &[Stroke]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    let mut count = 0u64;
    for s in strokes.iter().filter(|s| is_handwriting_stroke(s)) {
        count += 1;
        fnv_mix_u64(&mut hash, s.id);
        fnv_mix_u64(&mut hash, s.points.len() as u64);
        if let Some(first) = s.points.first() {
            fnv_mix_f32(&mut hash, first.position.x);
            fnv_mix_f32(&mut hash, first.position.y);
        }
        if let Some(last) = s.points.last() {
            fnv_mix_f32(&mut hash, last.position.x);
            fnv_mix_f32(&mut hash, last.position.y);
        }
        fnv_mix_f32(&mut hash, s.aabb.min.x);
        fnv_mix_f32(&mut hash, s.aabb.min.y);
        fnv_mix_f32(&mut hash, s.aabb.max.x);
        fnv_mix_f32(&mut hash, s.aabb.max.y);
    }
    if count == 0 {
        0
    } else {
        fnv_mix_u64(&mut hash, count);
        hash.max(1)
    }
}

/// Entsprechender Fingerprint direkt aus serialisierten `StrokeData` (für ungeladene Seiten).
pub fn page_data_handwriting_fingerprint(strokes: &[StrokeData]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    let mut count = 0u64;
    for s in strokes.iter().filter(|s| is_handwriting_stroke_data(s)) {
        count += 1;
        fnv_mix_u64(&mut hash, s.id);
        fnv_mix_u64(&mut hash, s.samples.len() as u64);
        if let Some(first) = s.samples.first() {
            fnv_mix_f32(&mut hash, first.x);
            fnv_mix_f32(&mut hash, first.y);
        }
        if let Some(last) = s.samples.last() {
            fnv_mix_f32(&mut hash, last.x);
            fnv_mix_f32(&mut hash, last.y);
        }
        let mut min_x = f32::INFINITY;
        let mut min_y = f32::INFINITY;
        let mut max_x = f32::NEG_INFINITY;
        let mut max_y = f32::NEG_INFINITY;
        for pt in &s.samples {
            min_x = min_x.min(pt.x);
            min_y = min_y.min(pt.y);
            max_x = max_x.max(pt.x);
            max_y = max_y.max(pt.y);
        }
        fnv_mix_f32(&mut hash, min_x);
        fnv_mix_f32(&mut hash, min_y);
        fnv_mix_f32(&mut hash, max_x);
        fnv_mix_f32(&mut hash, max_y);
    }
    if count == 0 {
        0
    } else {
        fnv_mix_u64(&mut hash, count);
        hash.max(1)
    }
}

#[inline]
fn fnv_mix_u64(hash: &mut u64, val: u64) {
    for b in val.to_le_bytes() {
        *hash ^= b as u64;
        *hash = hash.wrapping_mul(0x100000001b3);
    }
}

#[inline]
fn fnv_mix_f32(hash: &mut u64, val: f32) {
    // Auf 0.5 Einheiten quantisieren, damit winzige Float-Rundungen keinen Re-OCR auslösen
    let q = (val * 2.0).round() as i64;
    fnv_mix_u64(hash, q as u64);
}

// ---------------------------------------------------------------------------
// Lokaler OCR-Cache pro Notizbuch (.ocr_cache.json)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageOcrEntry {
    pub fingerprint: u64,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OcrCache {
    #[serde(default = "default_cache_version")]
    pub version: u32,
    #[serde(default)]
    pub pages: HashMap<String, PageOcrEntry>,
}

fn default_cache_version() -> u32 {
    OCR_CACHE_VERSION
}

impl Default for OcrCache {
    fn default() -> Self {
        Self {
            version: OCR_CACHE_VERSION,
            pages: HashMap::new(),
        }
    }
}

impl OcrCache {
    /// Lädt `<nb_dir>/.ocr_cache.json`. Bei fehlender oder defekter Datei wird ein leerer Cache
    /// zurückgegeben.
    pub fn load(nb_dir: &Path) -> Self {
        let path = nb_dir.join(OCR_CACHE_FILENAME);
        let Ok(meta) = fs::metadata(&path) else {
            return Self::default();
        };
        if meta.len() > MAX_OCR_CACHE_BYTES {
            return Self::default();
        }
        let Ok(bytes) = fs::read(&path) else {
            return Self::default();
        };
        match serde_json::from_slice::<Self>(&bytes) {
            Ok(cache) if cache.version == OCR_CACHE_VERSION => cache,
            _ => Self::default(),
        }
    }

    /// Speichert den Cache atomar nach `<nb_dir>/.ocr_cache.json`.
    pub fn save(&self, nb_dir: &Path) -> Result<(), String> {
        let path = nb_dir.join(OCR_CACHE_FILENAME);
        let json = serde_json::to_vec_pretty(self)
            .map_err(|e| format!("OCR-Cache Serialisierung fehlgeschlagen: {e}"))?;
        storage::atomic_write_file(&path, &json)
            .map_err(|e| format!("OCR-Cache konnte nicht gespeichert werden: {e}"))
    }

    /// Liefert `true`, wenn für `page_id` bereits ein aktueller Eintrag mit gleichem Fingerprint
    /// vorliegt.
    pub fn is_up_to_date(&self, page_id: u64, fingerprint: u64) -> bool {
        self.pages
            .get(&page_id.to_string())
            .is_some_and(|e| e.fingerprint == fingerprint)
    }

    /// Aktualisiert den Eintrag einer Seite. Leere Seiten (Fingerprint 0) werden entfernt.
    pub fn set_page(&mut self, page_id: u64, fingerprint: u64, text: String) {
        let key = page_id.to_string();
        if fingerprint == 0 || text.trim().is_empty() {
            if fingerprint == 0 {
                self.pages.remove(&key);
            } else {
                self.pages.insert(
                    key,
                    PageOcrEntry {
                        fingerprint,
                        text: String::new(),
                    },
                );
            }
        } else {
            self.pages.insert(key, PageOcrEntry { fingerprint, text });
        }
    }

    /// Liefert den erkannten Text einer Seite (falls vorhanden und nicht leer).
    pub fn page_text(&self, page_id: u64) -> Option<&str> {
        self.pages
            .get(&page_id.to_string())
            .map(|e| e.text.as_str())
            .filter(|s| !s.is_empty())
    }
}

// ---------------------------------------------------------------------------
// 1. Vektor-Zeilensegmentierung
// ---------------------------------------------------------------------------

struct LineBand<'a> {
    strokes: Vec<&'a Stroke>,
    min_y: f32,
    max_y: f32,
    min_x: f32,
    max_x: f32,
}

impl<'a> LineBand<'a> {
    fn new(s: &'a Stroke) -> Self {
        Self {
            strokes: vec![s],
            min_y: s.aabb.min.y,
            max_y: s.aabb.max.y,
            min_x: s.aabb.min.x,
            max_x: s.aabb.max.x,
        }
    }

    fn height(&self) -> f32 {
        (self.max_y - self.min_y).max(1.0)
    }

    fn center_y(&self) -> f32 {
        0.5 * (self.min_y + self.max_y)
    }

    fn push(&mut self, s: &'a Stroke) {
        self.min_y = self.min_y.min(s.aabb.min.y);
        self.max_y = self.max_y.max(s.aabb.max.y);
        self.min_x = self.min_x.min(s.aabb.min.x);
        self.max_x = self.max_x.max(s.aabb.max.x);
        self.strokes.push(s);
    }

    fn merge_from(&mut self, other: LineBand<'a>) {
        self.min_y = self.min_y.min(other.min_y);
        self.max_y = self.max_y.max(other.max_y);
        self.min_x = self.min_x.min(other.min_x);
        self.max_x = self.max_x.max(other.max_x);
        self.strokes.extend(other.strokes);
    }
}

/// Gruppiert Handschrift-Striche deterministisch in Zeilen (oben → unten) und sortiert innerhalb
/// jeder Zeile von links nach rechts. Kleine Diakritika (Punkte auf i/j/ä/ö/ü) werden an die
/// darunterliegende Zeile angehängt.
pub fn group_strokes_into_lines(strokes: &[Stroke]) -> Vec<Vec<&Stroke>> {
    let mut hw: Vec<&Stroke> = strokes
        .iter()
        .filter(|s| is_handwriting_stroke(s))
        .collect();
    if hw.is_empty() {
        return Vec::new();
    }

    // Nach vertikalem Mittelpunkt sortieren (oben → unten), bei Gleichstand links → rechts
    hw.sort_by(|a, b| {
        let ca = 0.5 * (a.aabb.min.y + a.aabb.max.y);
        let cb = 0.5 * (b.aabb.min.y + b.aabb.max.y);
        ca.total_cmp(&cb)
            .then_with(|| a.aabb.min.x.total_cmp(&b.aabb.min.x))
    });

    // Pass 1: Striche mit vertikaler Überlappung zu Zeilenbändern gruppieren
    let mut bands: Vec<LineBand<'_>> = Vec::new();
    for s in hw {
        let s_min = s.aabb.min.y;
        let s_max = s.aabb.max.y;
        let s_h = (s_max - s_min).max(1.0);
        let s_cy = 0.5 * (s_min + s_max);

        let mut best_idx: Option<usize> = None;
        let mut best_score = 0.0_f32;
        for (i, band) in bands.iter().enumerate() {
            let b_h = band.height();
            let overlap = (s_max.min(band.max_y) - s_min.max(band.min_y)).max(0.0);
            let min_h = s_h.min(b_h).max(1.0);
            let overlap_ratio = overlap / min_h;
            let margin = b_h * 0.2;
            let center_in_band = s_cy >= band.min_y - margin && s_cy <= band.max_y + margin;

            if overlap_ratio >= 0.30 || center_in_band {
                let score = overlap_ratio + if center_in_band { 0.5 } else { 0.0 };
                if score > best_score {
                    best_score = score;
                    best_idx = Some(i);
                }
            }
        }

        if let Some(idx) = best_idx {
            bands[idx].push(s);
        } else {
            bands.push(LineBand::new(s));
        }
    }

    // Pass 2: Kleine Diakritika-Bänder (i-Punkte, Umlaut-Punkte) in die direkt darunterliegende
    // Zeile mergen, sofern sie horizontal überlappen und vertikal nahe sind.
    if bands.len() > 1 {
        let mut heights: Vec<f32> = bands.iter().map(LineBand::height).collect();
        heights.sort_by(f32::total_cmp);
        let median_h = heights[heights.len() / 2].max(10.0);

        let mut i = 0;
        while i < bands.len() {
            if bands[i].height() < median_h * 0.45 {
                let small_min_x = bands[i].min_x;
                let small_max_x = bands[i].max_x;
                let small_max_y = bands[i].max_y;
                let small_min_y = bands[i].min_y;
                let horiz_pad = median_h * 0.6;
                let max_gap = median_h * 0.75;

                // Nächstgelegenes Band suchen (bevorzugt direkt darunter, z. B. Umlaut-Punkte)
                let target = bands
                    .iter()
                    .enumerate()
                    .filter(|&(j, b)| {
                        if j == i || b.height() < median_h * 0.45 {
                            return false;
                        }
                        let horiz_overlap = small_max_x + horiz_pad >= b.min_x
                            && small_min_x - horiz_pad <= b.max_x;
                        let gap_below = (b.min_y - small_max_y).max(0.0);
                        let gap_above = (small_min_y - b.max_y).max(0.0);
                        horiz_overlap && gap_below.min(gap_above) <= max_gap
                    })
                    .min_by(|(_, a), (_, b)| {
                        let da = (a.center_y() - bands[i].center_y()).abs();
                        let db = (b.center_y() - bands[i].center_y()).abs();
                        da.total_cmp(&db)
                    })
                    .map(|(j, _)| j);

                if let Some(mut dst) = target {
                    let removed = bands.remove(i);
                    if dst > i {
                        dst -= 1;
                    }
                    bands[dst].merge_from(removed);
                    continue;
                }
            }
            i += 1;
        }
    }

    // Zeilen von oben nach unten, Striche innerhalb einer Zeile von links nach rechts sortieren
    bands.sort_by(|a, b| a.center_y().total_cmp(&b.center_y()));
    bands
        .into_iter()
        .map(|mut band| {
            band.strokes
                .sort_by(|a, b| a.aabb.min.x.total_cmp(&b.aabb.min.x));
            band.strokes
        })
        .collect()
}

// ---------------------------------------------------------------------------
// 2. Schneller CPU-Rasterer (48 px Höhe, NCHW [1, 3, 48, W], Normierung [-1, 1])
// ---------------------------------------------------------------------------

/// Rastert eine einzelne Handschrift-Zeile auf `target_height` Pixel Höhe (dunkel auf weißem
/// Grund) und liefert `(width, nchw_tensor_data)` im Format `[1, 3, target_height, width]` mit
/// PP-OCRv5-Normierung `(v / 255.0 - 0.5) / 0.5`.
pub fn rasterize_line(strokes: &[&Stroke], target_height: u32) -> Option<(u32, Vec<f32>)> {
    if strokes.is_empty() || target_height <= 2 * RASTER_PADDING {
        return None;
    }

    // Gesamt-Bounding-Box aller Strichpunkte bestimmen
    let mut bounds: Option<Aabb> = None;
    for s in strokes {
        for p in &s.points {
            bounds = Some(match bounds {
                Some(mut b) => {
                    b.expand_by_point(p.position);
                    b
                }
                None => Aabb::from_point(p.position),
            });
        }
    }
    let bounds = bounds?;
    let content_w = (bounds.max.x - bounds.min.x).max(1.0);
    let content_h = (bounds.max.y - bounds.min.y).max(1.0);

    let inner_h = (target_height - 2 * RASTER_PADDING) as f32;
    let mut scale = inner_h / content_h;

    let raw_w = (content_w * scale).ceil() as u32 + 2 * RASTER_PADDING;
    let width = raw_w.clamp(target_height, MAX_LINE_WIDTH);
    if raw_w > MAX_LINE_WIDTH {
        scale = (MAX_LINE_WIDTH - 2 * RASTER_PADDING) as f32 / content_w;
    }

    let h = target_height as usize;
    let w = width as usize;
    // Graustufenpuffer: 1.0 = weiß (Papier), 0.0 = schwarz (Tinte)
    let mut gray = vec![1.0_f32; h * w];

    // Vertikale Zentrierung bei extrem breiten Zeilen (wenn `scale` begrenzt wurde)
    let drawn_h = content_h * scale;
    let offset_x = RASTER_PADDING as f32 - bounds.min.x * scale;
    let offset_y = 0.5 * (target_height as f32 - drawn_h) - bounds.min.y * scale;

    for s in strokes {
        let radius = (s.width * 0.5 * scale).clamp(1.0, 2.2);
        if s.points.len() == 1 {
            let p = Vec2::new(
                s.points[0].position.x * scale + offset_x,
                s.points[0].position.y * scale + offset_y,
            );
            draw_capsule(&mut gray, w, h, p, p, radius);
        } else {
            for pair in s.points.windows(2) {
                let a = Vec2::new(
                    pair[0].position.x * scale + offset_x,
                    pair[0].position.y * scale + offset_y,
                );
                let b = Vec2::new(
                    pair[1].position.x * scale + offset_x,
                    pair[1].position.y * scale + offset_y,
                );
                draw_capsule(&mut gray, w, h, a, b, radius);
            }
        }
    }

    // In NCHW [1, 3, H, W] umwandeln: (pixel - 0.5) / 0.5 = 2 * pixel - 1
    // (1.0 weiß -> +1.0, 0.0 schwarz -> -1.0)
    let plane_len = h * w;
    let mut tensor = vec![0.0_f32; 3 * plane_len];
    for (idx, &v) in gray.iter().enumerate() {
        let norm = v * 2.0 - 1.0;
        tensor[idx] = norm;
        tensor[plane_len + idx] = norm;
        tensor[2 * plane_len + idx] = norm;
    }

    Some((width, tensor))
}

/// Zeichnet eine anti-aliased dunkle Kapsel zwischen `a` und `b` in den Graustufenpuffer.
fn draw_capsule(buf: &mut [f32], w: usize, h: usize, a: Vec2, b: Vec2, radius: f32) {
    let pad = radius + 1.0;
    let min_x = (a.x.min(b.x) - pad).floor().max(0.0) as usize;
    let max_x = ((a.x.max(b.x) + pad).ceil() as usize).min(w.saturating_sub(1));
    let min_y = (a.y.min(b.y) - pad).floor().max(0.0) as usize;
    let max_y = ((a.y.max(b.y) + pad).ceil() as usize).min(h.saturating_sub(1));

    let ab = b - a;
    let ab_len_sq = ab.length_squared();

    for py in min_y..=max_y {
        let fy = py as f32 + 0.5;
        let row = py * w;
        for px in min_x..=max_x {
            let fx = px as f32 + 0.5;
            let p = Vec2::new(fx, fy);
            let dist = if ab_len_sq < 1e-6 {
                (p - a).length()
            } else {
                let t = ((p - a).dot(ab) / ab_len_sq).clamp(0.0, 1.0);
                (p - (a + ab * t)).length()
            };
            if dist < radius + 1.0 {
                // Weiches Anti-Aliasing über 1 px Rand
                let cov = (radius + 0.5 - dist).clamp(0.0, 1.0);
                let val = 1.0 - cov;
                let cell = &mut buf[row + px];
                if val < *cell {
                    *cell = val;
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 3. CTC Greedy Decoding
// ---------------------------------------------------------------------------

/// Baut das CTC-Vokabular aus den Zeilen einer PaddleOCR-`dict.txt` auf:
/// Index `0` ist das CTC-Blank-Token, `1..=N` die Wörterbuchzeilen und `N+1` das Leerzeichen
/// (`use_space_char = true` in PaddleOCR).
pub fn build_ctc_vocab(dict_contents: &str) -> Vec<String> {
    let mut vocab = Vec::new();
    vocab.push(String::new()); // Index 0 = CTC blank
    for line in dict_contents.lines() {
        vocab.push(line.to_string());
    }
    vocab.push(" ".to_string()); // PaddleOCR space char am Ende
    vocab
}

/// Dekodiert eine Sequenz von Klassen-Wahrscheinlichkeiten/Logits (`time_steps × num_classes`,
/// flach im Row-Major-Layout) per CTC Greedy Decoding.
pub fn ctc_greedy_decode(
    logits: &[f32],
    time_steps: usize,
    num_classes: usize,
    vocab: &[String],
) -> String {
    if time_steps == 0 || num_classes == 0 || logits.len() < time_steps * num_classes {
        return String::new();
    }

    let mut out = String::new();
    let mut prev_idx: usize = 0;

    for t in 0..time_steps {
        let row = &logits[t * num_classes..(t + 1) * num_classes];
        let mut best_idx = 0usize;
        let mut best_val = f32::NEG_INFINITY;
        for (c, &v) in row.iter().enumerate() {
            if v > best_val {
                best_val = v;
                best_idx = c;
            }
        }
        if best_idx != 0 && best_idx != prev_idx {
            if let Some(tok) = vocab.get(best_idx) {
                out.push_str(tok);
            }
        }
        prev_idx = best_idx;
    }

    // Mehrfache Leerzeichen zusammenfassen und Ränder trimmen
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

// ---------------------------------------------------------------------------
// 4. ONNX-Inferenz-Engine (`tract-onnx`)
// ---------------------------------------------------------------------------

type RunnableOcrModel = std::sync::Arc<TypedRunnableModel>;

/// Geladene Offline-Handschrift-OCR-Engine (`latin_PP-OCRv5_mobile_rec`).
pub struct HandwritingOcrEngine {
    model: RunnableOcrModel,
    vocab: Vec<String>,
}

impl HandwritingOcrEngine {
    /// Lädt `latin_ppocrv5_rec.onnx` und `latin_ppocrv5_dict.txt` aus `dir`.
    pub fn load_from_dir(dir: &Path) -> Result<Self, String> {
        let model_path = dir.join(REC_MODEL_FILENAME);
        let dict_path = dir.join(DICT_FILENAME);

        let dict_contents = fs::read_to_string(&dict_path)
            .map_err(|e| format!("Wörterbuch konnte nicht gelesen werden: {e}"))?;
        let vocab = build_ctc_vocab(&dict_contents);

        let mut onnx_model = tract_onnx::onnx()
            .model_for_path(&model_path)
            .map_err(|e| format!("ONNX-Modell konnte nicht geladen werden: {e}"))?;

        // Eingabeform fest auf Batch=1, 3 Kanäle, Höhe=48, symbolische Breite W setzen,
        // damit tract das Modell für beliebige Zeilenbreiten optimieren kann.
        let w_sym = onnx_model.symbols.sym("W");
        onnx_model
            .set_input_fact(
                0,
                InferenceFact::dt_shape(
                    f32::datum_type(),
                    tvec![
                        TDim::from(1),
                        TDim::from(3),
                        TDim::from(TARGET_HEIGHT as i64),
                        TDim::from(w_sym),
                    ],
                ),
            )
            .map_err(|e| format!("ONNX-Input-Shape konnte nicht gesetzt werden: {e}"))?;

        let model = onnx_model
            .into_optimized()
            .map_err(|e| format!("ONNX-Optimierung fehlgeschlagen: {e}"))?
            .into_runnable()
            .map_err(|e| format!("ONNX-Runnable fehlgeschlagen: {e}"))?;

        Ok(Self { model, vocab })
    }

    /// Erkennt den Text aus einem vorbereiteten NCHW-Tensor `[1, 3, TARGET_HEIGHT, width]`.
    pub fn recognize_tensor(&self, width: u32, data: Vec<f32>) -> Result<String, String> {
        let tensor = Tensor::from_shape(&[1, 3, TARGET_HEIGHT as usize, width as usize], &data)
            .map_err(|e| format!("Tensor-Erstellung fehlgeschlagen: {e}"))?;

        let outputs = self
            .model
            .run(tvec!(tensor.into()))
            .map_err(|e| format!("ONNX-Inferenz fehlgeschlagen: {e}"))?;

        let out = outputs
            .first()
            .ok_or_else(|| "ONNX-Modell lieferte keine Ausgabe".to_string())?;
        let shape = out.shape();
        if shape.len() != 3 || shape[0] != 1 {
            return Err(format!("Unerwartete ONNX-Ausgabeform: {shape:?}"));
        }
        let time_steps = shape[1];
        let num_classes = shape[2];
        let plain = out
            .try_as_plain_ram()
            .map_err(|e| format!("ONNX-Ausgabe konnte nicht gelesen werden: {e}"))?;
        let slice = plain
            .as_slice::<f32>()
            .map_err(|e| format!("ONNX-Ausgabe kein f32-Tensor: {e}"))?;

        Ok(ctc_greedy_decode(
            slice,
            time_steps,
            num_classes,
            &self.vocab,
        ))
    }

    /// Segmentiert `strokes` in Zeilen und erkennt jede Zeile einzeln.
    pub fn recognize_strokes(&self, strokes: &[Stroke]) -> Result<String, String> {
        let lines = group_strokes_into_lines(strokes);
        if lines.is_empty() {
            return Ok(String::new());
        }
        let mut recognized_lines = Vec::new();
        for line in lines {
            if let Some((w, tensor_data)) = rasterize_line(&line, TARGET_HEIGHT) {
                let text = self.recognize_tensor(w, tensor_data)?;
                if !text.is_empty() {
                    recognized_lines.push(text);
                }
            }
        }
        Ok(recognized_lines.join("\n"))
    }
}

// ---------------------------------------------------------------------------
// 5. Modell-Download (HTTPS + SHA-256 + Größenlimit + atomares Schreiben)
// ---------------------------------------------------------------------------

fn validate_https_url(url_str: &str) -> Result<url::Url, String> {
    let parsed = url::Url::parse(url_str)
        .map_err(|e| t!("Ungültige Download-URL '{url}': {e}", url = url_str, e = e))?;
    if parsed.scheme() != "https" {
        return Err(t!(
            "Unsicheres Protokoll '{scheme}' abgewiesen – Sprachpakete werden nur über HTTPS geladen.",
            scheme = parsed.scheme()
        ));
    }
    Ok(parsed)
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut hex = String::with_capacity(64);
    for b in digest {
        use std::fmt::Write;
        let _ = write!(&mut hex, "{b:02x}");
    }
    hex
}

fn download_verified_file(
    url_str: &str,
    expected_sha256: &str,
    max_bytes: usize,
    mut on_progress: impl FnMut(f32),
) -> Result<Vec<u8>, String> {
    let _ = validate_https_url(url_str)?;

    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(15))
        .timeout_read(Duration::from_secs(60))
        .build();

    let resp = agent
        .get(url_str)
        .call()
        .map_err(|e| t!("Download fehlgeschlagen: {e}", e = e))?;

    let content_len = resp
        .header("Content-Length")
        .and_then(|s| s.parse::<usize>().ok());
    if let Some(len) = content_len {
        if len > max_bytes {
            return Err(t!("Datei überschreitet die zulässige Größe").to_string());
        }
    }

    let mut reader = resp.into_reader();
    let mut buf = Vec::with_capacity(content_len.unwrap_or(64 * 1024).min(max_bytes));
    let mut chunk = [0u8; 32 * 1024];
    let mut last_report = Instant::now();

    loop {
        let n = reader
            .read(&mut chunk)
            .map_err(|e| t!("Lesefehler beim Download: {e}", e = e))?;
        if n == 0 {
            break;
        }
        if buf.len() + n > max_bytes {
            return Err(t!("Datei überschreitet die zulässige Größe").to_string());
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(total) = content_len {
            if total > 0 && last_report.elapsed() >= Duration::from_millis(80) {
                on_progress((buf.len() as f32 / total as f32).clamp(0.0, 1.0));
                last_report = Instant::now();
            }
        }
    }
    on_progress(1.0);

    let actual = sha256_hex(&buf);
    if !actual.eq_ignore_ascii_case(expected_sha256) {
        return Err(t!(
            "Prüfsumme stimmt nicht überein (erwartet {expected}, erhalten {actual})",
            expected = &expected_sha256[..12],
            actual = &actual[..12]
        ));
    }

    Ok(buf)
}

/// Löscht das lokal installierte Sprachpaket.
pub fn remove_model_in(dir: &Path) -> Result<(), String> {
    for name in [REC_MODEL_FILENAME, DICT_FILENAME] {
        let p = dir.join(name);
        if p.exists() {
            fs::remove_file(&p).map_err(|e| format!("Konnte {name} nicht löschen: {e}"))?;
        }
    }
    let _ = fs::remove_dir(dir);
    Ok(())
}

// ---------------------------------------------------------------------------
// 6. Asynchroner Controller (Download, Hintergrund-Indexierung, Auswahl→Text)
// ---------------------------------------------------------------------------

/// Aktueller Zustand der Handschrifterkennung für die UI.
#[derive(Debug, Clone, PartialEq)]
pub enum OcrStatus {
    /// Sprachpaket nicht installiert.
    NotInstalled,
    /// Sprachpaket wird heruntergeladen (`0.0..=1.0`).
    Downloading { percent: f32 },
    /// Sprachpaket aktiv und bereit.
    Ready {
        indexing: bool,
        indexed_pages: usize,
        total_pages: usize,
    },
    /// Letzter Fehler (Download oder Laden).
    Error(String),
}

/// Ereignis vom Hintergrund-Worker an `AppState`.
#[derive(Debug, Clone)]
pub enum OcrEvent {
    /// Eine Seite wurde im Hintergrund indexiert.
    PageIndexed { page_id: u64 },
    /// Komplettes Notizbuch wurde fertig indexiert.
    NotebookIndexed { recognized_pages: usize },
    /// Auswahl wurde erfolgreich in Text umgewandelt.
    SelectionConverted {
        stroke_ids: Vec<u64>,
        bounds: Aabb,
        text: String,
    },
    /// Fehler bei der Umwandlung einer Auswahl.
    SelectionError(String),
}

enum WorkerCommand {
    IndexPage {
        nb_dir: PathBuf,
        page_id: u64,
        fingerprint: u64,
        strokes: Vec<Stroke>,
    },
    IndexNotebook {
        nb_dir: PathBuf,
        /// `(page_id, Option<bereits geladene Striche>)`
        pages: Vec<(u64, Option<Vec<Stroke>>)>,
    },
    ConvertSelection {
        stroke_ids: Vec<u64>,
        bounds: Aabb,
        strokes: Vec<Stroke>,
    },
    UnloadModel,
}

enum WorkerReply {
    DownloadProgress(f32),
    DownloadFinished(Result<(), String>),
    IndexingProgress {
        indexed: usize,
        total: usize,
    },
    IndexingDone {
        recognized_pages: usize,
    },
    PageIndexed {
        page_id: u64,
    },
    SelectionConverted {
        stroke_ids: Vec<u64>,
        bounds: Aabb,
        text: String,
    },
    SelectionError(String),
    EngineError(String),
}

/// Verwaltet Download, Hintergrund-Suchindex und Auswahl-Erkennung ohne die UI zu blockieren.
pub struct OcrController {
    status: OcrStatus,
    cmd_tx: Option<Sender<WorkerCommand>>,
    reply_tx: Sender<WorkerReply>,
    reply_rx: Receiver<WorkerReply>,
    /// Gedrosselte Seiten-Anfragen (erst kurz nach dem Speichern abschicken).
    pending_page: Option<(Instant, PathBuf, u64, u64, Vec<Stroke>)>,
}

impl Default for OcrController {
    fn default() -> Self {
        Self::new()
    }
}

impl OcrController {
    pub fn new() -> Self {
        let (reply_tx, reply_rx) = mpsc::channel();
        let status = if is_model_installed() {
            OcrStatus::Ready {
                indexing: false,
                indexed_pages: 0,
                total_pages: 0,
            }
        } else {
            OcrStatus::NotInstalled
        };
        Self {
            status,
            cmd_tx: None,
            reply_tx,
            reply_rx,
            pending_page: None,
        }
    }

    pub fn status(&self) -> &OcrStatus {
        &self.status
    }

    pub fn is_ready(&self) -> bool {
        matches!(self.status, OcrStatus::Ready { .. })
    }

    /// Stellt sicher, dass der Hintergrund-Worker-Thread läuft.
    fn ensure_worker(&mut self) -> Option<&Sender<WorkerCommand>> {
        if self.cmd_tx.is_none() {
            let dir = model_dir()?;
            let (cmd_tx, cmd_rx) = mpsc::channel::<WorkerCommand>();
            let reply_tx = self.reply_tx.clone();
            thread::Builder::new()
                .name("ink-notes-ocr".to_string())
                .spawn(move || ocr_worker_loop(dir, cmd_rx, reply_tx))
                .ok()?;
            self.cmd_tx = Some(cmd_tx);
        }
        self.cmd_tx.as_ref()
    }

    /// Startet den asynchronen Download des ~8-MB-Sprachpakets (HTTPS + SHA-256).
    pub fn start_download(&mut self) {
        if matches!(self.status, OcrStatus::Downloading { .. }) {
            return;
        }
        let Some(dir) = model_dir() else {
            self.status = OcrStatus::Error(t!("Kein Datenordner verfügbar").to_string());
            return;
        };
        self.status = OcrStatus::Downloading { percent: 0.0 };
        let reply_tx = self.reply_tx.clone();
        let base_url = model_base_url();

        let _ = thread::Builder::new()
            .name("ink-notes-ocr-dl".to_string())
            .spawn(move || {
                let res = (|| -> Result<(), String> {
                    fs::create_dir_all(&dir)
                        .map_err(|e| format!("Modell-Ordner konnte nicht angelegt werden: {e}"))?;

                    let rec_url = format!("{base_url}/{REC_MODEL_FILENAME}");
                    let dict_url = format!("{base_url}/{DICT_FILENAME}");

                    let rec_bytes =
                        download_verified_file(&rec_url, REC_MODEL_SHA256, MAX_MODEL_BYTES, |p| {
                            let _ = reply_tx.send(WorkerReply::DownloadProgress(p * 0.95));
                        })?;
                    let dict_bytes =
                        download_verified_file(&dict_url, DICT_SHA256, MAX_DICT_BYTES, |p| {
                            let _ = reply_tx.send(WorkerReply::DownloadProgress(0.95 + p * 0.05));
                        })?;

                    storage::atomic_write_file(&dir.join(REC_MODEL_FILENAME), &rec_bytes)
                        .map_err(|e| format!("Modell konnte nicht gespeichert werden: {e}"))?;
                    storage::atomic_write_file(&dir.join(DICT_FILENAME), &dict_bytes)
                        .map_err(|e| format!("Wörterbuch konnte nicht gespeichert werden: {e}"))?;
                    Ok(())
                })();
                let _ = reply_tx.send(WorkerReply::DownloadFinished(res));
            });
    }

    /// Entfernt das Sprachpaket von der Festplatte und entlädt das Modell aus dem RAM.
    pub fn remove_model(&mut self) -> Result<(), String> {
        self.pending_page = None;
        if let Some(tx) = &self.cmd_tx {
            let _ = tx.send(WorkerCommand::UnloadModel);
        }
        if let Some(dir) = model_dir() {
            remove_model_in(&dir)?;
        }
        self.status = OcrStatus::NotInstalled;
        Ok(())
    }

    /// Plant die Hintergrund-Erkennung einer gespeicherten Seite (mit 1,5 s Drosselung).
    pub fn schedule_page(&mut self, nb_dir: PathBuf, page_id: u64, strokes: &[Stroke]) {
        if !self.is_ready() {
            return;
        }
        let fingerprint = page_handwriting_fingerprint(strokes);
        let hw_strokes: Vec<Stroke> = strokes
            .iter()
            .filter(|s| is_handwriting_stroke(s))
            .cloned()
            .collect();
        self.pending_page = Some((
            Instant::now() + Duration::from_millis(1500),
            nb_dir,
            page_id,
            fingerprint,
            hw_strokes,
        ));
    }

    /// Startet die Hintergrund-Indexierung aller Seiten eines Notizbuchs (überspringt unveränderte
    /// Seiten anhand des Fingerprints).
    pub fn index_notebook(&mut self, nb_dir: PathBuf, pages: Vec<(u64, Option<Vec<Stroke>>)>) {
        if !self.is_ready() || pages.is_empty() {
            return;
        }
        let total = pages.len();
        if let Some(tx) = self.ensure_worker() {
            if tx
                .send(WorkerCommand::IndexNotebook { nb_dir, pages })
                .is_ok()
            {
                self.status = OcrStatus::Ready {
                    indexing: true,
                    indexed_pages: 0,
                    total_pages: total,
                };
            }
        }
    }

    /// Startet die asynchrone Texterkennung für die aktuell ausgewählten Handschrift-Striche.
    pub fn convert_selection(&mut self, strokes: &[Stroke]) -> Result<(), String> {
        if !self.is_ready() {
            return Err(t!("Handschrift-Sprachpaket ist nicht installiert").to_string());
        }
        let hw: Vec<Stroke> = strokes
            .iter()
            .filter(|s| is_handwriting_stroke(s))
            .cloned()
            .collect();
        if hw.is_empty() {
            return Err(t!("Keine Handschrift-Striche ausgewählt").to_string());
        }
        let mut bounds = hw[0].aabb;
        let mut stroke_ids = Vec::with_capacity(hw.len());
        for s in &hw {
            bounds = bounds.union(&s.aabb);
            stroke_ids.push(s.id);
        }
        let tx = self
            .ensure_worker()
            .ok_or_else(|| t!("OCR-Worker konnte nicht gestartet werden").to_string())?;
        tx.send(WorkerCommand::ConvertSelection {
            stroke_ids,
            bounds,
            strokes: hw,
        })
        .map_err(|_| t!("OCR-Worker nicht erreichbar").to_string())
    }

    /// Pollt Antworten des Hintergrund-Workers und schickt fällige gedrosselte Seiten ab.
    /// Gibt `(status_changed, events)` zurück.
    pub fn update(&mut self) -> (bool, Vec<OcrEvent>) {
        let mut changed = false;
        let mut events = Vec::new();

        // Gedrosselte Einzel-Seite abschicken, sobald die Wartezeit abgelaufen ist
        if let Some((due, _, _, _, _)) = &self.pending_page {
            if Instant::now() >= *due {
                if let Some((_, nb_dir, page_id, fingerprint, strokes)) = self.pending_page.take() {
                    if let Some(tx) = self.ensure_worker() {
                        let _ = tx.send(WorkerCommand::IndexPage {
                            nb_dir,
                            page_id,
                            fingerprint,
                            strokes,
                        });
                    }
                }
            }
        }

        while let Ok(reply) = self.reply_rx.try_recv() {
            match reply {
                WorkerReply::DownloadProgress(p) => {
                    self.status = OcrStatus::Downloading { percent: p };
                    changed = true;
                }
                WorkerReply::DownloadFinished(Ok(())) => {
                    self.status = OcrStatus::Ready {
                        indexing: false,
                        indexed_pages: 0,
                        total_pages: 0,
                    };
                    changed = true;
                }
                WorkerReply::DownloadFinished(Err(err)) => {
                    self.status = OcrStatus::Error(err);
                    changed = true;
                }
                WorkerReply::IndexingProgress { indexed, total } => {
                    self.status = OcrStatus::Ready {
                        indexing: true,
                        indexed_pages: indexed,
                        total_pages: total,
                    };
                    changed = true;
                }
                WorkerReply::IndexingDone { recognized_pages } => {
                    self.status = OcrStatus::Ready {
                        indexing: false,
                        indexed_pages: 0,
                        total_pages: 0,
                    };
                    changed = true;
                    events.push(OcrEvent::NotebookIndexed { recognized_pages });
                }
                WorkerReply::PageIndexed { page_id } => {
                    events.push(OcrEvent::PageIndexed { page_id });
                }
                WorkerReply::SelectionConverted {
                    stroke_ids,
                    bounds,
                    text,
                } => {
                    events.push(OcrEvent::SelectionConverted {
                        stroke_ids,
                        bounds,
                        text,
                    });
                }
                WorkerReply::SelectionError(err) => {
                    events.push(OcrEvent::SelectionError(err));
                }
                WorkerReply::EngineError(err) => {
                    self.status = OcrStatus::Error(err);
                    changed = true;
                }
            }
        }

        (changed, events)
    }

    /// Nächster Zeitpunkt, zu dem `update()` wieder laufen sollte (für `ControlFlow::WaitUntil`).
    pub fn next_wake(&self) -> Option<Instant> {
        if matches!(self.status, OcrStatus::Downloading { .. })
            || matches!(self.status, OcrStatus::Ready { indexing: true, .. })
        {
            return Some(Instant::now() + Duration::from_millis(120));
        }
        self.pending_page.as_ref().map(|(due, _, _, _, _)| *due)
    }
}

fn ocr_worker_loop(
    model_dir: PathBuf,
    cmd_rx: Receiver<WorkerCommand>,
    reply_tx: Sender<WorkerReply>,
) {
    let mut engine: Option<HandwritingOcrEngine> = None;

    let ensure_engine = |engine: &mut Option<HandwritingOcrEngine>| -> Result<(), String> {
        if engine.is_none() {
            *engine = Some(HandwritingOcrEngine::load_from_dir(&model_dir)?);
        }
        Ok(())
    };

    while let Ok(cmd) = cmd_rx.recv() {
        match cmd {
            WorkerCommand::UnloadModel => {
                engine = None;
            }
            WorkerCommand::IndexPage {
                nb_dir,
                page_id,
                fingerprint,
                strokes,
            } => {
                let mut cache = OcrCache::load(&nb_dir);
                if cache.is_up_to_date(page_id, fingerprint) {
                    continue;
                }
                if fingerprint == 0 {
                    cache.set_page(page_id, 0, String::new());
                    let _ = cache.save(&nb_dir);
                    continue;
                }
                if let Err(err) = ensure_engine(&mut engine) {
                    let _ = reply_tx.send(WorkerReply::EngineError(err));
                    continue;
                }
                if let Some(eng) = &engine {
                    if let Ok(text) = eng.recognize_strokes(&strokes) {
                        cache.set_page(page_id, fingerprint, text);
                        let _ = cache.save(&nb_dir);
                        let _ = reply_tx.send(WorkerReply::PageIndexed { page_id });
                    }
                }
            }
            WorkerCommand::IndexNotebook { nb_dir, pages } => {
                let total = pages.len();
                let mut cache = OcrCache::load(&nb_dir);
                let mut recognized_pages = 0usize;
                let mut dirty = false;

                for (idx, (page_id, maybe_strokes)) in pages.into_iter().enumerate() {
                    let _ = reply_tx.send(WorkerReply::IndexingProgress {
                        indexed: idx,
                        total,
                    });

                    let (fingerprint, strokes_owned) = match maybe_strokes {
                        Some(strokes) => {
                            let fp = page_handwriting_fingerprint(&strokes);
                            (fp, Some(strokes))
                        }
                        None => match storage::load_page(&nb_dir, page_id) {
                            Ok(data) => {
                                let fp = page_data_handwriting_fingerprint(&data.strokes);
                                if cache.is_up_to_date(page_id, fp) {
                                    continue;
                                }
                                let strokes: Vec<Stroke> = data
                                    .strokes
                                    .iter()
                                    .filter(|s| is_handwriting_stroke_data(s))
                                    .map(StrokeData::to_stroke)
                                    .collect();
                                (fp, Some(strokes))
                            }
                            Err(_) => continue,
                        },
                    };

                    if cache.is_up_to_date(page_id, fingerprint) {
                        continue;
                    }
                    if fingerprint == 0 {
                        cache.set_page(page_id, 0, String::new());
                        dirty = true;
                        continue;
                    }
                    if let Err(err) = ensure_engine(&mut engine) {
                        let _ = reply_tx.send(WorkerReply::EngineError(err));
                        break;
                    }
                    if let (Some(eng), Some(strokes)) = (&engine, strokes_owned) {
                        if let Ok(text) = eng.recognize_strokes(&strokes) {
                            cache.set_page(page_id, fingerprint, text);
                            recognized_pages += 1;
                            dirty = true;
                        }
                    }
                }
                if dirty {
                    let _ = cache.save(&nb_dir);
                }
                let _ = reply_tx.send(WorkerReply::IndexingDone { recognized_pages });
            }
            WorkerCommand::ConvertSelection {
                stroke_ids,
                bounds,
                strokes,
            } => {
                if let Err(err) = ensure_engine(&mut engine) {
                    let _ = reply_tx.send(WorkerReply::SelectionError(err));
                    continue;
                }
                if let Some(eng) = &engine {
                    match eng.recognize_strokes(&strokes) {
                        Ok(text) if !text.trim().is_empty() => {
                            let _ = reply_tx.send(WorkerReply::SelectionConverted {
                                stroke_ids,
                                bounds,
                                text,
                            });
                        }
                        Ok(_) => {
                            let _ = reply_tx.send(WorkerReply::SelectionError(
                                t!("Kein Text in der Auswahl erkannt").to_string(),
                            ));
                        }
                        Err(err) => {
                            let _ = reply_tx.send(WorkerReply::SelectionError(err));
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::StylusSample;

    fn make_pen_stroke(id: u64, pts: &[(f32, f32)]) -> Stroke {
        let samples: Vec<StylusSample> = pts
            .iter()
            .enumerate()
            .map(|(i, &(x, y))| StylusSample {
                position: Vec2::new(x, y),
                pressure: 0.5,
                timestamp: i as f64 * 0.01,
            })
            .collect();
        let bounds = Aabb::from_points(&samples.iter().map(|s| s.position).collect::<Vec<_>>());
        Stroke {
            id,
            color: [0.1, 0.1, 0.1, 1.0],
            points: samples,
            vertices: Vec::new(),
            aabb: bounds,
            shape: None,
            tool: StrokeTool::Pen,
            width: 5.0,
            finalized: true,
            line_style: crate::document::LineStyle::Solid,
            fill_color: None,
            fill_style: crate::document::FillStyle::None,
            image: None,
            text: None,
        }
    }

    #[test]
    fn test_group_strokes_into_lines_sorts_top_to_bottom_and_left_to_right() {
        // Zeile 2 zuerst, Zeile 1 rechts vor links + Umlautpunkt über Zeile 1
        let s_row2_right = make_pen_stroke(1, &[(300.0, 200.0), (350.0, 240.0)]);
        let s_row1_right = make_pen_stroke(2, &[(260.0, 100.0), (310.0, 140.0)]);
        let s_row2_left = make_pen_stroke(3, &[(100.0, 205.0), (160.0, 238.0)]);
        let s_row1_left = make_pen_stroke(4, &[(100.0, 102.0), (150.0, 138.0)]);
        let s_umlaut_dot = make_pen_stroke(5, &[(120.0, 88.0), (124.0, 92.0)]);

        let strokes = vec![
            s_row2_right,
            s_row1_right,
            s_row2_left,
            s_row1_left,
            s_umlaut_dot,
        ];
        let lines = group_strokes_into_lines(&strokes);
        assert_eq!(lines.len(), 2, "erwartet genau 2 Zeilen");

        let line1_ids: Vec<u64> = lines[0].iter().map(|s| s.id).collect();
        let line2_ids: Vec<u64> = lines[1].iter().map(|s| s.id).collect();
        assert_eq!(line1_ids, vec![4, 5, 2]);
        assert_eq!(line2_ids, vec![3, 1]);
    }

    #[test]
    fn test_rasterize_line_produces_48px_normalized_tensor() {
        let s1 = make_pen_stroke(1, &[(10.0, 10.0), (10.0, 50.0), (40.0, 50.0)]);
        let (w, tensor) = rasterize_line(&[&s1], TARGET_HEIGHT).expect("rasterized");
        assert!(w >= TARGET_HEIGHT);
        assert_eq!(tensor.len(), 3 * TARGET_HEIGHT as usize * w as usize);

        // Hintergrund-Ecke (0, 0) muss weiß (+1.0) sein
        assert!((tensor[0] - 1.0).abs() < 1e-4);
        // Es muss mindestens ein dunkles Tintenpixel (< -0.5) geben
        assert!(tensor.iter().any(|&v| v < -0.5));
        // Alle Werte im Bereich [-1.0, 1.0]
        assert!(tensor.iter().all(|&v| (-1.0001..=1.0001).contains(&v)));
    }

    #[test]
    fn test_ctc_greedy_decode_handles_blanks_duplicates_and_german_umlauts() {
        let vocab = build_ctc_vocab("H\nä\nl\no\nß\nü");
        // vocab: 0="", 1="H", 2="ä", 3="l", 4="o", 5="ß", 6="ü", 7=" "
        let num_classes = vocab.len();
        // Sequenz: H, H, blank, ä, l, l, blank, l, o, space, ß, ü
        let seq = [1, 1, 0, 2, 3, 3, 0, 3, 4, 7, 5, 6];
        let mut logits = vec![-10.0_f32; seq.len() * num_classes];
        for (t, &cls) in seq.iter().enumerate() {
            logits[t * num_classes + cls] = 10.0;
        }
        let decoded = ctc_greedy_decode(&logits, seq.len(), num_classes, &vocab);
        assert_eq!(decoded, "Hällo ßü");
    }

    #[test]
    fn test_ocr_cache_fingerprint_skips_unchanged_pages() {
        let dir = tempfile::tempdir().expect("tempdir");
        let s1 = make_pen_stroke(1, &[(10.0, 10.0), (50.0, 50.0)]);
        let fp1 = page_handwriting_fingerprint(std::slice::from_ref(&s1));
        assert_ne!(fp1, 0);

        let mut cache = OcrCache::default();
        assert!(!cache.is_up_to_date(1, fp1));
        cache.set_page(1, fp1, "Hallo Welt".to_string());
        cache.save(dir.path()).expect("save cache");

        let loaded = OcrCache::load(dir.path());
        assert!(loaded.is_up_to_date(1, fp1));
        assert_eq!(loaded.page_text(1), Some("Hallo Welt"));

        // Nach Hinzufügen eines zweiten Strichs ändert sich der Fingerprint
        let s2 = make_pen_stroke(2, &[(60.0, 10.0), (90.0, 50.0)]);
        let fp2 = page_handwriting_fingerprint(&[s1, s2]);
        assert_ne!(fp1, fp2);
        assert!(!loaded.is_up_to_date(1, fp2));
    }

    #[test]
    fn test_real_ppocrv5_model_if_present_in_tmp() {
        let tmp_dir = Path::new("/tmp/ink-notes-ocr-models");
        if !is_model_installed_in(tmp_dir) {
            return;
        }
        let engine = HandwritingOcrEngine::load_from_dir(tmp_dir)
            .expect("PP-OCRv5 ONNX model must load with tract-onnx");
        // Ein einfaches großes "H" aus 3 Strichen zeichnen
        let left = make_pen_stroke(1, &[(20.0, 20.0), (20.0, 120.0)]);
        let bar = make_pen_stroke(2, &[(20.0, 70.0), (70.0, 70.0)]);
        let right = make_pen_stroke(3, &[(70.0, 20.0), (70.0, 120.0)]);
        let text = engine
            .recognize_strokes(&[left, bar, right])
            .expect("inference must succeed");
        assert!(
            text.contains('H'),
            "erwartet 'H' im erkannten Text, erhalten: {text:?}"
        );
    }
}
