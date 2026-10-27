//! Notices changes that another device made to the open notebook through a sync client
//! such as Nextcloud or Google Drive (Issue #49).
//!
//! Every few seconds the notebook folder is scanned (only file times and sizes). Pages whose
//! file changed are compared with the page in memory and reloaded if they differ; a changed
//! page order, new or removed pages and titles from the manifest are taken over; conflict
//! copies made by sync clients (`page_3 (conflicted copy …).json`, `page_3 (1).json`,
//! `page_3.sync-conflict-….json`) become pages of their own, so nothing is lost. The check
//! only runs while all local changes are saved, so it never overwrites anything unsaved.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use crate::document::{Notebook, Page, PageBackground};
use crate::storage::{self, NotebookManifest, OpenMarker, PageData};

/// Time between two checks.
pub const INTERVAL: Duration = Duration::from_secs(3);

/// Modification time, size and, for small files, a hash of the content.
type Fingerprint = (Option<SystemTime>, u64, Option<u64>);

/// Files up to this size are also compared by content: file times are only as fine as the
/// kernel clock tick, so a file rewritten right after a check with the same length (e.g. a
/// manifest with one page replaced by another) would otherwise look unchanged.
const HASHED_FILE_SIZE: u64 = 64 * 1024;

fn content_hash(path: &Path) -> Option<u64> {
    use std::hash::{Hash, Hasher};
    let bytes = fs::read(path).ok()?;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    Some(hasher.finish())
}

/// What a check found and took over.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Outcome {
    /// Pages reloaded because their file changed
    pub reloaded_pages: usize,
    /// Pages added, removed, reordered or retitled through the manifest
    pub structure_changed: bool,
    /// Ids of the conflict copies added as pages (they need saving)
    pub conflict_pages: Vec<u64>,
    /// Another device has just opened the notebook
    pub opened_elsewhere: Option<OpenMarker>,
}

impl Outcome {
    pub fn is_empty(&self) -> bool {
        *self == Outcome::default()
    }
}

pub struct DiskWatch {
    pub dir: PathBuf,
    known: HashMap<String, Fingerprint>,
    /// Files that are neither pages nor conflict copies we could read
    ignored: HashSet<String>,
    next_check: Instant,
}

/// Fingerprints of the JSON files in the notebook folder.
fn scan(dir: &Path) -> HashMap<String, Fingerprint> {
    let Ok(entries) = fs::read_dir(dir) else {
        return HashMap::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.ends_with(".json") {
                return None;
            }
            let meta = entry.metadata().ok()?;
            if !meta.is_file() {
                return None;
            }
            let hash = (meta.len() <= HASHED_FILE_SIZE)
                .then(|| content_hash(&entry.path()))
                .flatten();
            Some((name, (meta.modified().ok(), meta.len(), hash)))
        })
        .collect()
}

/// Page id of a regular page file (`page_<id>.json`).
fn page_file_id(name: &str) -> Option<u64> {
    name.strip_prefix("page_")?
        .strip_suffix(".json")?
        .parse()
        .ok()
}

/// Page id a conflict copy belongs to: `page_<id>` followed by something else.
pub fn conflict_copy_of(name: &str) -> Option<u64> {
    if page_file_id(name).is_some() {
        return None;
    }
    let rest = name.strip_prefix("page_")?.strip_suffix(".json")?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    let tail = &rest[digits.len()..];
    let marked =
        tail.starts_with(" (") || tail.starts_with(".sync-conflict") || tail.starts_with('_');
    (marked && !digits.is_empty())
        .then(|| digits.parse().ok())
        .flatten()
}

/// Page content without the format version (that differs between app versions).
fn content(mut data: PageData) -> PageData {
    data.format_version = 0;
    data
}

impl DiskWatch {
    pub fn new(dir: PathBuf, now: Instant) -> Self {
        Self {
            known: scan(&dir),
            dir,
            ignored: HashSet::new(),
            next_check: now + INTERVAL,
        }
    }

    pub fn deadline(&self) -> Instant {
        self.next_check
    }

    pub fn is_due(&self, now: Instant) -> bool {
        now >= self.next_check
    }

    /// Skips the check that is due now (e.g. while drawing).
    pub fn postpone(&mut self, now: Instant) {
        self.next_check = now + INTERVAL;
    }

