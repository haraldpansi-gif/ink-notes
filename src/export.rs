//! Verlustfreier Vektor-PDF-Export des gesamten Notizbuchs via printpdf.
//!
//! Features:
//! - Hochformat DIN A4 (210 mm x 297 mm)
//! - 2 Layer pro Seite: Layer 1 ("Background") und Layer 2 ("Ink")
//! - Y-Achsen-Spiegelung (PDF-Ursprung unten links vs. Bildschirmursprung oben links)
//! - Vektor-Hintergründe (Kariert 5mm, Liniert, Punktraster, Blanko)
//! - Zwei Export-Strategien:
//!   - `FilledPolygon`: Glatte gefüllte Polygone aus den Triangle-Strip-Hüllen (variable Pinselstärke)
//!   - `BezierCurve`: Glatte kubische Bézier-Kurven via `printpdf::Line`

use std::error::Error;
use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};

use printpdf::path::{PaintMode, WindingOrder};
use printpdf::{Color, Line, Mm, PdfDocument, PdfDocumentReference, Point, Polygon, Rgb};

use crate::background::{GRID_5MM_SPACING, LINED_SPACING};
use crate::document::{
    Notebook, Page, PageBackground, Stroke, A4_HEIGHT_MM, A4_WIDTH_MM, PAGE_HEIGHT, PAGE_WIDTH,
};

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
#[inline]
pub fn page_to_pdf_mm(x: f32, y: f32) -> (f32, f32) {
    let x_norm = (x / PAGE_WIDTH).clamp(0.0, 1.0);
    let y_norm = (y / PAGE_HEIGHT).clamp(0.0, 1.0);

    let x_mm = x_norm * A4_WIDTH_MM;
    let y_mm = (1.0 - y_norm) * A4_HEIGHT_MM; // PDF-Ursprung liegt unten links

    (x_mm, y_mm)
}

/// Erzeugt ein PDF-Dokument aus einem Notizbuch und speichert es als Datei.
pub fn export_notebook_to_pdf<P: AsRef<Path>>(
    notebook: &Notebook,
    path: P,
    strategy: ExportStrategy,
) -> Result<(), Box<dyn Error>> {
    let file = File::create(path)?;
    let mut writer = BufWriter::new(file);
    let doc = generate_pdf_document(notebook, strategy)?;
    doc.save(&mut writer)?;
    Ok(())
}

/// Erzeugt ein PDF-Dokument aus einem Notizbuch und gibt die rohen Bytes zurück.
pub fn export_notebook_to_bytes(
    notebook: &Notebook,
    strategy: ExportStrategy,
) -> Result<Vec<u8>, Box<dyn Error>> {
    let doc = generate_pdf_document(notebook, strategy)?;
    let bytes = doc.save_to_bytes()?;
    Ok(bytes)
}

/// Generiert das `printpdf::PdfDocumentReference` für alle Seiten des Notizbuchs.
pub fn generate_pdf_document(
    notebook: &Notebook,
    strategy: ExportStrategy,
) -> Result<PdfDocumentReference, Box<dyn Error>> {
    if notebook.pages.is_empty() {
        return Err("Notizbuch enthält keine Seiten".into());
    }
    // Nicht geladene Seiten würden sonst stillschweigend als leere Seiten exportiert
    if let Some(page) = notebook
        .pages
        .iter()
        .find(|p| !p.is_loaded || p.load_failed)
    {
        return Err(format!(
            "Seite {} ist nicht geladen – Export abgebrochen (vorher storage::load_all_pages aufrufen)",
            page.id
        )
        .into());
    }

    // 1. Erstes Dokument mit Seite 1 erstellen
    let title = "ink-notes Notebook";
    let (doc, page1_idx, layer1_bg) =
        PdfDocument::new(title, Mm(A4_WIDTH_MM), Mm(A4_HEIGHT_MM), "Background");

    // Seite 1 befüllen
    let page1_ref = doc.get_page(page1_idx);
    let layer1_bg_ref = page1_ref.get_layer(layer1_bg);
    render_background_layer(&layer1_bg_ref, notebook.pages[0].background);

    let layer1_ink_ref = page1_ref.add_layer("Ink");
    render_ink_layer(&layer1_ink_ref, &notebook.pages[0], strategy);

    // 2. Alle nachfolgenden Seiten hinzufügen
    for page in &notebook.pages[1..] {
        let (page_idx, layer_bg) = doc.add_page(Mm(A4_WIDTH_MM), Mm(A4_HEIGHT_MM), "Background");
        let page_ref = doc.get_page(page_idx);
        let layer_bg_ref = page_ref.get_layer(layer_bg);
        render_background_layer(&layer_bg_ref, page.background);

        let layer_ink_ref = page_ref.add_layer("Ink");
        render_ink_layer(&layer_ink_ref, page, strategy);
    }

    Ok(doc)
}

