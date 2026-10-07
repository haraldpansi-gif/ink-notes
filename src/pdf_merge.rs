//! Legt beim PDF-Export die Originalseiten importierter PDFs als Vektorgrafik unter die
//! Notizen (Text bleibt scharf und auswählbar).
//!
//! Vorgehen: Das von printpdf erzeugte PDF wird mit `lopdf` geöffnet. Jede benötigte Seite
//! des Quell-PDFs wird als Form-XObject (Inhalt + Ressourcen der Seite) übernommen und am
//! Anfang der Zielseite gezeichnet, also unter Hintergrund, Bildern und Tinte.

use std::collections::{BTreeMap, HashMap};

use printpdf::lopdf::{Dictionary, Document, Object, ObjectId, Stream};

/// Eine Zielseite, unter die eine Seite eines Quell-PDFs gelegt wird.
#[derive(Debug, Clone, PartialEq)]
pub struct Underlay {
    /// Seitennummer im Ziel-PDF, ab 0
    pub target_page: usize,
    /// Schlüssel des Quell-PDFs in `sources`
    pub source: String,
    /// Seitennummer im Quell-PDF, ab 0
    pub source_page: usize,
    /// Zielrechteck in PDF-Punkten (links, unten, Breite, Höhe; Ursprung unten links)
    pub rect: [f32; 4],
}

/// Nicht übernommene Unterlagen samt Grund.
pub type FailedUnderlays = Vec<(Underlay, String)>;

/// Affine PDF-Matrix `[a b c d e f]`: x' = a·x + c·y + e, y' = b·x + d·y + f.
pub type Matrix = [f32; 6];

/// `outer ∘ inner`: erst `inner`, dann `outer` anwenden.
pub fn compose(outer: Matrix, inner: Matrix) -> Matrix {
    let [a1, b1, c1, d1, e1, f1] = outer;
    let [a2, b2, c2, d2, e2, f2] = inner;
    [
        a1 * a2 + c1 * b2,
        b1 * a2 + d1 * b2,
        a1 * c2 + c1 * d2,
        b1 * c2 + d1 * d2,
        a1 * e2 + c1 * f2 + e1,
        b1 * e2 + d1 * f2 + f1,
    ]
}

/// Matrix, die den Seitenbereich `bbox` (links, unten, rechts, oben) einer um `rotate` Grad
/// im Uhrzeigersinn gedrehten Seite so abbildet, wie ein Betrachter sie zeigt, eingepasst
/// in `rect` (links, unten, Breite, Höhe).
pub fn placement_matrix(bbox: [f32; 4], rotate: i64, rect: [f32; 4]) -> Matrix {
    let (w, h) = ((bbox[2] - bbox[0]).max(1e-3), (bbox[3] - bbox[1]).max(1e-3));
    let to_origin = [1.0, 0.0, 0.0, 1.0, -bbox[0], -bbox[1]];
    let (rotation, shown_w, shown_h): (Matrix, f32, f32) = match rotate.rem_euclid(360) {
        90 => ([0.0, -1.0, 1.0, 0.0, 0.0, w], h, w),
        180 => ([-1.0, 0.0, 0.0, -1.0, w, h], w, h),
        270 => ([0.0, 1.0, -1.0, 0.0, h, 0.0], h, w),
        _ => ([1.0, 0.0, 0.0, 1.0, 0.0, 0.0], w, h),
    };
    let scale = (rect[2] / shown_w).min(rect[3] / shown_h);
    let place = [scale, 0.0, 0.0, scale, rect[0], rect[1]];
    compose(place, compose(rotation, to_origin))
}

/// Erbt ein Seitenattribut über die `Parent`-Kette.
fn inherited<'a>(doc: &'a Document, page: &'a Dictionary, key: &[u8]) -> Option<&'a Object> {
    let mut node = page;
    for _ in 0..64 {
        if let Ok(value) = node.get(key) {
            return Some(value);
        }
        let parent = node.get(b"Parent").and_then(Object::as_reference).ok()?;
        node = doc.get_dictionary(parent).ok()?;
    }
    None
}

fn resolve<'a>(doc: &'a Document, object: &'a Object) -> &'a Object {
    doc.dereference(object).map(|(_, o)| o).unwrap_or(object)
}