    /// Checks right away at the next opportunity (e.g. when the window gets the focus).
    pub fn check_soon(&mut self, now: Instant) {
        self.next_check = now;
    }

    /// Takes over what changed on disk. Call only while all local changes are saved.
    pub fn check(
        &mut self,
        notebook: &mut Notebook,
        manifest: &mut NotebookManifest,
        now: Instant,
    ) -> Outcome {
        self.next_check = now + INTERVAL;
        let mut outcome = Outcome::default();
        let current = scan(&self.dir);
        let changed = |name: &str| current.get(name) != self.known.get(name);
        let mut retry: HashSet<String> = HashSet::new();

        if changed(storage::OPEN_MARKER_FILE) {
            outcome.opened_elsewhere = fs::read(self.dir.join(storage::OPEN_MARKER_FILE))
                .ok()
                .and_then(|b| serde_json::from_slice::<OpenMarker>(&b).ok())
                .filter(|m| m.device != storage::device_name());
        }

        // Page order, titles, new and removed pages
        let mut fresh_pages: HashSet<u64> = HashSet::new();
        if changed("manifest.json") {
            match storage::load_manifest(&self.dir) {
                Ok(theirs) => match self.apply_manifest(notebook, manifest, theirs) {
                    Ok(Some(new_ids)) => {
                        fresh_pages = new_ids;
                        outcome.structure_changed = true;
                    }
                    Ok(None) => {}
                    Err(()) => {
                        retry.insert("manifest.json".to_string());
                    }
                },
                Err(_) => {
                    retry.insert("manifest.json".to_string());
                }
            }
        }

        // Changed page files
        for name in current.keys().filter(|n| changed(n)) {
            let Some(id) = page_file_id(name) else {
                continue;
            };
            let Some(page) = notebook.pages.iter_mut().find(|p| p.id == id) else {
                continue;
            };
            if !page.is_loaded || fresh_pages.contains(&id) {
                continue;
            }
            match storage::load_page(&self.dir, id) {
                Ok(data) => {
                    if content(PageData::from(&*page)) != content(data.clone()) {
                        let mut reloaded = data.to_page();
                        reloaded.title = reloaded.title.or_else(|| page.title.clone());
                        *page = reloaded;
                        outcome.reloaded_pages += 1;
                    }
                }
                // Possibly still being written by the sync client: try again next time
                Err(_) => {
                    retry.insert(name.clone());
                }
            }
        }

        // Conflict copies of sync clients become pages of their own
        let mut copies: Vec<&String> = current
            .keys()
            .filter(|n| !self.ignored.contains(*n))
            .filter(|n| conflict_copy_of(n).is_some())
            .collect();
        copies.sort();
        for name in copies {
            let path = self.dir.join(name);
            let data = fs::read(&path)
                .ok()
                .and_then(|b| serde_json::from_slice::<PageData>(&b).ok());
            match data {
                Some(data) => {
                    let of = conflict_copy_of(name).unwrap_or(0);
                    outcome
                        .conflict_pages
                        .push(add_conflict_page(notebook, data, of));
                    // Keep the file, but don't take it over again
                    let _ = fs::rename(&path, path.with_extension("json.imported"));
                }
                None => {
                    self.ignored.insert(name.clone());
                }
            }
        }
        if !outcome.conflict_pages.is_empty() {
            manifest.sync_from_notebook(notebook);
        }

        // Remember what we have seen, except files to look at again
        let mut known = current;
        for name in retry {
            match self.known.get(&name) {
                Some(old) => known.insert(name, *old),
                None => known.remove(&name),
            };
        }
        self.known = known;
        outcome
    }