/// Rendert das Vektorraster auf Layer 1 ("Background").
fn render_background_layer(layer: &printpdf::PdfLayerReference, background: PageBackground) {
    let grid_color = Color::Rgb(Rgb::new(0.88, 0.88, 0.88, None));
    layer.set_outline_color(grid_color.clone());
    layer.set_outline_thickness(0.4); // Feine 0.15 mm Haarlinie (ca. 0.4 pt)
    layer.set_fill_color(grid_color);

    match background {
        PageBackground::Blank => {}

        PageBackground::Grid5mm => {
            let spacing_mm = GRID_5MM_SPACING / 10.0; // 5.0 mm

            // Vertikale Linien alle 5 mm
            let mut x = spacing_mm;
            while x < A4_WIDTH_MM {
                let line = Line {
                    points: vec![
                        (Point::new(Mm(x), Mm(0.0)), false),
                        (Point::new(Mm(x), Mm(A4_HEIGHT_MM)), false),
                    ],
                    is_closed: false,
                };
                layer.add_line(line);
                x += spacing_mm;
            }

            // Horizontale Linien alle 5 mm
            let mut y = spacing_mm;
            while y < A4_HEIGHT_MM {
                let line = Line {
                    points: vec![
                        (Point::new(Mm(0.0), Mm(y)), false),
                        (Point::new(Mm(A4_WIDTH_MM), Mm(y)), false),
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
            while y <= A4_HEIGHT_MM - margin_top_mm {
                let line = Line {
                    points: vec![
                        (Point::new(Mm(0.0), Mm(y)), false),
                        (Point::new(Mm(A4_WIDTH_MM), Mm(y)), false),
                    ],
                    is_closed: false,
                };
                layer.add_line(line);
                y += spacing_mm;
            }
        }

        PageBackground::DotGrid => {
            let spacing_mm = GRID_5MM_SPACING / 10.0; // 5.0 mm
            let half_dot = 0.2; // 0.4 mm Kantenlänge

            let mut y = spacing_mm;
            while y < A4_HEIGHT_MM {
                let mut x = spacing_mm;
                while x < A4_WIDTH_MM {
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
    }
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
            let (ox, oy) = page_to_pdf_mm(origin.x, origin.y);
            let (xx, xy) = page_to_pdf_mm(x_axis_end.x, x_axis_end.y);
            let (yx, yy) = page_to_pdf_mm(y_axis_end.x, y_axis_end.y);

            // Achse 1 (Y)
            let line_y = Line {
                points: vec![
                    (Point::new(Mm(ox), Mm(oy)), false),
                    (Point::new(Mm(yx), Mm(yy)), false),
                ],
                is_closed: false,
            };
            layer.add_line(line_y);

            // Achse 2 (X)
            let line_x = Line {
                points: vec![
                    (Point::new(Mm(ox), Mm(oy)), false),
                    (Point::new(Mm(xx), Mm(xy)), false),
                ],
                is_closed: false,
            };
            layer.add_line(line_x);

            // Pfeilspitzen als gefüllte Dreiecke
            if *with_arrows {
                layer.set_fill_color(stroke_color);

                let y_dir = (*y_axis_end - *origin).normalize_or_zero();
                let arrow_len_page = 25.0;
                let arrow_w_page = 16.0;

                let base_y = *y_axis_end - y_dir * arrow_len_page;
                let left_y = base_y + glam::Vec2::new(-y_dir.y, y_dir.x) * (arrow_w_page * 0.5);
                let right_y = base_y - glam::Vec2::new(-y_dir.y, y_dir.x) * (arrow_w_page * 0.5);

                let (tx, ty) = page_to_pdf_mm(y_axis_end.x, y_axis_end.y);
                let (lx, ly) = page_to_pdf_mm(left_y.x, left_y.y);
                let (rx, ry) = page_to_pdf_mm(right_y.x, right_y.y);

                let arrow_poly_y = Polygon {
                    rings: vec![vec![
                        (Point::new(Mm(tx), Mm(ty)), false),
                        (Point::new(Mm(lx), Mm(ly)), false),
                        (Point::new(Mm(rx), Mm(ry)), false),
                    ]],
                    mode: PaintMode::Fill,
                    winding_order: WindingOrder::NonZero,
                };
                layer.add_polygon(arrow_poly_y);

                let x_dir = (*x_axis_end - *origin).normalize_or_zero();
                let base_x = *x_axis_end - x_dir * arrow_len_page;
                let left_x = base_x + glam::Vec2::new(-x_dir.y, x_dir.x) * (arrow_w_page * 0.5);
                let right_x = base_x - glam::Vec2::new(-x_dir.y, x_dir.x) * (arrow_w_page * 0.5);

                let (tx_x, ty_x) = page_to_pdf_mm(x_axis_end.x, x_axis_end.y);
                let (lx_x, ly_x) = page_to_pdf_mm(left_x.x, left_x.y);
                let (rx_x, ry_x) = page_to_pdf_mm(right_x.x, right_x.y);

                let arrow_poly_x = Polygon {
                    rings: vec![vec![
                        (Point::new(Mm(tx_x), Mm(ty_x)), false),
                        (Point::new(Mm(lx_x), Mm(ly_x)), false),
                        (Point::new(Mm(rx_x), Mm(ry_x)), false),
                    ]],
                    mode: PaintMode::Fill,
                    winding_order: WindingOrder::NonZero,
                };
                layer.add_polygon(arrow_poly_x);
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

        let export_res =
            export_notebook_to_pdf(&notebook, &pdf_path, ExportStrategy::FilledPolygon);
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
        let bytes_res = export_notebook_to_bytes(&notebook, ExportStrategy::BezierCurve);
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

        let bytes_res = export_notebook_to_bytes(&notebook, ExportStrategy::FilledPolygon);
        assert!(bytes_res.is_ok(), "Shape export must succeed");
        let bytes = bytes_res.unwrap();
        assert!(bytes.len() > 1024);
        assert!(bytes.starts_with(b"%PDF-1."));
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
