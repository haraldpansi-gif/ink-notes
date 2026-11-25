//! Volltext-Suche über Notizbuch-Titel, Seitentitel, Textfelder (inkl. LaTeX-Code) und den Text
//! importierter PDF-Seiten.
//!
//! Durchsucht wahlweise das aktuelle Notizbuch (direkt im Speicher, inkl. ungespeicherter
//! Änderungen) oder alle gespeicherten Notizbücher im Speicherordner. PDF-Text wird pro PDF-Datei
//! einmalig mit `lopdf` extrahiert und im Speicher gecacht.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use crate::assets::AssetStore;
use crate::document::{Notebook, PdfBackground};
use crate::storage;

/// Größte PDF-Datei, aus der Text für die Suche extrahiert wird (128 MiB).
const MAX_PDF_SEARCH_BYTES: u64 = 128 * 1024 * 1024;
/// Maximale dekomprimierte Stream-Größe pro PDF-Seite bei der Text-Extraktion (4 MiB).
const MAX_PDF_PAGE_DECOMPRESSED_BYTES: usize = 4 * 1024 * 1024;
/// Höchstzahl gleichzeitig im Speicher gecachter PDF-Texte.
const MAX_CACHED_PDFS: usize = 32;

static PDF_TEXT_CACHE: OnceLock<Mutex<HashMap<PathBuf, Vec<String>>>> = OnceLock::new();

/// Suchbereich im Such-Dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SearchScope {
    #[default]
    CurrentNotebook,
    AllNotebooks,
}

/// Ein Suchtreffer mit Ziel-Notizbuch, Seitenindex und kurzem Textausschnitt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHit {
    pub notebook_id: String,
    pub notebook_title: String,
    pub page_index: usize,
    pub page_id: u64,
    pub snippet: String,
}

/// Schneidet einen kompakten Ausschnitt um das erste Vorkommen von `needle_lower` herum aus.
fn make_snippet(haystack: &str, needle_lower: &str) -> Option<String> {
    let clean = haystack.replace(['\n', '\r'], " ");
    let lower = clean.to_lowercase();
    let byte_pos = lower.find(needle_lower)?;
    let char_pos = lower[..byte_pos].chars().count();
    let chars: Vec<char> = clean.chars().collect();
    let start = char_pos.saturating_sub(24);
    let end = (char_pos + needle_lower.chars().count() + 36).min(chars.len());
    let mut out = String::new();
    if start > 0 {
        out.push('…');
    }
    out.extend(&chars[start..end]);
    if end < chars.len() {
        out.push('…');
    }
    Some(out.trim().to_string())
}

/// Extrahiert den Text aller Seiten einer PDF-Datei (0-basiert) sicher mit Größenlimit und
/// Panic-Schutz. Bei Fehlern wird ein leerer Vektor zurückgegeben.
fn extract_pdf_pages_text(path: &Path) -> Vec<String> {
    if std::fs::metadata(path)
        .map(|m| m.len() > MAX_PDF_SEARCH_BYTES)
        .unwrap_or(true)
    {
        return Vec::new();
    }
    let path_owned = path.to_path_buf();
    std::panic::catch_unwind(move || {
        let Ok(doc) = lopdf::Document::load(&path_owned) else {
            return Vec::new();
        };
        let page_count = doc.get_pages().len() as u32;
        (1..=page_count)
            .map(|page_num| {
                doc.extract_text_with_limit(&[page_num], MAX_PDF_PAGE_DECOMPRESSED_BYTES)
                    .unwrap_or_default()
            })
            .collect()
    })
    .unwrap_or_default()
}

/// Liefert den gecachten Text einer einzelnen 0-basierten PDF-Seite im Asset-Ordner von `nb_dir`.
fn pdf_page_text(nb_dir: &Path, pdf: &PdfBackground) -> Option<String> {
    let path = AssetStore::new(nb_dir).path_of(&pdf.asset).ok()?;
    let cache = PDF_TEXT_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(guard) = cache.lock() {
        if let Some(pages) = guard.get(&path) {
            return pages
                .get(pdf.page as usize)
                .filter(|s| !s.is_empty())
                .cloned();
        }
    }
    let extracted = extract_pdf_pages_text(&path);
    let result = extracted
        .get(pdf.page as usize)
        .filter(|s| !s.is_empty())
        .cloned();
    if let Ok(mut guard) = cache.lock() {
        if guard.len() >= MAX_CACHED_PDFS {
            guard.clear();
        }
        guard.insert(path, extracted);
    }
    result
}

