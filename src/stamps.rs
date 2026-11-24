//! Stempel / wiederverwendbare Elemente (`<storage_dir>/stamps.json`).
//!
//! Nutzer können eine markierte Auswahl (Vektorstriche, Formen und Textfelder) als Stempel
//! speichern und später mit einem Tipp in beliebigen Notizbüchern wieder einfügen.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use glam::Vec2;
use serde::{Deserialize, Serialize};

use crate::background::{adapt_ink_color, PaperTheme};
use crate::document::{Page, Stroke, StrokeTool};
use crate::storage;
use crate::t;

/// Höchstens so viele Stempel werden gespeichert.
pub const MAX_STAMPS: usize = 64;

/// Maximale Dateigröße von `stamps.json` beim Laden (16 MiB).
const MAX_STAMPS_FILE_BYTES: u64 = 16 * 1024 * 1024;

/// Dateiname im gemeinsamen Notizbuch-Ordner (neben `library.json`).
const STAMPS_FILE: &str = "stamps.json";

static STAMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// Ein gespeicherter Stempel: Name und normalisierte Vektor-/Text-Elemente (Ursprung bei (0, 0)).
#[derive(Debug, Clone)]
pub struct Stamp {
    pub id: String,
    pub name: String,
    pub strokes: Vec<Stroke>,
}

/// Serialisierbare Darstellung eines Stempels (ohne GPU-Vertizes, nutzt `storage::StrokeData`).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct StampRecord {
    id: String,
    name: String,
    strokes: Vec<storage::StrokeData>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct StampFile {
    stamps: Vec<StampRecord>,
}

impl Stamp {
    /// Baut einen Stempel aus ausgewählten Strichen. Bilder werden herausgefiltert, da sie
    /// notizbuch-lokale Asset-Dateien referenzieren.
    pub fn from_strokes(name: String, strokes: &[Stroke]) -> Result<Self, String> {
        let mut filtered: Vec<Stroke> = strokes.iter().filter(|s| !s.is_image()).cloned().collect();
        if filtered.is_empty() {
            return Err(t!("Bilder können nicht als Stempel gespeichert werden").to_string());
        }

        if let Some(aabb) = Page::selection_aabb(&filtered) {
            let shift = -aabb.min;
            for s in &mut filtered {
                s.translate(shift);
            }
        }

        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let seq = STAMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let id = format!("stamp_{millis}_{seq}");

        Ok(Self {
            id,
            name,
            strokes: filtered,
        })
    }

    /// Erzeugt frische Striche mit neuen IDs, deren Mitte bei `center` auf dem Blatt liegt.
    pub fn instantiate(&self, center: Vec2, next_id_gen: &mut impl FnMut() -> u64) -> Vec<Stroke> {
        let offset = match Page::selection_aabb(&self.strokes) {
            Some(aabb) => center - (aabb.min + aabb.max) * 0.5,
            None => center,
        };
        self.strokes
            .iter()
            .map(|s| {
                let mut cloned = s.clone();
                cloned.id = next_id_gen();
                cloned.translate(offset);
                cloned
            })
            .collect()
    }
}

/// Sammlung aller gespeicherten Stempel.
#[derive(Debug, Clone, Default)]
pub struct StampLibrary {
    pub stamps: Vec<Stamp>,
}

impl StampLibrary {
    pub fn path(base_dir: &Path) -> PathBuf {
        base_dir.join(STAMPS_FILE)
    }

    /// Lädt die Stempel-Bibliothek; bei fehlender, zu großer oder beschädigter Datei wird eine
    /// leere Bibliothek zurückgegeben. Bildverweise werden beim Laden herausgefiltert.
    pub fn load(base_dir: &Path) -> Self {
        let path = Self::path(base_dir);
        if std::fs::metadata(&path)
            .map(|m| m.len() > MAX_STAMPS_FILE_BYTES)
            .unwrap_or(false)
        {
            return Self::default();
        }
        let Ok(content) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        let file: StampFile = serde_json::from_str(&content).unwrap_or_default();
        let mut stamps: Vec<Stamp> = file
            .stamps
            .into_iter()
            .filter(|r| !r.id.is_empty())
            .map(|r| Stamp {
                id: r.id,
                name: r.name,
                strokes: r
                    .strokes
                    .iter()
                    .filter(|s| s.image.is_none())
                    .map(storage::StrokeData::to_stroke)
                    .collect(),
            })
            .filter(|s| !s.strokes.is_empty())
            .collect();
        stamps.truncate(MAX_STAMPS);
        Self { stamps }
    }

