//! Import of Rnote notebooks (.rnote) (Issue #47).
//!
//! An .rnote file is gzip-compressed JSON: `{"version": "0.x.y", "data": {"engine_snapshot":
//! {...}}}`. The snapshot is Rnote's internal state without a specification, so it is read
//! leniently as JSON and only what we need is picked out; files from Rnote 0.5.10 on are
//! supported (the layout of older ones differs too much). Coordinates are pixels at the
//! document's dpi.
//!
//! Pages: fixed-size and continuous documents become one page per Rnote page (a stroke goes
//! to the page its middle lies on), infinite documents an endless notebook.

use base64::Engine;
use glam::DVec2;
use serde_json::Value;
use std::path::Path;

use crate::document::{Notebook, PageBackground};
use crate::import::{self, ImportReport, Imported};
use crate::storage::{NotebookLayout, NotebookManifest, StrokeData};
use crate::t;
use crate::text::TextFont;

/// At most this many pages are created (protects against nonsensical document sizes).
const MAX_PAGES: usize = 2000;

/// Is the file by its name an Rnote notebook?
pub fn is_rnote_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("rnote"))
}

/// Parsed Rnote document, still in Rnote pixels.
#[derive(Debug, Clone, PartialEq)]
pub struct RnoteDocument {
    /// Page size in pixels and dpi
    pub format: DVec2,
    pub dpi: f64,
    /// Document bounds (upper left corner and size)
    pub origin: DVec2,
    pub size: DVec2,
    pub endless: bool,
    pub background: PageBackground,
    /// In drawing order
    pub items: Vec<RnoteItem>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RnoteItem {
    Ink {
        color: [f32; 4],
        highlighter: bool,
        points: Vec<DVec2>,
        /// Drawn width per point
        widths: Vec<f64>,
    },
    Text {
        content: String,
        size: f64,
        mono: bool,
        /// Upper left corner and axes
        affine: [DVec2; 3],
        max_width: Option<f64>,
        color: [f32; 4],
    },
    Bitmap {
        width: u32,
        height: u32,
        /// Straight (not premultiplied) RGBA
        pixels: Vec<u8>,
        corners: [DVec2; 4],
    },
    /// SVG image (Rnote stores imported PDF pages like this), rasterized on import
    Svg { svg: String, corners: [DVec2; 4] },
    /// Element without a counterpart or unreadable
    Unsupported,
}

impl RnoteItem {
    /// Point that decides which page the element belongs to.
    fn anchor(&self) -> Option<DVec2> {
        match self {
            RnoteItem::Ink { points, .. } => {
                let (min, max) = points.iter().fold(
                    (DVec2::splat(f64::MAX), DVec2::splat(f64::MIN)),
                    |(a, b), &p| (a.min(p), b.max(p)),
                );
                (!points.is_empty()).then(|| (min + max) / 2.0)
            }
            RnoteItem::Text { affine, .. } => Some(affine[2]),
            RnoteItem::Bitmap { corners, .. } | RnoteItem::Svg { corners, .. } => {
                Some((corners[0] + corners[2]) / 2.0)
            }
            RnoteItem::Unsupported => None,
        }
    }
}

fn num(value: &Value, key: &str) -> Option<f64> {
    value.get(key)?.as_f64().filter(|v| v.is_finite())
}

/// Vectors are stored as `[x, y]` (older versions possibly as `{"x": .., "y": ..}`).
fn vec2(value: &Value) -> Option<DVec2> {
    match value {
        Value::Array(a) if a.len() == 2 => Some(DVec2::new(a[0].as_f64()?, a[1].as_f64()?)),
        Value::Object(_) => Some(DVec2::new(num(value, "x")?, num(value, "y")?)),
        _ => None,
    }
    .filter(|v| v.is_finite())
}

/// Affine transformation as x axis, y axis and translation. Since Rnote 0.15 `affine` holds
/// the six values; before, `transform.affine` held a column-major 3×3 matrix.
fn affine(value: &Value) -> Option<[DVec2; 3]> {
    let values = value
        .get("affine")
        .or_else(|| value.get("transform")?.get("affine"))?
        .as_array()?;
    let v: Vec<f64> = values.iter().filter_map(Value::as_f64).collect();
    let [a, b, c, d, e, f] = match v.len() {
        6 => [v[0], v[1], v[2], v[3], v[4], v[5]],
        9 => [v[0], v[1], v[3], v[4], v[6], v[7]],
        _ => return None,
    };
    Some([DVec2::new(a, b), DVec2::new(c, d), DVec2::new(e, f)])
}

fn apply(affine: &[DVec2; 3], p: DVec2) -> DVec2 {
    affine[0] * p.x + affine[1] * p.y + affine[2]
}

fn color(value: Option<&Value>) -> Option<[f32; 4]> {
    let value = value.filter(|v| v.is_object())?;
    let c = |k| num(value, k).unwrap_or(0.0).clamp(0.0, 1.0) as f32;
    Some([
        c("r"),
        c("g"),
        c("b"),
        num(value, "a").map_or(1.0, |a| a.clamp(0.0, 1.0) as f32),
    ])
}

/// The one variant of an externally tagged enum: `{"name": {...}}`.
fn variant(value: &Value) -> Option<(&str, &Value)> {
    let object = value.as_object()?;
    let (name, inner) = object.iter().next()?;
    Some((name.as_str(), inner))
}

/// Width for the pressure according to Rnote's pressure curve.
fn curve_width(curve: &str, width: f64, pressure: f64) -> f64 {
    let p = pressure.clamp(0.0, 1.0);
    match curve {
        "const" => width,
        "sqrt" => width * p.sqrt(),
        "cbrt" => width * p.cbrt(),
        "pow2" => width * p.powi(2),
        "pow3" => width * p.powi(3),
        _ => width * p,
    }
}

/// Style options (smooth, rough or textured).
struct Style {
    width: f64,
    stroke_color: Option<[f32; 4]>,
    fill_color: Option<[f32; 4]>,
    pressure_curve: String,
}

fn style(value: Option<&Value>) -> Option<Style> {
    let (kind, options) = variant(value?)?;
    let curve = if kind == "rough" {
        "const".to_string()
    } else {
        options
            .get("pressure_curve")
            .and_then(Value::as_str)
            .unwrap_or("linear")
            .to_string()
    };
    Some(Style {
        width: num(options, "stroke_width").unwrap_or(1.0).max(0.1),
        stroke_color: color(options.get("stroke_color")),
        fill_color: color(options.get("fill_color")),
        pressure_curve: curve,
    })
}

/// Points along a Bézier curve (without its start), at most every 2 pixels; for a line
/// just its end.
fn bezier(start: DVec2, controls: &[DVec2], end: DVec2) -> Vec<DVec2> {
    if controls.is_empty() {
        return vec![end];
    }
    let length = controls
        .iter()
        .chain([&end])
        .fold((0.0, start), |(sum, prev), &p| (sum + prev.distance(p), p))
        .0;
    let steps = ((length / 2.0).ceil() as usize).clamp(1, 32);
    (1..=steps)
        .map(|i| {
            let t = i as f64 / steps as f64;
            let u = 1.0 - t;
            match controls {
                [cp] => start * u * u + *cp * 2.0 * u * t + end * t * t,
                [cp1, cp2] => {
                    start * u * u * u
                        + *cp1 * 3.0 * u * u * t
                        + *cp2 * 3.0 * u * t * t
                        + end * t * t * t
                }
                _ => start + (end - start) * t,
            }
        })
        .collect()
}

fn element(value: &Value) -> Option<(DVec2, f64)> {
    Some((
        vec2(value.get("pos")?)?,
        num(value, "pressure").unwrap_or(0.5),
    ))
}

fn brushstroke(value: &Value, highlighter: bool) -> Option<RnoteItem> {
    let style = style(value.get("style"))?;
    let color = style.stroke_color?;
    let path = value.get("path")?;
    let (mut prev, mut prev_pressure) = element(path.get("start")?)?;
    let mut points = vec![prev];
    let mut pressures = vec![prev_pressure];
    for segment in path.get("segments")?.as_array()? {
        let Some((kind, seg)) = variant(segment) else {
            continue;
        };
        let Some((end, end_pressure)) = seg.get("end").and_then(element) else {
            continue;
        };
        let controls: Vec<DVec2> = match kind {
            "quadbezto" | "quadbez" => seg.get("cp").and_then(vec2).into_iter().collect(),
            "cubbezto" | "cubbez" => [seg.get("cp1"), seg.get("cp2")]
                .into_iter()
                .filter_map(|v| v.and_then(vec2))
                .collect(),
            _ => Vec::new(),
        };
        let new = bezier(prev, &controls, end);
        let n = new.len() as f64;
        for (i, p) in new.into_iter().enumerate() {
            let t = (i + 1) as f64 / n;
            points.push(p);
            pressures.push(prev_pressure + (end_pressure - prev_pressure) * t);
        }
        prev = end;
        prev_pressure = end_pressure;
    }
    let widths = pressures
        .iter()
        .map(|&p| curve_width(&style.pressure_curve, style.width, p))
        .collect();
    Some(RnoteItem::Ink {
        highlighter: highlighter || color[3] < 0.99,
        color,
        points,
        widths,
    })
}

/// Outline of a shape as polylines.
fn shape_lines(shape: &Value) -> Option<Vec<Vec<DVec2>>> {
    let (kind, s) = variant(shape)?;
    let v = |k: &str| s.get(k).and_then(vec2);
    let lines = match kind {
        "line" => vec![vec![v("start")?, v("end")?]],
        "arrow" => {
            let (start, tip) = (v("start")?, v("tip")?);
            let back = (start - tip).normalize_or_zero();
            let length = (start.distance(tip) * 0.4).min(20.0);
            let wing = |angle: f64| tip + DVec2::from_angle(angle).rotate(back) * length;
            vec![vec![start, tip], vec![wing(0.5), tip, wing(-0.5)]]
        }
        "rect" => {
            let half = vec2(s.get("cuboid")?.get("half_extents")?)?;
            let affine = affine(s)?;
            let corners = [
                (-1.0, -1.0),
                (1.0, -1.0),
                (1.0, 1.0),
                (-1.0, 1.0),
                (-1.0, -1.0),
            ];
            vec![corners
                .iter()
                .map(|&(x, y)| apply(&affine, DVec2::new(x * half.x, y * half.y)))
                .collect()]
        }
        "ellipse" => {
            let radii = v("radii")?;
            let affine = affine(s)?;
            vec![(0..=64)
                .map(|i| {
                    let a = i as f64 / 64.0 * std::f64::consts::TAU;
                    apply(&affine, DVec2::new(a.cos() * radii.x, a.sin() * radii.y))
                })
                .collect()]
        }
        "quadbez" | "cubbez" => {
            let start = v("start")?;
            let controls: Vec<DVec2> = ["cp", "cp1", "cp2"].iter().filter_map(|k| v(k)).collect();
            let mut points = vec![start];
            points.extend(bezier(start, &controls, v("end")?));
            vec![points]
        }
        "polyline" | "polygon" => {
            let start = v("start")?;
            let mut points = vec![start];
            points.extend(s.get("path")?.as_array()?.iter().filter_map(vec2));
            if kind == "polygon" {
                points.push(start);
            }
            vec![points]
        }
        _ => return None,
    };
    Some(lines)
}

fn shapestroke(value: &Value, highlighter: bool) -> Vec<RnoteItem> {
    let Some(Style {
        width,
        stroke_color,
        fill_color,
        ..
    }) = style(value.get("style"))
    else {
        return vec![RnoteItem::Unsupported];
    };
    let (Some(color), Some(lines)) = (
        stroke_color.or(fill_color),
        value.get("shape").and_then(shape_lines),
    ) else {
        return vec![RnoteItem::Unsupported];
    };
    lines
        .into_iter()
        .map(|points| RnoteItem::Ink {
            color,
            highlighter: highlighter || color[3] < 0.99,
            widths: vec![width; points.len()],
            points,
        })
        .collect()
}

fn textstroke(value: &Value) -> Option<RnoteItem> {
    let style = value.get("text_style")?;
    Some(RnoteItem::Text {
        content: value.get("text")?.as_str()?.to_string(),
        size: num(style, "font_size").unwrap_or(32.0).max(1.0),
        mono: style
            .get("font_family")
            .and_then(Value::as_str)
            .is_some_and(|f| f.to_ascii_lowercase().contains("mono")),
        affine: affine(value)?,
        max_width: num(style, "max_width").filter(|w| *w > 0.0),
        color: color(style.get("color")).unwrap_or([0.0, 0.0, 0.0, 1.0]),
    })
}

fn bitmapimage(value: &Value) -> Option<RnoteItem> {
    let image = value.get("image")?;
    let width = num(image, "pixel_width")? as u32;
    let height = num(image, "pixel_height")? as u32;
    let mut pixels = base64::engine::general_purpose::STANDARD
        .decode(image.get("data")?.as_str()?)
        .ok()?;
    if pixels.len() != width as usize * height as usize * 4 {
        return None;
    }
    unpremultiply(&mut pixels);
    Some(RnoteItem::Bitmap {
        width,
        height,
        pixels,
        corners: rectangle_corners(value.get("rectangle")?)?,
    })
}

fn vectorimage(value: &Value) -> Option<RnoteItem> {
    Some(RnoteItem::Svg {
        svg: value.get("svg_data")?.as_str()?.to_string(),
        corners: rectangle_corners(value.get("rectangle")?)?,
    })
}

/// Corners (upper left, upper right, lower right, lower left) of an Rnote rectangle: a
/// box around the origin, placed by its affine transformation.
fn rectangle_corners(rectangle: &Value) -> Option<[DVec2; 4]> {
    let half = vec2(rectangle.get("cuboid")?.get("half_extents")?)?;
    let affine = affine(rectangle)?;
    Some(
        [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)]
            .map(|(x, y)| apply(&affine, DVec2::new(x * half.x, y * half.y))),
    )
}