fn read_box(doc: &Document, object: Option<&Object>) -> Option<[f32; 4]> {
    let array = resolve(doc, object?).as_array().ok()?;
    let values: Vec<f32> = array
        .iter()
        .filter_map(|v| resolve(doc, v).as_float().ok())
        .collect();
    let [x0, y0, x1, y1] = <[f32; 4]>::try_from(values).ok()?;
    Some([x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1)])
}

/// Baut aus einer Seite des (bereits umnummerierten) Quelldokuments ein Form-XObject.
/// Liefert das XObject und die Matrix für die Platzierung in `rect`.
fn page_as_form(
    doc: &Document,
    page_id: ObjectId,
    rect: [f32; 4],
) -> Result<(Stream, Matrix), String> {
    let page = doc.get_dictionary(page_id).map_err(|e| e.to_string())?;
    let media =
        read_box(doc, inherited(doc, page, b"MediaBox")).unwrap_or([0.0, 0.0, 595.0, 842.0]);
    let bbox = match read_box(doc, inherited(doc, page, b"CropBox")) {
        // Sichtbarer Bereich: Schnitt aus Crop- und Media-Box
        Some(c) => [
            c[0].max(media[0]),
            c[1].max(media[1]),
            c[2].min(media[2]),
            c[3].min(media[3]),
        ],
        None => media,
    };
    let rotate = inherited(doc, page, b"Rotate")
        .and_then(|r| resolve(doc, r).as_i64().ok())
        .unwrap_or(0);
    let resources = inherited(doc, page, b"Resources")
        .cloned()
        .unwrap_or_else(|| Object::Dictionary(Dictionary::new()));

    // Ein einzelner Inhaltsstrom wird unverändert (samt Kompression) übernommen,
    // mehrere werden entpackt und aneinandergehängt
    let streams = doc.get_page_contents(page_id);
    let (content, filter) = match streams.as_slice() {
        [single] => {
            let stream = doc
                .get_object(*single)
                .and_then(Object::as_stream)
                .map_err(|e| e.to_string())?;
            let filter = ["Filter", "DecodeParms"]
                .into_iter()
                .filter_map(|k| Some((k, stream.dict.get(k.as_bytes()).ok()?.clone())))
                .collect::<Vec<_>>();
            (stream.content.clone(), filter)
        }
        _ => {
            let mut content = Vec::new();
            for id in streams {
                let stream = doc
                    .get_object(id)
                    .and_then(Object::as_stream)
                    .map_err(|e| e.to_string())?;
                let data = stream
                    .decompressed_content()
                    .map_err(|e| format!("Inhalt nicht lesbar: {e}"))?;
                content.extend_from_slice(&data);
                content.push(b'\n');
            }
            (content, Vec::new())
        }
    };

    let mut dict = Dictionary::new();
    dict.set("Type", Object::Name(b"XObject".to_vec()));
    dict.set("Subtype", Object::Name(b"Form".to_vec()));
    dict.set(
        "BBox",
        Object::Array(bbox.iter().map(|&v| Object::Real(v)).collect()),
    );
    dict.set("Resources", resources);
    for (key, value) in filter {
        dict.set(key, value);
    }
    Ok((
        Stream::new(dict, content),
        placement_matrix(bbox, rotate, rect),
    ))
}

