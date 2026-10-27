//! Import of Xournal++ (.xopp) and Xournal (.xoj) notebooks (Issue #47).
//!
//! Both formats are gzip-compressed XML: `<page>` elements with a `<background>` and
//! `<layer>`s holding `<stroke>`, `<text>` and `<image>` elements; coordinates are PDF points.
//! The import creates a new notebook: one page per Xournal page, all layers flattened in
//! order. PDF backgrounds are taken over if the PDF can still be found; LaTeX formulas
//! (`<teximage>`) have no counterpart and are skipped.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use base64::Engine;
use quick_xml::events::{BytesStart, Event};
use quick_xml::XmlVersion;

use crate::document::{Notebook, PageBackground, PdfBackground};
use crate::import::{self, ImportReport, Imported};
use crate::storage::{NotebookManifest, StrokeData};
use crate::t;
use crate::text::TextFont;

/// Is the file by its name a Xournal++ or Xournal notebook?
pub fn is_xopp_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("xopp") || e.eq_ignore_ascii_case("xoj"))
}

/// Parsed Xournal document, still in PDF points.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct XoppDocument {
    pub pages: Vec<XoppPage>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct XoppPage {
    pub width: f32,
    pub height: f32,
    pub background: XoppBackground,
    pub items: Vec<XoppItem>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub enum XoppBackground {
    #[default]
    Plain,
    Lined,
    Graph,
    Dotted,
    /// Page `page` (from 0) of a PDF; `file` is only given on the first PDF page
    Pdf {
        file: Option<String>,
        page: u32,
    },
    /// Image file as background
    Pixmap {
        file: String,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum XoppItem {
    Stroke {
        highlighter: bool,
        color: [f32; 4],
        /// Nominal width, then optionally one width per point (or per segment in Xournal)
        widths: Vec<f32>,
        points: Vec<glam::Vec2>,
    },
    Text {
        mono: bool,
        size: f32,
        pos: glam::Vec2,
        color: [f32; 4],
        content: String,
    },
    /// Embedded image (base64 PNG) with its rectangle (left, top, right, bottom)
    Image { rect: [f32; 4], data: String },
    /// Element without a counterpart (LaTeX formula)
    Unsupported,
}

/// Reads the (usually gzip-compressed) XML of a Xournal file.
pub fn decompress(bytes: &[u8]) -> Result<String, String> {
    String::from_utf8(import::gunzip(bytes)?).map_err(|_| t!("Keine Xournal-Datei").to_string())
}

fn attributes(e: &BytesStart) -> HashMap<String, String> {
    e.attributes()
        .flatten()
        .filter_map(|a| {
            let value = a.normalized_value(XmlVersion::Implicit1_0).ok()?;
            Some((
                String::from_utf8_lossy(a.key.as_ref()).into_owned(),
                value.into_owned(),
            ))
        })
        .collect()
}

fn number(attrs: &HashMap<String, String>, key: &str) -> Option<f32> {
    attrs
        .get(key)?
        .trim()
        .parse::<f32>()
        .ok()
        .filter(|v| v.is_finite())
}

/// Xournal colors: `#rrggbbaa` or one of the names of the original Xournal palette.
fn parse_color(value: &str) -> [f32; 4] {
    let rgba: u32 = match value.trim() {
        "black" => 0x000000ff,
        "blue" => 0x3333ccff,
        "red" => 0xff0000ff,
        "green" => 0x008000ff,
        "gray" => 0x808080ff,
        "lightblue" => 0x00c0ffff,
        "lightgreen" => 0x00ff00ff,
        "magenta" => 0xff00ffff,
        "orange" => 0xff8000ff,
        "yellow" => 0xffff00ff,
        "white" => 0xffffffff,
        hex => {
            let hex = hex.trim_start_matches('#');
            match (u32::from_str_radix(hex, 16), hex.len()) {
                (Ok(v), 8) => v,
                (Ok(v), 6) => (v << 8) | 0xff,
                _ => 0x000000ff,
            }
        }
    };
    [
        (rgba >> 24) as f32 / 255.0,
        ((rgba >> 16) & 0xff) as f32 / 255.0,
        ((rgba >> 8) & 0xff) as f32 / 255.0,
        (rgba & 0xff) as f32 / 255.0,
    ]
}

fn numbers(text: &str) -> Vec<f32> {
    text.split_ascii_whitespace()
        .filter_map(|v| v.parse::<f32>().ok())
        .filter(|v| v.is_finite())
        .collect()
}

/// Element whose text content is still being collected.
enum Open {
    Stroke(HashMap<String, String>),
    Text(HashMap<String, String>),
    Image(HashMap<String, String>),
}

/// Parses the XML of a Xournal file.
pub fn parse(xml: &str) -> Result<XoppDocument, String> {
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut doc = XoppDocument::default();
    let mut seen_root = false;
    let mut open: Option<Open> = None;
    let mut content = String::new();
    loop {
        let event = reader
            .read_event()
            .map_err(|e| t!("Datei beschädigt: {err}", err = e))?;
        match event {
            Event::Start(ref e) | Event::Empty(ref e) => {
                let is_start = matches!(event, Event::Start(_));
                let attrs = attributes(e);
                match e.name().as_ref() {
                    b"xournal" => seen_root = true,
                    b"page" => doc.pages.push(XoppPage {
                        width: number(&attrs, "width").unwrap_or(595.0).max(1.0),
                        height: number(&attrs, "height").unwrap_or(842.0).max(1.0),
                        ..Default::default()
                    }),
                    b"background" => {
                        if let Some(page) = doc.pages.last_mut() {
                            page.background = parse_background(&attrs);
                        }
                    }
                    b"stroke" if is_start => open = Some(Open::Stroke(attrs)),
                    b"text" if is_start => open = Some(Open::Text(attrs)),
                    b"image" if is_start => open = Some(Open::Image(attrs)),
                    b"teximage" => {
                        if let Some(page) = doc.pages.last_mut() {
                            page.items.push(XoppItem::Unsupported);
                        }
                    }
                    _ => {}
                }
                content.clear();
            }
            Event::Text(t) if open.is_some() => {
                content.push_str(&t.decode().map_err(|e| e.to_string())?);
            }
            Event::CData(t) if open.is_some() => {
                content.push_str(&t.decode().map_err(|e| e.to_string())?);
            }
            Event::GeneralRef(r) if open.is_some() => {
                if let Ok(Some(c)) = r.resolve_char_ref() {
                    content.push(c);
                } else if let Ok(name) = r.decode() {
                    if let Some(s) = quick_xml::escape::resolve_predefined_entity(&name) {
                        content.push_str(s);
                    }
                }
            }
            Event::End(_) => {
                if let (Some(element), Some(page)) = (open.take(), doc.pages.last_mut()) {
                    page.items.extend(finish_item(element, &content));
                }
                content.clear();
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if !seen_root {
        return Err(t!("Keine Xournal-Datei").to_string());
    }
    Ok(doc)
}

fn parse_background(attrs: &HashMap<String, String>) -> XoppBackground {
    match attrs.get("type").map(String::as_str) {
        Some("pdf") => {
            // Xournal++ has written page numbers like "2ll"; only the digits count
            let page = attrs
                .get("pageno")
                .map(|v| {
                    v.chars()
                        .take_while(char::is_ascii_digit)
                        .collect::<String>()
                })
                .and_then(|v| v.parse::<u32>().ok())
                .unwrap_or(1);
            XoppBackground::Pdf {
                file: attrs.get("filename").filter(|f| !f.is_empty()).cloned(),
                page: page.saturating_sub(1),
            }
        }
        Some("pixmap") => match attrs.get("filename") {
            Some(file) if !file.is_empty() => XoppBackground::Pixmap { file: file.clone() },
            _ => XoppBackground::Plain,
        },
        _ => match attrs.get("style").map(String::as_str) {
            Some("lined" | "ruled") => XoppBackground::Lined,
            Some("graph" | "isograph") => XoppBackground::Graph,
            Some("dotted" | "isodotted") => XoppBackground::Dotted,
            _ => XoppBackground::Plain,
        },
    }
}

fn finish_item(element: Open, content: &str) -> Option<XoppItem> {
    match element {
        Open::Stroke(attrs) => {
            let coords = numbers(content);
            let points: Vec<glam::Vec2> = coords
                .as_chunks::<2>()
                .0
                .iter()
                .map(|&[x, y]| glam::Vec2::new(x, y))
                .collect();
            if points.is_empty() {
                return None;
            }
            Some(XoppItem::Stroke {
                highlighter: attrs.get("tool").is_some_and(|t| t == "highlighter"),
                color: parse_color(attrs.get("color").map_or("black", String::as_str)),
                widths: attrs.get("width").map(|w| numbers(w)).unwrap_or_default(),
                points,
            })
        }
        Open::Text(attrs) => {
            if content.trim().is_empty() {
                return None;
            }
            Some(XoppItem::Text {
                mono: attrs
                    .get("font")
                    .is_some_and(|f| f.to_ascii_lowercase().contains("mono")),
                size: number(&attrs, "size").unwrap_or(12.0).max(1.0),
                pos: glam::Vec2::new(
                    number(&attrs, "x").unwrap_or(0.0),
                    number(&attrs, "y").unwrap_or(0.0),
                ),
                color: parse_color(attrs.get("color").map_or("black", String::as_str)),
                content: content.to_string(),
            })
        }
        Open::Image(attrs) => {
            let rect = ["left", "top", "right", "bottom"].map(|k| number(&attrs, k));
            match rect {
                [Some(l), Some(t), Some(r), Some(b)] => Some(XoppItem::Image {
                    rect: [l, t, r, b],
                    data: content.to_string(),
                }),
                _ => Some(XoppItem::Unsupported),
            }
        }
    }
}

/// Our stroke width and sample pressures for Xournal widths (nominal width, then optionally
/// one width per point, or per segment in the original Xournal).
fn stroke_widths(widths: &[f32], count: usize, scale: f32) -> (f32, Vec<f32>) {
    let nominal = widths.first().copied().unwrap_or(1.0).max(0.01) * scale;
    let per_point = widths.get(1..).unwrap_or_default();
    if per_point.is_empty() || per_point.len() + 1 < count {
        return (nominal, vec![0.5; count]);
    }
    let scaled: Vec<f32> = (0..count)
        .map(|i| per_point[i.min(per_point.len() - 1)] * scale)
        .collect();
    import::fit_widths(&scaled)
}

/// Finds a referenced file: as given, next to the Xournal file, or as Xournal++ attachment
/// (`<file>.xopp.<name>`).
fn find_file(name: &str, xopp_path: &Path) -> Option<PathBuf> {
    let given = PathBuf::from(name);
    let dir = xopp_path.parent().unwrap_or(Path::new("."));
    let mut candidates = vec![given.clone(), dir.join(&given)];
    candidates.push(PathBuf::from(format!("{}.{name}", xopp_path.display())));
    if let Some(file_name) = given.file_name() {
        candidates.push(dir.join(file_name));
    }
    candidates.into_iter().find(|p| p.is_file())
}

/// Creates a new notebook from a Xournal++/Xournal file.
pub fn create_notebook_from_xopp(
    base_dir: &Path,
    title: &str,
    xopp_path: &Path,
) -> Result<(Notebook, NotebookManifest, PathBuf, ImportReport), String> {
    let bytes = std::fs::read(xopp_path).map_err(|e| e.to_string())?;
    let doc = parse(&decompress(&bytes)?)?;
    if doc.pages.is_empty() {
        return Err(t!("Die Datei enthält keine Seiten").to_string());
    }
    import::create_notebook(base_dir, title, |dir| convert(&doc, xopp_path, dir))
}

/// PDF used as background: asset name and page sizes in points.
struct PdfSource {
    asset: String,
    sizes: Vec<(f32, f32)>,
}

fn convert(doc: &XoppDocument, xopp_path: &Path, notebook_dir: &Path) -> Result<Imported, String> {
    let assets = crate::assets::AssetStore::new(notebook_dir);
    let mut report = ImportReport::default();
    let mut pdfs: HashMap<String, Option<PdfSource>> = HashMap::new();
    let mut current_pdf: Option<String> = None;
    let mut next_id: u64 = 1;
    let mut pages = Vec::with_capacity(doc.pages.len());

    for (index, xpage) in doc.pages.iter().enumerate() {
        // Same rules as for imported PDFs: portrait pages become A4, the content keeps its
        // aspect ratio, centred horizontally and aligned at the top
        let size = crate::document::pdf_page_size(xpage.width, xpage.height);
        let placement = crate::document::pdf_placement(xpage.width, xpage.height, size);
        let [origin, top_right, _, bottom_left] = placement;
        let scale =
            ((top_right.x - origin.x) / xpage.width).min((bottom_left.y - origin.y) / xpage.height);
        let map = |p: glam::Vec2| origin + p * scale;

        let mut background = PageBackground::Blank;
        let mut pdf_background = None;
        let mut strokes: Vec<StrokeData> = Vec::new();
        let mut new_id = || {
            next_id += 1;
            next_id - 1
        };

        match &xpage.background {
            XoppBackground::Plain => {}
            XoppBackground::Lined => background = PageBackground::Lined,
            XoppBackground::Graph => background = PageBackground::Grid5mm,
            XoppBackground::Dotted => background = PageBackground::DotGrid,
            XoppBackground::Pdf { file, page } => {
                if let Some(file) = file {
                    current_pdf = Some(file.clone());
                }
                if let Some(file) = &current_pdf {
                    let source = pdfs
                        .entry(file.clone())
                        .or_insert_with(|| load_pdf(file, xopp_path, &assets));
                    match source {
                        Some(source) => {
                            if let Some(&(width_pt, height_pt)) = source.sizes.get(*page as usize) {
                                pdf_background = Some(PdfBackground {
                                    asset: source.asset.clone(),
                                    page: *page,
                                    width_pt,
                                    height_pt,
                                });
                            }
                        }
                        None => report.missing_pdf = Some(file.clone()),
                    }
                }
            }
            XoppBackground::Pixmap { file } => {
                let image = find_file(file, xopp_path)
                    .and_then(|p| std::fs::read(p).ok())
                    .and_then(|b| assets.import_bytes(&b).ok());
                match image {
                    Some(image) => {
                        strokes.push(import::image_stroke(new_id(), image.asset, placement))
                    }
                    None => report.skipped += 1,
                }
            }
        }

        for item in &xpage.items {
            match item {
                XoppItem::Stroke {
                    highlighter,
                    color,
                    widths,
                    points,
                } => {
                    let (width, pressures) = stroke_widths(widths, points.len(), scale);
                    let points: Vec<glam::Vec2> = points.iter().map(|&p| map(p)).collect();
                    strokes.push(import::ink_stroke(
                        new_id(),
                        *color,
                        *highlighter,
                        &points,
                        width,
                        &pressures,
                    ));
                }
                XoppItem::Text {
                    mono,
                    size,
                    pos,
                    color,
                    content,
                } => {
                    let font = if *mono {
                        TextFont::Mono
                    } else {
                        TextFont::Sans
                    };
                    // Xournal doesn't wrap: as wide as the longest line
                    strokes.push(import::text_stroke(
                        new_id(),
                        content,
                        size * scale,
                        font,
                        map(*pos),
                        [glam::Vec2::X, glam::Vec2::Y],
                        None,
                        *color,
                    ));
                }
                XoppItem::Image { rect, data } => {
                    let compact: String =
                        data.chars().filter(|c| !c.is_ascii_whitespace()).collect();
                    let image = base64::engine::general_purpose::STANDARD
                        .decode(compact)
                        .ok()
                        .and_then(|b| assets.import_bytes(&b).ok());
                    match image {
                        Some(image) => {
                            let [l, t, r, b] = *rect;
                            let corners = [
                                glam::Vec2::new(l, t),
                                glam::Vec2::new(r, t),
                                glam::Vec2::new(r, b),
                                glam::Vec2::new(l, b),
                            ]
                            .map(map);
                            strokes.push(import::image_stroke(new_id(), image.asset, corners));
                        }
                        None => report.skipped += 1,
                    }
                }
                XoppItem::Unsupported => report.skipped += 1,
            }
        }

        pages.push(import::page(
            index as u64 + 1,
            size,
            background,
            pdf_background,
            strokes,
        ));
    }
    Ok(Imported {
        pages,
        next_stroke_id: next_id,
        layout: None,
        report,
    })
}

fn load_pdf(file: &str, xopp_path: &Path, assets: &crate::assets::AssetStore) -> Option<PdfSource> {
    let bytes = std::fs::read(find_file(file, xopp_path)?).ok()?;
    let sizes = crate::pdf_doc::page_sizes(bytes.clone()).ok()?;
    let asset = assets.import_pdf(&bytes).ok()?;
    Some(PdfSource { asset, sizes })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::StrokeTool;
    use crate::storage;
    use std::io::Write;

    fn gzip(xml: &str) -> Vec<u8> {
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(xml.as_bytes()).unwrap();
        encoder.finish().unwrap()
    }

    fn png_base64() -> String {
        let image = image::RgbaImage::from_pixel(4, 2, image::Rgba([200, 30, 30, 255]));
        let mut png = Vec::new();
        image
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        base64::engine::general_purpose::STANDARD.encode(png)
    }

    const SAMPLE: &str = r##"<?xml version="1.0" standalone="no"?>
<xournal creator="Xournal++ 1.2.3" fileversion="4">
<title>Xournal++ document</title>
<page width="595.27559" height="841.88976">
<background type="solid" color="#ffffffff" style="lined"/>
<layer>
<stroke tool="pen" ts="0" fn="" color="#3333ccff" width="1.41 1.0 2.0 1.5" capStyle="round">10 20 30 40 50 60</stroke>
<stroke tool="highlighter" color="#ffff00ff" width="8.5">0 0 100 0</stroke>
</layer>
<layer>
<text font="Sans" size="12" x="72" y="100" color="#000000ff">Hallo &amp; Welt
zweite Zeile</text>
<teximage text="x^2" left="0" top="0" right="10" bottom="10">AAAA</teximage>
</layer>
</page>
<page width="841.88976" height="595.27559">
<background type="pdf" domain="absolute" filename="/nirgends/skript.pdf" pageno="1ll"/>
<layer/>
</page>
<page width="841.88976" height="595.27559">
<background type="pdf" pageno="2ll"/>
<layer/>
</page>
</xournal>"##;

    #[test]
    fn test_parse_xopp() {
        let doc = parse(&decompress(&gzip(SAMPLE)).unwrap()).unwrap();
        assert_eq!(doc.pages.len(), 3);
        let first = &doc.pages[0];
        assert_eq!(first.background, XoppBackground::Lined);
        assert_eq!(first.items.len(), 4);
        match &first.items[0] {
            XoppItem::Stroke {
                highlighter,
                color,
                widths,
                points,
            } => {
                assert!(!highlighter);
                assert_eq!(*color, [0.2, 0.2, 0.8, 1.0]);
                assert_eq!(widths, &[1.41, 1.0, 2.0, 1.5]);
                assert_eq!(points.len(), 3);
                assert_eq!(points[2], glam::Vec2::new(50.0, 60.0));
            }
            other => panic!("expected a stroke, got {other:?}"),
        }
        assert!(matches!(
            first.items[1],
            XoppItem::Stroke {
                highlighter: true,
                ..
            }
        ));
        match &first.items[2] {
            XoppItem::Text { content, size, .. } => {
                assert_eq!(content, "Hallo & Welt\nzweite Zeile");
                assert_eq!(*size, 12.0);
            }
            other => panic!("expected a text, got {other:?}"),
        }
        assert_eq!(first.items[3], XoppItem::Unsupported);
        assert_eq!(
            doc.pages[1].background,
            XoppBackground::Pdf {
                file: Some("/nirgends/skript.pdf".into()),
                page: 0
            }
        );
        assert_eq!(
            doc.pages[2].background,
            XoppBackground::Pdf {
                file: None,
                page: 1
            }
        );
    }

    #[test]
    fn test_parse_rejects_other_files() {
        assert!(parse("<svg></svg>").is_err());
        assert!(decompress(&[0x1f, 0x8b, 1, 2, 3]).is_err());
    }

    #[test]
    fn test_colors() {
        assert_eq!(parse_color("#ff000080")[3], 128.0 / 255.0);
        assert_eq!(parse_color("red"), [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(parse_color("#00ff00"), [0.0, 1.0, 0.0, 1.0]);
        assert_eq!(parse_color("Unsinn"), [0.0, 0.0, 0.0, 1.0]);
    }

    #[test]
    fn test_pressure_widths_map_to_our_width_model() {
        // Without pressure: nominal width, medium pressure (drawn exactly that wide)
        assert_eq!(stroke_widths(&[2.0], 3, 10.0), (20.0, vec![0.5; 3]));
        // With pressure: the widest point is drawn exactly as wide as in Xournal++
        let (width, pressures) = stroke_widths(&[1.0, 3.0, 1.5], 2, 10.0);
        let drawn: Vec<f32> = pressures.iter().map(|p| width * (0.5 + p)).collect();
        assert!((drawn[0] - 30.0).abs() < 1e-3);
        assert!((drawn[1] - 15.0).abs() < 1e-3);
        // Xournal: one width per segment, the last point repeats the last one
        let (_, pressures) = stroke_widths(&[1.0, 2.0, 1.0], 3, 1.0);
        assert_eq!(pressures.len(), 3);
        assert_eq!(pressures[1], pressures[2]);
    }

    #[test]
    fn test_create_notebook_from_xopp() {
        let temp_dir = tempfile::tempdir().unwrap();
        let source_dir = tempfile::tempdir().unwrap();
        let pdf = crate::pdf_doc::tests::sample_pdf(&[(842.0, 595.0, 0), (842.0, 595.0, 0)]);
        std::fs::write(source_dir.path().join("skript.pdf"), &pdf).unwrap();
        // The PDF moved next to the notebook; its image is embedded
        let xml = SAMPLE.replace("<layer/>\n</page>\n</xournal>", &format!(
            "<layer><image left=\"0\" top=\"0\" right=\"40\" bottom=\"20\">{}</image></layer>\n</page>\n</xournal>",
            png_base64()
        ));
        let path = source_dir.path().join("Mathe.xopp");
        std::fs::write(&path, gzip(&xml)).unwrap();

        let (notebook, manifest, dir, report) =
            create_notebook_from_xopp(temp_dir.path(), "Mathe", &path).unwrap();
        assert_eq!(manifest.title, "Mathe");
        assert_eq!(notebook.pages.len(), 3);
        assert_eq!(
            report,
            ImportReport {
                skipped: 1,
                missing_pdf: None
            }
        );

        let (mut reloaded, _) = storage::load_notebook(&dir).unwrap();
        storage::load_all_pages(&dir, &mut reloaded).unwrap();
        let first = &reloaded.pages[0];
        assert_eq!(first.background, PageBackground::Lined);
        assert_eq!(first.size, crate::document::A4_SIZE);
        assert_eq!(first.strokes.len(), 3);
        let pen = &first.strokes[0];
        assert_eq!(pen.tool, StrokeTool::Pen);
        // Points → page units (10 per mm): 10 pt = 35.3 units
        assert!((pen.points[0].position.x - 35.28).abs() < 0.05);
        assert!(!pen.vertices.is_empty());
        let marker = &first.strokes[1];
        assert_eq!(marker.tool, StrokeTool::Highlighter);
        assert_eq!(marker.color[3], import::HIGHLIGHTER_ALPHA);
        assert!(first.strokes[2].is_text());

        for (i, page) in reloaded.pages[1..].iter().enumerate() {
            let bg = page
                .pdf_background
                .as_ref()
                .expect("PDF background missing");
            assert_eq!(bg.page, i as u32);
            assert!(page.size.x > page.size.y);
        }
        assert!(reloaded.pages[2].strokes[0].is_image());
        // Stroke ids are unique and the notebook continues after them
        assert!(reloaded.next_stroke_id > 4);
    }

    #[test]
    fn test_missing_pdf_is_reported() {
        let temp_dir = tempfile::tempdir().unwrap();
        let source_dir = tempfile::tempdir().unwrap();
        let path = source_dir.path().join("Mathe.xoj");
        std::fs::write(&path, SAMPLE).unwrap();
        let (notebook, _, _, report) =
            create_notebook_from_xopp(temp_dir.path(), "Mathe", &path).unwrap();
        assert_eq!(notebook.pages.len(), 3);
        assert_eq!(report.missing_pdf.as_deref(), Some("/nirgends/skript.pdf"));
        assert!(notebook.pages[1].pdf_background.is_none());
    }
}
