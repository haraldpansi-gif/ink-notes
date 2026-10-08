//! Export einzelner Seiten als PNG oder SVG und einer Auswahl als Bild.
//!
//! Grundlage ist immer der PDF-Export: die Seite wird als einseitiges PDF erzeugt (mit
//! Bildern, importierten PDF-Seiten und Formen) und anschließend mit `hayro` gerastert bzw.
//! mit `hayro-svg` nach SVG umgewandelt. So sehen PDF, PNG und SVG gleich aus.

use std::error::Error;

use hayro::hayro_interpret::InterpreterSettings;
use hayro::hayro_syntax::Pdf;
use hayro::vello_cpu::color::palette::css::{TRANSPARENT, WHITE};
use hayro::{PixmapSettings, RenderCache, RenderSettings};

use crate::assets::{AssetStore, RgbaImage};
use crate::background::PatternColor;
use crate::document::{Aabb, Notebook, Page, PageBackground, A4_WIDTH_MM, PAGE_WIDTH};
use crate::export::{export_notebook_to_bytes, ExportStrategy};

/// Auflösung des PNG-Exports einer Seite.
pub const PAGE_PNG_DPI: f32 = 200.0;
/// Auflösung einer in die Zwischenablage kopierten Auswahl.
pub const SELECTION_PNG_DPI: f32 = 150.0;

/// Hintergrund beim Bild-Export.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageBackground {
    /// Weißes Papier mit Raster bzw. importierter PDF-Seite
    Paper,
    /// Transparent, nur Bilder und Tinte (ohne Raster und ohne importierte PDF-Seite)
    Transparent,
}

/// Erzeugt ein einseitiges PDF aus `page`.
fn single_page_pdf(
    page: &Page,
    assets: Option<&AssetStore>,
    pattern: PatternColor,
) -> Result<Vec<u8>, Box<dyn Error>> {
    let notebook = Notebook::from_pages(vec![page.clone()], 0, 1, page.id + 1);
    export_notebook_to_bytes(&notebook, ExportStrategy::FilledPolygon, assets, pattern)
}

/// Seite ohne Raster und PDF-Hintergrund (für transparenten Export und Auswahlen).
fn content_only(page: &Page) -> Page {
    let mut page = page.clone();
    page.background = PageBackground::Blank;
    page.pdf_background = None;
    page
}

/// Rastert die erste Seite eines PDFs mit `dpi`; Farben ohne Vormultiplikation.
fn rasterize(pdf_bytes: Vec<u8>, dpi: f32, transparent: bool) -> Result<RgbaImage, Box<dyn Error>> {
    let pdf = Pdf::new(pdf_bytes).map_err(|e| format!("PDF nicht lesbar: {e:?}"))?;
    let page = pdf.pages().first().ok_or("PDF ohne Seite")?;
    let scale = dpi / 72.0;
    let pixmap = hayro::render(
        page,
        &RenderCache::new(),
        &InterpreterSettings::default(),
        &RenderSettings::default(),
        &PixmapSettings {
            x_scale: scale,
            y_scale: scale,
            bg_color: if transparent { TRANSPARENT } else { WHITE },
        },
    );
    let mut pixels = pixmap.data_as_u8_slice().to_vec();
    // hayro liefert vormultiplizierte Farben; PNG erwartet gerade Farben
    for px in pixels.as_chunks_mut::<4>().0 {
        let a = px[3] as u32;
        if a > 0 && a < 255 {
            for c in &mut px[..3] {
                *c = ((*c as u32 * 255 + a / 2) / a).min(255) as u8;
            }
        }
    }
    Ok(RgbaImage {
        width: pixmap.width() as u32,
        height: pixmap.height() as u32,
        pixels,
    })
}

/// Kodiert ein Bild als PNG.
pub fn encode_png(image: &RgbaImage) -> Result<Vec<u8>, Box<dyn Error>> {
    let buffer = image::RgbaImage::from_raw(image.width, image.height, image.pixels.clone())
        .ok_or("Pixeldaten passen nicht zur Bildgröße")?;
    let mut out = Vec::new();
    image::DynamicImage::ImageRgba8(buffer)
        .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)?;
    Ok(out)
}