    /// Takes over the page list of a manifest written elsewhere. Returns the ids of pages
    /// that are now unloaded placeholders, or `None` if nothing about the pages changed.
    /// Fails (to be tried again) while the page to show can't be read yet.
    fn apply_manifest(
        &self,
        notebook: &mut Notebook,
        manifest: &mut NotebookManifest,
        theirs: NotebookManifest,
    ) -> Result<Option<HashSet<u64>>, ()> {
        let mut ours = manifest.clone();
        ours.sync_from_notebook(notebook);
        if ours.pages == theirs.pages && ours.title == theirs.title {
            return Ok(None);
        }
        let current_id = notebook.current_page().id;

        // The page shown afterwards must be loaded. Read it before changing anything: a file
        // the sync client is still writing must not end up set aside as damaged.
        let shown = theirs
            .pages
            .iter()
            .find(|m| m.id == current_id)
            .or(theirs.pages.first())
            .map(|m| m.id);
        let mut shown_data = None;
        if let Some(id) = shown {
            if !notebook.pages.iter().any(|p| p.id == id && p.is_loaded) {
                match storage::load_page(&self.dir, id) {
                    Ok(data) => shown_data = Some(data),
                    // Never saved because still empty
                    Err(storage::StorageError::PageNotFound(_)) => {}
                    Err(_) => return Err(()),
                }
            }
        }

        let mut old: HashMap<u64, Page> = notebook.pages.drain(..).map(|p| (p.id, p)).collect();
        let mut placeholders = HashSet::new();
        for meta in &theirs.pages {
            let background = meta.background.unwrap_or(PageBackground::Grid5mm);
            let size = storage::page_size_from(meta.size);
            let mut page = match old.remove(&meta.id) {
                Some(mut page) => {
                    page.background = background;
                    page.size = size;
                    page
                }
                None => {
                    placeholders.insert(meta.id);
                    Page::new_unloaded(meta.id, background).with_size(size)
                }
            };
            page.title = meta.title.clone();
            notebook.pages.push(page);
        }
        if notebook.pages.is_empty() {
            notebook.pages.push(Page::new(1, PageBackground::Grid5mm));
        }
        notebook.current_page_index = notebook
            .pages
            .iter()
            .position(|p| p.id == current_id)
            .unwrap_or(0)
            .min(notebook.pages.len() - 1);
        notebook.next_page_id = notebook.next_page_id.max(theirs.next_page_id);
        notebook.next_stroke_id = notebook.next_stroke_id.max(theirs.next_stroke_id);
        let index = notebook.current_page_index;
        if !notebook.pages[index].is_loaded {
            let page = &mut notebook.pages[index];
            let mut loaded = match shown_data {
                Some(data) => data.to_page(),
                None => Page::new(page.id, page.background).with_size(page.size),
            };
            loaded.title = loaded.title.or(page.title.take());
            *page = loaded;
            placeholders.remove(&page.id);
        }
        let layout = manifest.layout;
        *manifest = theirs;
        manifest.layout = manifest.layout.or(layout);
        manifest.sync_from_notebook(notebook);
        Ok(Some(placeholders))
    }
}

