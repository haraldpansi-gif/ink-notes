//! Import of Saber notebooks (.sbn2, .sba, old .sbn) (Issue #47).
//!
//! A `.sbn2` file is a BSON document; images and PDFs lie next to it as `<file>.sbn2.0`,
//! `<file>.sbn2.1`, …; a `.sba` archive is a zip with `main.sbn2` and its assets, and the old
//! `.sbn` is the same document as JSON. Pages (`z`) hold strokes (`s`), images (`i`), typed
//! text as a Quill delta (`q`) and an optional background image or PDF page (`b`). Points are
//! three little-endian `f32` (x, y, pressure) since format version 13.
//!
//! Stroke widths follow perfect_freehand: the diameter is `size · (1 − thinning · (1 − 2p))`.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::document::{Notebook, PageBackground, PdfBackground};
use crate::import::{self, ImportReport, Imported};
use crate::storage::{NotebookManifest, StrokeData};
use crate::t;
use crate::text::TextFont;

/// Saber's default page size, used when a page has none.
const DEFAULT_PAGE: glam::Vec2 = glam::Vec2::new(1000.0, 1400.0);

/// Is the file by its name a Saber notebook?
pub fn is_saber_path(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
        ["sbn2", "sba", "sbn"]
            .iter()
            .any(|x| e.eq_ignore_ascii_case(x))
    })
}

/// A BSON (or JSON) value as far as Saber uses it.
#[derive(Debug, Clone, PartialEq)]
pub enum Bson {
    Null,
    Bool(bool),
    Int(i64),
    Double(f64),
    Str(String),
    Binary(Vec<u8>),
    Array(Vec<Bson>),
    Doc(Vec<(String, Bson)>),
}

