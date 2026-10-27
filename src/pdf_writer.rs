//! Small PDF writer on top of `lopdf` for the vector export.
//!
//! Replaces printpdf 0.7, which still depended on lopdf 0.31. The interface follows the part
//! of printpdf that [`crate::export`] used: a document with pages, each page with named
//! layers (optional content groups, shown as layers in PDF viewers) that collect drawing
//! operations, embedded TrueType fonts for real, searchable text, images and bookmarks.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::rc::Rc;

use ab_glyph::Font as _;
use lopdf::content::{Content, Operation};
use lopdf::{Dictionary, Document, Object, ObjectId, Stream, StringFormat};

/// Length in millimetres.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct Mm(pub f32);

/// Length in PDF points (1/72 inch).
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct Pt(pub f32);

impl From<Mm> for Pt {
    fn from(mm: Mm) -> Self {
        Pt(mm.0 * 72.0 / 25.4)
    }
}

/// Size in pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Px(pub usize);

/// Point on the page (origin bottom left).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    pub x: Pt,
    pub y: Pt,
}

impl Point {
    pub fn new(x: Mm, y: Mm) -> Self {
        Self {
            x: x.into(),
            y: y.into(),
        }
    }
}

/// Open or closed polyline; a point marked `true` followed by another marked `true` starts a
/// cubic Bézier segment (the two are its control points, the next one its end point).
#[derive(Debug, Clone)]
pub struct Line {
    pub points: Vec<(Point, bool)>,
    pub is_closed: bool,
}

/// How a polygon is painted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaintMode {
    Fill,
    Stroke,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindingOrder {
    NonZero,
}