/// Durchsucht den Text einer optionalen PDF-Hintergrundseite und ergänzt ggf. einen Treffer.
#[allow(clippy::too_many_arguments)]
fn search_pdf_background(
    hits: &mut Vec<SearchHit>,
    max_hits: usize,
    nb_dir: &Path,
    notebook_id: &str,
    notebook_title: &str,
    page_index: usize,
    page_id: u64,
    pdf: Option<&PdfBackground>,
    needle: &str,
) {
    if hits.len() >= max_hits {
        return;
    }
    let Some(pdf) = pdf else {
        return;
    };
    if let Some(text) = pdf_page_text(nb_dir, pdf) {
        if let Some(snippet) = make_snippet(&text, needle) {
            hits.push(SearchHit {
                notebook_id: notebook_id.to_string(),
                notebook_title: notebook_title.to_string(),
                page_index,
                page_id,
                snippet: format!("PDF: {snippet}"),
            });
        }
    }
}

/// Durchsucht den erkannten Handschrift-Text einer Seite aus dem lokalen `.ocr_cache.json`.
#[allow(clippy::too_many_arguments)]
fn search_ocr_text(
    hits: &mut Vec<SearchHit>,
    max_hits: usize,
    notebook_id: &str,
    notebook_title: &str,
    page_index: usize,
    page_id: u64,
    ocr_cache: &crate::handwriting_ocr::OcrCache,
    needle: &str,
) {
    if hits.len() >= max_hits {
        return;
    }
    if let Some(text) = ocr_cache.page_text(page_id) {
        if let Some(snippet) = make_snippet(text, needle) {
            hits.push(SearchHit {
                notebook_id: notebook_id.to_string(),
                notebook_title: notebook_title.to_string(),
                page_index,
                page_id,
                snippet: format!("✍: {snippet}"),
            });
        }
    }
}

/// Durchsucht das aktuell geöffnete Notizbuch (Seitentitel, Textfelder, PDF-Hintergründe und
/// lokalen Handschrift-OCR-Cache; für ungeladene Seiten direkt aus `page_<id>.json`).
pub fn search_notebook(
    notebook_id: &str,
    notebook_title: &str,
    notebook: &Notebook,
    storage_dir: &Path,
    query: &str,
    max_hits: usize,
) -> Vec<SearchHit> {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() || max_hits == 0 {
        return Vec::new();
    }
    let mut hits = Vec::new();
    let ocr_cache = crate::handwriting_ocr::OcrCache::load(storage_dir);

    for (page_index, page) in notebook.pages.iter().enumerate() {
        if hits.len() >= max_hits {
            break;
        }
        if let Some(title) = &page.title {
            if let Some(snippet) = make_snippet(title, &needle) {
                hits.push(SearchHit {
                    notebook_id: notebook_id.to_string(),
                    notebook_title: notebook_title.to_string(),
                    page_index,
                    page_id: page.id,
                    snippet,
                });
                if hits.len() >= max_hits {
                    break;
                }
            }
        }
        if page.is_loaded {
            for stroke in &page.strokes {
                if let Some(text) = &stroke.text {
                    if let Some(snippet) = make_snippet(&text.content, &needle) {
                        hits.push(SearchHit {
                            notebook_id: notebook_id.to_string(),
                            notebook_title: notebook_title.to_string(),
                            page_index,
                            page_id: page.id,
                            snippet,
                        });
                        if hits.len() >= max_hits {
                            break;
                        }
                    }
                }
            }
            search_pdf_background(
                &mut hits,
                max_hits,
                storage_dir,
                notebook_id,
                notebook_title,
                page_index,
                page.id,
                page.pdf_background.as_ref(),
                &needle,
            );
        } else if let Ok(data) = storage::load_page(storage_dir, page.id) {
            for stroke in &data.strokes {
                if let Some(text) = &stroke.text {
                    if let Some(snippet) = make_snippet(&text.content, &needle) {
                        hits.push(SearchHit {
                            notebook_id: notebook_id.to_string(),
                            notebook_title: notebook_title.to_string(),
                            page_index,
                            page_id: page.id,
                            snippet,
                        });
                        if hits.len() >= max_hits {
                            break;
                        }
                    }
                }
            }
            search_pdf_background(
                &mut hits,
                max_hits,
                storage_dir,
                notebook_id,
                notebook_title,
                page_index,
                page.id,
                data.pdf_background.as_ref(),
                &needle,
            );
        }
        search_ocr_text(
            &mut hits,
            max_hits,
            notebook_id,
            notebook_title,
            page_index,
            page.id,
            &ocr_cache,
            &needle,
        );
    }

    hits
}

