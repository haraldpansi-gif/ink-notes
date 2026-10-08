//! Verlustfreier Vektor-PDF-Export des gesamten Notizbuchs via printpdf.
//!
//! Features:
//! - Hochformat DIN A4 (210 mm x 297 mm)
//! - Layer pro Seite: "Background", "Images" (nur bei eingefügten Bildern), "Text" (nur bei
//!   Textfeldern; echter, durchsuchbarer Text mit eingebetteter Schrift) und "Ink"
//! - Y-Achsen-Spiegelung (PDF-Ursprung unten links vs. Bildschirmursprung oben links)
//! - Vektor-Hintergründe (Kariert 5mm, Liniert, Punktraster, Blanko)
//! - Zwei Export-Strategien:
//!   - `FilledPolygon`: Glatte gefüllte Polygone aus den Triangle-Strip-Hüllen (variable Pinselstärke)
//!   - `BezierCurve`: Glatte kubische Bézier-Kurven via `printpdf::Line`

use std::collections::HashMap;
use std::error::Error;
use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};

use printpdf::path::{PaintMode, WindingOrder};
use printpdf::{Color, Line, Mm, PdfDocument, PdfDocumentReference, Point, Polygon, Rgb};

use crate::assets::AssetStore;
use crate::background::{PatternColor, DOT_SIZE, GRID_5MM_SPACING, LINED_SPACING};
use crate::document::{
    Notebook, Page, PageBackground, Stroke, A4_HEIGHT_MM, A4_WIDTH_MM, PAGE_HEIGHT, PAGE_WIDTH,
};
use ab_glyph::Font as _;

use crate::text::TextFont;

/// Bereinigt einen Titel für die Verwendung in Dateinamen (entfernt illegale Zeichen).
pub fn sanitize_filename_component(title: &str) -> String {
    let sanitized: String = title
        .chars()
        .map(|c| match c {
            '/' | '\\' | '\0' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c => c,
        })
        .collect();
    let trimmed = sanitized.trim();
    if trimmed.is_empty() {
        "Notizbuch".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Ermittelt das Standard-Dokumentenverzeichnis ($XDG_DOCUMENTS_DIR mit Fallback ~/Documents).
pub fn default_documents_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("XDG_DOCUMENTS_DIR") {
        if !dir.trim().is_empty() {
            return PathBuf::from(dir);
        }
    }

    if let Ok(home) = std::env::var("HOME") {
        if !home.trim().is_empty() {
            return PathBuf::from(home).join("Documents");
        }
    }

    PathBuf::from(".")
}

/// Ermittelt den kollisionsfreien Export-Pfad für ein Notizbuch im übergebenen Verzeichnis.
/// Existiert die Datei bereits, wird ein Zähler angehängt (`_1`, `_2`, etc.).
pub fn resolve_pdf_export_path_in_dir(dir: &Path, title: &str) -> PathBuf {
    let sanitized_title = sanitize_filename_component(title);
    let base_name = format!("{}.pdf", sanitized_title);
    let primary_path = dir.join(&base_name);

    if !primary_path.exists() {
        return primary_path;
    }

    let mut counter = 1;
    loop {
        let candidate_name = format!("{}_{}.pdf", sanitized_title, counter);
        let candidate_path = dir.join(candidate_name);
        if !candidate_path.exists() {
            return candidate_path;
        }
        counter += 1;
    }
}

/// Kollisionsfreier Pfad `~/Documents/<name>.<ext>` (Verzeichnis wird angelegt).
pub fn resolve_export_path(name: &str, ext: &str) -> PathBuf {
    let dir = default_documents_dir();
    let _ = std::fs::create_dir_all(&dir);
    let base = sanitize_filename_component(name);
    let mut candidate = dir.join(format!("{base}.{ext}"));
    let mut counter = 1;
    while candidate.exists() {
        candidate = dir.join(format!("{base}_{counter}.{ext}"));
        counter += 1;
    }
    candidate
}

/// Ermittelt das Ziel für den PDF-Export unter Berücksichtigung von XDG_DOCUMENTS_DIR
/// und automatischer Namenskollisionsvermeidung. Erstellt das Verzeichnis, falls nötig.
pub fn resolve_pdf_export_path(title: &str) -> PathBuf {
    let docs_dir = default_documents_dir();
    let _ = std::fs::create_dir_all(&docs_dir);
    resolve_pdf_export_path_in_dir(&docs_dir, title)
}

/// Wählbare Exportstrategie für Vektorstriche.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExportStrategy {
    /// Gefüllte Polygone aus den Triangle-Strip-Hüllen (ideal für variable Druckstärken).
    #[default]
    FilledPolygon,
    /// Kubische Bézier-Kurven mit Segment-Linienstärke.
    BezierCurve,
}

/// Rechnet eine interne Seitenkoordinate (x, y) in PDF-Millimeter-Koordinaten um (mit Y-Inversion).
///
/// Bezugshöhe ist die A4-Seite; für anders hohe Seiten verschiebt [`begin_page_layer`] die
/// Ebene entsprechend.
#[inline]
pub fn page_to_pdf_mm(x: f32, y: f32) -> (f32, f32) {
    let x_mm = x / PAGE_WIDTH * A4_WIDTH_MM;
    let y_mm = (1.0 - y / PAGE_HEIGHT) * A4_HEIGHT_MM; // PDF-Ursprung liegt unten links
    (x_mm, y_mm)
}

/// PDF-Punkte je Seiteneinheit.
const PT_PER_UNIT: f32 = A4_WIDTH_MM / PAGE_WIDTH * 72.0 / 25.4;

/// Größe der Seite `page` im PDF.
fn pdf_page_mm(page: &Page) -> (Mm, Mm) {
    let mm = page.size / crate::document::UNITS_PER_MM;
    (Mm(mm.x), Mm(mm.y))
}

/// Beginnt eine Ebene: Koordinaten beziehen sich auf eine A4-hohe Seite (siehe
/// [`page_to_pdf_mm`]) und werden auf die tatsächliche Höhe von `page` verschoben.
fn begin_page_layer(layer: &printpdf::PdfLayerReference, page: &Page) {
    layer.save_graphics_state();
    let dy = (page.size.y - PAGE_HEIGHT) * PT_PER_UNIT;
    if dy != 0.0 {
        layer.set_ctm(printpdf::CurTransMat::Translate(
            printpdf::Pt(0.0),
            printpdf::Pt(dy),
        ));
    }
}

/// Beendet eine mit [`begin_page_layer`] begonnene Ebene.
fn end_page_layer(layer: &printpdf::PdfLayerReference) {
    layer.restore_graphics_state();
}

/// Erzeugt ein PDF-Dokument aus einem Notizbuch und speichert es als Datei.
/// `assets`: Bilder des Notizbuchs (ohne werden Bilder als graue Fläche ausgegeben);
/// `pattern`: Farbe von Karos, Linien und Punkten.
pub fn export_notebook_to_pdf<P: AsRef<Path>>(
    notebook: &Notebook,
    path: P,
    strategy: ExportStrategy,
    assets: Option<&AssetStore>,
    pattern: PatternColor,
) -> Result<(), Box<dyn Error>> {
    let bytes = export_notebook_to_bytes(notebook, strategy, assets, pattern)?;
    let file = File::create(path)?;
    let mut writer = BufWriter::new(file);
    std::io::Write::write_all(&mut writer, &bytes)?;
    std::io::Write::flush(&mut writer)?;
    Ok(())
}

/// Erzeugt ein PDF-Dokument aus einem Notizbuch und gibt die rohen Bytes zurück.
pub fn export_notebook_to_bytes(
    notebook: &Notebook,
    strategy: ExportStrategy,
    assets: Option<&AssetStore>,
    pattern: PatternColor,
) -> Result<Vec<u8>, Box<dyn Error>> {
    let doc = generate_pdf_document(notebook, strategy, assets, pattern)?;
    let bytes = add_alpha_states(fix_text_encoding(doc.save_to_bytes()?, notebook)?, notebook)?;

    // Importierte PDF-Seiten als Vektorgrafik unterlegen
    let underlays = pdf_underlays(notebook);
    if underlays.is_empty() {
        return Ok(bytes);
    }
    let sources: std::collections::HashMap<String, Vec<u8>> = underlays
        .iter()
        .filter_map(|u| Some((u.source.clone(), assets?.read_bytes(&u.source).ok()?)))
        .collect();
    let (merged, failed) = crate::pdf_merge::apply_underlays(&bytes, &underlays, &sources)?;
    if failed.is_empty() {
        return Ok(merged);
    }

    // Nicht übernehmbare Seiten (z. B. verschlüsselte PDFs) als Bild einbetten
    let mut raster = std::collections::HashSet::new();
    for (underlay, err) in &failed {
        eprintln!(
            "[PDF-Export] Seite {} des PDFs als Bild statt als Vektor: {err}",
            underlay.source_page + 1
        );
        raster.insert(underlay.target_page);
    }
    let doc = generate_pdf_document_with(notebook, strategy, assets, pattern, &raster)?;
    let bytes = add_alpha_states(fix_text_encoding(doc.save_to_bytes()?, notebook)?, notebook)?;
    let remaining: Vec<_> = underlays
        .into_iter()
        .filter(|u| !raster.contains(&u.target_page))
        .collect();
    if remaining.is_empty() {
        return Ok(bytes);
    }
    let (merged, _) = crate::pdf_merge::apply_underlays(&bytes, &remaining, &sources)?;
    Ok(merged)
}