/// Closed shape of one or more rings (same point marking as [`Line`]).
#[derive(Debug, Clone)]
pub struct Polygon {
    pub rings: Vec<Vec<(Point, bool)>>,
    pub mode: PaintMode,
    pub winding_order: WindingOrder,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rgb {
    pub r: f32,
    pub g: f32,
    pub b: f32,
}

impl Rgb {
    /// The last argument (ICC profile in printpdf) is ignored.
    pub fn new(r: f32, g: f32, b: f32, _icc: Option<()>) -> Self {
        Self { r, g, b }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Color {
    Rgb(Rgb),
}

/// Change of the coordinate system (`cm`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CurTransMat {
    Translate(Pt, Pt),
    Raw([f32; 6]),
}

/// Text position within a text section (`Tm`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TextMatrix {
    Translate(Pt, Pt),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineCapStyle {
    Butt,
    Round,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineJoinStyle {
    Miter,
    Round,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorSpace {
    Rgb,
    Greyscale,
}

/// Image data already compressed in this format (embedded unchanged).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFilter {
    /// JPEG
    DCT,
}

/// Image with 8 bits per component.
#[derive(Debug, Clone)]
pub struct ImageXObject {
    pub width: Px,
    pub height: Px,
    pub color_space: ColorSpace,
    pub interpolate: bool,
    /// Raw samples, or the compressed data for `image_filter`
    pub image_data: Vec<u8>,
    pub image_filter: Option<ImageFilter>,
}

/// Font embedded with [`PdfDocumentReference::add_external_font`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndirectFontRef {
    index: usize,
}

impl IndirectFontRef {
    fn resource_name(&self) -> String {
        format!("F{}", self.index)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PdfPageIndex(pub usize);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PdfLayerIndex(pub usize);

struct LayerData {
    name: String,
    operations: Vec<Operation>,
    /// Font of the current text section (to record the glyphs it uses)
    current_font: Option<usize>,
}

struct PageData {
    width: Pt,
    height: Pt,
    layers: Vec<LayerData>,
    images: Vec<ImageXObject>,
}

struct FontData {
    face: ab_glyph::FontRef<'static>,
    bytes: &'static [u8],
    used_glyphs: BTreeSet<u16>,
}

struct DocumentData {
    title: String,
    pages: Vec<PageData>,
    fonts: Vec<FontData>,
    bookmarks: Vec<(String, usize)>,
}

/// Entry point like printpdf's `PdfDocument::new`.
pub struct PdfDocument;

impl PdfDocument {
    /// New document with a first page of the given size and its first layer.
    #[allow(clippy::new_ret_no_self)]
    pub fn new(
        title: &str,
        width: Mm,
        height: Mm,
        layer: &str,
    ) -> (PdfDocumentReference, PdfPageIndex, PdfLayerIndex) {
        let doc = PdfDocumentReference(Rc::new(RefCell::new(DocumentData {
            title: title.to_string(),
            pages: Vec::new(),
            fonts: Vec::new(),
            bookmarks: Vec::new(),
        })));
        let (page, layer) = doc.add_page(width, height, layer);
        (doc, page, layer)
    }
}

/// The document being built; cheap to clone, all clones share it.
#[derive(Clone)]
pub struct PdfDocumentReference(Rc<RefCell<DocumentData>>);

impl PdfDocumentReference {
    /// Appends a page with its first layer.
    pub fn add_page(&self, width: Mm, height: Mm, layer: &str) -> (PdfPageIndex, PdfLayerIndex) {
        let mut doc = self.0.borrow_mut();
        doc.pages.push(PageData {
            width: width.into(),
            height: height.into(),
            layers: vec![LayerData {
                name: layer.to_string(),
                operations: Vec::new(),
                current_font: None,
            }],
            images: Vec::new(),
        });
        (PdfPageIndex(doc.pages.len() - 1), PdfLayerIndex(0))
    }

    /// Bookmark (outline entry) pointing at a page.
    pub fn add_bookmark(&self, title: String, page: PdfPageIndex) {
        self.0.borrow_mut().bookmarks.push((title, page.0));
    }

    pub fn get_page(&self, page: PdfPageIndex) -> PdfPageReference {
        PdfPageReference {
            doc: self.clone(),
            page: page.0,
        }
    }

    /// Embeds a TrueType/OpenType font with TrueType outlines (Type0 font, Identity-H).
    pub fn add_external_font(
        &self,
        bytes: &'static [u8],
    ) -> Result<IndirectFontRef, Box<dyn Error>> {
        let face = ab_glyph::FontRef::try_from_slice(bytes)?;
        let mut doc = self.0.borrow_mut();
        doc.fonts.push(FontData {
            face,
            bytes,
            used_glyphs: BTreeSet::new(),
        });
        Ok(IndirectFontRef {
            index: doc.fonts.len() - 1,
        })
    }

    /// Writes the document.
    pub fn save_to_bytes(self) -> Result<Vec<u8>, Box<dyn Error>> {
        let data = self.0.borrow();
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();

        // Fonts: shared by all pages
        let mut font_resources = Dictionary::new();
        for (index, font) in data.fonts.iter().enumerate() {
            let id = write_font(&mut doc, font, index)?;
            font_resources.set(format!("F{index}"), Object::Reference(id));
        }

        // One optional content group per layer name, in the order of appearance
        let mut layer_names: Vec<&str> = Vec::new();
        for layer in data.pages.iter().flat_map(|p| &p.layers) {
            if !layer_names.contains(&layer.name.as_str()) {
                layer_names.push(&layer.name);
            }
        }
        let ocgs: Vec<ObjectId> = layer_names
            .iter()
            .map(|name| {
                let mut ocg = Dictionary::new();
                ocg.set("Type", Object::Name(b"OCG".to_vec()));
                ocg.set("Name", text_string(name));
                doc.add_object(ocg)
            })
            .collect();

        let mut page_ids = Vec::new();
        for page in &data.pages {
            let mut operations = Vec::new();
            let mut properties = Dictionary::new();
            for layer in &page.layers {
                let index = layer_names
                    .iter()
                    .position(|n| *n == layer.name)
                    .unwrap_or(0);
                let tag = format!("MC{index}");
                properties.set(tag.clone(), Object::Reference(ocgs[index]));
                operations.push(Operation::new(
                    "BDC",
                    vec![Object::Name(b"OC".to_vec()), Object::Name(tag.into_bytes())],
                ));
                operations.push(Operation::new("q", vec![]));
                operations.extend(layer.operations.iter().cloned());
                operations.push(Operation::new("Q", vec![]));
                operations.push(Operation::new("EMC", vec![]));
            }
            let mut contents = Stream::new(Dictionary::new(), Content { operations }.encode()?);
            contents.compress()?;
            let contents_id = doc.add_object(contents);

            let mut xobjects = Dictionary::new();
            for (i, image) in page.images.iter().enumerate() {
                let id = doc.add_object(image_stream(image)?);
                xobjects.set(format!("X{i}"), Object::Reference(id));
            }
            let mut resources = Dictionary::new();
            resources.set("Font", Object::Dictionary(font_resources.clone()));
            resources.set("XObject", Object::Dictionary(xobjects));
            resources.set("Properties", Object::Dictionary(properties));
            let resources_id = doc.add_object(resources);

            let mut dict = Dictionary::new();
            dict.set("Type", Object::Name(b"Page".to_vec()));
            dict.set("Parent", Object::Reference(pages_id));
            dict.set(
                "MediaBox",
                Object::Array(vec![
                    0.into(),
                    0.into(),
                    page.width.0.into(),
                    page.height.0.into(),
                ]),
            );
            dict.set("Contents", Object::Reference(contents_id));
            dict.set("Resources", Object::Reference(resources_id));
            page_ids.push(doc.add_object(dict));
        }

        let mut pages = Dictionary::new();
        pages.set("Type", Object::Name(b"Pages".to_vec()));
        pages.set(
            "Kids",
            Object::Array(page_ids.iter().map(|id| Object::Reference(*id)).collect()),
        );
        pages.set("Count", Object::Integer(page_ids.len() as i64));
        doc.objects.insert(pages_id, Object::Dictionary(pages));

        let mut catalog = Dictionary::new();
        catalog.set("Type", Object::Name(b"Catalog".to_vec()));
        catalog.set("Pages", Object::Reference(pages_id));
        let ocg_refs: Vec<Object> = ocgs.iter().map(|id| Object::Reference(*id)).collect();
        let mut default_config = Dictionary::new();
        default_config.set("Order", Object::Array(ocg_refs.clone()));
        let mut oc_properties = Dictionary::new();
        oc_properties.set("OCGs", Object::Array(ocg_refs));
        oc_properties.set("D", Object::Dictionary(default_config));
        catalog.set("OCProperties", Object::Dictionary(oc_properties));
        if !data.bookmarks.is_empty() {
            let outlines = write_outlines(&mut doc, &data.bookmarks, &page_ids);
            catalog.set("Outlines", Object::Reference(outlines));
            catalog.set("PageMode", Object::Name(b"UseOutlines".to_vec()));
        }
        let catalog_id = doc.add_object(catalog);

        let mut info = Dictionary::new();
        info.set("Title", text_string(&data.title));
        info.set("Producer", text_string("ink-notes"));
        let info_id = doc.add_object(info);

        doc.trailer.set("Root", Object::Reference(catalog_id));
        doc.trailer.set("Info", Object::Reference(info_id));
        let mut out = Vec::new();
        doc.save_to(&mut out)?;
        Ok(out)
    }
}

/// PDF text string: literal for ASCII, otherwise UTF-16BE with byte order mark.
pub fn text_string(text: &str) -> Object {
    if text.is_ascii() {
        return Object::String(text.as_bytes().to_vec(), StringFormat::Literal);
    }
    let mut bytes = vec![0xFE, 0xFF];
    for unit in text.encode_utf16() {
        bytes.extend_from_slice(&unit.to_be_bytes());
    }
    Object::String(bytes, StringFormat::Hexadecimal)
}

/// Outline tree with one entry per bookmark; returns the outline root.
fn write_outlines(
    doc: &mut Document,
    bookmarks: &[(String, usize)],
    pages: &[ObjectId],
) -> ObjectId {
    let root_id = doc.new_object_id();
    let ids: Vec<ObjectId> = bookmarks.iter().map(|_| doc.new_object_id()).collect();
    for (i, (title, page)) in bookmarks.iter().enumerate() {
        let mut item = Dictionary::new();
        item.set("Title", text_string(title));
        item.set("Parent", Object::Reference(root_id));
        if i > 0 {
            item.set("Prev", Object::Reference(ids[i - 1]));
        }
        if let Some(next) = ids.get(i + 1) {
            item.set("Next", Object::Reference(*next));
        }
        if let Some(page_id) = pages.get(*page) {
            item.set(
                "Dest",
                Object::Array(vec![
                    Object::Reference(*page_id),
                    Object::Name(b"Fit".to_vec()),
                ]),
            );
        }
        doc.objects.insert(ids[i], Object::Dictionary(item));
    }
    let mut root = Dictionary::new();
    root.set("Type", Object::Name(b"Outlines".to_vec()));
    root.set("First", Object::Reference(ids[0]));
    root.set("Last", Object::Reference(ids[ids.len() - 1]));
    root.set("Count", Object::Integer(ids.len() as i64));
    doc.objects.insert(root_id, Object::Dictionary(root));
    root_id
}

/// Image XObject stream.
fn image_stream(image: &ImageXObject) -> Result<Stream, Box<dyn Error>> {
    let mut dict = Dictionary::new();
    dict.set("Type", Object::Name(b"XObject".to_vec()));
    dict.set("Subtype", Object::Name(b"Image".to_vec()));
    dict.set("Width", Object::Integer(image.width.0 as i64));
    dict.set("Height", Object::Integer(image.height.0 as i64));
    let color_space: &[u8] = match image.color_space {
        ColorSpace::Rgb => b"DeviceRGB",
        ColorSpace::Greyscale => b"DeviceGray",
    };
    dict.set("ColorSpace", Object::Name(color_space.to_vec()));
    dict.set("BitsPerComponent", Object::Integer(8));
    dict.set("Interpolate", Object::Boolean(image.interpolate));
    match image.image_filter {
        Some(ImageFilter::DCT) => {
            dict.set("Filter", Object::Name(b"DCTDecode".to_vec()));
            Ok(Stream::new(dict, image.image_data.clone()).with_compression(false))
        }
        None => {
            let mut stream = Stream::new(dict, image.image_data.clone());
            stream.compress()?;
            Ok(stream)
        }
    }
}

/// Embeds a font as Type0 font with a CIDFontType2 descendant (glyph ids as character codes),
/// widths and a ToUnicode map of the glyphs used, so that the text can be searched and copied.
fn write_font(
    doc: &mut Document,
    font: &FontData,
    index: usize,
) -> Result<ObjectId, Box<dyn Error>> {
    let face = &font.face;
    let units_per_em = face.units_per_em().unwrap_or(1000.0);
    let scale = 1000.0 / units_per_em;
    let name = format!("InkNotesFont{index}");

    let mut file = Stream::new(
        Dictionary::from_iter(vec![("Length1", Object::Integer(font.bytes.len() as i64))]),
        font.bytes.to_vec(),
    );
    file.compress()?;
    let file_id = doc.add_object(file);

    let ascent = (face.ascent_unscaled() * scale).round() as i64;
    let descent = (face.descent_unscaled() * scale).round() as i64;
    let mut descriptor = Dictionary::new();
    descriptor.set("Type", Object::Name(b"FontDescriptor".to_vec()));
    descriptor.set("FontName", Object::Name(name.clone().into_bytes()));
    descriptor.set("Flags", Object::Integer(32));
    descriptor.set(
        "FontBBox",
        Object::Array(vec![0.into(), descent.into(), 1000.into(), ascent.into()]),
    );
    descriptor.set("ItalicAngle", Object::Integer(0));
    descriptor.set("Ascent", Object::Integer(ascent));
    descriptor.set("Descent", Object::Integer(descent));
    descriptor.set("CapHeight", Object::Integer(ascent));
    descriptor.set("StemV", Object::Integer(80));
    descriptor.set("FontFile2", Object::Reference(file_id));
    let descriptor_id = doc.add_object(descriptor);

    // Widths of the glyphs used: gid [width]
    let mut widths = Vec::new();
    for &gid in &font.used_glyphs {
        let width = face.h_advance_unscaled(ab_glyph::GlyphId(gid)) * scale;
        widths.push(Object::Integer(gid as i64));
        widths.push(Object::Array(vec![Object::Integer(width.round() as i64)]));
    }
    let mut system_info = Dictionary::new();
    system_info.set("Registry", Object::string_literal("Adobe"));
    system_info.set("Ordering", Object::string_literal("Identity"));
    system_info.set("Supplement", Object::Integer(0));
    let mut descendant = Dictionary::new();
    descendant.set("Type", Object::Name(b"Font".to_vec()));
    descendant.set("Subtype", Object::Name(b"CIDFontType2".to_vec()));
    descendant.set("BaseFont", Object::Name(name.clone().into_bytes()));
    descendant.set("CIDSystemInfo", Object::Dictionary(system_info));
    descendant.set("FontDescriptor", Object::Reference(descriptor_id));
    descendant.set("CIDToGIDMap", Object::Name(b"Identity".to_vec()));
    descendant.set("DW", Object::Integer(1000));
    descendant.set("W", Object::Array(widths));
    let descendant_id = doc.add_object(descendant);

    let mut to_unicode = Stream::new(Dictionary::new(), to_unicode_cmap(font).into_bytes());
    to_unicode.compress()?;
    let to_unicode_id = doc.add_object(to_unicode);

    let mut dict = Dictionary::new();
    dict.set("Type", Object::Name(b"Font".to_vec()));
    dict.set("Subtype", Object::Name(b"Type0".to_vec()));
    dict.set("BaseFont", Object::Name(name.into_bytes()));
    dict.set("Encoding", Object::Name(b"Identity-H".to_vec()));
    dict.set(
        "DescendantFonts",
        Object::Array(vec![Object::Reference(descendant_id)]),
    );
    dict.set("ToUnicode", Object::Reference(to_unicode_id));
    Ok(doc.add_object(dict))
}

/// CMap from the glyph ids used to their characters (UTF-16BE).
fn to_unicode_cmap(font: &FontData) -> String {
    let mut chars: BTreeMap<u16, char> = BTreeMap::new();
    for (gid, c) in font.face.codepoint_ids() {
        if font.used_glyphs.contains(&gid.0) {
            // Prefer the lowest code point (e.g. a letter over a compatibility form)
            let entry = chars.entry(gid.0).or_insert(c);
            if c < *entry {
                *entry = c;
            }
        }
    }
    let mut cmap = String::from(
        "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n\
         /CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n\
         /CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n\
         1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n",
    );
    let entries: Vec<(u16, char)> = chars.into_iter().collect();
    for block in entries.chunks(100) {
        cmap.push_str(&format!("{} beginbfchar\n", block.len()));
        for (gid, c) in block {
            let mut units = [0u16; 2];
            let hex: String = c
                .encode_utf16(&mut units)
                .iter()
                .map(|u| format!("{u:04X}"))
                .collect();
            cmap.push_str(&format!("<{gid:04X}> <{hex}>\n"));
        }
        cmap.push_str("endbfchar\n");
    }
    cmap.push_str("endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n");
    cmap
}

pub struct PdfPageReference {
    doc: PdfDocumentReference,
    page: usize,
}

impl PdfPageReference {
    pub fn get_layer(&self, layer: PdfLayerIndex) -> PdfLayerReference {
        PdfLayerReference {
            doc: self.doc.clone(),
            page: self.page,
            layer: layer.0,
        }
    }

    /// Adds a layer drawn above the existing ones.
    pub fn add_layer(&self, name: &str) -> PdfLayerReference {
        let mut doc = self.doc.0.borrow_mut();
        let layers = &mut doc.pages[self.page].layers;
        layers.push(LayerData {
            name: name.to_string(),
            operations: Vec::new(),
            current_font: None,
        });
        PdfLayerReference {
            doc: self.doc.clone(),
            page: self.page,
            layer: layers.len() - 1,
        }
    }
}

/// A layer of a page; all drawing goes through it.
#[derive(Clone)]
pub struct PdfLayerReference {
    doc: PdfDocumentReference,
    page: usize,
    layer: usize,
}

impl PdfLayerReference {
    fn with_layer<T>(&self, f: impl FnOnce(&mut LayerData) -> T) -> T {
        let mut doc = self.doc.0.borrow_mut();
        f(&mut doc.pages[self.page].layers[self.layer])
    }

    fn push(&self, operator: &str, operands: Vec<Object>) {
        self.add_operation(Operation::new(operator, operands));
    }

    pub fn add_operation(&self, operation: Operation) {
        self.with_layer(|layer| layer.operations.push(operation));
    }

    pub fn save_graphics_state(&self) {
        self.push("q", vec![]);
    }

    pub fn restore_graphics_state(&self) {
        self.push("Q", vec![]);
    }

    pub fn set_ctm(&self, matrix: CurTransMat) {
        let m = match matrix {
            CurTransMat::Translate(x, y) => [1.0, 0.0, 0.0, 1.0, x.0, y.0],
            CurTransMat::Raw(m) => m,
        };
        self.push("cm", m.iter().map(|&v| v.into()).collect());
    }

    pub fn set_fill_color(&self, color: Color) {
        let Color::Rgb(c) = color;
        self.push("rg", vec![c.r.into(), c.g.into(), c.b.into()]);
    }

    pub fn set_outline_color(&self, color: Color) {
        let Color::Rgb(c) = color;
        self.push("RG", vec![c.r.into(), c.g.into(), c.b.into()]);
    }

    pub fn set_outline_thickness(&self, thickness: f32) {
        self.push("w", vec![thickness.into()]);
    }

    pub fn set_line_cap_style(&self, style: LineCapStyle) {
        let value = match style {
            LineCapStyle::Butt => 0,
            LineCapStyle::Round => 1,
        };
        self.push("J", vec![Object::Integer(value)]);
    }

    pub fn set_line_join_style(&self, style: LineJoinStyle) {
        let value = match style {
            LineJoinStyle::Miter => 0,
            LineJoinStyle::Round => 1,
        };
        self.push("j", vec![Object::Integer(value)]);
    }

    pub fn add_line(&self, line: Line) {
        if line.points.is_empty() {
            return;
        }
        let mut operations = path_operations(&line.points);
        operations.push(Operation::new(
            if line.is_closed { "s" } else { "S" },
            vec![],
        ));
        self.with_layer(|layer| layer.operations.extend(operations));
    }

    pub fn add_polygon(&self, polygon: Polygon) {
        let mut operations: Vec<Operation> = polygon
            .rings
            .iter()
            .filter(|ring| !ring.is_empty())
            .flat_map(|ring| path_operations(ring))
            .collect();
        if operations.is_empty() {
            return;
        }
        operations.push(Operation::new(
            match polygon.mode {
                PaintMode::Fill => "f",
                PaintMode::Stroke => "s",
            },
            vec![],
        ));
        self.with_layer(|layer| layer.operations.extend(operations));
    }

    /// Draws `image` into the unit square of the current coordinate system.
    pub fn add_image(&self, image: ImageXObject) {
        let mut doc = self.doc.0.borrow_mut();
        let page = &mut doc.pages[self.page];
        page.images.push(image);
        let name = format!("X{}", page.images.len() - 1);
        page.layers[self.layer]
            .operations
            .push(Operation::new("Do", vec![Object::Name(name.into_bytes())]));
    }

    pub fn begin_text_section(&self) {
        self.push("BT", vec![]);
    }

    pub fn end_text_section(&self) {
        self.push("ET", vec![]);
        self.with_layer(|layer| layer.current_font = None);
    }

    pub fn set_font(&self, font: &IndirectFontRef, size: f32) {
        self.push(
            "Tf",
            vec![Object::Name(font.resource_name().into_bytes()), size.into()],
        );
        self.with_layer(|layer| layer.current_font = Some(font.index));
    }

    pub fn set_text_matrix(&self, matrix: TextMatrix) {
        let TextMatrix::Translate(x, y) = matrix;
        self.push(
            "Tm",
            vec![
                1.into(),
                0.into(),
                0.into(),
                1.into(),
                x.0.into(),
                y.0.into(),
            ],
        );
    }

    /// Shows glyphs of the current font; each comes with a shift before it in 1/1000 em
    /// (positive moves left, as in `TJ`).
    pub fn write_positioned_codepoints(&self, glyphs: impl IntoIterator<Item = (i64, u16)>) {
        let mut list = Vec::new();
        let mut used = Vec::new();
        for (shift, gid) in glyphs {
            if shift != 0 {
                list.push(Object::Integer(shift));
            }
            list.push(Object::String(
                gid.to_be_bytes().to_vec(),
                StringFormat::Hexadecimal,
            ));
            used.push(gid);
        }
        let mut doc = self.doc.0.borrow_mut();
        let layer = &mut doc.pages[self.page].layers[self.layer];
        layer
            .operations
            .push(Operation::new("TJ", vec![Object::Array(list)]));
        if let Some(font) = layer.current_font {
            doc.fonts[font].used_glyphs.extend(used);
        }
    }
}

/// `m`, `l` and `c` operations for a point list (see [`Line`] for the Bézier marking).
fn path_operations(points: &[(Point, bool)]) -> Vec<Operation> {
    let xy = |p: &Point| -> [Object; 2] { [p.x.0.into(), p.y.0.into()] };
    let mut operations = vec![Operation::new("m", xy(&points[0].0).to_vec())];
    let mut current = 1;
    while current < points.len() {
        let (previous, point) = (&points[current - 1], &points[current]);
        if previous.1 && point.1 {
            if let (Some(c2), Some(end)) = (points.get(current + 1), points.get(current + 2)) {
                let operands = [xy(&point.0), xy(&c2.0), xy(&end.0)].concat();
                operations.push(Operation::new("c", operands));
                current += 3;
                continue;
            }
        }
        operations.push(Operation::new("l", xy(&point.0).to_vec()));
        current += 1;
    }
    operations
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(points: &[(f32, f32, bool)]) -> Vec<(Point, bool)> {
        points
            .iter()
            .map(|&(x, y, b)| (Point::new(Mm(x), Mm(y)), b))
            .collect()
    }

    #[test]
    fn test_path_operations_lines_and_curves() {
        let ops = path_operations(&line(&[
            (0.0, 0.0, true),
            (1.0, 0.0, true),
            (2.0, 0.0, false),
            (3.0, 0.0, false),
            (4.0, 0.0, false),
        ]));
        let names: Vec<&str> = ops.iter().map(|o| o.operator.as_str()).collect();
        assert_eq!(names, ["m", "c", "l"]);
        assert_eq!(ops[1].operands.len(), 6);
    }

    #[test]
    fn test_document_with_text_image_and_bookmarks() {
        let (doc, page, layer) = PdfDocument::new("Test", Mm(210.0), Mm(297.0), "Background");
        let font = doc
            .add_external_font(crate::text::TextFont::Sans.bytes())
            .unwrap();
        doc.add_bookmark("Übung – 1".to_string(), page);
        let background = doc.get_page(page).get_layer(layer);
        background.add_polygon(Polygon {
            rings: vec![line(&[
                (0.0, 0.0, false),
                (10.0, 0.0, false),
                (10.0, 10.0, false),
            ])],
            mode: PaintMode::Fill,
            winding_order: WindingOrder::NonZero,
        });
        let text = doc.get_page(page).add_layer("Text");
        let face = crate::text::TextFont::Sans.face();
        let gids: Vec<u16> = "Hä".chars().map(|c| face.glyph_id(c).0).collect();
        text.begin_text_section();
        text.set_font(&font, 12.0);
        text.write_positioned_codepoints(gids.iter().map(|&g| (0, g)));
        text.end_text_section();
        text.add_image(ImageXObject {
            width: Px(1),
            height: Px(1),
            color_space: ColorSpace::Rgb,
            interpolate: true,
            image_data: vec![255, 0, 0],
            image_filter: None,
        });
        let bytes = doc.save_to_bytes().unwrap();

        let pdf = Document::load_mem(&bytes).unwrap();
        assert_eq!(pdf.get_pages().len(), 1);
        let page_id = *pdf.get_pages().values().next().unwrap();
        let content = pdf.get_page_content(page_id);
        let ops = Content::decode(&content).unwrap().operations;
        let names: Vec<&str> = ops.iter().map(|o| o.operator.as_str()).collect();
        for expected in ["BDC", "f", "BT", "Tf", "TJ", "ET", "Do", "EMC"] {
            assert!(names.contains(&expected), "{expected} missing in {names:?}");
        }
        // The ToUnicode map knows both characters
        let fonts = pdf.get_page_fonts(page_id).unwrap();
        let font = fonts.values().next().unwrap();
        let to_unicode = pdf
            .get_object(font.get(b"ToUnicode").unwrap().as_reference().unwrap())
            .unwrap()
            .as_stream()
            .unwrap()
            .decompressed_content()
            .unwrap();
        let cmap = String::from_utf8(to_unicode).unwrap();
        assert!(cmap.contains("<0048>"), "{cmap}");
        assert!(cmap.contains("<00E4>"), "{cmap}");
    }
}