impl Bson {
    pub fn get(&self, key: &str) -> Option<&Bson> {
        match self {
            Bson::Doc(fields) => fields
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v)
                .filter(|v| **v != Bson::Null),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match *self {
            Bson::Double(v) if v.is_finite() => Some(v),
            Bson::Int(v) => Some(v as f64),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match *self {
            Bson::Int(v) => Some(v),
            Bson::Double(v) if v.is_finite() => Some(v as i64),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Bson::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_array(&self) -> &[Bson] {
        match self {
            Bson::Array(items) => items,
            _ => &[],
        }
    }

    fn num(&self, key: &str) -> Option<f64> {
        self.get(key)?.as_f64()
    }

    fn bool(&self, key: &str) -> Option<bool> {
        match self.get(key)? {
            Bson::Bool(b) => Some(*b),
            _ => None,
        }
    }

    fn from_json(value: serde_json::Value) -> Bson {
        use serde_json::Value;
        match value {
            Value::Null => Bson::Null,
            Value::Bool(b) => Bson::Bool(b),
            Value::Number(n) => n
                .as_i64()
                .map(Bson::Int)
                .unwrap_or_else(|| Bson::Double(n.as_f64().unwrap_or(0.0))),
            Value::String(s) => Bson::Str(s),
            Value::Array(items) => Bson::Array(items.into_iter().map(Bson::from_json).collect()),
            Value::Object(map) => Bson::Doc(
                map.into_iter()
                    .map(|(k, v)| (k, Bson::from_json(v)))
                    .collect(),
            ),
        }
    }
}

/// Reads a BSON document.
pub fn parse_bson(bytes: &[u8]) -> Result<Bson, String> {
    let mut pos = 0;
    document(bytes, &mut pos, 0).ok_or_else(|| t!("Keine Saber-Datei").to_string())
}

fn take<'a>(bytes: &'a [u8], pos: &mut usize, n: usize) -> Option<&'a [u8]> {
    let slice = bytes.get(*pos..pos.checked_add(n)?)?;
    *pos += n;
    Some(slice)
}

fn int32(bytes: &[u8], pos: &mut usize) -> Option<i32> {
    Some(i32::from_le_bytes(take(bytes, pos, 4)?.try_into().ok()?))
}

fn cstring(bytes: &[u8], pos: &mut usize) -> Option<String> {
    let len = bytes.get(*pos..)?.iter().position(|&b| b == 0)?;
    let s = String::from_utf8_lossy(take(bytes, pos, len)?).into_owned();
    *pos += 1;
    Some(s)
}

/// A document (or array, which is a document with the keys "0", "1", …).
fn document(bytes: &[u8], pos: &mut usize, depth: usize) -> Option<Bson> {
    if depth > 64 {
        return None;
    }
    let start = *pos;
    let len = usize::try_from(int32(bytes, pos)?).ok()?;
    let end = start.checked_add(len)?;
    if end > bytes.len() || len < 5 {
        return None;
    }
    let mut fields = Vec::new();
    loop {
        let kind = *bytes.get(*pos)?;
        *pos += 1;
        if kind == 0 {
            break;
        }
        let key = cstring(bytes, pos)?;
        let value = match kind {
            0x01 => Bson::Double(f64::from_le_bytes(take(bytes, pos, 8)?.try_into().ok()?)),
            0x02 => {
                let n = usize::try_from(int32(bytes, pos)?).ok()?;
                let s = take(bytes, pos, n)?;
                Bson::Str(String::from_utf8_lossy(s.strip_suffix(&[0]).unwrap_or(s)).into_owned())
            }
            0x03 => document(bytes, pos, depth + 1)?,
            0x04 => match document(bytes, pos, depth + 1)? {
                Bson::Doc(items) => Bson::Array(items.into_iter().map(|(_, v)| v).collect()),
                other => other,
            },
            0x05 => {
                let n = usize::try_from(int32(bytes, pos)?).ok()?;
                *pos += 1; // subtype
                Bson::Binary(take(bytes, pos, n)?.to_vec())
            }
            0x07 => {
                take(bytes, pos, 12)?;
                Bson::Null
            }
            0x08 => Bson::Bool(*take(bytes, pos, 1)?.first()? != 0),
            0x09 | 0x11 | 0x12 => {
                Bson::Int(i64::from_le_bytes(take(bytes, pos, 8)?.try_into().ok()?))
            }
            0x0A | 0x06 | 0x7F | 0xFF => Bson::Null,
            0x10 => Bson::Int(int32(bytes, pos)? as i64),
            _ => return None,
        };
        fields.push((key, value));
    }
    (*pos == end).then_some(Bson::Doc(fields))
}

/// Reads the files of a zip archive (stored or deflated), at most `limit` bytes in total.
fn unzip(bytes: &[u8], limit: usize) -> Result<HashMap<String, Vec<u8>>, String> {
    let broken = || t!("Datei beschädigt: {err}", err = "zip").to_string();
    let u16_at = |at: usize| -> Option<usize> {
        Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?) as usize)
    };
    let u32_at = |at: usize| -> Option<usize> {
        Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?) as usize)
    };
    // End of central directory: the last record with its signature
    let eocd = (0..bytes.len().saturating_sub(21))
        .rev()
        .find(|&i| bytes[i..].starts_with(&[0x50, 0x4b, 0x05, 0x06]))
        .ok_or_else(broken)?;
    let count = u16_at(eocd + 10).ok_or_else(broken)?;
    let mut at = u32_at(eocd + 16).ok_or_else(broken)?;
    let mut files = HashMap::new();
    let mut total = 0;
    for _ in 0..count {
        let entry = (|| {
            if !bytes.get(at..)?.starts_with(&[0x50, 0x4b, 0x01, 0x02]) {
                return None;
            }
            let method = u16_at(at + 10)?;
            let compressed = u32_at(at + 20)?;
            let size = u32_at(at + 24)?;
            let name_len = u16_at(at + 28)?;
            let skip = name_len + u16_at(at + 30)? + u16_at(at + 32)?;
            let local = u32_at(at + 42)?;
            let name = String::from_utf8_lossy(bytes.get(at + 46..at + 46 + name_len)?);
            at += 46 + skip;
            let data_start = local + 30 + u16_at(local + 26)? + u16_at(local + 28)?;
            let data = bytes.get(data_start..data_start.checked_add(compressed)?)?;
            Some((name.into_owned(), method, size, data))
        })();
        let (name, method, size, data) = entry.ok_or_else(broken)?;
        total += size;
        if total > limit {
            return Err(broken());
        }
        let content = match method {
            0 => data.to_vec(),
            8 => {
                let mut out = Vec::with_capacity(size);
                flate2::read::DeflateDecoder::new(data)
                    .take(size as u64)
                    .read_to_end(&mut out)
                    .map_err(|_| broken())?;
                out
            }
            _ => return Err(broken()),
        };
        files.insert(name, content);
    }
    Ok(files)
}