/// printpdf schreibt Lesezeichen-Titel als rohes UTF-8, PDF-Betrachter lesen Textstrings aber
/// als PDFDocEncoding oder UTF-16BE: Umlaute und Gedankenstriche würden verstümmelt. Titel
/// mit Nicht-ASCII-Zeichen werden deshalb nach UTF-16BE (mit BOM) umgeschrieben.
fn fix_text_encoding(bytes: Vec<u8>, notebook: &Notebook) -> Result<Vec<u8>, Box<dyn Error>> {
    let needs_fix = notebook
        .pages
        .iter()
        .filter_map(|p| p.title.as_deref())
        .any(|t| !t.is_ascii());
    if !needs_fix {
        return Ok(bytes);
    }
    let mut doc = printpdf::lopdf::Document::load_mem(&bytes)?;
    for object in doc.objects.values_mut() {
        let Ok(dict) = object.as_dict_mut() else {
            continue;
        };
        if let Ok(printpdf::lopdf::Object::String(raw, format)) = dict.get_mut(b"Title") {
            if let Some(utf16) = utf16be_text(raw) {
                *raw = utf16;
                *format = printpdf::lopdf::StringFormat::Hexadecimal;
            }
        }
    }
    let mut out = Vec::new();
    doc.save_to(&mut out)?;
    Ok(out)
}

/// Name des Grafikzustands für halbtransparente Tinte mit Deckkraft `alpha` (auf Prozent
/// gerundet); `None` für deckende Farben.
fn alpha_state_name(alpha: f32) -> Option<String> {
    let percent = (alpha.clamp(0.0, 1.0) * 100.0).round() as u32;
    (percent < 100).then(|| format!("InkAlpha{percent}"))
}

/// printpdf kann keine Grafikzustände mit Deckkraft anlegen: Die Tintenebene verweist mit
/// `gs` auf die Namen aus [`alpha_state_name`], die Zustände selbst (`ca`/`CA`) werden hier
/// in die Ressourcen jeder Seite eingetragen. Ohne halbtransparente Striche bleibt das PDF
/// unverändert.
fn add_alpha_states(bytes: Vec<u8>, notebook: &Notebook) -> Result<Vec<u8>, Box<dyn Error>> {
    use printpdf::lopdf::{Dictionary, Object};

    let mut states: Vec<(String, f32)> = notebook
        .pages
        .iter()
        .flat_map(|p| &p.strokes)
        .filter(|s| !s.is_object())
        .filter_map(|s| alpha_state_name(s.color[3]).map(|name| (name, s.color[3])))
        .collect();
    if states.is_empty() {
        return Ok(bytes);
    }
    states.sort_by(|a, b| a.0.cmp(&b.0));
    states.dedup_by(|a, b| a.0 == b.0);

    let mut doc = printpdf::lopdf::Document::load_mem(&bytes)?;
    let page_ids: Vec<_> = doc.get_pages().into_values().collect();
    for page_id in page_ids {
        let resources = dict_object_id(&mut doc, page_id, b"Resources")?;
        let ext_g_state = dict_object_id(&mut doc, resources, b"ExtGState")?;
        let dict = doc.get_object_mut(ext_g_state)?.as_dict_mut()?;
        for (name, alpha) in &states {
            let alpha = (alpha.clamp(0.0, 1.0) * 100.0).round() / 100.0;
            let mut state = Dictionary::new();
            state.set("Type", Object::Name(b"ExtGState".to_vec()));
            state.set("ca", Object::Real(alpha));
            state.set("CA", Object::Real(alpha));
            dict.set(name.as_bytes().to_vec(), Object::Dictionary(state));
        }
    }
    let mut out = Vec::new();
    doc.save_to(&mut out)?;
    Ok(out)
}

/// Kennung des Wörterbuchs unter `key` im Objekt `owner`; ein eingebettetes oder fehlendes
/// Wörterbuch wird dafür zu einem eigenen Objekt.
fn dict_object_id(
    doc: &mut printpdf::lopdf::Document,
    owner: printpdf::lopdf::ObjectId,
    key: &[u8],
) -> Result<printpdf::lopdf::ObjectId, Box<dyn Error>> {
    use printpdf::lopdf::{Dictionary, Object};

    let entry = doc.get_object(owner)?.as_dict()?.get(key).ok().cloned();
    if let Some(Object::Reference(id)) = entry {
        return Ok(id);
    }
    let dict = match entry {
        Some(Object::Dictionary(dict)) => dict,
        _ => Dictionary::new(),
    };
    let id = doc.add_object(dict);
    doc.get_object_mut(owner)?
        .as_dict_mut()?
        .set(key.to_vec(), Object::Reference(id));
    Ok(id)
}

/// UTF-8-Text als PDF-Textstring in UTF-16BE mit BOM; `None` bei reinem ASCII oder
/// ungültigem UTF-8.
fn utf16be_text(raw: &[u8]) -> Option<Vec<u8>> {
    let text = std::str::from_utf8(raw).ok()?;
    if text.is_ascii() {
        return None;
    }
    let mut out = vec![0xFE, 0xFF];
    for unit in text.encode_utf16() {
        out.extend_from_slice(&unit.to_be_bytes());
    }
    Some(out)
}

/// Unterlagen für alle Seiten mit importierter PDF-Seite.
fn pdf_underlays(notebook: &Notebook) -> Vec<crate::pdf_merge::Underlay> {
    notebook
        .pages
        .iter()
        .enumerate()
        .filter_map(|(i, page)| {
            let pdf = page.pdf_background.as_ref()?;
            let corners = pdf.corners(page.size);
            // Unterlagen werden ohne die Verschiebung von `begin_page_layer` gesetzt
            let shift = glam::Vec2::new(0.0, (page.size.y - PAGE_HEIGHT) * PT_PER_UNIT);
            let top_right = page_to_pdf_pt(corners[1]) + shift;
            let bottom_left = page_to_pdf_pt(corners[3]) + shift;
            let size = top_right - bottom_left;
            Some(crate::pdf_merge::Underlay {
                target_page: i,
                source: pdf.asset.clone(),
                source_page: pdf.page as usize,
                rect: [bottom_left.x, bottom_left.y, size.x, size.y],
            })
        })
        .collect()
}

/// Generiert das `printpdf::PdfDocumentReference` für alle Seiten des Notizbuchs.
/// Importierte PDF-Seiten sind darin noch nicht enthalten; die legt
/// [`export_notebook_to_bytes`] anschließend als Vektorgrafik darunter.
pub fn generate_pdf_document(
    notebook: &Notebook,
    strategy: ExportStrategy,
    assets: Option<&AssetStore>,
    pattern: PatternColor,
) -> Result<PdfDocumentReference, Box<dyn Error>> {
    generate_pdf_document_with(
        notebook,
        strategy,
        assets,
        pattern,
        &std::collections::HashSet::new(),
    )
}

/// Wie [`generate_pdf_document`]; die PDF-Seiten der Seiten in `raster_pdf_pages` werden
/// gerendert und als Bild eingebettet.
fn generate_pdf_document_with(
    notebook: &Notebook,
    strategy: ExportStrategy,
    assets: Option<&AssetStore>,
    pattern: PatternColor,
    raster_pdf_pages: &std::collections::HashSet<usize>,
) -> Result<PdfDocumentReference, Box<dyn Error>> {
    if notebook.pages.is_empty() {
        return Err("Notizbuch enthält keine Seiten".into());
    }
    // Nicht geladene Seiten würden sonst stillschweigend als leere Seiten exportiert
    if let Some(page) = notebook.pages.iter().find(|p| {
        !p.is_loaded || p.persist == crate::document::PersistState::ReadOnlyAfterLoadError
    }) {
        return Err(format!(
            "Seite {} ist nicht geladen – Export abgebrochen (vorher storage::load_all_pages aufrufen)",
            page.id
        )
        .into());
    }

    // Erste Seite legt das Dokument an, alle weiteren werden angehängt (je in ihrer Größe)
    let title = "ink-notes Notebook";
    let (width, height) = pdf_page_mm(&notebook.pages[0]);
    let (doc, page1_idx, layer1_bg) = PdfDocument::new(title, width, height, "Background");
    let fonts = embed_text_fonts(&doc, notebook)?;

    for (i, page) in notebook.pages.iter().enumerate() {
        let (page_idx, layer_bg) = if i == 0 {
            (page1_idx, layer1_bg)
        } else {
            let (width, height) = pdf_page_mm(page);
            doc.add_page(width, height, "Background")
        };
        // Seitentitel werden zu Lesezeichen (Inhaltsverzeichnis im PDF-Betrachter)
        if let Some(title) = &page.title {
            doc.add_bookmark(title.clone(), page_idx);
        }
        let page_ref = doc.get_page(page_idx);
        render_background_layer(&page_ref.get_layer(layer_bg), page, pattern);
        let layer = |name: &str, render: &dyn Fn(&printpdf::PdfLayerReference)| {
            let layer = page_ref.add_layer(name);
            begin_page_layer(&layer, page);
            render(&layer);
            end_page_layer(&layer);
        };
        if raster_pdf_pages.contains(&i) {
            layer("PDF", &|l| render_pdf_raster(l, page, assets));
        }
        if page.strokes.iter().any(Stroke::is_image) {
            layer("Images", &|l| render_image_layer(l, page, assets));
        }
        if page.strokes.iter().any(Stroke::is_text) {
            layer("Text", &|l| render_text_layer(l, page, &fonts));
        }
        layer("Ink", &|l| render_ink_layer(l, page, strategy));
    }

    Ok(doc)
}

