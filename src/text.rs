//! Textfelder (Text-Werkzeug): Datenmodell, Zeilenumbruch und Rasterung.
//!
//! Ein Textfeld ist ein Objekt mit vier Ecken wie ein Bild (siehe `Stroke::text`). Der Text
//! wird in einem eigenen Koordinatensystem gesetzt – Breite = Umbruchbreite, Höhe = Zeilen ×
//! Zeilenhöhe, in Seiteneinheiten – und dieses Rechteck auf die Ecken abgebildet. So wirken
//! Verschieben, Drehen und Skalieren der Auswahl ohne Sonderfälle auf Text.
//!
//! Bildschirm (Textur), PDF-Export (eingebettete Schrift, durchsuchbar) und Bearbeitung (egui)
//! verwenden dieselben Schriften aus egui und denselben Zeilenumbruch.

use std::sync::OnceLock;

use crate::t;

use ab_glyph::{Font, FontRef, GlyphId, PxScale};
use serde::{Deserialize, Serialize};

use crate::assets::RgbaImage;
use crate::document::{Stroke, UNITS_PER_MM};

/// Schriftart eines Textfelds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum TextFont {
    /// Proportionalschrift (Ubuntu Light) für Überschriften und Notizen
    #[default]
    Sans,
    /// Festbreitenschrift (Hack) für Code
    Mono,
}

impl TextFont {
    pub fn label(self) -> &'static str {
        match self {
            TextFont::Sans => t!("Normal"),
            TextFont::Mono => t!("Code"),
        }
    }

    /// Name der Schrift in den egui-Standardschriften.
    fn egui_name(self) -> &'static str {
        match self {
            TextFont::Sans => "Ubuntu-Light",
            TextFont::Mono => "Hack",
        }
    }

    /// egui-Schriftfamilie, deren erste Schrift diese ist (für die Bearbeitung).
    pub fn egui_family(self) -> egui::FontFamily {
        match self {
            TextFont::Sans => egui::FontFamily::Proportional,
            TextFont::Mono => egui::FontFamily::Monospace,
        }
    }

    /// TrueType-Daten der Schrift (aus den in egui eingebauten Schriften).
    pub fn bytes(self) -> &'static [u8] {
        static DEFINITIONS: OnceLock<egui::FontDefinitions> = OnceLock::new();
        let definitions = DEFINITIONS.get_or_init(egui::FontDefinitions::default);
        &definitions.font_data[self.egui_name()].font
    }

    pub fn face(self) -> &'static FontRef<'static> {
        static SANS: OnceLock<FontRef<'static>> = OnceLock::new();
        static MONO: OnceLock<FontRef<'static>> = OnceLock::new();
        let cell = match self {
            TextFont::Sans => &SANS,
            TextFont::Mono => &MONO,
        };
        cell.get_or_init(|| {
            FontRef::try_from_slice(self.bytes()).expect("eingebaute egui-Schrift ist gültig")
        })
    }

    fn units_per_em(self) -> f32 {
        self.face().units_per_em().unwrap_or(1000.0)
    }

    /// Höhe von Ober- bis Unterlänge je Em: ab_glyph skaliert auf diese Höhe, PDF, egui und
    /// dieses Modul messen die Schriftgröße als Em.
    pub fn height_per_em(self) -> f32 {
        let face = self.face();
        (face.ascent_unscaled() - face.descent_unscaled()) / self.units_per_em()
    }
}

/// Seiteneinheiten je typografischem Punkt.
pub const UNITS_PER_PT: f32 = 25.4 / 72.0 * UNITS_PER_MM;
/// Wählbare Schriftgrößen in Punkt.
pub const TEXT_SIZES_PT: [f32; 7] = [8.0, 10.0, 12.0, 14.0, 18.0, 24.0, 36.0];
pub const DEFAULT_TEXT_SIZE_PT: f32 = 12.0;
/// Kleinste Umbruchbreite eines neuen Textfelds (3 cm).
pub const MIN_WRAP_WIDTH: f32 = 300.0;
/// Abstand neuer Textfelder zum rechten Blattrand (1 cm).
pub const RIGHT_MARGIN: f32 = 100.0;
/// Ein Tabulator wird durch so viele Leerzeichen ersetzt.
const TAB_WIDTH: usize = 4;