/// Durchsucht alle Notizbücher im Basis-Ordner (beginnend beim aktuell geöffneten Notizbuch).
pub fn search_all_notebooks(
    base_dir: &Path,
    current_id: &str,
    current_title: &str,
    current_notebook: &Notebook,
    current_dir: &Path,
    query: &str,
    max_hits: usize,
) -> Vec<SearchHit> {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() || max_hits == 0 {
        return Vec::new();
    }

    let mut hits = search_notebook(
        current_id,
        current_title,
        current_notebook,
        current_dir,
        query,
        max_hits,
    );
    if hits.len() >= max_hits {
        return hits;
    }

    for summary in storage::list_notebooks(base_dir) {
        if hits.len() >= max_hits {
            break;
        }
        if summary.id == current_id || summary.status == storage::NotebookStatus::Damaged {
            continue;
        }
        let nb_dir = base_dir.join(&summary.id);
        let manifest_path = nb_dir.join("manifest.json");
        let Ok(content) = std::fs::read_to_string(&manifest_path) else {
            continue;
        };
        let Ok(manifest) = serde_json::from_str::<storage::NotebookManifest>(&content) else {
            continue;
        };
        if let Some(snippet) = make_snippet(&manifest.title, &needle) {
            hits.push(SearchHit {
                notebook_id: summary.id.clone(),
                notebook_title: manifest.title.clone(),
                page_index: 0,
                page_id: manifest.pages.first().map(|p| p.id).unwrap_or(1),
                snippet,
            });
            if hits.len() >= max_hits {
                break;
            }
        }
        let page_ids: Vec<u64> = if !manifest.pages.is_empty() {
            manifest.pages.iter().map(|p| p.id).collect()
        } else {
            manifest.page_ids.clone()
        };
        let ocr_cache = crate::handwriting_ocr::OcrCache::load(&nb_dir);
        for (page_index, page_id) in page_ids.into_iter().enumerate() {
            if hits.len() >= max_hits {
                break;
            }
            if let Some(title) = manifest.title_for_page(page_id) {
                if let Some(snippet) = make_snippet(&title, &needle) {
                    hits.push(SearchHit {
                        notebook_id: summary.id.clone(),
                        notebook_title: manifest.title.clone(),
                        page_index,
                        page_id,
                        snippet,
                    });
                    if hits.len() >= max_hits {
                        break;
                    }
                }
            }
            if let Ok(data) = storage::load_page(&nb_dir, page_id) {
                for stroke in &data.strokes {
                    if let Some(text) = &stroke.text {
                        if let Some(snippet) = make_snippet(&text.content, &needle) {
                            hits.push(SearchHit {
                                notebook_id: summary.id.clone(),
                                notebook_title: manifest.title.clone(),
                                page_index,
                                page_id,
                                snippet,
                            });
                            if hits.len() >= max_hits {
                                break;
                            }
                        }
                    }
                }
                search_pdf_background(
                    &mut hits,
                    max_hits,
                    &nb_dir,
                    &summary.id,
                    &manifest.title,
                    page_index,
                    page_id,
                    data.pdf_background.as_ref(),
                    &needle,
                );
            }
            search_ocr_text(
                &mut hits,
                max_hits,
                &summary.id,
                &manifest.title,
                page_index,
                page_id,
                &ocr_cache,
                &needle,
            );
        }
    }

    hits
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::{PageBackground, Stroke};
    use crate::text::{TextBox, TextFont};
    use glam::Vec2;

    #[test]
    fn test_search_current_notebook_finds_titles_and_text_boxes() {
        let dir = tempfile::tempdir().unwrap();
        let mut nb = Notebook::new();
        nb.pages[0].title = Some("Kapitel 1 – Halbleiter".to_string());
        nb.add_page(PageBackground::Blank);
        let text = TextBox::new(
            "Transistor $I_B < 0$ sperrt".to_string(),
            14.0,
            TextFont::Sans,
            400.0,
        );
        let corners = [
            Vec2::new(100.0, 100.0),
            Vec2::new(500.0, 100.0),
            Vec2::new(500.0, 200.0),
            Vec2::new(100.0, 200.0),
        ];
        nb.pages[1].add_stroke(Stroke::new_text(1, text, corners, [0.0, 0.0, 0.0, 1.0]));

        let hits = search_notebook("nb1", "ET", &nb, dir.path(), "halbleiter", 20);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].page_index, 0);

        let hits = search_notebook("nb1", "ET", &nb, dir.path(), "i_b < 0", 20);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].page_index, 1);
        assert!(hits[0].snippet.contains("I_B < 0"));
    }

    #[test]
    fn test_search_all_notebooks_finds_other_notebooks_on_disk() {
        let base = tempfile::tempdir().unwrap();
        let (mut nb_a, mut m_a, dir_a) =
            storage::create_new_notebook(base.path(), "Mathe").unwrap();
        nb_a.pages[0].title = Some("Integrale".to_string());
        m_a.sync_from_notebook(&nb_a);
        storage::save_manifest(&dir_a, &m_a).unwrap();

        let (nb_b, m_b, dir_b) = storage::create_new_notebook(base.path(), "Physik").unwrap();
        let hits = search_all_notebooks(
            base.path(),
            &m_b.id,
            &m_b.title,
            &nb_b,
            &dir_b,
            "integrale",
            20,
        );
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].notebook_id, m_a.id);
        assert_eq!(hits[0].page_index, 0);
    }

    #[test]
    fn test_search_finds_text_in_imported_pdf_background() {
        use lopdf::dictionary;

        let dir = tempfile::tempdir().unwrap();
        // Minimales echtes PDF mit Text über lopdf erzeugen
        let mut doc = lopdf::Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let font_id = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Helvetica",
        });
        let resources_id = doc.add_object(dictionary! {
            "Font" => dictionary! { "F1" => font_id },
        });
        let content = lopdf::content::Content {
            operations: vec![
                lopdf::content::Operation::new("BT", vec![]),
                lopdf::content::Operation::new("Tf", vec!["F1".into(), 12.into()]),
                lopdf::content::Operation::new("Td", vec![50.into(), 700.into()]),
                lopdf::content::Operation::new(
                    "Tj",
                    vec![lopdf::Object::string_literal("Kirchhoffsche Maschenregel")],
                ),
                lopdf::content::Operation::new("ET", vec![]),
            ],
        };
        let content_id = doc.add_object(lopdf::Stream::new(
            dictionary! {},
            content.encode().unwrap(),
        ));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Contents" => content_id,
            "Resources" => resources_id,
            "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
        });
        doc.objects.insert(
            pages_id,
            lopdf::Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => vec![page_id.into()],
                "Count" => 1,
            }),
        );
        let catalog_id = doc.add_object(dictionary! {
            "Type" => "Catalog",
            "Pages" => pages_id,
        });
        doc.trailer.set("Root", catalog_id);

        let mut pdf_bytes = Vec::new();
        doc.save_to(&mut pdf_bytes).unwrap();
        let asset_name = AssetStore::new(dir.path()).import_pdf(&pdf_bytes).unwrap();

        let mut nb = Notebook::new();
        nb.pages[0].pdf_background = Some(PdfBackground {
            asset: asset_name,
            page: 0,
            width_pt: 595.0,
            height_pt: 842.0,
        });

        let hits = search_notebook("nb1", "ET", &nb, dir.path(), "maschenregel", 20);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].page_index, 0);
        assert!(hits[0].snippet.starts_with("PDF: "));
        assert!(hits[0].snippet.contains("Maschenregel"));
    }

    #[test]
    fn test_search_finds_handwriting_from_ocr_cache() {
        let dir = tempfile::tempdir().unwrap();
        let mut nb = Notebook::new();
        nb.add_page(PageBackground::Blank);

        let mut cache = crate::handwriting_ocr::OcrCache::default();
        cache.set_page(
            nb.pages[1].id,
            42,
            "Prüfungsvorbereitung Übung 3\nKirchhoffsche Regel".to_string(),
        );
        cache.save(dir.path()).unwrap();

        let hits = search_notebook("nb1", "ET", &nb, dir.path(), "übung 3", 20);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].page_index, 1);
        assert!(hits[0].snippet.starts_with("✍: "));
        assert!(hits[0].snippet.contains("Übung 3"));
    }
}