/// Rendert das Vektorraster auf Layer 1 ("Background").
fn render_background_layer(
    layer: &printpdf::PdfLayerReference,
    page: &Page,
    pattern: PatternColor,
) {
    let background = page.background;
    let (Mm(page_width_mm), Mm(page_height_mm)) = pdf_page_mm(page);
    // Exportiert wird immer auf weißem Papier
    let [r, g, b, _] = pattern.color(crate::background::PaperTheme::White);
    let grid_color = Color::Rgb(Rgb::new(r, g, b, None));
    layer.set_outline_color(grid_color.clone());
    layer.set_outline_thickness(0.4); // Feine 0.15 mm Haarlinie (ca. 0.4 pt)
    layer.set_fill_color(grid_color);

    match background {
        PageBackground::Blank => {}

        PageBackground::Grid5mm => {
            let spacing_mm = GRID_5MM_SPACING / 10.0; // 5.0 mm

            // Vertikale Linien alle 5 mm
            let mut x = spacing_mm;
            while x < page_width_mm {
                let line = Line {
                    points: vec![
                        (Point::new(Mm(x), Mm(0.0)), false),
                        (Point::new(Mm(x), Mm(page_height_mm)), false),
                    ],
                    is_closed: false,
                };
                layer.add_line(line);
                x += spacing_mm;
            }

            // Horizontale Linien alle 5 mm
            let mut y = spacing_mm;
            while y < page_height_mm {
                let line = Line {
                    points: vec![
                        (Point::new(Mm(0.0), Mm(y)), false),
                        (Point::new(Mm(page_width_mm), Mm(y)), false),
                    ],
                    is_closed: false,
                };
                layer.add_line(line);
                y += spacing_mm;
            }
        }

        PageBackground::Lined => {
            let spacing_mm = LINED_SPACING / 10.0; // 7.5 mm
            let margin_top_mm = 15.0;
            let margin_bottom_mm = 15.0;

            let mut y = margin_bottom_mm;
            while y <= page_height_mm - margin_top_mm {
                let line = Line {
                    points: vec![
                        (Point::new(Mm(0.0), Mm(y)), false),
                        (Point::new(Mm(page_width_mm), Mm(y)), false),
                    ],
                    is_closed: false,
                };
                layer.add_line(line);
                y += spacing_mm;
            }
        }

        PageBackground::DotGrid => {
            let spacing_mm = GRID_5MM_SPACING / 10.0; // 5.0 mm
            let half_dot = DOT_SIZE / 20.0; // Kantenlänge in mm wie am Bildschirm

            let mut y = spacing_mm;
            while y < page_height_mm {
                let mut x = spacing_mm;
                while x < page_width_mm {
                    let dot_poly = Polygon {
                        rings: vec![vec![
                            (Point::new(Mm(x - half_dot), Mm(y - half_dot)), false),
                            (Point::new(Mm(x + half_dot), Mm(y - half_dot)), false),
                            (Point::new(Mm(x + half_dot), Mm(y + half_dot)), false),
                            (Point::new(Mm(x - half_dot), Mm(y + half_dot)), false),
                        ]],
                        mode: PaintMode::Fill,
                        winding_order: WindingOrder::NonZero,
                    };
                    layer.add_polygon(dot_poly);
                    x += spacing_mm;
                }
                y += spacing_mm;
            }
        }
    }
}

/// Rendert alle Striche einer Seite auf Layer 2 ("Ink").
fn render_ink_layer(layer: &printpdf::PdfLayerReference, page: &Page, strategy: ExportStrategy) {
    for stroke in &page.strokes {
        if stroke.is_object() {
            continue;
        }
        // Halbtransparente Tinte (Textmarker): Deckkraft über einen Grafikzustand, den
        // `add_alpha_states` anlegt. Jeder Strich ist ein einziger Pfad und deckt daher jede
        // Stelle nur einmal, auch wo er sich selbst überlappt.
        let alpha_state = alpha_state_name(stroke.color[3]);
        if let Some(name) = &alpha_state {
            layer.save_graphics_state();
            layer.add_operation(printpdf::lopdf::content::Operation::new(
                "gs",
                vec![printpdf::lopdf::Object::Name(name.as_bytes().to_vec())],
            ));
        }
        if let Some(ref shape) = stroke.shape {
            render_recognized_shape(layer, shape, stroke.color);
        } else {
            match strategy {
                ExportStrategy::FilledPolygon => {
                    render_stroke_polygon(layer, stroke);
                }
                ExportStrategy::BezierCurve => {
                    render_stroke_bezier(layer, stroke);
                }
            }
        }
        if alpha_state.is_some() {
            layer.restore_graphics_state();
        }
    }
}

/// Auflösung, mit der Bilder höchstens eingebettet werden (Druckqualität).
pub const PDF_IMAGE_DPI: f32 = 300.0;

/// Seitenkoordinate in PDF-Punkten (ohne Begrenzung auf das Blatt, Ursprung unten links).
fn page_to_pdf_pt(p: glam::Vec2) -> glam::Vec2 {
    const PT_PER_MM: f32 = 72.0 / 25.4;
    glam::Vec2::new(
        p.x / PAGE_WIDTH * A4_WIDTH_MM * PT_PER_MM,
        (1.0 - p.y / PAGE_HEIGHT) * A4_HEIGHT_MM * PT_PER_MM,
    )
}

/// PDF-Transformationsmatrix, die das Einheitsquadrat eines Bildes (Ursprung unten links)
/// auf die Bildecken oben links, oben rechts, unten rechts, unten links abbildet.
pub fn image_matrix(corners: &[glam::Vec2; 4]) -> [f32; 6] {
    let [tl, _, br, bl] = corners.map(page_to_pdf_pt);
    let x_axis = br - bl;
    let y_axis = tl - bl;
    [x_axis.x, x_axis.y, y_axis.x, y_axis.y, bl.x, bl.y]
}

/// Anzahl der Bildkomponenten eines JPEG laut Frame-Header (1 = Graustufen, 3 = Farbe,
/// 4 = CMYK), `None` bei unlesbarem Header.
fn jpeg_components(bytes: &[u8]) -> Option<u8> {
    let mut i = 2;
    while i + 9 < bytes.len() {
        if bytes[i] != 0xFF {
            return None;
        }
        let marker = bytes[i + 1];
        let len = u16::from_be_bytes([bytes[i + 2], bytes[i + 3]]) as usize;
        // SOF0–SOF15 außer DHT (C4), JPG (C8) und DAC (CC)
        if (0xC0..=0xCF).contains(&marker) && !matches!(marker, 0xC4 | 0xC8 | 0xCC) {
            return bytes.get(i + 9).copied();
        }
        i += 2 + len;
    }
    None
}

/// Bilddaten für das PDF: JPEG-Dateien werden unverändert eingebettet, alles andere als
/// RGB auf weißem Grund (Transparenz wird mit dem Papier verrechnet), höchstens mit
/// `PDF_IMAGE_DPI` bezogen auf die Größe auf der Seite.
fn pdf_image_xobject(
    assets: &AssetStore,
    asset: &str,
    corners: &[glam::Vec2; 4],
) -> Result<printpdf::ImageXObject, Box<dyn Error>> {
    use printpdf::{ColorBits, ColorSpace, ImageFilter, ImageXObject, Px};

    let bytes = assets.read_bytes(asset)?;
    let decoded = image::load_from_memory(&bytes)?;

    if asset.ends_with(".jpg") {
        let color_space = match jpeg_components(&bytes) {
            Some(1) => Some(ColorSpace::Greyscale),
            Some(3) => Some(ColorSpace::Rgb),
            _ => None,
        };
        if let Some(color_space) = color_space {
            return Ok(ImageXObject {
                width: Px(decoded.width() as usize),
                height: Px(decoded.height() as usize),
                color_space,
                bits_per_component: ColorBits::Bit8,
                interpolate: true,
                image_data: bytes,
                image_filter: Some(ImageFilter::DCT),
                smask: None,
                clipping_bbox: None,
            });
        }
    }

    // Größe auf der Seite in Zoll → benötigte Pixel
    let width_in = (corners[1] - corners[0]).length() / PAGE_WIDTH * A4_WIDTH_MM / 25.4;
    let height_in = (corners[3] - corners[0]).length() / PAGE_WIDTH * A4_WIDTH_MM / 25.4;
    let max_w = ((width_in * PDF_IMAGE_DPI).ceil() as u32).max(1);
    let max_h = ((height_in * PDF_IMAGE_DPI).ceil() as u32).max(1);
    let decoded = if decoded.width() > max_w || decoded.height() > max_h {
        decoded.resize(max_w, max_h, image::imageops::FilterType::Triangle)
    } else {
        decoded
    };

    let rgba = decoded.into_rgba8();
    let mut rgb = Vec::with_capacity((rgba.width() * rgba.height() * 3) as usize);
    for px in rgba.pixels() {
        let alpha = px[3] as u32;
        for c in &px.0[..3] {
            rgb.push(((*c as u32 * alpha + 255 * (255 - alpha)) / 255) as u8);
        }
    }
    Ok(rgb_xobject(rgba.width(), rgba.height(), rgb))
}