/// Inhalt und Satz eines Textfelds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextBox {
    pub content: String,
    /// Schriftgröße (Em) in Seiteneinheiten
    pub size: f32,
    #[serde(default)]
    pub font: TextFont,
    /// Umbruchbreite in Seiteneinheiten (Breite des ungedrehten, unskalierten Felds)
    pub wrap_width: f32,
}

/// Eine gesetzte Zeile: Zeichen (ohne Zeichen, die die Schrift nicht kennt) und ihre
/// Glyphen mit x-Position.
#[derive(Debug, Clone, PartialEq)]
pub struct LaidLine {
    pub text: String,
    pub glyphs: Vec<(GlyphId, f32)>,
    pub width: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TextLayout {
    pub lines: Vec<LaidLine>,
    /// Oberlänge (Abstand Zeilenoberkante → Grundlinie)
    pub ascent: f32,
    pub line_height: f32,
}

impl TextLayout {
    /// Höhe des Felds; ein leeres Feld ist eine Zeile hoch.
    pub fn height(&self) -> f32 {
        self.lines.len().max(1) as f32 * self.line_height
    }

    /// Grundlinie der Zeile `i` (von der Oberkante des Felds).
    pub fn baseline(&self, i: usize) -> f32 {
        i as f32 * self.line_height + self.ascent
    }
}

impl TextBox {
    pub fn new(content: String, size: f32, font: TextFont, wrap_width: f32) -> Self {
        Self {
            content,
            size,
            font,
            wrap_width,
        }
    }

    /// Größe des ungedrehten, unskalierten Felds in Seiteneinheiten.
    pub fn box_size(&self) -> glam::Vec2 {
        glam::Vec2::new(self.wrap_width.max(self.size), self.layout().height())
    }

    /// Width of the field while it is being edited: as wide as the longest line plus room for
    /// the next character, at most the wrap width, so a new field starts short (Issue #34).
    pub fn edit_width(&self) -> f32 {
        let widest = self
            .layout()
            .lines
            .iter()
            .map(|line| line.width)
            .fold(0.0, f32::max);
        (widest + self.size)
            .max(3.0 * self.size)
            .min(self.wrap_width.max(self.size))
    }