/// Adds a conflict copy as a new page after page `of` (or at the end), with fresh ids.
/// Returns the id of the new page.
fn add_conflict_page(notebook: &mut Notebook, mut data: PageData, of: u64) -> u64 {
    let id = notebook.next_page_id;
    data.id = id;
    notebook.next_page_id += 1;
    for stroke in &mut data.strokes {
        stroke.id = notebook.next_stroke_id;
        notebook.next_stroke_id += 1;
    }
    let original = notebook.pages.iter().position(|p| p.id == of);
    let title = original
        .and_then(|i| notebook.pages[i].title.clone())
        .or(data.title.take());
    data.title = Some(match title {
        Some(title) => crate::t!("Konfliktkopie: {title}", title = title).to_string(),
        None => crate::t!("Konfliktkopie").to_string(),
    });
    let page = data.to_page();
    let at = original.map_or(notebook.pages.len(), |i| i + 1);
    notebook.pages.insert(at, page);
    if at <= notebook.current_page_index {
        notebook.current_page_index += 1;
    }
    id
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::{Stroke, StylusSample};

    fn stroke(id: u64, y: f32) -> Stroke {
        let points = (0..5)
            .map(|i| StylusSample {
                position: glam::Vec2::new(100.0 + 50.0 * i as f32, y),
                pressure: 0.5,
                timestamp: 0.0,
            })
            .collect();
        Stroke::new(id, points, Vec::new(), [0.0, 0.0, 0.0, 1.0])
    }

    /// Saved notebook with one stroke on its only page.
    fn saved_notebook(dir: &Path) -> (Notebook, NotebookManifest) {
        let mut notebook = Notebook::new();
        notebook.current_page_mut().add_stroke(stroke(1, 100.0));
        notebook.next_stroke_id = 2;
        let mut manifest = NotebookManifest::new("nb", "Mathe");
        storage::save_notebook(dir, &notebook, &mut manifest).unwrap();
        (notebook, manifest)
    }

    #[test]
    fn test_conflict_copy_names() {
        assert_eq!(
            conflict_copy_of("page_3 (conflicted copy 2026-10-27).json"),
            Some(3)
        );
        assert_eq!(conflict_copy_of("page_12 (1).json"), Some(12));
        assert_eq!(
            conflict_copy_of("page_3.sync-conflict-20261027-1200-ABC.json"),
            Some(3)
        );
        assert_eq!(conflict_copy_of("page_3_conflict.json"), Some(3));
        assert_eq!(conflict_copy_of("page_3.json"), None);
        assert_eq!(conflict_copy_of("page_x (1).json"), None);
        assert_eq!(conflict_copy_of("manifest (1).json"), None);
        assert_eq!(conflict_copy_of("page_3.json.imported"), None);
    }

    #[test]
    fn test_own_saves_are_not_reported() {
        let dir = tempfile::tempdir().unwrap();
        let (mut notebook, mut manifest) = saved_notebook(dir.path());
        let now = Instant::now();
        let mut watch = DiskWatch::new(dir.path().to_path_buf(), now);

        notebook.current_page_mut().add_stroke(stroke(2, 200.0));
        notebook.add_page(PageBackground::Blank);
        storage::save_notebook(dir.path(), &notebook, &mut manifest).unwrap();

        let outcome = watch.check(&mut notebook, &mut manifest, now);
        assert!(outcome.is_empty(), "{outcome:?}");
        assert_eq!(notebook.pages.len(), 2);
    }

    #[test]
    fn test_changes_from_another_device_are_taken_over() {
        let dir = tempfile::tempdir().unwrap();
        let (mut notebook, mut manifest) = saved_notebook(dir.path());
        let now = Instant::now();
        let mut watch = DiskWatch::new(dir.path().to_path_buf(), now);
        assert!(!watch.is_due(now));

        // The other device adds a stroke on page 1 and a second page with a title
        let (mut theirs, mut their_manifest) = storage::load_notebook(dir.path()).unwrap();
        theirs.current_page_mut().add_stroke(stroke(5, 300.0));
        let index = theirs.add_page(PageBackground::Lined);
        theirs.pages[index].add_stroke(stroke(6, 400.0));
        theirs.pages[index].title = Some("Übung".to_string());
        theirs.next_stroke_id = 7;
        storage::save_notebook(dir.path(), &theirs, &mut their_manifest).unwrap();

        let outcome = watch.check(&mut notebook, &mut manifest, now);
        assert_eq!(outcome.reloaded_pages, 1);
        assert!(outcome.structure_changed);
        assert!(outcome.conflict_pages.is_empty());
        assert_eq!(notebook.pages.len(), 2);
        assert_eq!(notebook.pages[0].strokes.len(), 2);
        assert_eq!(notebook.pages[1].title.as_deref(), Some("Übung"));
        assert_eq!(notebook.pages[1].background, PageBackground::Lined);
        assert!(notebook.next_stroke_id >= 7);
        assert_eq!(manifest.pages.len(), 2);

        // The new page loads from disk when needed
        storage::ensure_page_loaded(dir.path(), &mut notebook, 1).unwrap();
        assert_eq!(notebook.pages[1].strokes.len(), 1);

        // Nothing new: nothing to report
        let outcome = watch.check(&mut notebook, &mut manifest, now);
        assert!(outcome.is_empty(), "{outcome:?}");
    }

    #[test]
    fn test_page_still_being_synced_is_tried_again() {
        let dir = tempfile::tempdir().unwrap();
        let (mut notebook, mut manifest) = saved_notebook(dir.path());
        let now = Instant::now();
        let mut watch = DiskWatch::new(dir.path().to_path_buf(), now);

        // Elsewhere page 1 was deleted and page 2 added; its file is still half written
        let (mut theirs, mut their_manifest) = storage::load_notebook(dir.path()).unwrap();
        let index = theirs.add_page(PageBackground::Blank);
        theirs.pages[index].add_stroke(stroke(9, 100.0));
        storage::save_notebook(dir.path(), &theirs, &mut their_manifest).unwrap();
        their_manifest.pages.remove(0);
        storage::save_manifest(dir.path(), &their_manifest).unwrap();
        let page_file = dir.path().join("page_2.json");
        let complete = fs::read(&page_file).unwrap();
        fs::write(&page_file, &complete[..complete.len() / 2]).unwrap();

        let outcome = watch.check(&mut notebook, &mut manifest, now);
        assert!(outcome.is_empty(), "{outcome:?}");
        assert_eq!(notebook.pages.len(), 1);
        assert_eq!(notebook.pages[0].id, 1);
        assert!(page_file.exists(), "must not be set aside as damaged");

        fs::write(&page_file, &complete).unwrap();
        let outcome = watch.check(&mut notebook, &mut manifest, now);
        assert!(outcome.structure_changed);
        assert_eq!(notebook.pages.len(), 1);
        assert_eq!(notebook.current_page().id, 2);
        assert!(notebook.current_page().is_loaded);
        assert_eq!(notebook.current_page().strokes.len(), 1);
    }

    #[test]
    fn test_conflict_copy_becomes_a_page() {
        let dir = tempfile::tempdir().unwrap();
        let (mut notebook, mut manifest) = saved_notebook(dir.path());
        notebook.add_page(PageBackground::Blank);
        notebook.current_page_index = 1;
        storage::save_notebook(dir.path(), &notebook, &mut manifest).unwrap();
        let now = Instant::now();
        let mut watch = DiskWatch::new(dir.path().to_path_buf(), now);

        let mut copy = PageData::from(&notebook.pages[0]);
        copy.strokes.push(
            PageData::from(&{
                let mut page = Page::new(1, PageBackground::Blank);
                page.add_stroke(stroke(1, 500.0));
                page
            })
            .strokes[0]
                .clone(),
        );
        let name = "page_1 (conflicted copy 2026-10-27).json";
        fs::write(dir.path().join(name), serde_json::to_vec(&copy).unwrap()).unwrap();
        fs::write(dir.path().join("page_2 (1).json"), b"{ kaputt").unwrap();

        let outcome = watch.check(&mut notebook, &mut manifest, now);
        assert_eq!(outcome.conflict_pages.len(), 1);
        assert_eq!(notebook.pages.len(), 3);
        let added = &notebook.pages[1];
        assert_eq!(added.id, outcome.conflict_pages[0]);
        assert_eq!(added.strokes.len(), 2);
        assert!(added.title.is_some());
        // Fresh ids, no clash with the original page
        assert!(added.strokes.iter().all(|s| s.id >= 2));
        assert_eq!(notebook.pages[0].strokes.len(), 1);
        // The current page stays the same
        assert_eq!(notebook.current_page_index, 2);
        assert_eq!(manifest.pages.len(), 3);
        assert!(!dir.path().join(name).exists());
        assert!(dir.path().join(format!("{name}.imported")).exists());

        // Taken over only once; the unreadable file is left alone
        let outcome = watch.check(&mut notebook, &mut manifest, now);
        assert!(outcome.is_empty(), "{outcome:?}");
        assert!(dir.path().join("page_2 (1).json").exists());
    }

    #[test]
    fn test_open_marker() {
        let dir = tempfile::tempdir().unwrap();
        let (mut notebook, mut manifest) = saved_notebook(dir.path());
        let marker_path = dir.path().join(storage::OPEN_MARKER_FILE);
        let foreign = OpenMarker {
            device: "tablet-of-pia".to_string(),
            since: 1,
        };
        fs::write(&marker_path, serde_json::to_vec(&foreign).unwrap()).unwrap();

        // Opening warns about the other device and puts our own marker in place
        let lock = storage::NotebookLock::acquire(dir.path()).unwrap();
        assert_eq!(lock.open_elsewhere.as_ref(), Some(&foreign));
        let ours: OpenMarker = serde_json::from_slice(&fs::read(&marker_path).unwrap()).unwrap();
        assert_eq!(ours.device, storage::device_name());

        // The other device opens it again
        let now = Instant::now();
        let mut watch = DiskWatch::new(dir.path().to_path_buf(), now);
        fs::write(&marker_path, serde_json::to_vec(&foreign).unwrap()).unwrap();
        let outcome = watch.check(&mut notebook, &mut manifest, now);
        assert_eq!(outcome.opened_elsewhere, Some(foreign));

        // A foreign marker is not removed on closing
        drop(lock);
        assert!(marker_path.exists());
        let lock = storage::NotebookLock::acquire(dir.path()).unwrap();
        drop(lock);
        assert!(!marker_path.exists());
    }
}