/// Rendert die eingefügten Bilder einer Seite. Fehlt eine Bilddatei, erscheint an ihrer
/// Stelle eine graue Fläche, damit der Export nicht abbricht.
fn render_image_layer(
    layer: &printpdf::PdfLayerReference,
    page: &Page,
    assets: Option<&AssetStore>,
) {
    for stroke in &page.strokes {
        let (Some(asset), Some(corners)) = (stroke.image.as_deref(), stroke.image_corners()) else {
            continue;
        };
        let xobject = match assets.map(|a| pdf_image_xobject(a, asset, &corners)) {
            Some(Ok(xobject)) => xobject,
            other => {
                if let Some(Err(err)) = other {
                    eprintln!("[PDF-Export] Bild '{asset}' fehlt oder ist beschädigt: {err}");
                }
                placeholder_xobject()
            }
        };
        draw_image_xobject(layer, xobject, &corners);
    }
}

/// Bettet die von Textfeldern des Notizbuchs verwendeten Schriften ein.
fn embed_text_fonts(
    doc: &PdfDocumentReference,
    notebook: &Notebook,
) -> Result<HashMap<TextFont, printpdf::IndirectFontRef>, Box<dyn Error>> {
    let mut fonts = HashMap::new();
    let used = notebook
        .pages
        .iter()
        .flat_map(|p| &p.strokes)
        .filter_map(|s| s.text.as_ref().map(|t| t.font));
    for font in used {
        if let std::collections::hash_map::Entry::Vacant(entry) = fonts.entry(font) {
            entry.insert(doc.add_external_font(font.bytes())?);
        }
    }
    Ok(fonts)
}

/// Rendert die Textfelder einer Seite als echten Text: Jedes Feld wird in seinem eigenen
/// Koordinatensystem (eine Einheit = eine Seiteneinheit, y nach oben) gesetzt, das eine
/// Transformationsmatrix auf seine Ecken abbildet – so stimmen Drehung und Skalierung.
fn render_text_layer(
    layer: &printpdf::PdfLayerReference,
    page: &Page,
    fonts: &HashMap<TextFont, printpdf::IndirectFontRef>,
) {
    for stroke in &page.strokes {
        let (Some(text), Some(corners)) = (&stroke.text, stroke.object_corners()) else {
            continue;
        };
        let Some(font) = fonts.get(&text.font) else {
            continue;
        };
        let layout = text.layout();
        let size = glam::Vec2::new(text.wrap_width.max(text.size), layout.height());
        let (origin, [right, down]) = crate::text::frame_axes(&corners, size);
        // Seite → PDF-Punkte: x · k, Höhe − y · k
        let k = PT_PER_UNIT;
        let o = page_to_pdf_pt(origin);
        let matrix = [right.x * k, -right.y * k, -down.x * k, down.y * k, o.x, o.y];
        layer.save_graphics_state();
        layer.set_ctm(printpdf::CurTransMat::Raw(matrix));
        layer.set_fill_color(Color::Rgb(Rgb::new(
            stroke.color[0],
            stroke.color[1],
            stroke.color[2],
            None,
        )));
        layer.begin_text_section();
        layer.set_font(font, text.size);
        for (i, line) in layout.lines.iter().enumerate() {
            if line.text.trim().is_empty() {
                continue;
            }
            layer.set_text_matrix(printpdf::TextMatrix::Translate(
                printpdf::Pt(0.0),
                printpdf::Pt(-layout.baseline(i)),
            ));
            // Glyphen samt Unterschneidung wie auf dem Bildschirm (Abstände in 1/1000 Em)
            let face = text.font.face();
            let per_em = 1000.0 / face.units_per_em().unwrap_or(1000.0);
            let mut previous: Option<ab_glyph::GlyphId> = None;
            layer.write_positioned_codepoints(line.glyphs.iter().map(|&(id, _)| {
                let kern = previous.map_or(0.0, |p| face.kern_unscaled(p, id));
                previous = Some(id);
                ((-kern * per_em).round() as i64, id.0)
            }));
        }
        layer.end_text_section();
        layer.restore_graphics_state();
    }
}

/// Graue 1×1-Fläche für fehlende Bilder.
fn placeholder_xobject() -> printpdf::ImageXObject {
    rgb_xobject(1, 1, vec![200, 200, 200])
}

fn rgb_xobject(width: u32, height: u32, rgb: Vec<u8>) -> printpdf::ImageXObject {
    printpdf::ImageXObject {
        width: printpdf::Px(width as usize),
        height: printpdf::Px(height as usize),
        color_space: printpdf::ColorSpace::Rgb,
        bits_per_component: printpdf::ColorBits::Bit8,
        interpolate: true,
        image_data: rgb,
        image_filter: None,
        smask: None,
        clipping_bbox: None,
    }
}

/// Zeichnet ein Bild so, dass es die Ecken oben links, oben rechts, unten rechts,
/// unten links ausfüllt.
fn draw_image_xobject(
    layer: &printpdf::PdfLayerReference,
    xobject: printpdf::ImageXObject,
    corners: &[glam::Vec2; 4],
) {
    let (width, height) = (xobject.width.0 as f32, xobject.height.0 as f32);
    layer.save_graphics_state();
    layer.set_ctm(printpdf::CurTransMat::Raw(image_matrix(corners)));
    // Bei 72 dpi ist ein Pixel ein Punkt; die Skalierung bildet das Bild auf das
    // Einheitsquadrat ab, die Matrix oben auf die Bildecken
    printpdf::Image::from(xobject).add_to_layer(
        layer.clone(),
        printpdf::ImageTransform {
            dpi: Some(72.0),
            scale_x: Some(1.0 / width),
            scale_y: Some(1.0 / height),
            ..Default::default()
        },
    );
    layer.restore_graphics_state();
}

/// Auflösung, mit der PDF-Seiten gerendert werden, die nicht als Vektor übernommen
/// werden können.
const PDF_RASTER_DPI: f32 = 200.0;

/// Bettet die importierte PDF-Seite als gerendertes Bild ein (Ausweichlösung).
fn render_pdf_raster(
    layer: &printpdf::PdfLayerReference,
    page: &Page,
    assets: Option<&AssetStore>,
) {
    let Some(pdf) = &page.pdf_background else {
        return;
    };
    let longest_pt = pdf.width_pt.max(pdf.height_pt);
    let max_pixels = (longest_pt / 72.0 * PDF_RASTER_DPI).ceil() as u32;
    let rendered = assets
        .ok_or_else(|| "kein Asset-Ordner".to_string())
        .and_then(|a| a.path_of(&pdf.asset).map_err(|e| e.to_string()))
        .and_then(|path| crate::pdf_doc::render_file_page(&path, pdf.page as usize, max_pixels));
    let xobject = match rendered {
        Ok(image) => {
            let rgb = image
                .pixels
                .as_chunks::<4>()
                .0
                .iter()
                .flat_map(|px| [px[0], px[1], px[2]])
                .collect();
            rgb_xobject(image.width, image.height, rgb)
        }
        Err(err) => {
            eprintln!(
                "[PDF-Export] PDF-Seite {} nicht darstellbar: {err}",
                pdf.page + 1
            );
            placeholder_xobject()
        }
    };
    draw_image_xobject(layer, xobject, &pdf.corners(page.size));
}