    /// Setzt den Text mit Zeilenumbruch an Leerzeichen; zu lange Wörter werden zeichenweise
    /// umbrochen, Zeilenumbrüche im Text bleiben erhalten.
    pub fn layout(&self) -> TextLayout {
        let face = self.font.face();
        let em = self.size / self.font.units_per_em();
        let line_height =
            (face.ascent_unscaled() - face.descent_unscaled() + face.line_gap_unscaled()) * em;
        let width = self.wrap_width.max(self.size);

        let mut lines = Vec::new();
        for paragraph in self.content.split('\n') {
            let mut chars: Vec<(char, GlyphId, f32)> = paragraph
                .chars()
                .flat_map(|c| {
                    let (c, n) = if c == '\t' { (' ', TAB_WIDTH) } else { (c, 1) };
                    std::iter::repeat_n(c, n)
                })
                .filter_map(|c| {
                    let id = face.glyph_id(c);
                    (id.0 != 0).then(|| (c, id, face.h_advance_unscaled(id) * em))
                })
                .collect();
            // Unterschneidung (wie egui) dem vorderen Zeichen des Paars zurechnen
            for i in 1..chars.len() {
                chars[i - 1].2 += face.kern_unscaled(chars[i - 1].1, chars[i].1) * em;
            }
            let mut start = 0;
            let mut x = 0.0;
            // Letztes Leerzeichen der Zeile (möglicher Umbruch)
            let mut last_space: Option<usize> = None;
            for i in 0..chars.len() {
                let (c, _, advance) = chars[i];
                if c != ' ' && x + advance > width && i > start {
                    let end = match last_space {
                        Some(space) if space > start => space,
                        _ => i,
                    };
                    lines.push(laid_line(&chars[start..end]));
                    start = end;
                    while start < i && chars[start].0 == ' ' {
                        start += 1;
                    }
                    x = chars[start..i].iter().map(|g| g.2).sum();
                    last_space = None;
                }
                x += advance;
                if c == ' ' {
                    last_space = Some(i);
                }
            }
            lines.push(laid_line(&chars[start..]));
        }
        // Leere Zeilen am Ende zählen mit (Platz für den Cursor), ein leerer Text ist eine Zeile
        TextLayout {
            lines,
            ascent: face.ascent_unscaled() * em,
            line_height,
        }
    }
}

fn laid_line(chars: &[(char, GlyphId, f32)]) -> LaidLine {
    let mut x = 0.0;
    let mut glyphs = Vec::with_capacity(chars.len());
    let mut width: f32 = 0.0;
    for &(c, id, advance) in chars {
        glyphs.push((id, x));
        x += advance;
        if c != ' ' {
            width = x;
        }
    }
    LaidLine {
        text: chars.iter().map(|g| g.0).collect(),
        glyphs,
        width,
    }
}

/// Ecken eines Felds der Größe `size` mit Ursprung (oben links) `origin` und den Achsen
/// `axes` (Seitenvektor je Einheit nach rechts bzw. unten): oben links, oben rechts,
/// unten rechts, unten links.
pub fn frame_corners(
    origin: glam::Vec2,
    axes: [glam::Vec2; 2],
    size: glam::Vec2,
) -> [glam::Vec2; 4] {
    let right = axes[0] * size.x;
    let down = axes[1] * size.y;
    [origin, origin + right, origin + right + down, origin + down]
}

/// Ursprung und Achsen (je Einheit) eines Felds der Größe `size` mit den Ecken `corners`.
pub fn frame_axes(corners: &[glam::Vec2; 4], size: glam::Vec2) -> (glam::Vec2, [glam::Vec2; 2]) {
    let [tl, tr, _, bl] = *corners;
    (
        tl,
        [(tr - tl) / size.x.max(1e-3), (bl - tl) / size.y.max(1e-3)],
    )
}

/// Laufende Bearbeitung eines Textfelds (neu oder vorhanden).
#[derive(Debug, Clone, PartialEq)]
pub struct TextEditing {
    /// Seite, auf der das Feld liegt
    pub page_id: u64,
    /// Bearbeitetes vorhandenes Feld im Zustand vor der Bearbeitung; `None` bei einem neuen
    pub original: Option<Stroke>,
    /// Ecke oben links und Achsen (Seitenvektor je Einheit nach rechts bzw. unten)
    pub origin: glam::Vec2,
    pub axes: [glam::Vec2; 2],
    pub text: TextBox,
    pub color: [f32; 4],
    /// Beim nächsten Bild den Tastaturfokus auf das Feld setzen
    pub focus: bool,
    /// Bearbeitung beenden (Esc)
    pub finish: bool,
}

impl TextEditing {
    /// Neues, leeres Feld: die erste Zeile mittig auf `pos`, umbrochen 1 cm vor dem rechten
    /// Rand eines Blatts der Breite `page_width` (mindestens 3 cm breit).
    pub fn new_at(
        page_id: u64,
        pos: glam::Vec2,
        page_width: f32,
        size: f32,
        font: TextFont,
        color: [f32; 4],
    ) -> Self {
        let right = page_width - RIGHT_MARGIN;
        let x = pos.x.min(right - MIN_WRAP_WIDTH).max(0.0);
        let text = TextBox::new(String::new(), size, font, (right - x).max(MIN_WRAP_WIDTH));
        let line_height = text.layout().line_height;
        Self {
            page_id,
            original: None,
            origin: glam::Vec2::new(x, (pos.y - line_height / 2.0).max(0.0)),
            axes: [glam::Vec2::X, glam::Vec2::Y],
            text,
            color,
            focus: true,
            finish: false,
        }
    }

