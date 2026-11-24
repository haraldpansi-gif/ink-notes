//! Volltext-Suche über Notizbuch-Titel, Seitentitel und Textfelder (inkl. LaTeX-Code).
//!
//! Durchsucht wahlweise das aktuelle Notizbuch (direkt im Speicher, inkl. ungespeicherter
//! Änderungen) oder alle gespeicherten Notizbücher im Speicherordner.

use std::path::Path;

use crate::document::Notebook;
use crate::storage;

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

/// Durchsucht das aktuell geöffnete Notizbuch (Seitentitel + Textfelder, für ungeladene Seiten
/// direkt aus `page_<id>.json`).
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
        }
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
            }
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
}