/// The document and a way to get its assets (by index).
struct Source {
    doc: Bson,
    assets: Box<dyn Fn(usize) -> Option<Vec<u8>>>,
}

fn load(path: &Path) -> Result<Source, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    if bytes.starts_with(&[0x50, 0x4b, 0x03, 0x04]) {
        let files = unzip(&bytes, 1024 * 1024 * 1024)?;
        let main = files
            .iter()
            .find(|(name, _)| name.ends_with(".sbn2"))
            .map(|(name, data)| (name.clone(), data))
            .ok_or_else(|| t!("Keine Saber-Datei").to_string())?;
        let doc = parse_bson(main.1)?;
        let prefix = main.0;
        return Ok(Source {
            doc,
            assets: Box::new(move |i| files.get(&format!("{prefix}.{i}")).cloned()),
        });
    }
    // .sbn2 is BSON, the old .sbn JSON
    let doc = match parse_bson(&bytes) {
        Ok(doc) => doc,
        Err(err) => serde_json::from_slice(&bytes)
            .map(Bson::from_json)
            .map_err(|_| err)?,
    };
    let base = path.to_path_buf();
    Ok(Source {
        doc,
        assets: Box::new(move |i| {
            std::fs::read(PathBuf::from(format!("{}.{i}", base.display()))).ok()
        }),
    })
}

/// Color stored as ARGB integer.
fn argb(value: Option<&Bson>) -> [f32; 4] {
    let v = value.and_then(Bson::as_i64).unwrap_or(0xff000000) as u32;
    [
        ((v >> 16) & 0xff) as f32 / 255.0,
        ((v >> 8) & 0xff) as f32 / 255.0,
        (v & 0xff) as f32 / 255.0,
        (v >> 24) as f32 / 255.0,
    ]
}

/// Points of a stroke: `f32` triples (version 13 on) or `{x, y, p}` documents.
fn points(stroke: &Bson) -> Vec<(glam::Vec2, Option<f32>)> {
    let offset = glam::Vec2::new(
        stroke.num("ox").unwrap_or(0.0) as f32,
        stroke.num("oy").unwrap_or(0.0) as f32,
    );
    let Some(list) = stroke.get("p") else {
        return Vec::new();
    };
    list.as_array()
        .iter()
        .filter_map(|point| match point {
            Bson::Binary(data) => {
                let values: Vec<f32> = data
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|b| f32::from_le_bytes(*b))
                    .collect();
                match values[..] {
                    [x, y] => Some((glam::Vec2::new(x, y), None)),
                    [x, y, p, ..] => Some((glam::Vec2::new(x, y), Some(p))),
                    _ => None,
                }
            }
            doc @ Bson::Doc(_) => Some((
                glam::Vec2::new(doc.num("x")? as f32, doc.num("y")? as f32),
                doc.num("p").map(|p| p as f32),
            )),
            _ => None,
        })
        .filter(|(p, _)| p.is_finite())
        .map(|(p, pressure)| (p + offset, pressure))
        .collect()
}

/// Outline points of a shape stroke (circle or rectangle).
fn shape_points(stroke: &Bson, shape: &str) -> Vec<glam::Vec2> {
    let n = |k| stroke.num(k).unwrap_or(0.0) as f32;
    match shape {
        "circle" => {
            let (center, r) = (glam::Vec2::new(n("cx"), n("cy")), n("r"));
            (0..=64)
                .map(|i| {
                    let a = i as f32 / 64.0 * std::f32::consts::TAU;
                    center + glam::Vec2::new(a.cos(), a.sin()) * r
                })
                .collect()
        }
        "rect" => {
            let (l, t, w, h) = (n("rl"), n("rt"), n("rw"), n("rh"));
            [(l, t), (l + w, t), (l + w, t + h), (l, t + h), (l, t)]
                .map(|(x, y)| glam::Vec2::new(x, y))
                .to_vec()
        }
        _ => Vec::new(),
    }
}

