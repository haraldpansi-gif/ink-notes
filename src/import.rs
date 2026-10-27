//! Import of other formats as a new notebook (Issue #47): PDF (`storage`), Xournal++
//! (`xopp`), Rnote (`rnote`) and Saber (`saber`). This module holds what the importers
//! share: building strokes, texts and images in the stored format and creating the notebook.

use std::io::Read;
use std::path::{Path, PathBuf};

use crate::document::{Notebook, Page, PageBackground, StrokeTool};
use crate::storage::{self, NotebookLayout, NotebookManifest, SampleData, StrokeData};
use crate::t;
use crate::text::{TextBox, TextFont};

/// Largest accepted file after decompression (guards against gzip bombs).
const MAX_UNCOMPRESSED_BYTES: u64 = 512 * 1024 * 1024;

/// Alpha of imported highlighter strokes that are stored opaque (as the app's highlighters).
pub const HIGHLIGHTER_ALPHA: f32 = 0.35;

/// What could not be taken over.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ImportReport {
    pub skipped: usize,
    /// The PDF background was referenced but not found or unreadable
    pub missing_pdf: Option<String>,
}

/// Result of an importer: pages, the next free stroke id, the layout (`None`: default).
pub struct Imported {
    pub pages: Vec<Page>,
    pub next_stroke_id: u64,
    pub layout: Option<NotebookLayout>,
    pub report: ImportReport,
}

/// Can the file be imported (by its name)?
pub fn is_importable_path(path: &Path) -> bool {
    crate::file_picker::is_pdf_path(path)
        || crate::xopp::is_xopp_path(path)
        || crate::rnote::is_rnote_path(path)
        || crate::saber::is_saber_path(path)
}

/// Imports a PDF, Xournal++, Rnote or Saber file as a new notebook titled `title`.
pub fn import_file(
    base_dir: &Path,
    title: &str,
    path: &Path,
) -> Result<(Notebook, NotebookManifest, PathBuf, ImportReport), String> {
    if crate::xopp::is_xopp_path(path) {
        crate::xopp::create_notebook_from_xopp(base_dir, title, path)
    } else if crate::rnote::is_rnote_path(path) {
        crate::rnote::create_notebook_from_rnote(base_dir, title, path)
    } else if crate::saber::is_saber_path(path) {
        crate::saber::create_notebook_from_saber(base_dir, title, path)
    } else {
        let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
        storage::create_notebook_from_pdf(base_dir, title, bytes)
            .map(|(nb, manifest, dir)| (nb, manifest, dir, ImportReport::default()))
    }
}

/// Decompresses gzip data; other data is returned unchanged.
pub fn gunzip(bytes: &[u8]) -> Result<Vec<u8>, String> {
    if !bytes.starts_with(&[0x1f, 0x8b]) {
        return Ok(bytes.to_vec());
    }
    let mut data = Vec::new();
    flate2::read::MultiGzDecoder::new(bytes)
        .take(MAX_UNCOMPRESSED_BYTES)
        .read_to_end(&mut data)
        .map_err(|e| t!("Datei nicht lesbar: {err}", err = e))?;
    Ok(data)
}

/// Creates a new notebook and fills it with the pages `build` creates in its folder (images
/// and PDFs go to its asset folder). On failure nothing is left behind.
pub fn create_notebook(
    base_dir: &Path,
    title: &str,
    build: impl FnOnce(&Path) -> Result<Imported, String>,
) -> Result<(Notebook, NotebookManifest, PathBuf, ImportReport), String> {
    let (_, _, dir) = storage::create_new_notebook(base_dir, title).map_err(|e| e.to_string())?;
    let result = (|| {
        let imported = build(&dir)?;
        let page_count = imported.pages.len() as u64;
        let notebook =
            Notebook::from_pages(imported.pages, 0, imported.next_stroke_id, page_count + 1);
        let mut manifest = storage::load_manifest(&dir).map_err(|e| e.to_string())?;
        if imported.layout.is_some() {
            manifest.layout = imported.layout;
        }
        storage::save_notebook(&dir, &notebook, &mut manifest).map_err(|e| e.to_string())?;
        Ok((notebook, manifest, imported.report))
    })();
    match result {
        Ok((notebook, manifest, report)) => Ok((notebook, manifest, dir, report)),
        Err(err) => {
            let _ = std::fs::remove_dir_all(&dir);
            Err(err)
        }
    }
}

/// Builds a page from stored-format strokes.
pub fn page(
    id: u64,
    size: glam::Vec2,
    background: PageBackground,
    pdf_background: Option<crate::document::PdfBackground>,
    strokes: Vec<StrokeData>,
) -> Page {
    storage::PageData {
        format_version: storage::CURRENT_FORMAT_VERSION,
        id,
        background,
        pdf_background,
        title: None,
        size: (size != crate::document::A4_SIZE).then(|| size.to_array()),
        strokes,
    }
    .to_page()
}