/// Seite als PNG (A4 mit [`PAGE_PNG_DPI`]).
pub fn page_png(
    page: &Page,
    assets: Option<&AssetStore>,
    background: ImageBackground,
    pattern: PatternColor,
) -> Result<Vec<u8>, Box<dyn Error>> {
    let image = match background {
        ImageBackground::Paper => {
            rasterize(single_page_pdf(page, assets, pattern)?, PAGE_PNG_DPI, false)?
        }
        ImageBackground::Transparent => rasterize(
            single_page_pdf(&content_only(page), assets, pattern)?,
            PAGE_PNG_DPI,
            true,
        )?,
    };
    encode_png(&image)
}

/// Seite als SVG (Vektor; Schriften importierter PDFs werden zu Pfaden).
pub fn page_svg(
    page: &Page,
    assets: Option<&AssetStore>,
    pattern: PatternColor,
) -> Result<String, Box<dyn Error>> {
    let pdf = Pdf::new(single_page_pdf(page, assets, pattern)?)
        .map_err(|e| format!("PDF nicht lesbar: {e:?}"))?;
    let pdf_page = pdf.pages().first().ok_or("PDF ohne Seite")?;
    Ok(hayro_svg::convert(
        pdf_page,
        &hayro_svg::RenderCache::new(),
        &InterpreterSettings::default(),
        &hayro_svg::SvgRenderSettings {
            bg_color: [255, 255, 255, 255],
        },
    ))
}