/// Typed text of a page: the inserted strings of its Quill delta.
fn quill_text(delta: &Bson) -> String {
    let text: String = delta
        .as_array()
        .iter()
        .filter_map(|op| op.get("insert")?.as_str())
        .collect();
    text.trim_end().to_string()
}

/// Creates a new notebook from a Saber file.
pub fn create_notebook_from_saber(
    base_dir: &Path,
    title: &str,
    path: &Path,
) -> Result<(Notebook, NotebookManifest, PathBuf, ImportReport), String> {
    let source = load(path)?;
    // The very first files were only a list of strokes; versions up to 7 kept strokes and
    // images outside the pages, which is read as well
    if !matches!(source.doc, Bson::Doc(_)) {
        return Err(t!(
            "Saber-Datei zu alt: bitte in einem aktuellen Saber öffnen und neu speichern"
        )
        .to_string());
    }
    import::create_notebook(base_dir, title, |dir| Ok(convert(&source, dir)))
}

/// A PDF of the notebook in our asset folder.
struct PdfSource {
    asset: String,
    sizes: Vec<(f32, f32)>,
}

fn convert(source: &Source, notebook_dir: &Path) -> Imported {
    let doc = &source.doc;
    let assets = crate::assets::AssetStore::new(notebook_dir);
    let mut report = ImportReport::default();
    let mut pdfs: HashMap<usize, Option<PdfSource>> = HashMap::new();
    let background = match doc.get("p").and_then(Bson::as_str) {
        Some("lined" | "college" | "college-rtl" | "cornell") => PageBackground::Lined,
        Some("grid") => PageBackground::Grid5mm,
        Some("dots") => PageBackground::DotGrid,
        _ => PageBackground::Blank,
    };
    let line_height = doc.num("l").unwrap_or(40.0) as f32;
    let mut xpages: Vec<&Bson> = doc
        .get("z")
        .map(Bson::as_array)
        .unwrap_or_default()
        .iter()
        .collect();
    let empty = Bson::Doc(Vec::new());
    if xpages.is_empty() {
        xpages.push(&empty);
    }
    // Before version 8 strokes and images were stored outside the pages, with a page index
    let old_items = |key: &str, index: usize| -> Vec<&Bson> {
        doc.get(key)
            .map(Bson::as_array)
            .unwrap_or_default()
            .iter()
            .filter(|item| item.num("i").unwrap_or(0.0) as usize == index)
            .collect()
    };

    let mut next_id: u64 = 1;
    let mut new_id = || {
        next_id += 1;
        next_id - 1
    };
    let mut pages = Vec::with_capacity(xpages.len());
    for (index, xpage) in xpages.iter().enumerate() {
        // Pages stored as [width, height] in very old files
        let page_size = match xpage {
            Bson::Array(wh) => glam::Vec2::new(
                wh.first().and_then(Bson::as_f64).unwrap_or(1000.0) as f32,
                wh.get(1).and_then(Bson::as_f64).unwrap_or(1400.0) as f32,
            ),
            _ => glam::Vec2::new(
                xpage.num("w").map_or(DEFAULT_PAGE.x, |w| w as f32),
                xpage.num("h").map_or(DEFAULT_PAGE.y, |h| h as f32),
            ),
        }
        .max(glam::Vec2::ONE);
        // Same rules as for imported PDFs: portrait pages become A4, the content keeps its
        // aspect ratio, centred horizontally and aligned at the top
        let size = crate::document::pdf_page_size(page_size.x, page_size.y);
        let placement = crate::document::pdf_placement(page_size.x, page_size.y, size);
        let scale = (placement[1].x - placement[0].x) / page_size.x;
        let map = |p: glam::Vec2| placement[0] + p * scale;

        let mut strokes: Vec<StrokeData> = Vec::new();
        let mut pdf_background = None;

        if let Some(image) = xpage.get("b") {
            let natural = glam::Vec2::new(
                image.num("nw").unwrap_or(0.0) as f32,
                image.num("nh").unwrap_or(0.0) as f32,
            );
            let corners = if natural.min_element() > 0.0 {
                crate::document::pdf_placement(natural.x, natural.y, size)
            } else {
                placement
            };
            let asset = image.num("a").map(|a| a as usize);
            if image.get("e").and_then(Bson::as_str) == Some(".pdf") {
                let pdf = asset.and_then(|a| {
                    pdfs.entry(a)
                        .or_insert_with(|| {
                            let bytes = (source.assets)(a)?;
                            let sizes = crate::pdf_doc::page_sizes(bytes.clone()).ok()?;
                            let asset = assets.import_pdf(&bytes).ok()?;
                            Some(PdfSource { asset, sizes })
                        })
                        .as_ref()
                });
                let page = image.num("pdfi").unwrap_or(0.0) as u32;
                match pdf.and_then(|pdf| Some((pdf, *pdf.sizes.get(page as usize)?))) {
                    Some((pdf, (width_pt, height_pt))) => {
                        pdf_background = Some(PdfBackground {
                            asset: pdf.asset.clone(),
                            page,
                            width_pt,
                            height_pt,
                        })
                    }
                    None => report.skipped += 1,
                }
            } else {
                match image_asset(image, source, &assets, corners) {
                    Some(asset) => strokes.push(import::image_stroke(new_id(), asset, corners)),
                    None => report.skipped += 1,
                }
            }
        }

        let images = xpage.get("i").map(Bson::as_array).unwrap_or_default();
        for image in images.iter().chain(old_items("i", index)) {
            let rect = ["x", "y", "w", "h"].map(|k| image.num(k).unwrap_or(0.0) as f32);
            let corners = [
                glam::Vec2::new(rect[0], rect[1]),
                glam::Vec2::new(rect[0] + rect[2], rect[1]),
                glam::Vec2::new(rect[0] + rect[2], rect[1] + rect[3]),
                glam::Vec2::new(rect[0], rect[1] + rect[3]),
            ]
            .map(map);
            match image_asset(image, source, &assets, corners) {
                Some(asset) => strokes.push(import::image_stroke(new_id(), asset, corners)),
                None => report.skipped += 1,
            }
        }

        if let Some(delta) = xpage.get("q") {
            let text = quill_text(delta);
            if !text.is_empty() {
                // Saber's text area starts 1.2 lines down, half a line in from the sides
                let origin = glam::Vec2::new(line_height * 0.5, line_height * 1.2);
                strokes.push(import::text_stroke(
                    new_id(),
                    &text,
                    line_height * 0.8 * scale,
                    TextFont::Sans,
                    map(origin),
                    [glam::Vec2::X, glam::Vec2::Y],
                    Some((page_size.x - line_height) * scale),
                    [0.0, 0.0, 0.0, 1.0],
                ));
            }
        }

        let page_strokes = xpage.get("s").map(Bson::as_array).unwrap_or_default();
        for stroke in page_strokes.iter().chain(old_items("s", index)) {
            let tool = stroke
                .get("ty")
                .and_then(Bson::as_str)
                .unwrap_or("fountainPen");
            if tool == "Eraser" || tool == "LaserPointer" {
                continue;
            }
            let size = stroke.num("s").unwrap_or(10.0) as f32;
            let thinning = stroke.num("t").unwrap_or(0.5) as f32;
            let pressure_enabled = stroke.bool("pe").unwrap_or(true);
            let (points, pressures): (Vec<glam::Vec2>, Vec<Option<f32>>) =
                match stroke.get("shape").and_then(Bson::as_str) {
                    Some(shape) => shape_points(stroke, shape)
                        .into_iter()
                        .map(|p| (p, None))
                        .unzip(),
                    None => points(stroke).into_iter().unzip(),
                };
            if points.is_empty() {
                report.skipped += 1;
                continue;
            }
            let widths: Vec<f32> = pressures
                .iter()
                .map(|p| {
                    let p = p
                        .filter(|_| pressure_enabled)
                        .unwrap_or(0.5)
                        .clamp(0.0, 1.0);
                    (size * (1.0 - thinning * (1.0 - 2.0 * p))).max(size * 0.05) * scale
                })
                .collect();
            let (width, pressures) = import::fit_widths(&widths);
            let points: Vec<glam::Vec2> = points.into_iter().map(map).collect();
            strokes.push(import::ink_stroke(
                new_id(),
                argb(stroke.get("c")),
                tool == "Highlighter",
                &points,
                width,
                &pressures,
            ));
        }

        pages.push(import::page(
            index as u64 + 1,
            size,
            background,
            pdf_background,
            strokes,
        ));
    }
    Imported {
        pages,
        next_stroke_id: next_id,
        layout: None,
        report,
    }
}