/// Our stroke width and the sample pressures for absolute widths per point (page units):
/// we draw a diameter of `width · (0.5 + p)`, so the widest point gets p = 1 and thinner
/// ones less (down to a third of the widest).
pub fn fit_widths(widths: &[f32]) -> (f32, Vec<f32>) {
    let widest = widths.iter().copied().fold(0.0, f32::max).max(0.01);
    let width = widest / 1.5;
    let pressures = widths
        .iter()
        .map(|w| (w / width - 0.5).clamp(0.0, 1.0))
        .collect();
    (width, pressures)
}

/// Pen or highlighter stroke through `points` (page units) with the given widths per point.
pub fn ink_stroke(
    id: u64,
    mut color: [f32; 4],
    highlighter: bool,
    points: &[glam::Vec2],
    width: f32,
    pressures: &[f32],
) -> StrokeData {
    let tool = if highlighter {
        if color[3] >= 0.99 {
            color[3] = HIGHLIGHTER_ALPHA;
        }
        StrokeTool::Highlighter
    } else {
        // Pens are always opaque (translucent pens are read as highlighters)
        color[3] = 1.0;
        StrokeTool::Pen
    };
    let samples = points
        .iter()
        .zip(pressures)
        .enumerate()
        .map(|(i, (p, &pressure))| SampleData {
            x: p.x,
            y: p.y,
            p: pressure,
            t: i as f64 * 0.005,
        })
        .collect();
    StrokeData {
        id,
        color,
        samples,
        shape: None,
        tool,
        width,
        finalized: true,
        image: None,
        text: None,
    }
}

/// Text field with its upper left corner at `origin` and the axes (page vector per unit to
/// the right and down). Without `wrap_width` it is as wide as its longest line.
#[allow(clippy::too_many_arguments)]
pub fn text_stroke(
    id: u64,
    content: &str,
    size: f32,
    font: TextFont,
    origin: glam::Vec2,
    axes: [glam::Vec2; 2],
    wrap_width: Option<f32>,
    mut color: [f32; 4],
) -> StrokeData {
    let wrap_width = wrap_width.unwrap_or_else(|| {
        let unwrapped = TextBox::new(content.to_string(), size, font, f32::MAX / 4.0);
        let widest = unwrapped
            .layout()
            .lines
            .iter()
            .map(|line| line.width)
            .fold(0.0, f32::max);
        widest + size
    });
    let text = TextBox::new(content.to_string(), size, font, wrap_width);
    let corners = crate::text::frame_corners(origin, axes, text.box_size());
    color[3] = 1.0;
    StrokeData {
        id,
        color,
        samples: corner_samples(corners),
        shape: None,
        tool: StrokeTool::Pen,
        width: 0.0,
        finalized: true,
        image: None,
        text: Some(text),
    }
}

/// Image with the corners upper left, upper right, lower right, lower left.
pub fn image_stroke(id: u64, asset: String, corners: [glam::Vec2; 4]) -> StrokeData {
    StrokeData {
        id,
        color: [1.0, 1.0, 1.0, 1.0],
        samples: corner_samples(corners),
        shape: None,
        tool: StrokeTool::Pen,
        width: 0.0,
        finalized: true,
        image: Some(asset),
        text: None,
    }
}

/// Premultiplied RGBA (Rnote, tiny-skia) to straight RGBA.
pub fn unpremultiply(pixels: &mut [u8]) {
    for px in pixels.as_chunks_mut::<4>().0 {
        let a = px[3] as u32;
        if a > 0 && a < 255 {
            for c in &mut px[..3] {
                *c = ((*c as u32 * 255 + a / 2) / a).min(255) as u8;
            }
        }
    }
}

/// Rasterizes an SVG for a box of `size` page units (about 250 dpi).
pub fn rasterize_svg(svg: &str, size: glam::Vec2) -> Option<(u32, u32, Vec<u8>)> {
    use resvg::{tiny_skia, usvg};
    let tree = usvg::Tree::from_str(svg, &usvg::Options::default()).ok()?;
    let max = crate::assets::MAX_IMPORT_DIMENSION as f32;
    let px_per_unit = 250.0 / 254.0;
    let width = (size.x * px_per_unit).round().clamp(1.0, max) as u32;
    let height = (size.y * px_per_unit).round().clamp(1.0, max) as u32;
    let mut pixmap = tiny_skia::Pixmap::new(width, height)?;
    let transform = tiny_skia::Transform::from_scale(
        width as f32 / tree.size().width(),
        height as f32 / tree.size().height(),
    );
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    let mut pixels = pixmap.take();
    unpremultiply(&mut pixels);
    Some((width, height, pixels))
}

fn corner_samples(corners: [glam::Vec2; 4]) -> Vec<SampleData> {
    corners
        .iter()
        .map(|c| SampleData {
            x: c.x,
            y: c.y,
            p: 1.0,
            t: 0.0,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fit_widths_keeps_the_drawn_widths() {
        let (width, pressures) = fit_widths(&[30.0, 15.0, 10.0, 2.0]);
        let drawn: Vec<f32> = pressures.iter().map(|p| width * (0.5 + p)).collect();
        assert!((drawn[0] - 30.0).abs() < 1e-3);
        assert!((drawn[1] - 15.0).abs() < 1e-3);
        assert!((drawn[2] - 10.0).abs() < 1e-3);
        // Thinner than a third of the widest point is not possible
        assert!((drawn[3] - 10.0).abs() < 1e-3);
    }
}