    /// Speichert die Stempel-Bibliothek atomar.
    pub fn save(&self, base_dir: &Path) -> Result<(), String> {
        let path = Self::path(base_dir);
        let file = StampFile {
            stamps: self
                .stamps
                .iter()
                .map(|s| StampRecord {
                    id: s.id.clone(),
                    name: s.name.clone(),
                    strokes: s.strokes.iter().map(storage::StrokeData::from).collect(),
                })
                .collect(),
        };
        let json = serde_json::to_string_pretty(&file).map_err(|e| e.to_string())?;
        storage::atomic_write_file(&path, json.as_bytes()).map_err(|e| e.to_string())
    }

    /// Fügt einen neuen Stempel aus der Auswahl hinzu und gibt seinen Namen zurück.
    pub fn add_from_strokes(
        &mut self,
        requested_name: Option<&str>,
        strokes: &[Stroke],
    ) -> Result<&Stamp, String> {
        if self.stamps.len() >= MAX_STAMPS {
            return Err(t!(
                "Höchstens {max} Stempel möglich – lösche zuerst einen alten Stempel",
                max = MAX_STAMPS
            ));
        }
        let trimmed = requested_name.map(str::trim).unwrap_or("");
        let name = if trimmed.is_empty() {
            t!("Stempel {number}", number = self.stamps.len() + 1)
        } else {
            trimmed.chars().take(40).collect()
        };
        let stamp = Stamp::from_strokes(name, strokes)?;
        self.stamps.push(stamp);
        Ok(self.stamps.last().expect("stamp was just pushed"))
    }

    pub fn get(&self, id: &str) -> Option<&Stamp> {
        self.stamps.iter().find(|s| s.id == id)
    }

    pub fn delete(&mut self, id: &str) -> Option<Stamp> {
        let pos = self.stamps.iter().position(|s| s.id == id)?;
        Some(self.stamps.remove(pos))
    }
}