/// Legt die Quellseiten unter die Zielseiten. `sources` enthält die Quell-PDFs als Bytes.
/// Gibt die neuen PDF-Bytes zurück und die Underlays, die nicht übernommen werden konnten
/// (z. B. verschlüsselte PDFs), samt Grund.
pub fn apply_underlays(
    target_bytes: &[u8],
    underlays: &[Underlay],
    sources: &HashMap<String, Vec<u8>>,
) -> Result<(Vec<u8>, FailedUnderlays), String> {
    let mut target = Document::load_mem(target_bytes).map_err(|e| e.to_string())?;
    let target_pages: Vec<ObjectId> = target.get_pages().into_values().collect();
    let mut failed = Vec::new();

    // Quell-PDFs einmal laden und mit freien Objektnummern in das Ziel übernehmen
    let mut loaded: HashMap<&str, Result<BTreeMap<u32, ObjectId>, String>> = HashMap::new();
    for underlay in underlays {
        if loaded.contains_key(underlay.source.as_str()) {
            continue;
        }
        let result = (|| {
            let bytes = sources
                .get(&underlay.source)
                .ok_or_else(|| "Quell-PDF fehlt".to_string())?;
            let mut source = Document::load_mem(bytes).map_err(|e| e.to_string())?;
            if source.is_encrypted() {
                return Err("Quell-PDF ist verschlüsselt".to_string());
            }
            source.renumber_objects_with(target.max_id + 1);
            let pages = source.get_pages();
            target.max_id = target.max_id.max(source.max_id);
            // Katalog und Seitenbaum der Quelle werden nicht referenziert und beim Speichern
            // mit `prune_objects` entfernt
            target.objects.extend(source.objects);
            Ok(pages)
        })();
        loaded.insert(&underlay.source, result);
    }

    for underlay in underlays {
        let result = (|| {
            let pages = loaded[underlay.source.as_str()]
                .as_ref()
                .map_err(Clone::clone)?;
            let source_page = *pages
                .get(&(underlay.source_page as u32 + 1))
                .ok_or_else(|| format!("Seite {} fehlt im Quell-PDF", underlay.source_page + 1))?;
            let target_page = *target_pages
                .get(underlay.target_page)
                .ok_or_else(|| "Zielseite fehlt".to_string())?;
            let (form, matrix) = page_as_form(&target, source_page, underlay.rect)?;
            let form_id = target.add_object(Object::Stream(form));
            let name = format!("InkPdf{}", form_id.0);
            add_xobject(&mut target, target_page, &name, form_id)?;
            let ops = format!(
                "q {} {} {} {} {} {} cm /{name} Do Q\n",
                matrix[0], matrix[1], matrix[2], matrix[3], matrix[4], matrix[5]
            );
            prepend_content(&mut target, target_page, ops.into_bytes())
        })();
        if let Err(err) = result {
            failed.push((underlay.clone(), err));
        }
    }

    target.prune_objects();
    // Neue, noch unkomprimierte Ströme packen (bereits gefilterte bleiben unverändert)
    target.compress();
    let mut out = Vec::new();
    target.save_to(&mut out).map_err(|e| e.to_string())?;
    Ok((out, failed))
}

/// Trägt ein XObject in die Ressourcen der Seite ein.
fn add_xobject(
    doc: &mut Document,
    page_id: ObjectId,
    name: &str,
    id: ObjectId,
) -> Result<(), String> {
    // Ressourcen der Seite als eigenes Wörterbuch an der Seite (geerbte werden übernommen)
    let resources = {
        let page = doc.get_dictionary(page_id).map_err(|e| e.to_string())?;
        match inherited(doc, page, b"Resources").map(|r| resolve(doc, r)) {
            Some(Object::Dictionary(d)) => d.clone(),
            _ => Dictionary::new(),
        }
    };
    let mut xobjects = match resources.get(b"XObject").map(|x| resolve(doc, x)) {
        Ok(Object::Dictionary(d)) => d.clone(),
        _ => Dictionary::new(),
    };
    xobjects.set(name, Object::Reference(id));
    let mut resources = resources;
    resources.set("XObject", Object::Dictionary(xobjects));
    let page = doc
        .get_object_mut(page_id)
        .and_then(Object::as_dict_mut)
        .map_err(|e| e.to_string())?;
    page.set("Resources", Object::Dictionary(resources));
    Ok(())
}