/// Bild der Objekte `ids` auf `page`, auf ihren Umriss zugeschnitten, mit transparentem
/// Hintergrund. `None`, wenn keines der Objekte auf der Seite liegt.
pub fn selection_image(
    page: &Page,
    ids: &[u64],
    assets: Option<&AssetStore>,
) -> Result<Option<RgbaImage>, Box<dyn Error>> {
    let mut selected = content_only(page);
    selected.strokes.retain(|s| ids.contains(&s.id));
    let Some(bounds) = Page::selection_aabb(&selected.strokes) else {
        return Ok(None);
    };
    let max_width = selected.strokes.iter().map(|s| s.width).fold(0.0, f32::max);
    let bounds: Aabb = bounds.expanded(max_width / 2.0 + 4.0);

    let full = rasterize(
        single_page_pdf(&selected, assets, PatternColor::default())?,
        SELECTION_PNG_DPI,
        true,
    )?;
    // Seiteneinheiten → Pixel
    let px_per_unit = SELECTION_PNG_DPI / 25.4 * A4_WIDTH_MM / PAGE_WIDTH;
    let x0 = ((bounds.min.x * px_per_unit).floor().max(0.0) as u32).min(full.width);
    let y0 = ((bounds.min.y * px_per_unit).floor().max(0.0) as u32).min(full.height);
    let x1 = ((bounds.max.x * px_per_unit).ceil().max(0.0) as u32).min(full.width);
    let y1 = ((bounds.max.y * px_per_unit).ceil().max(0.0) as u32).min(full.height);
    if x1 <= x0 || y1 <= y0 {
        return Ok(None);
    }
    let (w, h) = (x1 - x0, y1 - y0);
    let mut pixels = Vec::with_capacity((w * h * 4) as usize);
    for y in y0..y1 {
        let start = ((y * full.width + x0) * 4) as usize;
        pixels.extend_from_slice(&full.pixels[start..start + (w * 4) as usize]);
    }
    Ok(Some(RgbaImage {
        width: w,
        height: h,
        pixels,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::{Stroke, StylusSample, PAGE_HEIGHT};

    fn page_with_line() -> Page {
        let mut page = Page::new(1, PageBackground::Grid5mm);
        let samples: Vec<StylusSample> = (0..=20)
            .map(|i| StylusSample {
                position: glam::Vec2::new(500.0 + i as f32 * 20.0, 1000.0),
                pressure: 0.6,
                timestamp: i as f64 * 0.01,
            })
            .collect();
        let vertices = crate::storage::reconstruct_stroke_vertices(
            &samples,
            [0.9, 0.1, 0.1, 1.0],
            20.0,
            Default::default(),
            true,
        );
        let mut stroke = Stroke::new(7, samples, vertices, [0.9, 0.1, 0.1, 1.0]);
        stroke.width = 20.0;
        page.strokes.push(stroke);
        page
    }

    fn decode(png: &[u8]) -> image::RgbaImage {
        image::load_from_memory(png).unwrap().into_rgba8()
    }

    #[test]
    fn test_page_png_paper_and_transparent() {
        let page = page_with_line();
        let paper = decode(
            &page_png(&page, None, ImageBackground::Paper, PatternColor::default()).unwrap(),
        );
        // A4 mit 200 dpi
        assert_eq!(paper.width(), (A4_WIDTH_MM / 25.4 * PAGE_PNG_DPI) as u32);
        assert_eq!(paper.get_pixel(5, 5).0[3], 255, "Papier ist deckend");

        let clear = decode(
            &page_png(
                &page,
                None,
                ImageBackground::Transparent,
                PatternColor::default(),
            )
            .unwrap(),
        );
        assert_eq!(clear.get_pixel(5, 5).0[3], 0, "Hintergrund ist transparent");
        // Die Linie ist rot und deckend
        let px_per_unit = clear.width() as f32 / PAGE_WIDTH;
        let line = clear.get_pixel((700.0 * px_per_unit) as u32, (1000.0 * px_per_unit) as u32);
        assert!(
            line.0[0] > 180 && line.0[1] < 80 && line.0[3] > 200,
            "{line:?}"
        );
        assert!(clear.height() as f32 / clear.width() as f32 - PAGE_HEIGHT / PAGE_WIDTH < 0.01);
    }

    #[test]
    fn test_export_uses_pattern_color() {
        // Kariertes Blatt: das kräftige Muster ist im Export dunkler als das helle
        let page = Page::new(1, PageBackground::Grid5mm);
        let darkest = |pattern| {
            let png = decode(&page_png(&page, None, ImageBackground::Paper, pattern).unwrap());
            png.pixels().map(|p| p.0[1]).min().unwrap()
        };
        let light = darkest(PatternColor::Light);
        let strong = darkest(PatternColor::Strong);
        assert!(strong + 60 < light, "hell {light}, kräftig {strong}");
    }

    #[test]
    fn test_page_svg_contains_ink() {
        let svg = page_svg(&page_with_line(), None, PatternColor::default()).unwrap();
        assert!(svg.starts_with("<svg") || svg.contains("<svg"));
        assert!(svg.contains("<path"));
        assert!(svg.trim_end().ends_with("</svg>"));
    }

    /// Begrenzungsrechteck (x0, y0, x1, y1) der deckenden Pixel.
    fn ink_bounds(width: u32, pixels: &[u8]) -> (u32, u32, u32, u32) {
        let mut b = (u32::MAX, u32::MAX, 0, 0);
        for (i, p) in pixels.as_chunks::<4>().0.iter().enumerate() {
            if p[3] > 128 {
                let (x, y) = (i as u32 % width, i as u32 / width);
                b = (b.0.min(x), b.1.min(y), b.2.max(x), b.3.max(y));
            }
        }
        b
    }

    fn page_with_text() -> (Page, crate::text::TextBox) {
        let text = crate::text::TextBox::new(
            "Hallo Welt\nzweite Zeile".into(),
            100.0,
            crate::text::TextFont::Sans,
            1200.0,
        );
        let size = glam::Vec2::new(1200.0, text.layout().height());
        let corners = crate::text::frame_corners(
            glam::Vec2::new(300.0, 400.0),
            [glam::Vec2::X, glam::Vec2::Y],
            size,
        );
        let mut page = Page::new(1, PageBackground::Grid5mm);
        page.strokes.push(Stroke::new_text(
            3,
            text.clone(),
            corners,
            [0.0, 0.0, 0.0, 1.0],
        ));
        (page, text)
    }

    #[test]
    fn test_exported_text_matches_screen_rendering() {
        let (page, text) = page_with_text();
        let png = decode(
            &page_png(
                &page,
                None,
                ImageBackground::Transparent,
                PatternColor::default(),
            )
            .unwrap(),
        );
        let px_per_unit = png.width() as f32 / PAGE_WIDTH;
        let exported = ink_bounds(png.width(), png.as_raw());

        // Dieselbe Stelle, wie sie die Bildschirmtextur zeigt
        let screen = crate::text::rasterize(&text, [0.0, 0.0, 0.0, 1.0], px_per_unit);
        let local = ink_bounds(screen.width, &screen.pixels);
        let (ox, oy) = ((300.0 * px_per_unit) as u32, (400.0 * px_per_unit) as u32);
        let expected = (local.0 + ox, local.1 + oy, local.2 + ox, local.3 + oy);
        for (a, b) in [
            (exported.0, expected.0),
            (exported.1, expected.1),
            (exported.2, expected.2),
            (exported.3, expected.3),
        ] {
            assert!(
                a.abs_diff(b) <= 3,
                "PDF {exported:?} ≠ Bildschirm {expected:?}"
            );
        }
    }

    #[test]
    fn test_exported_text_follows_rotation() {
        let (mut page, _) = page_with_text();
        let png = decode(
            &page_png(
                &page,
                None,
                ImageBackground::Transparent,
                PatternColor::default(),
            )
            .unwrap(),
        );
        let straight = ink_bounds(png.width(), png.as_raw());

        let center = page.strokes[0].aabb.min;
        page.strokes[0].rotate(center, std::f32::consts::FRAC_PI_2);
        let png = decode(
            &page_png(
                &page,
                None,
                ImageBackground::Transparent,
                PatternColor::default(),
            )
            .unwrap(),
        );
        let rotated = ink_bounds(png.width(), png.as_raw());
        // 90° im Uhrzeigersinn: Breite und Höhe tauschen, Text liegt links vom Drehpunkt
        let (w, h) = (straight.2 - straight.0, straight.3 - straight.1);
        let (rw, rh) = (rotated.2 - rotated.0, rotated.3 - rotated.1);
        assert!(
            rw.abs_diff(h) <= 4 && rh.abs_diff(w) <= 4,
            "{straight:?} → {rotated:?}"
        );
        let px_per_unit = png.width() as f32 / PAGE_WIDTH;
        assert!((rotated.2 as f32) < 300.0 * px_per_unit + 2.0);
    }

    #[test]
    fn test_landscape_page_export_keeps_positions() {
        // Strich unten rechts auf einer Querformat-Seite: im PNG an derselben Stelle
        let mut page = page_with_line();
        page.size = glam::Vec2::new(2970.0, 1671.0);
        page.strokes[0].translate(glam::Vec2::new(1900.0, 500.0)); // x 2400–2800, y 1500
        let png = decode(
            &page_png(
                &page,
                None,
                ImageBackground::Transparent,
                PatternColor::default(),
            )
            .unwrap(),
        );
        let ratio = png.width() as f32 / png.height() as f32;
        assert!((ratio - 2970.0 / 1671.0).abs() < 0.01, "{ratio}");
        let px_per_unit = png.width() as f32 / 2970.0;
        let at = |x: f32, y: f32| {
            png.get_pixel((x * px_per_unit) as u32, (y * px_per_unit) as u32)
                .0
        };
        assert!(at(2600.0, 1500.0)[3] > 200, "Strich liegt unten rechts");
        assert!(at(2600.0, 1300.0)[3] == 0);
        let svg = page_svg(&page, None, PatternColor::default()).unwrap();
        assert!(svg.contains("<path"));
    }

    #[test]
    fn test_selection_image_is_cropped() {
        let page = page_with_line();
        let image = selection_image(&page, &[7], None).unwrap().unwrap();
        // Linie 400 Einheiten (40 mm) lang: Bild etwas breiter als 40 mm bei 150 dpi
        let expected = 40.0 / 25.4 * SELECTION_PNG_DPI;
        assert!(
            (image.width as f32) > expected && (image.width as f32) < expected * 1.25,
            "{} px",
            image.width
        );
        assert!(image.height < image.width / 3);
        assert!(image.pixels.as_chunks::<4>().0.iter().any(|p| p[3] > 200));
        assert!(selection_image(&page, &[99], None).unwrap().is_none());
    }
}