    /// Bearbeitung des vorhandenen Textfelds `stroke`.
    pub fn of_stroke(page_id: u64, stroke: &Stroke) -> Option<Self> {
        let text = stroke.text.clone()?;
        let corners = stroke.object_corners()?;
        let (origin, axes) = frame_axes(&corners, text.box_size());
        Some(Self {
            page_id,
            original: Some(stroke.clone()),
            origin,
            axes,
            text,
            color: stroke.color,
            focus: true,
            finish: false,
        })
    }

    /// Wurde gegenüber dem Ausgangszustand etwas geändert?
    pub fn is_changed(&self) -> bool {
        match &self.original {
            Some(original) => {
                original.text.as_ref() != Some(&self.text) || original.color != self.color
            }
            None => !self.is_empty(),
        }
    }

    /// Enthält das Feld nur Leerraum?
    pub fn is_empty(&self) -> bool {
        self.text.content.trim().is_empty()
    }

    /// Ergebnis als Textfeld mit der ID `id`; die Höhe passt sich dem Text an, Lage,
    /// Drehung und Skalierung bleiben. `None` für ein leeres Feld.
    pub fn to_stroke(&self, id: u64) -> Option<Stroke> {
        if self.is_empty() {
            return None;
        }
        let corners = frame_corners(self.origin, self.axes, self.text.box_size());
        Some(Stroke::new_text(id, self.text.clone(), corners, self.color))
    }
}

/// Exponent für die Kantendeckung der Glyphen (wie egui, damit Bearbeitung und Anzeige
/// gleich kräftig wirken).
const COVERAGE_GAMMA: f32 = 0.55;

/// Größte Kantenlänge einer Texttextur in Pixeln.
const MAX_TEXTURE_SIDE: f32 = 4096.0;
/// Größte Pixelzahl einer Texttextur (64 MB RGBA).
const MAX_TEXTURE_PIXELS: f32 = 16.0 * 1024.0 * 1024.0;

/// Rastert ein Textfeld mit `px_per_unit` Pixeln je Seiteneinheit (begrenzt auf eine
/// handhabbare Texturgröße). `color` ist sRGB wie die Tinte, ebenso die Pixel.
pub fn rasterize(text: &TextBox, color: [f32; 4], px_per_unit: f32) -> RgbaImage {
    let layout = text.layout();
    let size = glam::Vec2::new(text.wrap_width.max(text.size), layout.height());
    let scale = px_per_unit
        .min(MAX_TEXTURE_SIDE / size.x)
        .min(MAX_TEXTURE_SIDE / size.y)
        .min((MAX_TEXTURE_PIXELS / (size.x * size.y)).sqrt())
        .max(1e-3);
    let width = ((size.x * scale).ceil() as u32).max(1);
    let height = ((size.y * scale).ceil() as u32).max(1);

    let face = text.font.face();
    // ab_glyph misst die Skalierung als Höhe Ober- bis Unterlänge
    let px_scale = PxScale::from(text.size * scale * text.font.height_per_em());
    let mut coverage = vec![0.0_f32; (width * height) as usize];
    for (i, line) in layout.lines.iter().enumerate() {
        let baseline = layout.baseline(i) * scale;
        for &(id, x) in &line.glyphs {
            let glyph = id.with_scale_and_position(px_scale, ab_glyph::point(x * scale, baseline));
            let Some(outlined) = face.outline_glyph(glyph) else {
                continue;
            };
            let bounds = outlined.px_bounds();
            outlined.draw(|gx, gy, c| {
                let px = bounds.min.x as i64 + gx as i64;
                let py = bounds.min.y as i64 + gy as i64;
                if px >= 0 && py >= 0 && (px as u32) < width && (py as u32) < height {
                    let cell = &mut coverage[(py as u32 * width + px as u32) as usize];
                    *cell = (*cell + c).min(1.0);
                }
            });
        }
    }

    let rgb = [0, 1, 2].map(|i| (color[i].clamp(0.0, 1.0) * 255.0).round() as u8);
    let alpha = color[3].clamp(0.0, 1.0);
    let mut pixels = Vec::with_capacity(coverage.len() * 4);
    for c in coverage {
        pixels.extend_from_slice(&rgb);
        // Wie egui: Deckung anheben, sonst wirkt die dünne Schrift verkleinert blass
        pixels.push((c.powf(COVERAGE_GAMMA) * alpha * 255.0).round() as u8);
    }
    RgbaImage {
        width,
        height,
        pixels,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::PAGE_WIDTH;

    fn sample(content: &str, wrap_width: f32) -> TextBox {
        TextBox::new(content.to_string(), 50.0, TextFont::Sans, wrap_width)
    }

    #[test]
    fn test_edit_width_grows_with_the_text() {
        let empty = sample("", 1500.0);
        assert!((empty.edit_width() - 150.0).abs() < 1e-3);
        let short = sample("Hallo", 1500.0);
        let longer = sample("Hallo Welt, wie geht es", 1500.0);
        assert!(short.edit_width() < longer.edit_width());
        assert!(longer.edit_width() < 1500.0);
        // Long text wraps at the page edge and fills the full width
        let long = sample(&"Wort ".repeat(100), 1500.0);
        assert!((long.edit_width() - 1500.0).abs() < 1e-3);
    }

    #[test]
    fn test_fonts_load() {
        for font in [TextFont::Sans, TextFont::Mono] {
            assert!(font.face().glyph_id('ä').0 != 0);
            let factor = font.height_per_em();
            assert!((1.0..1.4).contains(&factor), "{factor}");
        }
    }

    #[test]
    fn test_layout_wraps_at_spaces() {
        let text = sample("eins zwei drei vier fünf sechs", 400.0);
        let layout = text.layout();
        assert!(layout.lines.len() > 1);
        for line in &layout.lines {
            assert!(line.width <= 400.0 + 1e-3, "{line:?}");
            assert!(!line.text.starts_with(' ') && !line.text.ends_with(' '));
        }
        let words: Vec<String> = layout.lines.iter().map(|l| l.text.clone()).collect();
        assert_eq!(words.join(" "), "eins zwei drei vier fünf sechs");
    }

    #[test]
    fn test_layout_breaks_long_words_and_keeps_newlines() {
        let text = sample("Donaudampfschifffahrtsgesellschaft\n\nende", 300.0);
        let layout = text.layout();
        assert!(layout.lines.iter().all(|l| l.width <= 300.0 + 1e-3));
        let joined: String = layout.lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(joined, "Donaudampfschifffahrtsgesellschaftende");
        // Leere Zeile bleibt erhalten
        assert!(layout.lines.iter().any(|l| l.text.is_empty()));
        assert_eq!(layout.lines.last().unwrap().text, "ende");
    }

    #[test]
    fn test_empty_text_is_one_line_high() {
        let layout = sample("", 500.0).layout();
        assert!((layout.height() - layout.line_height).abs() < 1e-3);
        assert!(layout.line_height > 50.0);
    }

    #[test]
    fn test_tabs_become_spaces_and_indent_is_kept() {
        let text = TextBox::new("\tx".into(), 50.0, TextFont::Mono, 2000.0);
        let line = &text.layout().lines[0];
        assert_eq!(line.text, "    x");
        // Festbreitenschrift: x steht vier Zeichenbreiten weiter
        let advance = line.glyphs[1].1;
        assert!((line.glyphs[4].1 - 4.0 * advance).abs() < 1e-3);
    }

    #[test]
    fn test_frame_round_trip() {
        let size = glam::Vec2::new(400.0, 120.0);
        let axes = [glam::Vec2::new(0.0, 2.0), glam::Vec2::new(-2.0, 0.0)];
        let corners = frame_corners(glam::Vec2::new(100.0, 50.0), axes, size);
        let (origin, back) = frame_axes(&corners, size);
        assert_eq!(origin, glam::Vec2::new(100.0, 50.0));
        assert!((back[0] - axes[0]).length() < 1e-4 && (back[1] - axes[1]).length() < 1e-4);
    }

    #[test]
    fn test_rasterize_draws_ink_in_color() {
        let text = sample("Hallo", 400.0);
        let image = rasterize(&text, [0.85, 0.2, 0.2, 1.0], 1.0);
        assert_eq!(image.width, 400);
        assert_eq!(image.height as f32, text.layout().height().ceil());
        let opaque = image.pixels.chunks(4).filter(|p| p[3] > 200).count();
        assert!(opaque > 100, "{opaque}");
        // Die Farbe ist schon sRGB und wird unverändert übernommen
        assert!(image.pixels.chunks(4).all(|p| p[..3] == [217, 51, 51]));
    }

    #[test]
    fn test_editing_new_text_box() {
        let edit = TextEditing::new_at(
            1,
            glam::Vec2::new(500.0, 800.0),
            PAGE_WIDTH,
            50.0,
            TextFont::Sans,
            [0.0; 4],
        );
        assert!((edit.text.wrap_width - (PAGE_WIDTH - RIGHT_MARGIN - 500.0)).abs() < 1e-3);
        assert!(edit.origin.y < 800.0);
        assert!(!edit.is_changed());
        assert!(edit.to_stroke(9).is_none(), "leeres Feld wird verworfen");

        // Ganz rechts: Feld wird nach links versetzt, damit es mindestens 3 cm breit ist
        let edge = TextEditing::new_at(
            1,
            glam::Vec2::new(2050.0, 100.0),
            PAGE_WIDTH,
            50.0,
            TextFont::Sans,
            [0.0; 4],
        );
        assert!((edge.text.wrap_width - MIN_WRAP_WIDTH).abs() < 1e-3);
        assert!(edge.origin.x + MIN_WRAP_WIDTH <= PAGE_WIDTH);
    }

    #[test]
    fn test_editing_keeps_transform_and_grows_with_text() {
        let mut edit = TextEditing::new_at(
            1,
            glam::Vec2::new(400.0, 400.0),
            PAGE_WIDTH,
            50.0,
            TextFont::Sans,
            [0.0; 4],
        );
        edit.text.content = "eine Zeile".into();
        let mut stroke = edit.to_stroke(5).unwrap();
        assert!(stroke.is_text() && stroke.is_object());
        stroke.rotate(glam::Vec2::new(400.0, 400.0), 0.5);
        stroke.scale(glam::Vec2::new(400.0, 400.0), glam::Vec2::new(2.0, 2.0));
        let before = stroke.object_corners().unwrap();

        let mut edit = TextEditing::of_stroke(1, &stroke).unwrap();
        assert!(!edit.is_changed());
        edit.text.content = "eine Zeile\nzwei Zeilen".into();
        assert!(edit.is_changed());
        let after = edit.to_stroke(5).unwrap().object_corners().unwrap();
        // Obere Kante bleibt, die Seitenkanten werden in ihrer Richtung doppelt so lang
        assert!((after[0] - before[0]).length() < 1e-2 && (after[1] - before[1]).length() < 1e-2);
        let (old_side, new_side) = (before[3] - before[0], after[3] - after[0]);
        assert!((new_side.length() / old_side.length() - 2.0).abs() < 1e-3);
        assert!(old_side.normalize().dot(new_side.normalize()) > 0.9999);
    }

    #[test]
    fn test_rasterize_limits_texture_size() {
        let text = sample("x", 2000.0);
        let image = rasterize(&text, [0.0, 0.0, 0.0, 1.0], 100.0);
        assert!(image.width as f32 <= MAX_TEXTURE_SIDE);
    }
}