/// Stores the image of `image` (asset file, inline bytes or SVG text) in our asset folder.
fn image_asset(
    image: &Bson,
    source: &Source,
    assets: &crate::assets::AssetStore,
    corners: [glam::Vec2; 4],
) -> Option<String> {
    let bytes = match (image.num("a"), image.get("b")) {
        (Some(a), _) => (source.assets)(a as usize)?,
        (None, Some(Bson::Binary(b))) => b.clone(),
        (None, Some(Bson::Array(b))) => b.iter().filter_map(|v| Some(v.as_i64()? as u8)).collect(),
        (None, Some(Bson::Str(svg))) => svg.clone().into_bytes(),
        _ => return None,
    };
    if image.get("e").and_then(Bson::as_str) == Some(".svg") {
        let size = glam::Vec2::new(
            corners[0].distance(corners[1]),
            corners[0].distance(corners[3]),
        );
        let (w, h, pixels) = import::rasterize_svg(std::str::from_utf8(&bytes).ok()?, size)?;
        return assets.import_rgba(w, h, pixels).ok().map(|i| i.asset);
    }
    assets.import_bytes(&bytes).ok().map(|i| i.asset)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::StrokeTool;

    /// BSON encoder for test files.
    fn encode(value: &Bson) -> Vec<u8> {
        fn doc(fields: &[(String, Bson)]) -> Vec<u8> {
            let mut body = Vec::new();
            for (key, value) in fields {
                let (kind, data): (u8, Vec<u8>) = match value {
                    Bson::Null => (0x0A, Vec::new()),
                    Bson::Bool(b) => (0x08, vec![*b as u8]),
                    Bson::Int(i) => (0x12, i.to_le_bytes().to_vec()),
                    Bson::Double(d) => (0x01, d.to_le_bytes().to_vec()),
                    Bson::Str(s) => {
                        let mut d = ((s.len() + 1) as i32).to_le_bytes().to_vec();
                        d.extend(s.as_bytes());
                        d.push(0);
                        (0x02, d)
                    }
                    Bson::Binary(b) => {
                        let mut d = (b.len() as i32).to_le_bytes().to_vec();
                        d.push(0);
                        d.extend(b);
                        (0x05, d)
                    }
                    Bson::Array(items) => {
                        let fields: Vec<(String, Bson)> = items
                            .iter()
                            .enumerate()
                            .map(|(i, v)| (i.to_string(), v.clone()))
                            .collect();
                        (0x04, doc(&fields))
                    }
                    Bson::Doc(fields) => (0x03, doc(fields)),
                };
                body.push(kind);
                body.extend(key.as_bytes());
                body.push(0);
                body.extend(data);
            }
            let mut out = ((body.len() + 5) as i32).to_le_bytes().to_vec();
            out.extend(body);
            out.push(0);
            out
        }
        match value {
            Bson::Doc(fields) => doc(fields),
            _ => panic!("only documents"),
        }
    }

    fn d(fields: &[(&str, Bson)]) -> Bson {
        Bson::Doc(
            fields
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect(),
        )
    }

    fn point(x: f32, y: f32, p: f32) -> Bson {
        Bson::Binary([x, y, p].iter().flat_map(|v| v.to_le_bytes()).collect())
    }

    fn png() -> Vec<u8> {
        let image = image::RgbaImage::from_pixel(4, 2, image::Rgba([200, 30, 30, 255]));
        let mut png = Vec::new();
        image
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        png
    }

    /// Version 19 document: page 1 with ink, highlighter, a circle, an image and text;
    /// page 2 with a PDF page as background. Assets: 0 = PNG, 1 = PDF.
    fn sample() -> Bson {
        let pen = d(&[
            (
                "p",
                Bson::Array(vec![point(100.0, 100.0, 1.0), point(200.0, 100.0, 0.25)]),
            ),
            ("ty", Bson::Str("fountainPen".into())),
            ("pe", Bson::Bool(true)),
            ("c", Bson::Int(0xff0000ff)),
            ("s", Bson::Double(10.0)),
            ("t", Bson::Double(0.5)),
            ("ox", Bson::Double(10.0)),
        ]);
        let marker = d(&[
            (
                "p",
                Bson::Array(vec![point(0.0, 300.0, 0.5), point(300.0, 300.0, 0.5)]),
            ),
            ("ty", Bson::Str("Highlighter".into())),
            ("c", Bson::Int(0x80ffff00)),
            ("s", Bson::Double(40.0)),
        ]);
        let circle = d(&[
            ("shape", Bson::Str("circle".into())),
            ("cx", Bson::Double(500.0)),
            ("cy", Bson::Double(500.0)),
            ("r", Bson::Double(50.0)),
            ("c", Bson::Int(0xff00ff00)),
            ("s", Bson::Double(5.0)),
        ]);
        let image = d(&[
            ("e", Bson::Str(".png".into())),
            ("a", Bson::Int(0)),
            ("x", Bson::Double(600.0)),
            ("y", Bson::Double(100.0)),
            ("w", Bson::Double(200.0)),
            ("h", Bson::Double(100.0)),
        ]);
        let text = Bson::Array(vec![
            d(&[("insert", Bson::Str("Hallo ".into()))]),
            d(&[("insert", Bson::Str("Welt\n".into()))]),
        ]);
        let page1 = d(&[
            ("w", Bson::Double(1000.0)),
            ("h", Bson::Double(1400.0)),
            ("s", Bson::Array(vec![pen, marker, circle])),
            ("i", Bson::Array(vec![image])),
            ("q", text),
        ]);
        let pdf = d(&[
            ("e", Bson::Str(".pdf".into())),
            ("a", Bson::Int(1)),
            ("pdfi", Bson::Int(1)),
            ("nw", Bson::Double(842.0)),
            ("nh", Bson::Double(595.0)),
        ]);
        let page2 = d(&[
            ("w", Bson::Double(1000.0)),
            ("h", Bson::Double(707.0)),
            ("b", pdf),
        ]);
        d(&[
            ("v", Bson::Int(19)),
            ("p", Bson::Str("dots".into())),
            ("l", Bson::Int(40)),
            ("z", Bson::Array(vec![page1, page2])),
        ])
    }

    fn sample_pdf() -> Vec<u8> {
        crate::pdf_doc::tests::sample_pdf(&[(842.0, 595.0, 0), (842.0, 595.0, 0)])
    }

    fn check(notebook: &Notebook, report: &ImportReport) {
        assert_eq!(*report, ImportReport::default());
        assert_eq!(notebook.pages.len(), 2);
        let first = &notebook.pages[0];
        assert_eq!(first.background, PageBackground::DotGrid);
        // Image, text, then the strokes
        assert_eq!(first.strokes.len(), 5);
        assert!(first.strokes[0].is_image());
        assert_eq!(
            first.strokes[1].text.as_ref().unwrap().content,
            "Hallo Welt"
        );
        let pen = &first.strokes[2];
        assert_eq!(pen.tool, StrokeTool::Pen);
        assert_eq!(pen.color, [0.0, 0.0, 1.0, 1.0]);
        // 1000 Saber units = 210 mm; the offset moves the stroke by 10
        assert!((pen.points[0].position.x - 231.0).abs() < 0.1);
        // perfect_freehand: size 10, thinning 0.5 gives 15 at full pressure and 7.5 at a
        // quarter, scaled by 2.1
        let drawn: Vec<f32> = pen
            .points
            .iter()
            .map(|s| pen.width * (0.5 + s.pressure))
            .collect();
        assert!((drawn[0] - 31.5).abs() < 0.01 && (drawn[1] - 15.75).abs() < 0.01);
        assert_eq!(first.strokes[3].tool, StrokeTool::Highlighter);
        assert!((first.strokes[3].color[3] - 128.0 / 255.0).abs() < 1e-3);
        assert_eq!(first.strokes[4].points.len(), 65);
        let bg = notebook.pages[1].pdf_background.as_ref().unwrap();
        assert_eq!(bg.page, 1);
        assert!(notebook.pages[1].size.x > notebook.pages[1].size.y);
    }

    #[test]
    fn test_parse_bson() {
        let value = sample();
        assert_eq!(parse_bson(&encode(&value)).unwrap(), value);
        assert!(parse_bson(b"\x05\x00\x00").is_err());
        let mut broken = encode(&value);
        broken.truncate(broken.len() - 3);
        assert!(parse_bson(&broken).is_err());
    }

    #[test]
    fn test_import_sbn2_with_assets_next_to_it() {
        let temp_dir = tempfile::tempdir().unwrap();
        let source_dir = tempfile::tempdir().unwrap();
        let path = source_dir.path().join("Mathe.sbn2");
        std::fs::write(&path, encode(&sample())).unwrap();
        std::fs::write(source_dir.path().join("Mathe.sbn2.0"), png()).unwrap();
        std::fs::write(source_dir.path().join("Mathe.sbn2.1"), sample_pdf()).unwrap();
        let (notebook, _, dir, report) =
            create_notebook_from_saber(temp_dir.path(), "Mathe", &path).unwrap();
        check(&notebook, &report);
        // Saved and loaded again
        let (mut reloaded, _) = crate::storage::load_notebook(&dir).unwrap();
        crate::storage::load_all_pages(&dir, &mut reloaded).unwrap();
        check(&reloaded, &report);
    }

    /// Zip archive with stored (uncompressed) files.
    fn zip(files: &[(&str, Vec<u8>)]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut central = Vec::new();
        for (name, data) in files {
            let offset = out.len() as u32;
            let header = |sig: &[u8], central: bool| {
                let mut h = sig.to_vec();
                if central {
                    h.extend([20, 0]);
                }
                h.extend([20, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
                h.extend((data.len() as u32).to_le_bytes());
                h.extend((data.len() as u32).to_le_bytes());
                h.extend((name.len() as u16).to_le_bytes());
                h.extend([0, 0]);
                if central {
                    h.extend([0; 10]);
                    h.extend(offset.to_le_bytes());
                }
                h.extend(name.as_bytes());
                h
            };
            out.extend(header(&[0x50, 0x4b, 0x03, 0x04], false));
            out.extend(data);
            central.extend(header(&[0x50, 0x4b, 0x01, 0x02], true));
        }
        let start = out.len() as u32;
        out.extend(&central);
        out.extend([0x50, 0x4b, 0x05, 0x06, 0, 0, 0, 0]);
        out.extend((files.len() as u16).to_le_bytes());
        out.extend((files.len() as u16).to_le_bytes());
        out.extend((central.len() as u32).to_le_bytes());
        out.extend(start.to_le_bytes());
        out.extend([0, 0]);
        out
    }

    #[test]
    fn test_import_sba_archive() {
        let temp_dir = tempfile::tempdir().unwrap();
        let source_dir = tempfile::tempdir().unwrap();
        let path = source_dir.path().join("Mathe.sba");
        let archive = zip(&[
            ("main.sbn2", encode(&sample())),
            ("main.sbn2.0", png()),
            ("main.sbn2.1", sample_pdf()),
        ]);
        std::fs::write(&path, archive).unwrap();
        let (notebook, _, _, report) =
            create_notebook_from_saber(temp_dir.path(), "Mathe", &path).unwrap();
        check(&notebook, &report);
    }

    #[test]
    fn test_old_json_files() {
        let temp_dir = tempfile::tempdir().unwrap();
        let source_dir = tempfile::tempdir().unwrap();
        // Version 7: strokes outside the pages, points as {x, y, p}
        let path = source_dir.path().join("Alt.sbn");
        std::fs::write(
            &path,
            r#"{"v": 7, "z": [{"w": 1000, "h": 1400}, {"w": 1000, "h": 1400}],
                "s": [{"i": 1, "p": [{"x": 0, "y": 0, "p": 0.5}, {"x": 100, "y": 0, "p": 0.5}]}]}"#,
        )
        .unwrap();
        let (notebook, _, _, _) =
            create_notebook_from_saber(temp_dir.path(), "Alt", &path).unwrap();
        assert_eq!(notebook.pages[0].strokes.len(), 0);
        assert_eq!(notebook.pages[1].strokes.len(), 1);

        // The very first format: just a list of strokes
        let path = source_dir.path().join("Uralt.sbn");
        std::fs::write(&path, "[]").unwrap();
        assert!(create_notebook_from_saber(temp_dir.path(), "Uralt", &path).is_err());
    }
}