/// Stellt einen Inhaltsstrom an den Anfang der Seite (zeichnet also zuunterst).
fn prepend_content(doc: &mut Document, page_id: ObjectId, content: Vec<u8>) -> Result<(), String> {
    let existing: Vec<Object> = doc
        .get_page_contents(page_id)
        .into_iter()
        .map(Object::Reference)
        .collect();
    let id = doc.add_object(Object::Stream(Stream::new(Dictionary::new(), content)));
    let mut contents = vec![Object::Reference(id)];
    contents.extend(existing);
    let page = doc
        .get_object_mut(page_id)
        .and_then(Object::as_dict_mut)
        .map_err(|e| e.to_string())?;
    page.set("Contents", Object::Array(contents));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(m: Matrix, x: f32, y: f32) -> (f32, f32) {
        (m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5])
    }

    fn close(a: (f32, f32), b: (f32, f32)) -> bool {
        (a.0 - b.0).abs() < 1e-3 && (a.1 - b.1).abs() < 1e-3
    }

    #[test]
    fn test_placement_matrix() {
        // Querformat-Folie 800 × 600 ohne Drehung in ein 400 × 300-Rechteck bei (10, 20)
        let m = placement_matrix([0.0, 0.0, 800.0, 600.0], 0, [10.0, 20.0, 400.0, 300.0]);
        assert!(close(apply(m, 0.0, 0.0), (10.0, 20.0)));
        assert!(close(apply(m, 800.0, 600.0), (410.0, 320.0)));

        // Verschobene MediaBox
        let m = placement_matrix([100.0, 50.0, 300.0, 150.0], 0, [0.0, 0.0, 200.0, 100.0]);
        assert!(close(apply(m, 100.0, 50.0), (0.0, 0.0)));

        // Um 90° gedrehte Hochformatseite (200 × 400) erscheint quer (400 × 200):
        // die linke untere Ecke landet links oben
        let m = placement_matrix([0.0, 0.0, 200.0, 400.0], 90, [0.0, 0.0, 400.0, 200.0]);
        assert!(close(apply(m, 0.0, 0.0), (0.0, 200.0)));
        assert!(close(apply(m, 200.0, 0.0), (0.0, 0.0)));
        assert!(close(apply(m, 0.0, 400.0), (400.0, 200.0)));

        // 180° und 270° (= -90°)
        let m = placement_matrix([0.0, 0.0, 200.0, 400.0], 180, [0.0, 0.0, 200.0, 400.0]);
        assert!(close(apply(m, 0.0, 0.0), (200.0, 400.0)));
        let m = placement_matrix([0.0, 0.0, 200.0, 400.0], -90, [0.0, 0.0, 400.0, 200.0]);
        assert!(close(apply(m, 0.0, 0.0), (400.0, 0.0)));
    }

    #[test]
    fn test_apply_underlays_embeds_source_pages() {
        let source = crate::pdf_doc::tests::sample_pdf(&[(842.0, 595.0, 0), (595.0, 842.0, 0)]);
        let target = crate::pdf_doc::tests::sample_pdf(&[(595.0, 842.0, 0), (595.0, 842.0, 0)]);
        let sources = HashMap::from([("folien".to_string(), source)]);
        let underlays = vec![
            Underlay {
                target_page: 0,
                source: "folien".into(),
                source_page: 0,
                rect: [0.0, 421.0, 595.0, 421.0],
            },
            Underlay {
                target_page: 1,
                source: "folien".into(),
                source_page: 5,
                rect: [0.0, 0.0, 595.0, 842.0],
            },
            Underlay {
                target_page: 1,
                source: "fehlt".into(),
                source_page: 0,
                rect: [0.0, 0.0, 595.0, 842.0],
            },
        ];
        let (bytes, failed) = apply_underlays(&target, &underlays, &sources).unwrap();
        // Fehlende Quellseite und fehlendes PDF werden gemeldet, der Rest klappt
        assert_eq!(failed.len(), 2);

        let doc = Document::load_mem(&bytes).unwrap();
        let pages: Vec<ObjectId> = doc.get_pages().into_values().collect();
        assert_eq!(
            pages.len(),
            2,
            "Quellseiten dürfen nicht als eigene Seiten auftauchen"
        );
        let first = doc.get_page_content(pages[0]).unwrap();
        let text = String::from_utf8_lossy(&first);
        assert!(
            text.starts_with("q "),
            "Unterlage muss zuerst gezeichnet werden: {text}"
        );
        assert!(text.contains("Do Q"));
        // Ursprünglicher Seiteninhalt bleibt erhalten
        assert!(text.contains("re f"));

        // Form-XObject mit dem Inhalt der Folie ist in den Ressourcen eingetragen
        let page = doc.get_dictionary(pages[0]).unwrap();
        let resources = page.get(b"Resources").unwrap().as_dict().unwrap();
        let xobjects = resources.get(b"XObject").unwrap().as_dict().unwrap();
        let (_, form_ref) = xobjects.iter().next().unwrap();
        let form = doc
            .get_object(form_ref.as_reference().unwrap())
            .unwrap()
            .as_stream()
            .unwrap();
        assert_eq!(
            form.dict.get(b"Subtype").unwrap().as_name().unwrap(),
            b"Form"
        );
        assert!(String::from_utf8_lossy(&form.content).contains("1 0 0 rg"));
    }
}