/// Rendert eine mathematisch bereinigte Form (Gerade, Kreis, Rechteck, Koordinatensystem)
/// als verlustfreien Vektorpfad im PDF.
fn render_recognized_shape(
    layer: &printpdf::PdfLayerReference,
    shape: &crate::geometry::RecognizedShape,
    color: [f32; 4],
) {
    use crate::geometry::RecognizedShape;

    let stroke_color = Color::Rgb(Rgb::new(color[0], color[1], color[2], None));
    layer.set_outline_color(stroke_color.clone());
    layer.set_outline_thickness(1.5); // ca. 0.5 mm Standard-Liniendicke
                                      // Wie auf dem Bildschirm: spitze Ecken und stumpfe Enden (der Grafikzustand kann noch
                                      // runde Verbindungen von einem vorherigen Freihandstrich enthalten)
    layer.set_line_cap_style(printpdf::LineCapStyle::Butt);
    layer.set_line_join_style(printpdf::LineJoinStyle::Miter);

    match shape {
        RecognizedShape::Line { start, end } => {
            let (x0, y0) = page_to_pdf_mm(start.x, start.y);
            let (x1, y1) = page_to_pdf_mm(end.x, end.y);
            let line = Line {
                points: vec![
                    (Point::new(Mm(x0), Mm(y0)), false),
                    (Point::new(Mm(x1), Mm(y1)), false),
                ],
                is_closed: false,
            };
            layer.add_line(line);
        }

        RecognizedShape::Circle { center, radius } => {
            let (cx, cy) = page_to_pdf_mm(center.x, center.y);
            let r_mm = radius * (A4_WIDTH_MM / PAGE_WIDTH);
            let segments = 64;
            let mut ring = Vec::with_capacity(segments);
            for i in 0..segments {
                let angle = (i as f32 / segments as f32) * std::f32::consts::TAU;
                let px = cx + angle.cos() * r_mm;
                let py = cy + angle.sin() * r_mm;
                ring.push((Point::new(Mm(px), Mm(py)), false));
            }
            let poly = Polygon {
                rings: vec![ring],
                mode: PaintMode::Stroke,
                winding_order: WindingOrder::NonZero,
            };
            layer.add_polygon(poly);
        }

        RecognizedShape::Ellipse {
            center,
            radius_x,
            radius_y,
            rotation,
        } => {
            let ring = (0..72)
                .map(|i| {
                    let t = i as f32 / 72.0 * std::f32::consts::TAU;
                    let p =
                        crate::geometry::ellipse_point(*center, *radius_x, *radius_y, *rotation, t);
                    let (x, y) = page_to_pdf_mm(p.x, p.y);
                    (Point::new(Mm(x), Mm(y)), false)
                })
                .collect();
            layer.add_polygon(Polygon {
                rings: vec![ring],
                mode: PaintMode::Stroke,
                winding_order: WindingOrder::NonZero,
            });
        }

        RecognizedShape::Arrow { start, end, double } => {
            // Gleiche Spitzen wie auf dem Bildschirm; der Schaft endet an ihrer Basis
            const PDF_SHAPE_HALF_WIDTH: f32 = 2.65;
            let shaft_len = (*end - *start).length();
            let dir = (*end - *start).normalize_or_zero();
            let end_head = crate::geometry::arrow_head(*end, dir, PDF_SHAPE_HALF_WIDTH, shaft_len);
            let start_head =
                crate::geometry::arrow_head(*start, -dir, PDF_SHAPE_HALF_WIDTH, shaft_len);
            let line_start = if *double { start_head.0 } else { *start };
            let to_pdf = |p: glam::Vec2| {
                let (x, y) = page_to_pdf_mm(p.x, p.y);
                (Point::new(Mm(x), Mm(y)), false)
            };
            layer.add_line(Line {
                points: vec![to_pdf(line_start), to_pdf(end_head.0)],
                is_closed: false,
            });
            layer.set_fill_color(stroke_color);
            let mut heads = vec![(*end, end_head)];
            if *double {
                heads.push((*start, start_head));
            }
            for (tip, (_, left, right)) in heads {
                layer.add_polygon(Polygon {
                    rings: vec![vec![to_pdf(tip), to_pdf(left), to_pdf(right)]],
                    mode: PaintMode::Fill,
                    winding_order: WindingOrder::NonZero,
                });
            }
        }

        RecognizedShape::Rectangle { corners } => {
            let mut ring = Vec::with_capacity(4);
            for c in corners {
                let (x, y) = page_to_pdf_mm(c.x, c.y);
                ring.push((Point::new(Mm(x), Mm(y)), false));
            }
            let poly = Polygon {
                rings: vec![ring],
                mode: PaintMode::Stroke,
                winding_order: WindingOrder::NonZero,
            };
            layer.add_polygon(poly);
        }

        RecognizedShape::Triangle { corners } => {
            let mut ring = Vec::with_capacity(3);
            for c in corners {
                let (x, y) = page_to_pdf_mm(c.x, c.y);
                ring.push((Point::new(Mm(x), Mm(y)), false));
            }
            let poly = Polygon {
                rings: vec![ring],
                mode: PaintMode::Stroke,
                winding_order: WindingOrder::NonZero,
            };
            layer.add_polygon(poly);
        }

        RecognizedShape::Rhombus { corners } => {
            let mut ring = Vec::with_capacity(4);
            for c in corners {
                let (x, y) = page_to_pdf_mm(c.x, c.y);
                ring.push((Point::new(Mm(x), Mm(y)), false));
            }
            let poly = Polygon {
                rings: vec![ring],
                mode: PaintMode::Stroke,
                winding_order: WindingOrder::NonZero,
            };
            layer.add_polygon(poly);
        }

        RecognizedShape::Polygon { vertices } => {
            let mut ring = Vec::with_capacity(vertices.len());
            for c in vertices {
                let (x, y) = page_to_pdf_mm(c.x, c.y);
                ring.push((Point::new(Mm(x), Mm(y)), false));
            }
            let poly = Polygon {
                rings: vec![ring],
                mode: PaintMode::Stroke,
                winding_order: WindingOrder::NonZero,
            };
            layer.add_polygon(poly);
        }

        RecognizedShape::Octahedron {
            center,
            radius_x,
            radius_y,
            depth,
        } => {
            let edges = crate::geometry::octahedron_edges(*center, *radius_x, *radius_y, *depth);
            for (p1, p2) in edges {
                let (x1, y1) = page_to_pdf_mm(p1.x, p1.y);
                let (x2, y2) = page_to_pdf_mm(p2.x, p2.y);
                let line = Line {
                    points: vec![
                        (Point::new(Mm(x1), Mm(y1)), false),
                        (Point::new(Mm(x2), Mm(y2)), false),
                    ],
                    is_closed: false,
                };
                layer.add_line(line);
            }
        }

        RecognizedShape::CoordinateSystem {
            origin,
            x_axis_end,
            y_axis_end,
            with_arrows,
        } => {
            // Gleiche Pfeilspitzen wie auf dem Bildschirm; Achsen enden an der Basis der Spitze
            // (Linienstärke 1,5 pt ≈ 5,3 Seiteneinheiten, halbe Breite ≈ 2,65)
            const PDF_SHAPE_HALF_WIDTH: f32 = 2.65;
            let y_dir = (*y_axis_end - *origin).normalize_or_zero();
            let x_dir = (*x_axis_end - *origin).normalize_or_zero();
            let y_arrow = crate::geometry::arrow_head(
                *y_axis_end,
                y_dir,
                PDF_SHAPE_HALF_WIDTH,
                (*y_axis_end - *origin).length(),
            );
            let x_arrow = crate::geometry::arrow_head(
                *x_axis_end,
                x_dir,
                PDF_SHAPE_HALF_WIDTH,
                (*x_axis_end - *origin).length(),
            );
            let (y_line_end, x_line_end) = if *with_arrows {
                (y_arrow.0, x_arrow.0)
            } else {
                (*y_axis_end, *x_axis_end)
            };

            // Ein einziger Linienzug, damit die Ecke am Ursprung als Linienverbindung
            // geschlossen gezeichnet wird statt als zwei stumpfe Enden
            let points = [y_line_end, *origin, x_line_end]
                .iter()
                .map(|p| {
                    let (px, py) = page_to_pdf_mm(p.x, p.y);
                    (Point::new(Mm(px), Mm(py)), false)
                })
                .collect();
            layer.add_line(Line {
                points,
                is_closed: false,
            });

            // Pfeilspitzen als gefüllte Dreiecke
            if *with_arrows {
                layer.set_fill_color(stroke_color);
                for (tip, (_, left, right)) in [(*y_axis_end, y_arrow), (*x_axis_end, x_arrow)] {
                    let ring = [tip, left, right]
                        .iter()
                        .map(|p| {
                            let (px, py) = page_to_pdf_mm(p.x, p.y);
                            (Point::new(Mm(px), Mm(py)), false)
                        })
                        .collect();
                    layer.add_polygon(Polygon {
                        rings: vec![ring],
                        mode: PaintMode::Fill,
                        winding_order: WindingOrder::NonZero,
                    });
                }
            }
        }
    }
}

/// Strategie A: Gefülltes Polygon aus den Triangle-Strip-Hüllpunkten.
fn render_stroke_polygon(layer: &printpdf::PdfLayerReference, stroke: &Stroke) {
    if stroke.vertices.is_empty() {
        return;
    }

    let n = stroke.vertices.len() / 2;
    if n < 2 {
        // Einzelner Klickpunkt (Dot): Kreisförmiges Polygon rendern
        if let Some(first) = stroke.points.first() {
            let (cx, cy) = page_to_pdf_mm(first.position.x, first.position.y);
            let r_mm = (first.pressure * 0.75 + 0.25).max(0.2); // ca. 0.25 bis 1.0 mm

            let segments = 12;
            let mut dot_pts = Vec::with_capacity(segments);
            for i in 0..segments {
                let angle = (i as f32 / segments as f32) * std::f32::consts::TAU;
                let px = cx + angle.cos() * r_mm;
                let py = cy + angle.sin() * r_mm;
                dot_pts.push((Point::new(Mm(px), Mm(py)), false));
            }

            let fill_color = Color::Rgb(Rgb::new(
                stroke.color[0],
                stroke.color[1],
                stroke.color[2],
                None,
            ));
            layer.set_fill_color(fill_color);

            let poly = Polygon {
                rings: vec![dot_pts],
                mode: PaintMode::Fill,
                winding_order: WindingOrder::NonZero,
            };
            layer.add_polygon(poly);
        }
        return;
    }

    // Geschlossener Pfad um den Ribbon-Rand:
    // Left[0] -> Left[1] -> ... -> Left[n-1] -> Right[n-1] -> Right[n-2] -> ... -> Right[0]
    let mut ring = Vec::with_capacity(n * 2);

    // 1. Linke Hüllkurve vorwärts
    for i in 0..n {
        let v = stroke.vertices[i * 2];
        let (x, y) = page_to_pdf_mm(v.position[0], v.position[1]);
        ring.push((Point::new(Mm(x), Mm(y)), false));
    }

    // 2. Rechte Hüllkurve rückwärts
    for i in (0..n).rev() {
        let v = stroke.vertices[i * 2 + 1];
        let (x, y) = page_to_pdf_mm(v.position[0], v.position[1]);
        ring.push((Point::new(Mm(x), Mm(y)), false));
    }

    let fill_color = Color::Rgb(Rgb::new(
        stroke.color[0],
        stroke.color[1],
        stroke.color[2],
        None,
    ));
    layer.set_fill_color(fill_color);

    let poly = Polygon {
        rings: vec![ring],
        mode: PaintMode::Fill,
        winding_order: WindingOrder::NonZero,
    };
    layer.add_polygon(poly);
}