/// Premultiplied RGBA (Rnote, tiny-skia) to straight RGBA.
fn unpremultiply(pixels: &mut [u8]) {
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
fn rasterize_svg(svg: &str, size: glam::Vec2) -> Option<(u32, u32, Vec<u8>)> {
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

/// Rnote's version as (major, minor, patch).
fn version(text: &str) -> Option<(u64, u64, u64)> {
    let v = semver::Version::parse(text).ok()?;
    Some((v.major, v.minor, v.patch))
}

/// Parses the (decompressed) JSON of an .rnote file.
pub fn parse(json: &[u8]) -> Result<RnoteDocument, String> {
    let not_rnote = || t!("Keine Rnote-Datei").to_string();
    let wrapper: Value = serde_json::from_slice(json).map_err(|_| not_rnote())?;
    let version_text = wrapper
        .get("version")
        .and_then(Value::as_str)
        .ok_or_else(not_rnote)?;
    if version(version_text).is_none_or(|v| v < (0, 5, 10)) {
        return Err(t!(
            "Rnote-Datei zu alt ({version}): bitte in einem aktuellen Rnote öffnen und neu speichern",
            version = version_text
        )
        .to_string());
    }
    let snapshot = wrapper
        .get("data")
        .and_then(|d| d.get("engine_snapshot"))
        .ok_or_else(not_rnote)?;
    let document = snapshot.get("document").ok_or_else(not_rnote)?;
    // Since Rnote 0.13 format, background and layout are in `config`
    let config = document.get("config").unwrap_or(document);
    let format = config.get("format").unwrap_or(&Value::Null);
    let dpi = num(format, "dpi").filter(|d| *d > 0.0).unwrap_or(96.0);
    let format_size = DVec2::new(
        num(format, "width").unwrap_or(dpi * 210.0 / 25.4).max(1.0),
        num(format, "height").unwrap_or(dpi * 297.0 / 25.4).max(1.0),
    );
    let background = match config
        .get("background")
        .and_then(|b| b.get("pattern"))
        .and_then(Value::as_str)
    {
        Some("lines") => PageBackground::Lined,
        Some("grid" | "isometric_grid") => PageBackground::Grid5mm,
        Some("dots" | "isometric_dots") => PageBackground::DotGrid,
        _ => PageBackground::Blank,
    };
    let endless = matches!(
        config.get("layout").and_then(Value::as_str),
        Some("infinite" | "semi_infinite")
    );

    let strokes = snapshot
        .get("stroke_components")
        .and_then(Value::as_array)
        .ok_or_else(not_rnote)?;
    let chrono = snapshot
        .get("chrono_components")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    // Slot maps are stored as slot arrays; the chrono components share the slot index
    let mut ordered: Vec<(u64, Vec<RnoteItem>)> = Vec::new();
    for (index, slot) in strokes.iter().enumerate() {
        let Some(stroke) = slot.get("value").filter(|v| !v.is_null()) else {
            continue;
        };
        let chrono = chrono.get(index).and_then(|c| c.get("value"));
        let t = chrono.and_then(|c| c.get("t")).and_then(Value::as_u64);
        let highlighter = chrono
            .and_then(|c| c.get("layer"))
            .and_then(Value::as_str)
            .is_some_and(|l| l.eq_ignore_ascii_case("highlighter"));
        let items = match variant(stroke) {
            Some(("brushstroke", v)) => {
                vec![brushstroke(v, highlighter).unwrap_or(RnoteItem::Unsupported)]
            }
            Some(("shapestroke", v)) => shapestroke(v, highlighter),
            Some(("textstroke", v)) => vec![textstroke(v).unwrap_or(RnoteItem::Unsupported)],
            Some(("bitmapimage", v)) => vec![bitmapimage(v).unwrap_or(RnoteItem::Unsupported)],
            Some(("vectorimage", v)) => vec![vectorimage(v).unwrap_or(RnoteItem::Unsupported)],
            _ => vec![RnoteItem::Unsupported],
        };
        ordered.push((t.unwrap_or(index as u64), items));
    }
    ordered.sort_by_key(|(t, _)| *t);

    Ok(RnoteDocument {
        format: format_size,
        dpi,
        origin: DVec2::new(
            num(document, "x").unwrap_or(0.0),
            num(document, "y").unwrap_or(0.0),
        ),
        size: DVec2::new(
            num(document, "width").unwrap_or(format_size.x),
            num(document, "height").unwrap_or(format_size.y),
        ),
        endless,
        background,
        items: ordered.into_iter().flat_map(|(_, items)| items).collect(),
    })
}

/// Creates a new notebook from an Rnote file.
pub fn create_notebook_from_rnote(
    base_dir: &Path,
    title: &str,
    path: &Path,
) -> Result<(Notebook, NotebookManifest, std::path::PathBuf, ImportReport), String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let doc = parse(&import::gunzip(&bytes)?)?;
    import::create_notebook(base_dir, title, |dir| Ok(convert(&doc, dir)))
}

/// A target page: where its Rnote page starts, and how Rnote pixels map onto it.
struct Target {
    rnote_origin: DVec2,
    origin: glam::Vec2,
    scale: f32,
    size: glam::Vec2,
    strokes: Vec<StrokeData>,
}

impl Target {
    fn map(&self, p: DVec2) -> glam::Vec2 {
        self.origin + (p - self.rnote_origin).as_vec2() * self.scale
    }
}

fn convert(doc: &RnoteDocument, notebook_dir: &Path) -> Imported {
    let assets = crate::assets::AssetStore::new(notebook_dir);
    let mut report = ImportReport::default();
    // Page units per Rnote pixel at the document's dpi
    let units_per_px = (crate::document::UNITS_PER_MM as f64 * 25.4 / doc.dpi) as f32;

    let (columns, mut targets) = if doc.endless {
        let target = Target {
            rnote_origin: DVec2::ZERO,
            origin: crate::document::ENDLESS_HOME,
            scale: units_per_px,
            size: crate::document::ENDLESS_PAGE_SIZE,
            strokes: Vec::new(),
        };
        (1, vec![target])
    } else {
        // Same rules as for imported PDFs: portrait pages become A4, the content keeps its
        // aspect ratio, centred horizontally and aligned at the top
        let size_pt = (doc.format / doc.dpi * 72.0).as_vec2();
        let size = crate::document::pdf_page_size(size_pt.x, size_pt.y);
        let [origin, top_right, _, bottom_left] =
            crate::document::pdf_placement(size_pt.x, size_pt.y, size);
        let scale_pt =
            ((top_right.x - origin.x) / size_pt.x).min((bottom_left.y - origin.y) / size_pt.y);
        let scale = scale_pt * (72.0 / doc.dpi) as f32;
        // Rnote grows documents page by page; tolerate rounding in the stored size
        let count = |length: f64, page: f64| ((length / page - 0.01).ceil() as usize).max(1);
        let columns = count(doc.size.x, doc.format.x);
        let rows = count(doc.size.y, doc.format.y);
        let rows = rows.min((MAX_PAGES / columns).max(1));
        let targets = (0..rows * columns)
            .map(|i| Target {
                rnote_origin: doc.origin
                    + DVec2::new((i % columns) as f64, (i / columns) as f64) * doc.format,
                origin,
                scale,
                size,
                strokes: Vec::new(),
            })
            .collect();
        (columns, targets)
    };
    let rows = targets.len() / columns;

    let mut next_id: u64 = 1;
    for item in &doc.items {
        let Some(anchor) = item.anchor() else {
            report.skipped += 1;
            continue;
        };
        let cell = ((anchor - doc.origin) / doc.format).floor();
        let index = if doc.endless {
            0
        } else {
            let column = (cell.x.max(0.0) as usize).min(columns - 1);
            let row = (cell.y.max(0.0) as usize).min(rows - 1);
            row * columns + column
        };
        let target = &mut targets[index];
        let id = next_id;
        next_id += 1;
        let stroke = match item {
            RnoteItem::Ink {
                color,
                highlighter,
                points,
                widths,
            } => {
                let points: Vec<glam::Vec2> = points.iter().map(|&p| target.map(p)).collect();
                let widths: Vec<f32> = widths.iter().map(|&w| w as f32 * target.scale).collect();
                let (width, pressures) = import::fit_widths(&widths);
                import::ink_stroke(id, *color, *highlighter, &points, width, &pressures)
            }
            RnoteItem::Text {
                content,
                size,
                mono,
                affine,
                max_width,
                color,
            } => {
                let font = if *mono {
                    TextFont::Mono
                } else {
                    TextFont::Sans
                };
                import::text_stroke(
                    id,
                    content,
                    *size as f32 * target.scale,
                    font,
                    target.map(affine[2]),
                    [affine[0].as_vec2(), affine[1].as_vec2()],
                    max_width.map(|w| w as f32 * target.scale),
                    *color,
                )
            }
            RnoteItem::Bitmap {
                width,
                height,
                pixels,
                corners,
            } => match assets.import_rgba(*width, *height, pixels.clone()) {
                Ok(image) => import::image_stroke(id, image.asset, corners.map(|c| target.map(c))),
                Err(_) => {
                    report.skipped += 1;
                    continue;
                }
            },
            RnoteItem::Svg { svg, corners } => {
                let corners = corners.map(|c| target.map(c));
                let size = glam::Vec2::new(
                    corners[0].distance(corners[1]),
                    corners[0].distance(corners[3]),
                );
                let image = rasterize_svg(svg, size).and_then(|(width, height, pixels)| {
                    assets.import_rgba(width, height, pixels).ok()
                });
                match image {
                    Some(image) => import::image_stroke(id, image.asset, corners),
                    None => {
                        report.skipped += 1;
                        continue;
                    }
                }
            }
            RnoteItem::Unsupported => unreachable!("has no anchor"),
        };
        target.strokes.push(stroke);
    }

    let pages = targets
        .into_iter()
        .enumerate()
        .map(|(i, target)| {
            import::page(
                i as u64 + 1,
                target.size,
                doc.background,
                None,
                target.strokes,
            )
        })
        .collect();
    Imported {
        pages,
        next_stroke_id: next_id,
        layout: doc.endless.then_some(NotebookLayout::Endless),
        report,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::StrokeTool;
    use serde_json::json;
    use std::io::Write;

    fn slot(value: Value) -> Value {
        json!({ "value": value, "version": 1 })
    }

    /// A small Rnote 0.15 document: two A4 pages at 96 dpi.
    fn sample(layout: &str) -> Value {
        let brush = json!({ "brushstroke": {
            "path": {
                "start": { "pos": [100.0, 100.0], "pressure": 0.5 },
                "segments": [
                    { "lineto": { "end": { "pos": [120.0, 100.0], "pressure": 1.0 } } },
                    { "cubbezto": { "cp1": [130.0, 110.0], "cp2": [140.0, 110.0],
                                    "end": { "pos": [150.0, 100.0], "pressure": 0.25 } } }
                ]
            },
            "style": { "smooth": { "stroke_width": 4.0,
                "stroke_color": { "r": 0.0, "g": 0.0, "b": 1.0, "a": 1.0 },
                "fill_color": null, "pressure_curve": "linear" } }
        }});
        let rect = json!({ "shapestroke": {
            "shape": { "rect": { "cuboid": { "half_extents": [50.0, 20.0] },
                                 "affine": [1.0, 0.0, 0.0, 1.0, 300.0, 1300.0] } },
            "style": { "rough": { "stroke_width": 2.0,
                "stroke_color": { "r": 1.0, "g": 0.0, "b": 0.0, "a": 1.0 } } }
        }});
        let text = json!({ "textstroke": {
            "text": "Hallo",
            "affine": [1.0, 0.0, 0.0, 1.0, 50.0, 60.0],
            "text_style": { "font_family": "serif", "font_size": 32.0, "max_width": null,
                            "color": { "r": 0.0, "g": 0.0, "b": 0.0, "a": 1.0 } }
        }});
        let pixels = base64::engine::general_purpose::STANDARD.encode([128u8, 0, 0, 128].repeat(4));
        let bitmap = json!({ "bitmapimage": {
            "image": { "data": pixels, "pixel_width": 2, "pixel_height": 2,
                       "memory_format": "R8g8b8a8Premultiplied" },
            "rectangle": { "cuboid": { "half_extents": [10.0, 10.0] },
                           "affine": [1.0, 0.0, 0.0, 1.0, 400.0, 400.0] }
        }});
        let svg = json!({ "vectorimage": {
            "svg_data": "<svg xmlns='http://www.w3.org/2000/svg' width='10' height='10'><rect width='10' height='10' fill='green'/></svg>",
            "intrinsic_size": [10.0, 10.0],
            "rectangle": { "cuboid": { "half_extents": [40.0, 40.0] },
                           "affine": [1.0, 0.0, 0.0, 1.0, 400.0, 1500.0] }
        }});
        let marker = json!({ "brushstroke": {
            "path": { "start": { "pos": [10.0, 10.0], "pressure": 1.0 },
                      "segments": [ { "lineto": { "end": { "pos": [90.0, 10.0], "pressure": 1.0 } } } ] },
            "style": { "smooth": { "stroke_width": 12.0,
                "stroke_color": { "r": 1.0, "g": 1.0, "b": 0.0, "a": 1.0 },
                "fill_color": null, "pressure_curve": "const" } }
        }});
        let chrono = |t: u32, layer: &str| slot(json!({ "t": t, "layer": layer }));
        json!({
            "version": "0.15.0",
            "data": { "engine_snapshot": {
                "document": {
                    "config": {
                        "format": { "width": 793.701, "height": 1122.52, "dpi": 96.0 },
                        "background": { "pattern": "grid" },
                        "layout": layout
                    },
                    "x": 0.0, "y": 0.0, "width": 793.701, "height": 2245.04
                },
                "camera": {},
                // Slot 0 is the slot map's unused sentinel; the text was drawn first
                "stroke_components": [
                    { "value": null, "version": 0 },
                    slot(brush), slot(rect), slot(text), slot(bitmap), slot(svg), slot(marker)
                ],
                "chrono_components": [
                    { "value": null, "version": 0 },
                    chrono(2, "user_layer"), chrono(3, "user_layer"), chrono(1, "user_layer"),
                    chrono(4, "image"), chrono(5, "image"), chrono(6, "highlighter")
                ],
                "chrono_counter": 7
            }}
        })
    }

    fn write_rnote(dir: &Path, value: &Value) -> std::path::PathBuf {
        let path = dir.join("Skizze.rnote");
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(value.to_string().as_bytes()).unwrap();
        std::fs::write(&path, encoder.finish().unwrap()).unwrap();
        path
    }

    #[test]
    fn test_parse_rnote() {
        let doc = parse(sample("fixed_size").to_string().as_bytes()).unwrap();
        assert_eq!(doc.background, PageBackground::Grid5mm);
        assert!(!doc.endless);
        // In drawing order: the text was drawn first
        assert!(matches!(doc.items[0], RnoteItem::Text { .. }));
        match &doc.items[1] {
            RnoteItem::Ink {
                points,
                widths,
                color,
                highlighter,
            } => {
                assert!(!highlighter);
                assert_eq!(*color, [0.0, 0.0, 1.0, 1.0]);
                assert_eq!(points.first(), Some(&DVec2::new(100.0, 100.0)));
                assert_eq!(points.last(), Some(&DVec2::new(150.0, 100.0)));
                assert!(points.len() > 3, "the curve is sampled");
                assert_eq!(widths[0], 2.0);
                assert_eq!(widths[1], 4.0);
                assert_eq!(*widths.last().unwrap(), 1.0);
            }
            other => panic!("expected ink, got {other:?}"),
        }
        match &doc.items[2] {
            RnoteItem::Ink { points, .. } => {
                assert_eq!(points.len(), 5);
                assert_eq!(points[0], DVec2::new(250.0, 1280.0));
                assert_eq!(points[2], DVec2::new(350.0, 1320.0));
            }
            other => panic!("expected the rectangle, got {other:?}"),
        }
        match &doc.items[3] {
            RnoteItem::Bitmap { pixels, .. } => assert_eq!(&pixels[..4], &[255, 0, 0, 128]),
            other => panic!("expected a bitmap, got {other:?}"),
        }
        assert!(matches!(doc.items[4], RnoteItem::Svg { .. }));
        assert!(matches!(
            doc.items[5],
            RnoteItem::Ink {
                highlighter: true,
                ..
            }
        ));
    }

    #[test]
    fn test_older_formats() {
        // Before 0.13: format, background and layout directly in the document; before 0.15:
        // transformations as column-major 3×3 matrix
        let mut value = sample("continuous_vertical");
        value["version"] = json!("0.9.4");
        let document = &mut value["data"]["engine_snapshot"]["document"];
        let config = document["config"].take();
        for key in ["format", "background", "layout"] {
            document[key] = config[key].clone();
        }
        document.as_object_mut().unwrap().remove("config");
        let text =
            &mut value["data"]["engine_snapshot"]["stroke_components"][3]["value"]["textstroke"];
        text.as_object_mut().unwrap().remove("affine");
        text["transform"] = json!({ "affine": [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 50.0, 60.0, 1.0] });
        let doc = parse(value.to_string().as_bytes()).unwrap();
        assert_eq!(doc.background, PageBackground::Grid5mm);
        match &doc.items[0] {
            RnoteItem::Text { affine, .. } => assert_eq!(affine[2], DVec2::new(50.0, 60.0)),
            other => panic!("expected a text, got {other:?}"),
        }

        value["version"] = json!("0.5.5");
        assert!(parse(value.to_string().as_bytes()).is_err());
        assert!(parse(b"{\"a\": 1}").is_err());
    }

    #[test]
    fn test_create_notebook_from_rnote() {
        let temp_dir = tempfile::tempdir().unwrap();
        let source_dir = tempfile::tempdir().unwrap();
        let path = write_rnote(source_dir.path(), &sample("fixed_size"));
        let (notebook, manifest, dir, report) =
            create_notebook_from_rnote(temp_dir.path(), "Skizze", &path).unwrap();
        assert_eq!(report, ImportReport::default());
        assert_eq!(manifest.layout, NotebookManifest::new("x", "x").layout);

        let (mut reloaded, _) = crate::storage::load_notebook(&dir).unwrap();
        crate::storage::load_all_pages(&dir, &mut reloaded).unwrap();
        assert_eq!(notebook.pages.len(), 2);
        let [first, second] = &reloaded.pages[..] else {
            panic!("two pages expected");
        };
        assert_eq!(first.background, PageBackground::Grid5mm);
        // Text, pen, bitmap, highlighter on page 1; rectangle and SVG on page 2
        assert_eq!(first.strokes.len(), 4);
        assert_eq!(second.strokes.len(), 2);
        assert!(first.strokes[0].is_text());
        let pen = &first.strokes[1];
        assert_eq!(pen.tool, StrokeTool::Pen);
        // 96 dpi: 100 px = 26.46 mm
        assert!((pen.points[0].position.x - 264.6).abs() < 0.5);
        assert!(first.strokes[2].is_image());
        assert_eq!(first.strokes[3].tool, StrokeTool::Highlighter);
        // The rectangle sits on page 2 in page coordinates: 250 px - 0 / 1280 px - 1122.52 px
        let rect = &second.strokes[0];
        assert!((rect.points[0].position.x - 661.5).abs() < 0.5);
        assert!((rect.points[0].position.y - 416.7).abs() < 0.5);
        assert!(second.strokes[1].is_image());
    }

    #[test]
    fn test_infinite_documents_become_endless() {
        let temp_dir = tempfile::tempdir().unwrap();
        let source_dir = tempfile::tempdir().unwrap();
        let path = write_rnote(source_dir.path(), &sample("infinite"));
        let (notebook, manifest, _, _) =
            create_notebook_from_rnote(temp_dir.path(), "Skizze", &path).unwrap();
        assert_eq!(manifest.layout, Some(NotebookLayout::Endless));
        assert_eq!(notebook.pages.len(), 1);
        assert!(notebook.pages[0].is_endless());
        assert_eq!(notebook.pages[0].strokes.len(), 6);
        let pen = &notebook.pages[0].strokes[1];
        let home = crate::document::ENDLESS_HOME;
        assert!((pen.points[0].position.x - home.x - 264.6).abs() < 0.5);
    }
}