/// Zeichenbefehle für eine kleine Vorschau des Stempels in `rect`.
pub fn stamp_thumbnail_shapes(
    stamp: &Stamp,
    rect: egui::Rect,
    paper: PaperTheme,
    adapt_ink: bool,
) -> Vec<egui::Shape> {
    let bg_color = {
        let c = paper.paper_color();
        let to_srgb = |v: f32| -> u8 { (v.clamp(0.0, 1.0).powf(1.0 / 2.2) * 255.0).round() as u8 };
        egui::Color32::from_rgb(to_srgb(c[0]), to_srgb(c[1]), to_srgb(c[2]))
    };
    let mut shapes = vec![egui::Shape::rect_filled(
        rect,
        egui::CornerRadius::same(4),
        bg_color,
    )];

    let Some(aabb) = Page::selection_aabb(&stamp.strokes) else {
        return shapes;
    };
    let inner = rect.shrink(6.0);
    let size = (aabb.max - aabb.min).max(Vec2::splat(1.0));
    let scale = (inner.width() / size.x).min(inner.height() / size.y);
    let drawn = egui::vec2(size.x * scale, size.y * scale);
    let origin = inner.min + (inner.size() - drawn) * 0.5;
    let to_screen =
        |p: Vec2| origin + egui::vec2((p.x - aabb.min.x) * scale, (p.y - aabb.min.y) * scale);

    let adapt = adapt_ink && paper.is_dark();
    let to_color32 = |rgba: [f32; 4]| {
        let c = if adapt { adapt_ink_color(rgba) } else { rgba };
        egui::Color32::from_rgba_unmultiplied(
            (c[0].clamp(0.0, 1.0) * 255.0) as u8,
            (c[1].clamp(0.0, 1.0) * 255.0) as u8,
            (c[2].clamp(0.0, 1.0) * 255.0) as u8,
            (c[3].clamp(0.0, 1.0) * 255.0) as u8,
        )
    };

    for stroke in &stamp.strokes {
        if let (Some(text), Some(corners)) = (&stroke.text, stroke.object_corners()) {
            let color = to_color32(stroke.color).gamma_multiply(0.8);
            let layout = text.layout();
            let (t_origin, [right, down]) = crate::text::frame_axes(&corners, text.box_size());
            let width = (text.size * 0.45 * down.length() * scale).clamp(0.8, 4.0);
            for (i, line) in layout.lines.iter().enumerate() {
                if line.width <= 0.0 {
                    continue;
                }
                let start = t_origin + down * (layout.baseline(i) - text.size * 0.25);
                shapes.push(egui::Shape::line_segment(
                    [to_screen(start), to_screen(start + right * line.width)],
                    egui::Stroke::new(width, color),
                ));
            }
            continue;
        }
        if stroke.is_object() {
            continue;
        }
        let mut color = to_color32(stroke.color);
        if stroke.tool == StrokeTool::Highlighter {
            color = color.gamma_multiply(0.45);
        }
        let width = (stroke.width * scale).clamp(0.9, 3.5);
        let step = (stroke.points.len() / 64).max(1);
        let last = stroke.points.len().saturating_sub(1);
        let pts: Vec<egui::Pos2> = stroke
            .points
            .iter()
            .enumerate()
            .filter(|(i, _)| i % step == 0 || *i == last)
            .map(|(_, s)| to_screen(s.position))
            .collect();
        match pts.len() {
            0 => {}
            1 => shapes.push(egui::Shape::circle_filled(pts[0], width * 0.5, color)),
            _ => shapes.push(egui::Shape::line(pts, egui::Stroke::new(width, color))),
        }
    }

    shapes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::StylusSample;

    fn sample_stroke(id: u64, x0: f32, y0: f32, x1: f32, y1: f32) -> Stroke {
        let points = vec![
            StylusSample {
                position: Vec2::new(x0, y0),
                pressure: 0.5,
                timestamp: 0.0,
            },
            StylusSample {
                position: Vec2::new(x1, y1),
                pressure: 0.5,
                timestamp: 0.01,
            },
        ];
        Stroke::new(id, points, Vec::new(), [0.1, 0.2, 0.8, 1.0])
    }

    #[test]
    fn test_stamp_roundtrip_and_instantiate_centers_on_target() {
        let dir = tempfile::tempdir().unwrap();
        let mut lib = StampLibrary::default();
        let stroke = sample_stroke(1, 500.0, 400.0, 700.0, 500.0);

        let stamp = lib
            .add_from_strokes(Some("Widerstand"), &[stroke])
            .unwrap()
            .clone();
        assert_eq!(stamp.name, "Widerstand");
        let norm_aabb = Page::selection_aabb(&stamp.strokes).unwrap();
        assert!(norm_aabb.min.length() < 0.01);

        lib.save(dir.path()).unwrap();
        let loaded = StampLibrary::load(dir.path());
        assert_eq!(loaded.stamps.len(), 1);
        assert_eq!(loaded.stamps[0].name, "Widerstand");

        let mut next_id = 10;
        let target = Vec2::new(1000.0, 1200.0);
        let placed = loaded.stamps[0].instantiate(target, &mut || {
            next_id += 1;
            next_id
        });
        assert_eq!(placed.len(), 1);
        assert_eq!(placed[0].id, 11);
        let placed_aabb = Page::selection_aabb(&placed).unwrap();
        let center = (placed_aabb.min + placed_aabb.max) * 0.5;
        assert!((center - target).length() < 0.01);
    }

    #[test]
    fn test_stamp_rejects_image_only_selection_and_deletes() {
        let mut lib = StampLibrary::default();
        let img = Stroke::new_image(
            1,
            "img.png".to_string(),
            [
                Vec2::new(0.0, 0.0),
                Vec2::new(100.0, 0.0),
                Vec2::new(100.0, 100.0),
                Vec2::new(0.0, 100.0),
            ],
        );
        assert!(lib.add_from_strokes(None, &[img]).is_err());

        let s = sample_stroke(2, 10.0, 10.0, 50.0, 50.0);
        let id = lib.add_from_strokes(None, &[s]).unwrap().id.clone();
        assert!(lib.get(&id).is_some());
        assert!(lib.delete(&id).is_some());
        assert!(lib.stamps.is_empty());
    }

    #[test]
    fn test_stamp_load_filters_image_references() {
        let dir = tempfile::tempdir().unwrap();
        let img = Stroke::new_image(
            1,
            "evil.png".to_string(),
            [
                Vec2::new(0.0, 0.0),
                Vec2::new(100.0, 0.0),
                Vec2::new(100.0, 100.0),
                Vec2::new(0.0, 100.0),
            ],
        );
        let file = StampFile {
            stamps: vec![StampRecord {
                id: "s1".to_string(),
                name: "Bild".to_string(),
                strokes: vec![storage::StrokeData::from(&img)],
            }],
        };
        std::fs::write(
            StampLibrary::path(dir.path()),
            serde_json::to_string(&file).unwrap(),
        )
        .unwrap();
        let loaded = StampLibrary::load(dir.path());
        assert!(loaded.stamps.is_empty());
    }
}