/// Strategie B: Kubische Bézier-Kurven aus den Abtastpunkten via printpdf::Line.
fn render_stroke_bezier(layer: &printpdf::PdfLayerReference, stroke: &Stroke) {
    if stroke.points.is_empty() {
        return;
    }

    let outline_color = Color::Rgb(Rgb::new(
        stroke.color[0],
        stroke.color[1],
        stroke.color[2],
        None,
    ));
    layer.set_outline_color(outline_color);
    layer.set_line_cap_style(printpdf::LineCapStyle::Round);
    layer.set_line_join_style(printpdf::LineJoinStyle::Round);

    // Durchschnittliche Linienstärke aus dem Druck berechnen
    let avg_pressure =
        stroke.points.iter().map(|p| p.pressure).sum::<f32>() / stroke.points.len() as f32;
    let radius_mm = (avg_pressure * 0.75 + 0.25).max(0.25);
    let thickness_pt = radius_mm * 2.0 * (72.0 / 25.4); // mm in DTP-Punkte
    layer.set_outline_thickness(thickness_pt);

    if stroke.points.len() == 1 {
        // Einpunktiger Stroke
        let (x, y) = page_to_pdf_mm(stroke.points[0].position.x, stroke.points[0].position.y);
        let line = Line {
            points: vec![
                (Point::new(Mm(x), Mm(y)), false),
                (Point::new(Mm(x + 0.05), Mm(y)), false),
            ],
            is_closed: false,
        };
        layer.add_line(line);
        return;
    }

    let n = stroke.points.len();
    let mut line_points = Vec::new();

    // Catmull-Rom zu kubischen Bézier-Segmenten konvertieren
    for i in 0..n - 1 {
        let p_curr = stroke.points[i].position;
        let p_next = stroke.points[i + 1].position;

        let p_prev = if i == 0 {
            2.0 * p_curr - p_next
        } else {
            stroke.points[i - 1].position
        };

        let p_after = if i + 2 < n {
            stroke.points[i + 2].position
        } else {
            2.0 * p_next - p_curr
        };

        // Knotensequenz mit zentripetalem Catmull-Rom (alpha = 0.5)
        let d01 = (p_curr - p_prev).length();
        let d12 = (p_next - p_curr).length();
        let d23 = (p_after - p_next).length();

        let dt01 = if d01 > 1e-6 { d01.sqrt() } else { 1e-4 };
        let dt12 = if d12 > 1e-6 { d12.sqrt() } else { 1e-4 };
        let dt23 = if d23 > 1e-6 { d23.sqrt() } else { 1e-4 };

        // Geschwindigkeitsvektoren (Tangenten an P1 und P2)
        let v1 = (p_next - p_prev) / (dt01 + dt12);
        let v2 = (p_after - p_curr) / (dt12 + dt23);

        // Kubische Bézier-Kontrollpunkte B1 und B2
        let b1 = p_curr + (dt12 / 3.0) * v1;
        let b2 = p_next - (dt12 / 3.0) * v2;

        let (b0_x, b0_y) = page_to_pdf_mm(p_curr.x, p_curr.y);
        let (b1_x, b1_y) = page_to_pdf_mm(b1.x, b1.y);
        let (b2_x, b2_y) = page_to_pdf_mm(b2.x, b2.y);
        let (b3_x, b3_y) = page_to_pdf_mm(p_next.x, p_next.y);

        let pt0 = Point::new(Mm(b0_x), Mm(b0_y));
        let pt1 = Point::new(Mm(b1_x), Mm(b1_y));
        let pt2 = Point::new(Mm(b2_x), Mm(b2_y));
        let pt3 = Point::new(Mm(b3_x), Mm(b3_y));

        if i == 0 {
            line_points.push((pt0, true));
        }

        line_points.push((pt1, true));
        line_points.push((pt2, false));

        // Für das nächste Segment als Startpunkt mit true markieren, am Ende mit false
        let is_last_segment = i == n - 2;
        line_points.push((pt3, !is_last_segment));
    }

    if !line_points.is_empty() {
        let line = Line {
            points: line_points,
            is_closed: false,
        };
        layer.add_line(line);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::InkVertex;
    use crate::document::{StylusSample, PAGE_HEIGHT, PAGE_WIDTH};

    #[test]
    fn test_page_to_pdf_mm_mapping() {
        // Oben-Links (0, 0) -> PDF (0.0, 297.0)
        let (x0, y0) = page_to_pdf_mm(0.0, 0.0);
        assert!((x0 - 0.0).abs() < 1e-4);
        assert!((y0 - A4_HEIGHT_MM).abs() < 1e-4);

        // Unten-Rechts (PAGE_WIDTH, PAGE_HEIGHT) -> PDF (210.0, 0.0)
        let (x1, y1) = page_to_pdf_mm(PAGE_WIDTH, PAGE_HEIGHT);
        assert!((x1 - A4_WIDTH_MM).abs() < 1e-4);
        assert!((y1 - 0.0).abs() < 1e-4);

        // Mitte (PAGE_WIDTH / 2, PAGE_HEIGHT / 2) -> PDF (105.0, 148.5)
        let (xm, ym) = page_to_pdf_mm(PAGE_WIDTH / 2.0, PAGE_HEIGHT / 2.0);
        assert!((xm - 105.0).abs() < 1e-4);
        assert!((ym - 148.5).abs() < 1e-4);
    }

    #[test]
    fn test_pdf_export_generation() {
        // 1. Mock-Notebook mit 2 Seiten und unterschiedlichen Hintergründen
        let mut notebook = Notebook::new();
        notebook.pages[0].background = PageBackground::Grid5mm;

        // Stroke auf Seite 1 hinzufügen
        let stroke1 = Stroke::new(
            1,
            vec![
                StylusSample {
                    position: glam::Vec2::new(100.0, 200.0),
                    pressure: 0.5,
                    timestamp: 0.0,
                },
                StylusSample {
                    position: glam::Vec2::new(300.0, 400.0),
                    pressure: 0.8,
                    timestamp: 0.01,
                },
                StylusSample {
                    position: glam::Vec2::new(500.0, 300.0),
                    pressure: 0.6,
                    timestamp: 0.02,
                },
            ],
            vec![
                InkVertex::new(95.0, 200.0, [0.1, 0.1, 0.15, 1.0]),
                InkVertex::new(105.0, 200.0, [0.1, 0.1, 0.15, 1.0]),
                InkVertex::new(290.0, 400.0, [0.1, 0.1, 0.15, 1.0]),
                InkVertex::new(310.0, 400.0, [0.1, 0.1, 0.15, 1.0]),
                InkVertex::new(495.0, 300.0, [0.1, 0.1, 0.15, 1.0]),
                InkVertex::new(505.0, 300.0, [0.1, 0.1, 0.15, 1.0]),
            ],
            [0.1, 0.1, 0.15, 1.0],
        );
        notebook.pages[0].add_stroke(stroke1);

        // Seite 2 mit Liniertem Hintergrund
        let page2_idx = notebook.add_page(PageBackground::Lined);
        let stroke2 = Stroke::new(
            2,
            vec![
                StylusSample {
                    position: glam::Vec2::new(200.0, 500.0),
                    pressure: 0.5,
                    timestamp: 0.0,
                },
                StylusSample {
                    position: glam::Vec2::new(600.0, 500.0),
                    pressure: 0.5,
                    timestamp: 0.01,
                },
            ],
            vec![
                InkVertex::new(200.0, 498.0, [0.85, 0.2, 0.2, 1.0]),
                InkVertex::new(200.0, 502.0, [0.85, 0.2, 0.2, 1.0]),
                InkVertex::new(600.0, 498.0, [0.85, 0.2, 0.2, 1.0]),
                InkVertex::new(600.0, 502.0, [0.85, 0.2, 0.2, 1.0]),
            ],
            [0.85, 0.2, 0.2, 1.0],
        );
        notebook.pages[page2_idx].add_stroke(stroke2);

        // 2. Export als Datei testen (Strategie A: FilledPolygon)
        let temp_dir = std::env::temp_dir();
        let pdf_path = temp_dir.join("ink_notes_test_export.pdf");

        let export_res = export_notebook_to_pdf(
            &notebook,
            &pdf_path,
            ExportStrategy::FilledPolygon,
            None,
            PatternColor::default(),
        );
        assert!(
            export_res.is_ok(),
            "PDF-Export zu Datei muss erfolgreich sein"
        );

        // Datei-Existenz und Größe prüfen
        assert!(
            pdf_path.exists(),
            "Die generierte PDF-Datei muss existieren"
        );
        let metadata = std::fs::metadata(&pdf_path).unwrap();
        assert!(
            metadata.len() > 1024,
            "Dateigröße ({}) muss größer als 1 KB sein",
            metadata.len()
        );

        // PDF-Header verifizieren
        let content = std::fs::read(&pdf_path).unwrap();
        assert!(
            content.starts_with(b"%PDF-1."),
            "Datei muss mit gültigem PDF-Header (%PDF-1.) beginnen"
        );

        // 3. Export als Bytes testen (Strategie B: BezierCurve)
        let bytes_res = export_notebook_to_bytes(
            &notebook,
            ExportStrategy::BezierCurve,
            None,
            PatternColor::default(),
        );
        assert!(bytes_res.is_ok());
        let bytes = bytes_res.unwrap();
        assert!(bytes.len() > 1024);
        assert!(bytes.starts_with(b"%PDF-1."));

        // Aufräumen
        let _ = std::fs::remove_file(pdf_path);
    }

    #[test]
    fn test_pdf_export_recognized_shapes() {
        let mut notebook = Notebook::new();
        let page = notebook.current_page_mut();

        let line_stroke = Stroke::new(1, vec![], vec![], [0.0, 0.0, 0.0, 1.0]).with_shape(
            crate::geometry::RecognizedShape::Line {
                start: glam::Vec2::new(100.0, 100.0),
                end: glam::Vec2::new(500.0, 500.0),
            },
        );
        page.add_stroke(line_stroke);

        let circle_stroke = Stroke::new(2, vec![], vec![], [0.1, 0.4, 0.9, 1.0]).with_shape(
            crate::geometry::RecognizedShape::Circle {
                center: glam::Vec2::new(1000.0, 1000.0),
                radius: 200.0,
            },
        );
        page.add_stroke(circle_stroke);

        let rect_stroke = Stroke::new(3, vec![], vec![], [0.2, 0.7, 0.3, 1.0]).with_shape(
            crate::geometry::RecognizedShape::Rectangle {
                corners: [
                    glam::Vec2::new(200.0, 200.0),
                    glam::Vec2::new(600.0, 200.0),
                    glam::Vec2::new(600.0, 400.0),
                    glam::Vec2::new(200.0, 400.0),
                ],
            },
        );
        page.add_stroke(rect_stroke);

        let coords_stroke = Stroke::new(4, vec![], vec![], [0.8, 0.1, 0.1, 1.0]).with_shape(
            crate::geometry::RecognizedShape::CoordinateSystem {
                origin: glam::Vec2::new(300.0, 1500.0),
                x_axis_end: glam::Vec2::new(800.0, 1500.0),
                y_axis_end: glam::Vec2::new(300.0, 1000.0),
                with_arrows: true,
            },
        );
        page.add_stroke(coords_stroke);

        let ellipse_stroke = Stroke::new(40, vec![], vec![], [0.1, 0.5, 0.5, 1.0]).with_shape(
            crate::geometry::RecognizedShape::Ellipse {
                center: glam::Vec2::new(1400.0, 600.0),
                radius_x: 300.0,
                radius_y: 150.0,
                rotation: 0.4,
            },
        );
        page.add_stroke(ellipse_stroke);

        for (id, double) in [(41, false), (42, true)] {
            let arrow_stroke = Stroke::new(id, vec![], vec![], [0.2, 0.2, 0.2, 1.0]).with_shape(
                crate::geometry::RecognizedShape::Arrow {
                    start: glam::Vec2::new(1200.0, 1800.0 + id as f32 * 40.0),
                    end: glam::Vec2::new(1800.0, 1800.0 + id as f32 * 40.0),
                    double,
                },
            );
            page.add_stroke(arrow_stroke);
        }

        let tri_stroke = Stroke::new(5, vec![], vec![], [0.6, 0.2, 0.7, 1.0]).with_shape(
            crate::geometry::RecognizedShape::Triangle {
                corners: [
                    glam::Vec2::new(100.0, 500.0),
                    glam::Vec2::new(400.0, 500.0),
                    glam::Vec2::new(250.0, 300.0),
                ],
            },
        );
        page.add_stroke(tri_stroke);

        let rhombus_stroke = Stroke::new(6, vec![], vec![], [0.9, 0.5, 0.1, 1.0]).with_shape(
            crate::geometry::RecognizedShape::Rhombus {
                corners: [
                    glam::Vec2::new(500.0, 700.0),
                    glam::Vec2::new(700.0, 850.0),
                    glam::Vec2::new(500.0, 1000.0),
                    glam::Vec2::new(300.0, 850.0),
                ],
            },
        );
        page.add_stroke(rhombus_stroke);

        let octagon_stroke = Stroke::new(7, vec![], vec![], [0.1, 0.6, 0.6, 1.0]).with_shape(
            crate::geometry::RecognizedShape::Polygon {
                vertices: vec![
                    glam::Vec2::new(1200.0, 600.0),
                    glam::Vec2::new(1350.0, 650.0),
                    glam::Vec2::new(1400.0, 800.0),
                    glam::Vec2::new(1350.0, 950.0),
                    glam::Vec2::new(1200.0, 1000.0),
                    glam::Vec2::new(1050.0, 950.0),
                    glam::Vec2::new(1000.0, 800.0),
                    glam::Vec2::new(1050.0, 650.0),
                ],
            },
        );
        page.add_stroke(octagon_stroke);

        let octahedron_stroke = Stroke::new(8, vec![], vec![], [0.3, 0.3, 0.8, 1.0]).with_shape(
            crate::geometry::RecognizedShape::Octahedron {
                center: glam::Vec2::new(800.0, 1500.0),
                radius_x: 200.0,
                radius_y: 250.0,
                depth: 70.0,
            },
        );
        page.add_stroke(octahedron_stroke);

        let bytes_res = export_notebook_to_bytes(
            &notebook,
            ExportStrategy::FilledPolygon,
            None,
            PatternColor::default(),
        );
        assert!(bytes_res.is_ok(), "Shape export must succeed");
        let bytes = bytes_res.unwrap();
        assert!(bytes.len() > 1024);
        assert!(bytes.starts_with(b"%PDF-1."));
    }

    #[test]
    fn test_image_matrix_maps_unit_square_to_corners() {
        // Bild über die volle Seitenbreite, oberes Viertel der Seite
        let corners = [
            glam::Vec2::new(0.0, 0.0),
            glam::Vec2::new(PAGE_WIDTH, 0.0),
            glam::Vec2::new(PAGE_WIDTH, PAGE_HEIGHT / 4.0),
            glam::Vec2::new(0.0, PAGE_HEIGHT / 4.0),
        ];
        let [a, b, c, d, e, f] = image_matrix(&corners);
        let page_w_pt = A4_WIDTH_MM * 72.0 / 25.4;
        let page_h_pt = A4_HEIGHT_MM * 72.0 / 25.4;
        let map = |u: f32, v: f32| (a * u + c * v + e, b * u + d * v + f);
        let close = |(x, y): (f32, f32), (ex, ey): (f32, f32)| {
            (x - ex).abs() < 0.01 && (y - ey).abs() < 0.01
        };
        // Unten links des Bildes, oben rechts des Bildes (PDF-Ursprung unten links)
        assert!(close(map(0.0, 0.0), (0.0, page_h_pt * 0.75)));
        assert!(close(map(1.0, 1.0), (page_w_pt, page_h_pt)));
        // Gedrehtes Bild: Achsen folgen den Ecken
        let rotated = [
            glam::Vec2::new(500.0, 500.0),
            glam::Vec2::new(500.0, 900.0),
            glam::Vec2::new(300.0, 900.0),
            glam::Vec2::new(300.0, 500.0),
        ];
        let m = image_matrix(&rotated);
        assert!(
            m[0].abs() < 1e-3 && m[1] < 0.0,
            "x-Achse zeigt nach unten: {m:?}"
        );
    }

    #[test]
    fn test_pdf_export_embeds_images() {
        let dir = tempfile::tempdir().unwrap();
        let assets = AssetStore::new(dir.path());
        let jpg = crate::assets::tests::sample_image_bytes(64, 48, image::ImageFormat::Jpeg);
        let png = crate::assets::tests::sample_image_bytes(30, 20, image::ImageFormat::Png);
        let jpg_asset = assets.import_bytes(&jpg).unwrap().asset;
        let png_asset = assets.import_bytes(&png).unwrap().asset;

        let mut notebook = Notebook::new();
        let page = notebook.current_page_mut();
        for (id, asset) in [
            (1, jpg_asset),
            (2, png_asset),
            (3, format!("{}.png", "0".repeat(64))),
        ] {
            let y = id as f32 * 400.0;
            page.strokes.push(Stroke::new_image(
                id,
                asset,
                [
                    glam::Vec2::new(100.0, y),
                    glam::Vec2::new(700.0, y),
                    glam::Vec2::new(700.0, y + 300.0),
                    glam::Vec2::new(100.0, y + 300.0),
                ],
            ));
        }

        let bytes = export_notebook_to_bytes(
            &notebook,
            ExportStrategy::FilledPolygon,
            Some(&assets),
            PatternColor::default(),
        )
        .expect("Export mit Bildern");
        let contains = |needle: &[u8]| bytes.windows(needle.len()).any(|w| w == needle);
        assert!(
            contains(b"/DCTDecode"),
            "JPEG wird nicht unverändert eingebettet"
        );
        // JPEG-Daten liegen byte-identisch im PDF
        assert!(contains(&jpg[..64]));
        assert!(
            contains(b"/Images") || contains(b"(Images)"),
            "Bild-Ebene fehlt"
        );
        let image_objects = bytes
            .windows(b"/Subtype/Image".len())
            .filter(|w| *w == b"/Subtype/Image")
            .count()
            + bytes
                .windows(b"/Subtype /Image".len())
                .filter(|w| *w == b"/Subtype /Image")
                .count();
        // JPEG, PNG und Ersatzfläche für die fehlende Datei
        assert_eq!(image_objects, 3);

        // Ohne Asset-Ordner: Export klappt trotzdem (graue Flächen)
        assert!(export_notebook_to_bytes(
            &notebook,
            ExportStrategy::FilledPolygon,
            None,
            PatternColor::default()
        )
        .is_ok());
    }

    #[test]
    fn test_pdf_export_underlays_imported_pages() {
        let temp_dir = tempfile::tempdir().unwrap();
        let source = crate::pdf_doc::tests::sample_pdf(&[(842.0, 595.0, 0), (595.0, 842.0, 90)]);
        let (mut notebook, _, dir) =
            crate::storage::create_notebook_from_pdf(temp_dir.path(), "Folien", source).unwrap();
        crate::storage::load_all_pages(&dir, &mut notebook).unwrap();
        let assets = AssetStore::new(&dir);

        let bytes = export_notebook_to_bytes(
            &notebook,
            ExportStrategy::FilledPolygon,
            Some(&assets),
            PatternColor::default(),
        )
        .unwrap();
        let doc = printpdf::lopdf::Document::load_mem(&bytes).unwrap();
        assert_eq!(doc.get_pages().len(), 2);
        for page_id in doc.get_pages().into_values() {
            let content = doc.get_page_content(page_id).unwrap();
            assert!(
                String::from_utf8_lossy(&content).starts_with("q "),
                "Originalseite fehlt als Unterlage"
            );
        }

        // Exportiertes PDF lässt sich wieder rendern: Querformat-Folie füllt ihre Seite, rotes
        // Quadrat oben links
        let pdf = hayro::hayro_syntax::Pdf::new(bytes).unwrap();
        let image = crate::pdf_doc::render_page(&pdf, 0, 600).unwrap();
        let px = |x: u32, y: u32| {
            let i = ((y * image.width + x) * 4) as usize;
            image.pixels[i..i + 3].to_vec()
        };
        assert_eq!(px(5, 5), vec![255, 0, 0], "Quadrat der Folie oben links");
        let ratio = image.width as f32 / image.height as f32;
        assert!(
            (ratio - 842.0 / 595.0).abs() < 0.01,
            "Seite im Folienformat: {ratio}"
        );

        // Gedrehte Seite (/Rotate 90) erscheint wie im PDF-Betrachter: das Quadrat liegt
        // dort oben rechts, in der Quelle wie im Export
        let source_pdf = hayro::hayro_syntax::Pdf::new(
            assets
                .read_bytes(&notebook.pages[1].pdf_background.as_ref().unwrap().asset)
                .unwrap(),
        )
        .unwrap();
        let shown = crate::pdf_doc::render_page(&source_pdf, 1, 600).unwrap();
        let red_at = |img: &crate::assets::RgbaImage, x: u32, y: u32| {
            let i = ((y * img.width + x) * 4) as usize;
            img.pixels[i..i + 3] == [255, 0, 0]
        };
        assert!(red_at(&shown, shown.width - 5, 5));
        let exported = crate::pdf_doc::render_page(&pdf, 1, 600).unwrap();
        assert!(red_at(&exported, exported.width - 5, 5));
        assert!(!red_at(&exported, 5, 5));

        // Ohne Zugriff auf das PDF: Export klappt trotzdem (graue Ersatzfläche)
        assert!(export_notebook_to_bytes(
            &notebook,
            ExportStrategy::FilledPolygon,
            None,
            PatternColor::default()
        )
        .is_ok());
    }

    #[test]
    fn test_page_titles_become_pdf_bookmarks() {
        let mut notebook = Notebook::new();
        notebook.add_page(PageBackground::Blank);
        notebook.add_page(PageBackground::Blank);
        let ids: Vec<u64> = notebook.pages.iter().map(|p| p.id).collect();
        notebook.set_page_title(ids[0], "Einleitung");
        notebook.set_page_title(ids[2], "Übung 2 – Integrale");
        let bytes = export_notebook_to_bytes(
            &notebook,
            ExportStrategy::FilledPolygon,
            None,
            PatternColor::default(),
        )
        .unwrap();
        let doc = printpdf::lopdf::Document::load_mem(&bytes).unwrap();
        let catalog = doc.catalog().unwrap();
        assert!(catalog.has(b"Outlines"), "Lesezeichen fehlen");

        // Titel als PDF-Textstrings: ASCII unverändert, Umlaute als UTF-16BE mit BOM
        let titles: Vec<String> = doc
            .objects
            .values()
            .filter_map(|o| o.as_dict().ok())
            .filter(|d| d.has(b"Parent"))
            .filter_map(|d| match d.get(b"Title") {
                Ok(printpdf::lopdf::Object::String(raw, _)) => {
                    Some(if raw.starts_with(&[0xFE, 0xFF]) {
                        let units: Vec<u16> = raw[2..]
                            .chunks(2)
                            .map(|c| u16::from_be_bytes([c[0], c[1]]))
                            .collect();
                        String::from_utf16(&units).unwrap()
                    } else {
                        String::from_utf8(raw.clone()).unwrap()
                    })
                }
                _ => None,
            })
            .collect();
        assert!(titles.contains(&"Einleitung".to_string()), "{titles:?}");
        assert!(
            titles.contains(&"Übung 2 – Integrale".to_string()),
            "{titles:?}"
        );
    }

    #[test]
    fn test_jpeg_component_count() {
        let color = crate::assets::tests::sample_image_bytes(8, 8, image::ImageFormat::Jpeg);
        assert_eq!(jpeg_components(&color), Some(3));
        let gray = {
            let img = image::GrayImage::from_pixel(8, 8, image::Luma([90]));
            let mut out = Vec::new();
            image::DynamicImage::ImageLuma8(img)
                .write_to(
                    &mut std::io::Cursor::new(&mut out),
                    image::ImageFormat::Jpeg,
                )
                .unwrap();
            out
        };
        assert_eq!(jpeg_components(&gray), Some(1));
        assert_eq!(jpeg_components(b"\xFF\xD8kaputt"), None);
    }

    #[test]
    fn test_sanitize_filename_component() {
        assert_eq!(sanitize_filename_component("Notizbuch 1"), "Notizbuch 1");
        assert_eq!(
            sanitize_filename_component("Projekt/2026: Entwurf"),
            "Projekt_2026_ Entwurf"
        );
        assert_eq!(sanitize_filename_component("   "), "Notizbuch");
        assert_eq!(sanitize_filename_component(""), "Notizbuch");
        assert_eq!(
            sanitize_filename_component("Mathe * Vorlesung?"),
            "Mathe _ Vorlesung_"
        );
    }

    #[test]
    fn test_resolve_pdf_export_path_collision_avoidance() {
        let temp_dir = tempfile::tempdir().expect("Konnte tempdir nicht erstellen");
        let dir = temp_dir.path();

        // 1. Erstmaliger Export: keine Kollision
        let path1 = resolve_pdf_export_path_in_dir(dir, "Mathe Vorlesung");
        assert_eq!(path1, dir.join("Mathe Vorlesung.pdf"));

        // Datei anlegen
        std::fs::write(&path1, b"dummy pdf 1").expect("Konnte Datei nicht schreiben");

        // 2. Zweiter Export: Kollision -> _1
        let path2 = resolve_pdf_export_path_in_dir(dir, "Mathe Vorlesung");
        assert_eq!(path2, dir.join("Mathe Vorlesung_1.pdf"));

        // Zweite Datei anlegen
        std::fs::write(&path2, b"dummy pdf 2").expect("Konnte Datei nicht schreiben");

        // 3. Dritter Export: Kollision -> _2
        let path3 = resolve_pdf_export_path_in_dir(dir, "Mathe Vorlesung");
        assert_eq!(path3, dir.join("Mathe Vorlesung_2.pdf"));
    }
}
