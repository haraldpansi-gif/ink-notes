//! Built-in sync of the notebooks with a Nextcloud server (Issue #49, stage 2).
//!
//! The local folder stays the working copy: the app reads and writes it as before and works
//! offline. A background thread compares it with the folder `ink-notes` on the server every
//! 30 seconds and shortly after saving, using what both sides looked like after the last sync
//! (`nextcloud-sync.json`):
//! - changed on one side only: copied to the other side
//! - changed on both sides: manifests and `library.json` are merged; for a page the server's
//!   version is kept as a conflict copy next to it, which becomes a page of its own once the
//!   notebook is open (see `disk_watch.rs`), so nothing is lost
//! - deleted on one side and unchanged on the other: deleted there too, locally into `.trash`,
//!   on the server into its trash bin; a change wins over a deletion
//!
//! Only the notebook files themselves travel (manifests, pages, images, open markers); lock
//! files, backups and the trash stay local. While the open notebook has unsaved changes,
//! changes from the server to it wait for the next pass, so saving can't overwrite them.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::nextcloud::{self, Account, Entry, Error, Expect, Remote};
use crate::storage;
use crate::t;

/// Time between two passes.
pub const INTERVAL: Duration = Duration::from_secs(30);
/// Wait after saving before syncing, so a burst of saves goes up in one pass.
const AFTER_SAVE: Duration = Duration::from_secs(2);
/// A file whose size and time are as after the last pass counts as unchanged, unless its time
/// is this recent: file times are only as fine as the kernel clock tick, so a file rewritten
/// right after we looked could keep its time.
const SETTLED: Duration = Duration::from_secs(2);
/// How long the app waits for the sign-in in the browser.
const LOGIN_TIMEOUT: Duration = Duration::from_secs(20 * 60);

// ---------------------------------------------------------------------------------------------
// Which files travel

fn is_notebook_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && !name.contains(['\\', ':'])
        && !name.chars().any(char::is_control)
}

/// Files that are synced, as `/`-separated paths below the storage folder.
pub fn is_synced_file(path: &str) -> bool {
    let parts: Vec<&str> = path.split('/').collect();
    match parts.as_slice() {
        ["library.json"] => true,
        [notebook, name] => {
            is_notebook_name(notebook)
                && (*name == "manifest.json"
                    || *name == storage::OPEN_MARKER_FILE
                    || crate::disk_watch::page_file_id(name).is_some())
        }
        [notebook, "assets", name] => {
            is_notebook_name(notebook) && crate::assets::is_valid_asset_name(name)
        }
        _ => false,
    }
}

fn is_synced_dir(path: &str) -> bool {
    let parts: Vec<&str> = path.split('/').collect();
    match parts.as_slice() {
        [notebook] | [notebook, "assets"] => is_notebook_name(notebook),
        _ => false,
    }
}

/// Files merged field by field when both sides changed them.
fn is_merged_file(path: &str) -> bool {
    path == "library.json" || path.ends_with("/manifest.json")
}

fn local_path(dir: &Path, path: &str) -> PathBuf {
    path.split('/')
        .fold(dir.to_path_buf(), |p, part| p.join(part))
}

/// Notebook folder a path belongs to.
fn notebook_of(path: &str) -> Option<&str> {
    path.split_once('/').map(|(notebook, _)| notebook)
}

fn hash_bytes(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

// ---------------------------------------------------------------------------------------------
// What both sides looked like after the last pass

/// A file as it was on both sides after the last pass.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileState {
    pub etag: String,
    pub hash: String,
    pub size: u64,
    /// Local modification time (ns since 1970)
    pub mtime: u64,
    /// Content of a merged file, as common ancestor for the next merge
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncState {
    /// Local folder and server folder this state belongs to
    pub local: PathBuf,
    pub remote: String,
    pub files: BTreeMap<String, FileState>,
    /// ETags of server folders whose content in `files` is complete
    pub dirs: BTreeMap<String, String>,
}

impl SyncState {
    pub fn file_path() -> Option<PathBuf> {
        crate::paths::data_dir().map(|d| d.join("nextcloud-sync.json"))
    }

    /// The state for this pair of folders; a fresh one if it belongs to another pair (a fresh
    /// state never deletes anything).
    pub fn load(path: &Path, local: &Path, remote: &str) -> Self {
        fs::read(path)
            .ok()
            .and_then(|b| serde_json::from_slice::<SyncState>(&b).ok())
            .filter(|s| s.local == local && s.remote == remote)
            .unwrap_or_else(|| SyncState {
                local: local.to_path_buf(),
                remote: remote.to_string(),
                ..Default::default()
            })
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        storage::atomic_write_json(path, self).map_err(|e| e.to_string())
    }

    /// Forgets everything below a notebook folder (its files then count as new).
    fn forget(&mut self, notebook: &str) {
        let prefix = format!("{notebook}/");
        self.files.retain(|p, _| !p.starts_with(&prefix));
        self.dirs
            .retain(|p, _| p != notebook && !p.starts_with(&prefix));
    }
}

// ---------------------------------------------------------------------------------------------
// Looking at both sides

#[derive(Debug, Clone, PartialEq, Eq)]
struct LocalFile {
    size: u64,
    mtime: u64,
    hash: String,
    /// Not changed for a while, see [`SETTLED`]
    settled: bool,
}

fn mtime_of(meta: &fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos() as u64)
}

fn scan_local(dir: &Path, state: &SyncState) -> BTreeMap<String, LocalFile> {
    fn walk(dir: &Path, prefix: &str, state: &SyncState, out: &mut BTreeMap<String, LocalFile>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            let path = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() && is_synced_dir(&path) {
                walk(&entry.path(), &path, state, out);
            } else if kind.is_file() && is_synced_file(&path) {
                let Ok(meta) = entry.metadata() else {
                    continue;
                };
                let (size, mtime) = (meta.len(), mtime_of(&meta));
                let settled = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.elapsed().ok())
                    .is_some_and(|age| age >= SETTLED);
                let known = state
                    .files
                    .get(&path)
                    .filter(|s| settled && s.size == size && s.mtime == mtime);
                let hash = match known {
                    Some(s) => s.hash.clone(),
                    None => match fs::read(entry.path()) {
                        Ok(bytes) => hash_bytes(&bytes),
                        Err(_) => continue,
                    },
                };
                out.insert(
                    path,
                    LocalFile {
                        size,
                        mtime,
                        hash,
                        settled,
                    },
                );
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, "", state, &mut out);
    out
}

#[derive(Debug, Default)]
struct RemoteTree {
    files: BTreeMap<String, String>,
    dirs: BTreeMap<String, String>,
    /// The folder didn't exist and was just created
    created: bool,
}

/// Lists the server folder; subfolders whose ETag didn't change since the last pass are taken
/// from the state instead of being listed again.
fn scan_remote(remote: &mut dyn Remote, state: &SyncState) -> Result<RemoteTree, Error> {
    let mut tree = RemoteTree::default();
    let Some(entries) = remote.list("")? else {
        remote.mkdir("")?;
        tree.created = true;
        return Ok(tree);
    };
    let mut pending: Vec<Entry> = entries;
    while let Some(entry) = pending.pop() {
        if !entry.is_dir {
            if is_synced_file(&entry.path) {
                tree.files.insert(entry.path, entry.etag);
            }
            continue;
        }
        if !is_synced_dir(&entry.path) {
            continue;
        }
        let prefix = format!("{}/", entry.path);
        if state.dirs.get(&entry.path) == Some(&entry.etag) {
            for (path, file) in state.files.range(prefix.clone()..) {
                if !path.starts_with(&prefix) {
                    break;
                }
                tree.files.insert(path.clone(), file.etag.clone());
            }
            for (path, etag) in state.dirs.range(prefix.clone()..) {
                if !path.starts_with(&prefix) {
                    break;
                }
                tree.dirs.insert(path.clone(), etag.clone());
            }
        } else if let Some(children) = remote.list(&entry.path)? {
            pending.extend(children);
        } else {
            continue;
        }
        tree.dirs.insert(entry.path, entry.etag);
    }
    Ok(tree)
}

// ---------------------------------------------------------------------------------------------
// Merging manifests and library.json

/// Three-way merge of two JSON values with their common ancestor: what only one side changed
/// is taken over; lists of pages (objects with `id`) and of plain values are merged by
/// element; numbers changed on both sides take the larger value (page and stroke counters),
/// anything else changed on both sides keeps ours.
pub fn merge_json(base: Option<&Value>, ours: &Value, theirs: &Value) -> Value {
    if ours == theirs {
        return ours.clone();
    }
    if let Some(base) = base {
        if ours == base {
            return theirs.clone();
        }
        if theirs == base {
            return ours.clone();
        }
    }
    match (ours, theirs) {
        (Value::Object(o), Value::Object(t)) => {
            let b = base.and_then(Value::as_object);
            let keys: BTreeSet<&String> = o.keys().chain(t.keys()).collect();
            let mut out = serde_json::Map::new();
            for key in keys {
                let bv = b.and_then(|b| b.get(key));
                let merged = match (o.get(key), t.get(key)) {
                    (Some(ov), Some(tv)) => Some(merge_json(bv, ov, tv)),
                    // Removed on one side: gone unless the other side changed it
                    (Some(v), None) | (None, Some(v)) => (bv != Some(v)).then(|| v.clone()),
                    (None, None) => None,
                };
                if let Some(v) = merged {
                    out.insert(key.clone(), v);
                }
            }
            Value::Object(out)
        }
        (Value::Array(o), Value::Array(t)) => {
            let b = base.and_then(Value::as_array);
            let by_id = |v: &Value| v.get("id").cloned();
            let all = o.iter().chain(t.iter()).chain(b.into_iter().flatten());
            let all: Vec<&Value> = all.collect();
            if all.iter().all(|v| by_id(v).is_some()) {
                merge_list(b, o, t, &|v: &Value| by_id(v).unwrap_or(Value::Null))
            } else if all.iter().all(|v| !v.is_object() && !v.is_array()) {
                merge_list(b, o, t, &|v: &Value| v.clone())
            } else {
                ours.clone()
            }
        }
        (Value::Number(a), Value::Number(b)) => match (a.as_f64(), b.as_f64()) {
            (Some(x), Some(y)) if y > x => theirs.clone(),
            _ => ours.clone(),
        },
        _ => ours.clone(),
    }
}

/// Merges two versions of a list whose elements have a key; keeps our order and puts
/// elements only they added after their predecessor in their list.
fn merge_list(
    base: Option<&Vec<Value>>,
    ours: &[Value],
    theirs: &[Value],
    key: &dyn Fn(&Value) -> Value,
) -> Value {
    let find = |list: &[Value], k: &Value| list.iter().find(|v| key(v) == *k).cloned();
    let base_list: &[Value] = base.map_or(&[], |b| b.as_slice());
    let mut out: Vec<Value> = Vec::new();
    for o in ours {
        let k = key(o);
        let b = find(base_list, &k);
        match find(theirs, &k) {
            Some(t) => out.push(merge_json(b.as_ref(), o, &t)),
            // They removed it: gone unless we changed it
            None if b.as_ref() == Some(o) => {}
            None => out.push(o.clone()),
        }
    }
    for (i, t) in theirs.iter().enumerate() {
        let k = key(t);
        if out.iter().any(|v| key(v) == k) {
            continue;
        }
        let in_ours = ours.iter().any(|v| key(v) == k);
        let b = find(base_list, &k);
        // We removed it: gone unless they changed it
        if in_ours || b.as_ref() == Some(t) {
            continue;
        }
        let after = theirs[..i]
            .iter()
            .rev()
            .find_map(|p| out.iter().position(|v| key(v) == key(p)));
        out.insert(after.map_or(0, |pos| pos + 1), t.clone());
    }
    Value::Array(out)
}

/// Merges two versions of a manifest or `library.json`; `None` if one can't be read.
fn merge_file(path: &str, base: Option<&str>, ours: &[u8], theirs: &[u8]) -> Option<Vec<u8>> {
    let ours: Value = serde_json::from_slice(ours).ok()?;
    let theirs: Value = serde_json::from_slice(theirs).ok()?;
    let base: Option<Value> = base.and_then(|b| serde_json::from_str(b).ok());
    let merged = merge_json(base.as_ref(), &ours, &theirs);
    // Must still be a valid file
    let valid = if path == "library.json" {
        serde_json::from_value::<crate::library::Library>(merged.clone()).is_ok()
    } else {
        serde_json::from_value::<storage::NotebookManifest>(merged.clone()).is_ok()
    };
    valid.then(|| serde_json::to_vec(&merged).ok()).flatten()
}

// ---------------------------------------------------------------------------------------------
// One pass

/// What the app tells a pass.
#[derive(Debug, Clone, Default)]
pub struct Context {
    /// Notebook open in the app
    pub open_notebook: Option<String>,
    /// The open notebook has unsaved changes: changes from the server to it wait
    pub busy: bool,
}

/// What a pass did.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Report {
    pub downloaded: Vec<String>,
    pub uploaded: usize,
    pub deleted_local: usize,
    pub deleted_remote: usize,
    pub conflicts: usize,
    pub merged: usize,
    /// Changes left for the next pass
    pub deferred: usize,
}

impl Report {
    /// Something changed in the local folder.
    pub fn changed_local(&self) -> bool {
        !self.downloaded.is_empty() || self.deleted_local > 0 || self.conflicts > 0
    }
}

struct Pass<'a> {
    remote: &'a mut dyn Remote,
    dir: &'a Path,
    state: &'a mut SyncState,
    ctx: &'a Context,
    report: Report,
    /// Server folders known to exist
    remote_dirs: BTreeSet<String>,
    /// ETags of server folders after this pass, `None` once something in them was left over
    dir_etags: BTreeMap<String, String>,
    trash: Option<PathBuf>,
}

/// Moves a file or folder into `.trash` instead of deleting it.
fn to_trash(dir: &Path, from: &Path, name: &str) -> std::io::Result<()> {
    let trash = dir.join(".trash");
    storage::create_secure_dir_all(&trash)?;
    fs::rename(from, trash.join(name))
}

impl Pass<'_> {
    fn leave_for_later(&mut self, path: &str) {
        self.report.deferred += 1;
        let mut dir = path;
        while let Some((parent, _)) = dir.rsplit_once('/') {
            self.dir_etags.remove(parent);
            dir = parent;
        }
    }

    /// Changes from the server to the open notebook wait while it has unsaved changes.
    fn must_wait(&self, path: &str) -> bool {
        self.ctx.busy
            && notebook_of(path).is_some()
            && notebook_of(path) == self.ctx.open_notebook.as_deref()
    }

    /// State of a file with this content. `meta` was read before the content; without it the
    /// file is hashed again next time.
    fn file_state(
        path: &str,
        bytes: &[u8],
        etag: String,
        meta: Option<&fs::Metadata>,
    ) -> FileState {
        FileState {
            etag,
            hash: hash_bytes(bytes),
            size: meta.map_or(bytes.len() as u64, |m| m.len()),
            mtime: meta.map_or(0, mtime_of),
            base: is_merged_file(path).then(|| String::from_utf8_lossy(bytes).into_owned()),
        }
    }

    /// Writes a file unless it changed locally since we looked (`expected` is its hash then,
    /// `None` if it didn't exist).
    fn write_local(&self, path: &str, bytes: &[u8], expected: Option<&str>) -> Result<bool, Error> {
        let target = local_path(self.dir, path);
        let current = fs::read(&target).ok().map(|b| hash_bytes(&b));
        if current.as_deref() != expected {
            return Ok(false);
        }
        storage::atomic_write_file(&target, bytes)
            .map(|_| true)
            .map_err(|e| Error::Other(e.to_string()))
    }

    fn ensure_remote_parents(&mut self, path: &str) -> Result<(), Error> {
        let parts: Vec<&str> = path.split('/').collect();
        for i in 1..parts.len() {
            let dir = parts[..i].join("/");
            if !self.remote_dirs.contains(&dir) {
                self.remote.mkdir(&dir)?;
                self.remote_dirs.insert(dir);
            }
        }
        Ok(())
    }

    fn upload(&mut self, path: &str, expect: Expect<'_>) -> Result<(), Error> {
        let source = local_path(self.dir, path);
        let meta = fs::metadata(&source).ok();
        let Ok(bytes) = fs::read(&source) else {
            self.leave_for_later(path);
            return Ok(());
        };
        self.ensure_remote_parents(path)?;
        match self.remote.put(path, &bytes, expect) {
            Ok(etag) => {
                let state = Self::file_state(path, &bytes, etag, meta.as_ref());
                self.state.files.insert(path.to_string(), state);
                self.report.uploaded += 1;
                Ok(())
            }
            // Changed or created on the server meanwhile: next pass
            Err(Error::PreconditionFailed) => {
                self.leave_for_later(path);
                Ok(())
            }
            Err(err) => Err(err),
        }
    }

    fn download(
        &mut self,
        path: &str,
        listed_etag: &str,
        local: Option<&str>,
    ) -> Result<(), Error> {
        let (bytes, etag) = match self.remote.get(path) {
            Ok(got) => got,
            Err(Error::NotFound) => {
                self.leave_for_later(path);
                return Ok(());
            }
            Err(err) => return Err(err),
        };
        let etag = if etag.is_empty() {
            listed_etag.to_string()
        } else {
            etag
        };
        if self.write_local(path, &bytes, local)? {
            let state = Self::file_state(path, &bytes, etag, None);
            self.state.files.insert(path.to_string(), state);
            self.report.downloaded.push(path.to_string());
        } else {
            self.leave_for_later(path);
        }
        Ok(())
    }

    /// Both sides have the file and at least one changed it (or there is no common state).
    fn reconcile(&mut self, path: &str, local: &LocalFile, listed_etag: &str) -> Result<(), Error> {
        let (theirs, etag) = match self.remote.get(path) {
            Ok(got) => got,
            Err(Error::NotFound) => {
                self.leave_for_later(path);
                return Ok(());
            }
            Err(err) => return Err(err),
        };
        let etag = if etag.is_empty() {
            listed_etag.to_string()
        } else {
            etag
        };
        let Ok(ours) = fs::read(local_path(self.dir, path)) else {
            self.leave_for_later(path);
            return Ok(());
        };
        if hash_bytes(&ours) != local.hash {
            self.leave_for_later(path);
            return Ok(());
        }
        if hash_bytes(&theirs) == local.hash {
            let state = Self::file_state(path, &ours, etag, None);
            self.state.files.insert(path.to_string(), state);
            return Ok(());
        }
        if is_merged_file(path) {
            let base = self.state.files.get(path).and_then(|s| s.base.clone());
            if let Some(merged) = merge_file(path, base.as_deref(), &ours, &theirs) {
                if !self.write_local(path, &merged, Some(&local.hash))? {
                    self.leave_for_later(path);
                    return Ok(());
                }
                self.report.merged += 1;
                self.report.downloaded.push(path.to_string());
                return self.upload(path, Expect::Etag(&etag));
            }
        }
        let is_page =
            crate::disk_watch::page_file_id(path.rsplit('/').next().unwrap_or(path)).is_some();
        // An empty page (e.g. of the notebook every new installation starts with) gives way
        let blank = |bytes: &[u8]| {
            serde_json::from_slice::<storage::PageData>(bytes)
                .is_ok_and(|p| p.strokes.is_empty() && p.pdf_background.is_none())
        };
        if is_page && blank(&ours) && !blank(&theirs) {
            if self.write_local(path, &theirs, Some(&local.hash))? {
                let state = Self::file_state(path, &theirs, etag, None);
                self.state.files.insert(path.to_string(), state);
                self.report.downloaded.push(path.to_string());
            } else {
                self.leave_for_later(path);
            }
            return Ok(());
        }
        if is_page && !blank(&theirs) {
            // Keep their page next to ours; disk_watch turns it into a page of its own
            let stem = path.trim_end_matches(".json");
            if self.has_copy(stem, &theirs) {
                // Kept by an earlier pass whose upload didn't go through
                return self.upload(path, Expect::Etag(&etag));
            }
            let ts = unix_now();
            let mut n = 0;
            let copy = loop {
                let name = match n {
                    0 => format!("{stem} (conflict {ts}).json"),
                    n => format!("{stem} (conflict {ts}-{n}).json"),
                };
                if !local_path(self.dir, &name).exists() {
                    break name;
                }
                n += 1;
            };
            storage::atomic_write_file(&local_path(self.dir, &copy), &theirs)
                .map_err(|e| Error::Other(e.to_string()))?;
            self.report.conflicts += 1;
        }
        // Ours goes up (open markers, images with the same name have the same content)
        self.upload(path, Expect::Etag(&etag))
    }

    /// A conflict copy of `stem` with this content exists already (maybe already taken over
    /// as a page, then it ends in `.imported`).
    fn has_copy(&self, stem: &str, content: &[u8]) -> bool {
        let (folder, name) = stem.rsplit_once('/').unwrap_or(("", stem));
        let prefix = format!("{name} (conflict ");
        let Ok(entries) = fs::read_dir(local_path(self.dir, folder)) else {
            return false;
        };
        entries.flatten().any(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|n| n.starts_with(&prefix))
                && fs::read(entry.path()).is_ok_and(|b| b == content)
        })
    }

    fn delete_local(&mut self, path: &str, local: &LocalFile) -> Result<(), Error> {
        let source = local_path(self.dir, path);
        let unchanged = fs::read(&source).ok().map(|b| hash_bytes(&b)) == Some(local.hash.clone());
        if !unchanged {
            self.leave_for_later(path);
            return Ok(());
        }
        let trash = self
            .trash
            .get_or_insert_with(|| PathBuf::from(format!("sync_{}", unix_now())))
            .clone();
        let target = local_path(&self.dir.join(".trash").join(trash), path);
        let moved = target
            .parent()
            .map(storage::create_secure_dir_all)
            .unwrap_or(Ok(()))
            .and_then(|_| fs::rename(&source, &target));
        match moved {
            Ok(()) => {
                self.state.files.remove(path);
                self.report.deleted_local += 1;
            }
            Err(_) => self.leave_for_later(path),
        }
        Ok(())
    }

    fn delete_remote(&mut self, path: &str, etag: &str) -> Result<(), Error> {
        match self.remote.delete(path, Some(etag)) {
            Ok(()) => {
                self.state.files.remove(path);
                self.report.deleted_remote += 1;
                Ok(())
            }
            Err(Error::PreconditionFailed) => {
                self.leave_for_later(path);
                Ok(())
            }
            Err(err) => Err(err),
        }
    }

    /// A whole notebook vanished on one side.
    fn notebook_deletions(
        &mut self,
        local: &mut BTreeMap<String, LocalFile>,
        remote: &mut RemoteTree,
    ) -> Result<(), Error> {
        let known: BTreeSet<String> = self
            .state
            .files
            .keys()
            .filter_map(|p| notebook_of(p).map(str::to_string))
            .collect();
        for notebook in known {
            let prefix = format!("{notebook}/");
            let under = |p: &String| p.starts_with(&prefix);
            let local_dir = self.dir.join(&notebook);
            let here = local_dir.is_dir();
            let there = remote.dirs.contains_key(&notebook);
            if here && !there {
                // Deleted on the server: here too, unless it's open or changed here
                let changed = local
                    .iter()
                    .filter(|(p, _)| under(p))
                    .any(|(p, f)| self.state.files.get(p).map(|s| &s.hash) != Some(&f.hash));
                if self.ctx.open_notebook.as_deref() != Some(notebook.as_str()) && !changed {
                    let name = format!("{notebook}_{}", unix_now());
                    if to_trash(self.dir, &local_dir, &name).is_ok() {
                        local.retain(|p, _| !under(p));
                        self.report.deleted_local += 1;
                    }
                }
                // Otherwise it goes up again as new
                self.state.forget(&notebook);
            } else if there && !here {
                // Deleted here: on the server too, unless changed there
                let theirs: BTreeMap<&String, &String> =
                    remote.files.iter().filter(|(p, _)| under(p)).collect();
                let known: BTreeMap<&String, &String> = self
                    .state
                    .files
                    .iter()
                    .filter(|(p, _)| under(p))
                    .map(|(p, s)| (p, &s.etag))
                    .collect();
                let unchanged = theirs == known;
                if unchanged {
                    self.remote.delete(&notebook, None)?;
                    remote.files.retain(|p, _| !under(p));
                    remote.dirs.retain(|p, _| *p != notebook && !under(p));
                    self.remote_dirs.retain(|p| *p != notebook && !under(p));
                    self.report.deleted_remote += 1;
                }
                // Otherwise it comes down again as new
                self.state.forget(&notebook);
            }
        }
        Ok(())
    }
}

/// Brings the local folder `dir` and the server folder in line. The state is updated as far as
/// the pass got, also when it fails; save it in either case.
pub fn run_pass(
    remote: &mut dyn Remote,
    dir: &Path,
    state: &mut SyncState,
    ctx: &Context,
) -> Result<Report, Error> {
    // Rather stop than delete everything on one side: the storage folder is gone (e.g. a
    // drive not mounted) or the folder on the server was removed or renamed
    if !state.files.is_empty() && !dir.is_dir() {
        return Err(Error::Other(t!(
            "Der Speicherort {dir} fehlt. Zur Sicherheit wird nichts gelöscht.",
            dir = dir.display()
        )));
    }
    storage::create_secure_dir_all(dir).map_err(|e| Error::Other(e.to_string()))?;
    let mut remote_tree = scan_remote(remote, state)?;
    let mut local = scan_local(dir, state);
    if !state.files.is_empty() && remote_tree.created && !local.is_empty() {
        return Err(Error::Other(
            t!("Der Ordner „ink-notes“ auf dem Server fehlt. Zur Sicherheit wird nichts gelöscht – zum erneuten Hochladen die Verbindung trennen und neu verbinden.")
                .to_string(),
        ));
    }

    let mut pass = Pass {
        remote_dirs: remote_tree.dirs.keys().cloned().collect(),
        dir_etags: BTreeMap::new(),
        remote,
        dir,
        state,
        ctx,
        report: Report::default(),
        trash: None,
    };
    pass.notebook_deletions(&mut local, &mut remote_tree)?;
    // From here on, folder ETags only count again where the pass finishes everything
    pass.dir_etags = remote_tree.dirs.clone();
    pass.state.dirs.clear();

    let paths: BTreeSet<String> = local
        .keys()
        .chain(remote_tree.files.keys())
        .chain(pass.state.files.keys())
        .cloned()
        .collect();
    for path in paths {
        let known = pass.state.files.get(&path).cloned();
        let here = local.get(&path);
        let there = remote_tree.files.get(&path);
        let changed_here = here.map(|f| &f.hash) != known.as_ref().map(|s| &s.hash);
        let changed_there = there != known.as_ref().map(|s| &s.etag);
        match (here, there) {
            (None, None) => {
                pass.state.files.remove(&path);
            }
            (Some(f), Some(_)) if !changed_here && !changed_there => {
                // Remember size and time, so the file needn't be read next time
                if let Some(known) = pass.state.files.get_mut(&path).filter(|_| f.settled) {
                    known.size = f.size;
                    known.mtime = f.mtime;
                }
            }
            (Some(_), Some(etag)) if !changed_there => pass.upload(&path, Expect::Etag(etag))?,
            (Some(f), Some(etag)) if !changed_here => {
                if pass.must_wait(&path) {
                    pass.leave_for_later(&path);
                } else {
                    pass.download(&path, etag, Some(&f.hash))?;
                }
            }
            (Some(f), Some(etag)) => {
                if pass.must_wait(&path) {
                    pass.leave_for_later(&path);
                } else {
                    pass.reconcile(&path, f, etag)?;
                }
            }
            (Some(f), None) => match known {
                // Deleted on the server, unchanged here
                Some(_) if !changed_here => {
                    if pass.must_wait(&path) {
                        pass.leave_for_later(&path);
                    } else {
                        pass.delete_local(&path, f)?;
                    }
                }
                _ => pass.upload(&path, Expect::New)?,
            },
            (None, Some(etag)) => match known {
                // Deleted here, unchanged on the server
                Some(_) if !changed_there => pass.delete_remote(&path, etag)?,
                _ => {
                    if pass.must_wait(&path) {
                        pass.leave_for_later(&path);
                    } else {
                        pass.download(&path, etag, None)?;
                    }
                }
            },
        }
    }

    let Pass {
        report, dir_etags, ..
    } = pass;
    state.dirs = dir_etags;
    Ok(report)
}

// ---------------------------------------------------------------------------------------------
// Running in the background

/// Connection and sync status for the interface.
#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    /// Not connected
    Off,
    /// Looking for the server
    Connecting,
    /// The server's certificate needs the user's confirmation
    Untrusted {
        server: String,
        fingerprint: String,
    },
    /// Waiting for the sign-in in the browser
    WaitingForLogin {
        url: String,
    },
    /// Connected; time of the last finished pass
    Idle {
        last: Option<SystemTime>,
    },
    Syncing,
    Offline(String),
    Failed(String),
}

impl Status {
    pub fn is_connected(&self) -> bool {
        matches!(
            self,
            Status::Idle { .. } | Status::Syncing | Status::Offline(_) | Status::Failed(_)
        )
    }
}

/// What the interface shows.
#[derive(Debug, Clone, PartialEq)]
pub struct View {
    pub status: Status,
    /// `user@server` while connected
    pub account: Option<String>,
    /// Connected without encryption (http)
    pub insecure: bool,
}

impl Default for View {
    fn default() -> Self {
        Self {
            status: Status::Off,
            account: None,
            insecure: false,
        }
    }
}

#[derive(Debug)]
struct Shared {
    status: Status,
    local_dir: PathBuf,
    ctx: Context,
    next_pass: Instant,
    /// Set when a pass changed local files
    changed: bool,
    /// Finished sign-in, to be stored by the app
    signed_in: Option<Account>,
    stop: bool,
}

type SharedState = Arc<(Mutex<Shared>, Condvar)>;

/// Sync in the background (owned by the app's main loop).
pub struct CloudSync {
    shared: SharedState,
    account: Option<Account>,
    worker: Option<std::thread::JoinHandle<()>>,
    login_cancel: Option<Arc<AtomicBool>>,
}

fn lock(shared: &SharedState) -> std::sync::MutexGuard<'_, Shared> {
    shared.0.lock().unwrap_or_else(|e| e.into_inner())
}

fn set_status(shared: &SharedState, status: Status) {
    lock(shared).status = status;
}

impl CloudSync {
    /// Starts syncing `local_dir` if an account is stored.
    pub fn new(local_dir: PathBuf) -> Self {
        let mut sync = Self {
            shared: Arc::new((
                Mutex::new(Shared {
                    status: Status::Off,
                    local_dir,
                    ctx: Context::default(),
                    next_pass: Instant::now(),
                    changed: false,
                    signed_in: None,
                    stop: false,
                }),
                Condvar::new(),
            )),
            account: None,
            worker: None,
            login_cancel: None,
        };
        if let Some(account) = Account::file_path().and_then(|p| Account::load_from(&p)) {
            sync.start(account);
        }
        sync
    }

    pub fn view(&self) -> View {
        View {
            status: lock(&self.shared).status.clone(),
            account: self.account.as_ref().map(Account::label),
            insecure: self.account.as_ref().is_some_and(Account::is_insecure),
        }
    }

    fn start(&mut self, account: Account) {
        self.stop_worker();
        {
            let mut shared = lock(&self.shared);
            shared.stop = false;
            shared.status = Status::Idle { last: None };
            shared.next_pass = Instant::now();
        }
        let shared = self.shared.clone();
        let worker_account = account.clone();
        self.worker = Some(std::thread::spawn(move || worker(shared, worker_account)));
        self.account = Some(account);
    }

    fn stop_worker(&mut self) {
        if let Some(handle) = self.worker.take() {
            lock(&self.shared).stop = true;
            self.shared.1.notify_all();
            // A pass in progress finishes in the background
            drop(handle);
        }
    }

    /// Syncs soon, e.g. after saving or when the window gets the focus again.
    pub fn sync_soon(&self) {
        self.sync_after(AFTER_SAVE);
    }

    pub fn sync_now(&self) {
        self.sync_after(Duration::ZERO);
    }

    fn sync_after(&self, delay: Duration) {
        if self.worker.is_none() {
            return;
        }
        let mut shared = lock(&self.shared);
        let at = Instant::now() + delay;
        if at < shared.next_pass {
            shared.next_pass = at;
            self.shared.1.notify_all();
        }
    }

    /// Another storage folder is used from now on.
    pub fn set_local_dir(&self, dir: PathBuf) {
        lock(&self.shared).local_dir = dir;
        self.sync_now();
    }

    /// Called from the main loop: tells which notebook is open and whether it has unsaved
    /// changes; stores a finished sign-in. Returns true when local files changed.
    pub fn update(&mut self, open_notebook: &str, busy: bool) -> bool {
        let (signed_in, changed) = {
            let mut shared = lock(&self.shared);
            if shared.ctx.open_notebook.as_deref() != Some(open_notebook) {
                shared.ctx.open_notebook = Some(open_notebook.to_string());
            }
            shared.ctx.busy = busy;
            (shared.signed_in.take(), std::mem::take(&mut shared.changed))
        };
        if let Some(account) = signed_in {
            self.login_cancel = None;
            if let Some(path) = Account::file_path() {
                if let Err(err) = account.save_to(&path) {
                    set_status(
                        &self.shared,
                        Status::Failed(t!("Zugang nicht gespeichert: {err}", err = err)),
                    );
                    return changed;
                }
            }
            self.start(account);
        }
        changed
    }

    /// When the main loop should look again (while signing in or syncing).
    pub fn deadline(&self) -> Option<Instant> {
        let busy = matches!(
            lock(&self.shared).status,
            Status::Connecting | Status::WaitingForLogin { .. } | Status::Syncing
        );
        busy.then(|| Instant::now() + Duration::from_millis(500))
    }

    /// Looks for the server and signs in through the browser. `trusted` is the fingerprint of
    /// a certificate the user confirmed.
    pub fn connect(&mut self, input: &str, trusted: Option<String>) {
        let server = match nextcloud::normalize_server(input) {
            Ok(server) => server,
            Err(msg) => {
                set_status(&self.shared, Status::Failed(msg));
                return;
            }
        };
        self.cancel_login();
        let cancel = Arc::new(AtomicBool::new(false));
        self.login_cancel = Some(cancel.clone());
        set_status(&self.shared, Status::Connecting);
        let shared = self.shared.clone();
        std::thread::spawn(move || {
            let result = sign_in(&shared, &server, trusted, &cancel);
            if cancel.load(Ordering::Relaxed) {
                return;
            }
            match result {
                Ok(account) => {
                    let mut shared = lock(&shared);
                    shared.signed_in = Some(account);
                }
                Err(Error::Untrusted { fingerprint }) => set_status(
                    &shared,
                    Status::Untrusted {
                        server,
                        fingerprint,
                    },
                ),
                Err(err) => set_status(&shared, Status::Failed(err.to_string())),
            }
        });
    }

    pub fn cancel_login(&mut self) {
        if let Some(cancel) = self.login_cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        if self.account.is_none() {
            set_status(&self.shared, Status::Off);
        }
    }

    /// Stops syncing, forgets the account and revokes its app password. The notebooks stay.
    pub fn disconnect(&mut self) {
        self.cancel_login();
        self.stop_worker();
        if let Some(account) = self.account.take() {
            std::thread::spawn(move || {
                if let Err(err) = nextcloud::revoke(&account) {
                    eprintln!("[Nextcloud] App-Passwort nicht widerrufen: {err}");
                }
            });
        }
        for path in [Account::file_path(), SyncState::file_path()]
            .into_iter()
            .flatten()
        {
            let _ = fs::remove_file(path);
        }
        set_status(&self.shared, Status::Off);
    }
}

fn sign_in(
    shared: &SharedState,
    server: &str,
    trusted: Option<String>,
    cancel: &AtomicBool,
) -> Result<Account, Error> {
    let client = nextcloud::Client::new(trusted.clone());
    nextcloud::check_server(&client, server)?;
    let flow = nextcloud::start_login(&client, server)?;
    if webbrowser::open(&flow.login_url).is_err() {
        eprintln!(
            "[Nextcloud] Browser nicht geöffnet, Anmeldung unter {}",
            flow.login_url
        );
    }
    set_status(
        shared,
        Status::WaitingForLogin {
            url: flow.login_url.clone(),
        },
    );
    let started = Instant::now();
    loop {
        std::thread::sleep(Duration::from_secs(2));
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::Other(String::new()));
        }
        if started.elapsed() > LOGIN_TIMEOUT {
            return Err(Error::Other(
                t!("Anmeldung abgelaufen – bitte erneut verbinden").to_string(),
            ));
        }
        match nextcloud::poll_login(&client, &flow) {
            Ok(Some((login_name, app_password))) => {
                let user_id = nextcloud::user_id(&client, server, &login_name, &app_password)?;
                return Ok(Account {
                    server: server.to_string(),
                    login_name,
                    user_id,
                    app_password,
                    pinned_cert: trusted,
                });
            }
            Ok(None) => {}
            // Briefly offline while switching to the browser (Android): keep waiting
            Err(Error::Offline(_)) => {}
            Err(err) => return Err(err),
        }
    }
}

fn worker(shared: SharedState, account: Account) {
    let Some(state_path) = SyncState::file_path() else {
        set_status(&shared, Status::Failed(t!("Kein Datenordner").to_string()));
        return;
    };
    let mut remote = match nextcloud::WebDav::new(&account) {
        Ok(remote) => remote,
        Err(err) => {
            set_status(&shared, Status::Failed(err.to_string()));
            return;
        }
    };
    let remote_id = format!("{}/{}", account.server, account.user_id);
    loop {
        let (dir, ctx) = {
            let mut guard = lock(&shared);
            loop {
                if guard.stop {
                    return;
                }
                let now = Instant::now();
                if now >= guard.next_pass {
                    break;
                }
                let wait = guard.next_pass - now;
                guard = shared
                    .1
                    .wait_timeout(guard, wait)
                    .map(|(g, _)| g)
                    .unwrap_or_else(|e| e.into_inner().0);
            }
            guard.status = Status::Syncing;
            guard.next_pass = Instant::now() + INTERVAL;
            (guard.local_dir.clone(), guard.ctx.clone())
        };
        let mut state = SyncState::load(&state_path, &dir, &remote_id);
        let result = run_pass(&mut remote, &dir, &mut state, &ctx);
        if let Err(err) = state.save(&state_path) {
            eprintln!("[Nextcloud] Sync-Stand nicht gespeichert: {err}");
        }
        let mut guard = lock(&shared);
        if guard.stop {
            return;
        }
        guard.status = match result {
            Ok(report) => {
                if report.changed_local() {
                    guard.changed = true;
                }
                if report != Report::default() {
                    println!("[Nextcloud] {report:?}");
                }
                // Left over: try again soon
                if report.deferred > 0 {
                    guard.next_pass = guard.next_pass.min(Instant::now() + AFTER_SAVE * 2);
                }
                Status::Idle {
                    last: Some(SystemTime::now()),
                }
            }
            Err(Error::Offline(msg)) => Status::Offline(msg),
            Err(err) => Status::Failed(err.to_string()),
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// Server folder in memory; folder ETags change with anything inside, like Nextcloud's.
    #[derive(Default)]
    struct FakeRemote {
        files: BTreeMap<String, (Vec<u8>, String)>,
        dirs: BTreeSet<String>,
        exists: bool,
        /// Refuse this many uploads as changed meanwhile
        refuse_puts: usize,
        counter: u64,
        calls: HashMap<&'static str, usize>,
    }

    impl FakeRemote {
        fn new() -> Self {
            Self {
                exists: true,
                ..Default::default()
            }
        }

        fn next_etag(&mut self) -> String {
            self.counter += 1;
            format!("\"e{}\"", self.counter)
        }

        fn set(&mut self, path: &str, content: &str) {
            let etag = self.next_etag();
            self.add_parents(path);
            self.files
                .insert(path.to_string(), (content.as_bytes().to_vec(), etag));
        }

        fn add_parents(&mut self, path: &str) {
            let parts: Vec<&str> = path.split('/').collect();
            for i in 1..parts.len() {
                self.dirs.insert(parts[..i].join("/"));
            }
        }

        fn content(&self, path: &str) -> Option<String> {
            self.files
                .get(path)
                .map(|(b, _)| String::from_utf8_lossy(b).into_owned())
        }

        fn dir_etag(&self, dir: &str) -> String {
            let prefix = format!("{dir}/");
            let etags: String = self
                .files
                .iter()
                .filter(|(p, _)| p.starts_with(&prefix))
                .map(|(p, (_, e))| format!("{p}{e},"))
                .collect();
            hash_bytes(etags.as_bytes())
        }
    }

    impl Remote for FakeRemote {
        fn list(&mut self, dir: &str) -> Result<Option<Vec<Entry>>, Error> {
            *self.calls.entry("list").or_default() += 1;
            if !self.exists || (!dir.is_empty() && !self.dirs.contains(dir)) {
                return Ok(None);
            }
            let prefix = if dir.is_empty() {
                String::new()
            } else {
                format!("{dir}/")
            };
            let direct = |p: &String| {
                p.strip_prefix(&prefix)
                    .is_some_and(|rest| !rest.is_empty() && !rest.contains('/'))
            };
            let mut entries: Vec<Entry> = self
                .files
                .iter()
                .filter(|(p, _)| direct(p))
                .map(|(p, (_, e))| Entry {
                    path: p.clone(),
                    is_dir: false,
                    etag: e.clone(),
                })
                .collect();
            for d in self.dirs.iter().filter(|d| direct(d)) {
                entries.push(Entry {
                    path: d.clone(),
                    is_dir: true,
                    etag: self.dir_etag(d),
                });
            }
            Ok(Some(entries))
        }

        fn get(&mut self, path: &str) -> Result<(Vec<u8>, String), Error> {
            *self.calls.entry("get").or_default() += 1;
            self.files.get(path).cloned().ok_or(Error::NotFound)
        }

        fn put(&mut self, path: &str, data: &[u8], expect: Expect<'_>) -> Result<String, Error> {
            *self.calls.entry("put").or_default() += 1;
            if self.refuse_puts > 0 {
                self.refuse_puts -= 1;
                return Err(Error::PreconditionFailed);
            }
            let current = self.files.get(path).map(|(_, e)| e.as_str());
            let ok = match expect {
                Expect::New => current.is_none(),
                Expect::Etag(etag) => current == Some(etag),
            };
            if !ok {
                return Err(Error::PreconditionFailed);
            }
            let parent = path.rsplit_once('/').map(|(p, _)| p);
            if parent.is_some_and(|p| !self.dirs.contains(p)) {
                return Err(Error::Other("no parent".into()));
            }
            let etag = self.next_etag();
            self.files
                .insert(path.to_string(), (data.to_vec(), etag.clone()));
            Ok(etag)
        }

        fn mkdir(&mut self, path: &str) -> Result<(), Error> {
            if path.is_empty() {
                self.exists = true;
            } else {
                self.dirs.insert(path.to_string());
            }
            Ok(())
        }

        fn delete(&mut self, path: &str, etag: Option<&str>) -> Result<(), Error> {
            if let Some(etag) = etag {
                if self.files.get(path).map(|(_, e)| e.as_str()) != Some(etag) {
                    return Err(Error::PreconditionFailed);
                }
            }
            let prefix = format!("{path}/");
            self.files
                .retain(|p, _| p != path && !p.starts_with(&prefix));
            self.dirs.retain(|p| p != path && !p.starts_with(&prefix));
            Ok(())
        }
    }

    struct Device {
        dir: tempfile::TempDir,
        state: SyncState,
        ctx: Context,
    }

    impl Device {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            Self {
                state: SyncState {
                    local: dir.path().to_path_buf(),
                    remote: "test".into(),
                    ..Default::default()
                },
                dir,
                ctx: Context::default(),
            }
        }

        fn write(&self, path: &str, content: &str) {
            let target = local_path(self.dir.path(), path);
            fs::create_dir_all(target.parent().unwrap()).unwrap();
            fs::write(target, content).unwrap();
        }

        fn read(&self, path: &str) -> Option<String> {
            fs::read_to_string(local_path(self.dir.path(), path)).ok()
        }

        fn remove(&self, path: &str) {
            fs::remove_file(local_path(self.dir.path(), path)).unwrap();
        }

        fn sync(&mut self, remote: &mut FakeRemote) -> Report {
            run_pass(remote, self.dir.path(), &mut self.state, &self.ctx).unwrap()
        }

        fn files(&self) -> Vec<String> {
            let mut out: Vec<String> = Vec::new();
            fn walk(dir: &Path, prefix: &str, out: &mut Vec<String>) {
                for e in fs::read_dir(dir).unwrap().flatten() {
                    let name = e.file_name().into_string().unwrap();
                    let path = if prefix.is_empty() {
                        name.clone()
                    } else {
                        format!("{prefix}/{name}")
                    };
                    if e.file_type().unwrap().is_dir() {
                        walk(&e.path(), &path, out);
                    } else {
                        out.push(path);
                    }
                }
            }
            walk(self.dir.path(), "", &mut out);
            out.sort();
            out
        }
    }

    fn page(text: &str) -> String {
        format!("{{\"text\":\"{text}\"}}")
    }

    #[test]
    fn filters_synced_files() {
        assert!(is_synced_file("library.json"));
        assert!(is_synced_file("nb_1/manifest.json"));
        assert!(is_synced_file("nb_1/page_12.json"));
        assert!(is_synced_file("nb_1/.opened_by.json"));
        let asset = format!("nb_1/assets/{}.png", "a".repeat(64));
        assert!(is_synced_file(&asset));
        for local_only in [
            "nb_1/.lock",
            "nb_1/page_1.json.tmp.12.34",
            "nb_1/page_1 (conflict 5).json",
            "nb_1/page_1 (conflict 5).json.imported",
            "nb_1/manifest.json.bak",
            "nb_1/page_1.json.corrupt-5",
            ".trash/nb_1/manifest.json",
            "nb_1/assets/x.png",
            "other.txt",
            "a/b/c/d.json",
        ] {
            assert!(!is_synced_file(local_only), "{local_only}");
        }
    }

    #[test]
    fn copies_new_files_both_ways() {
        let mut server = FakeRemote::new();
        let mut a = Device::new();
        a.write("nb/manifest.json", "{}");
        a.write("nb/page_1.json", &page("a"));
        a.write("nb/.lock", "");
        let report = a.sync(&mut server);
        assert_eq!(report.uploaded, 2);
        assert_eq!(server.content("nb/page_1.json"), Some(page("a")));
        assert!(server.content("nb/.lock").is_none());

        let mut b = Device::new();
        let report = b.sync(&mut server);
        assert_eq!(report.downloaded.len(), 2);
        assert_eq!(b.read("nb/page_1.json"), Some(page("a")));

        // Nothing to do afterwards, and unchanged folders aren't listed again
        a.sync(&mut server);
        server.calls.clear();
        assert_eq!(a.sync(&mut server), Report::default());
        assert_eq!(b.sync(&mut server), Report::default());
        assert_eq!(server.calls.get("list"), Some(&2));
        assert_eq!(server.calls.get("get"), None);
    }

    #[test]
    fn creates_the_server_folder() {
        let mut server = FakeRemote::default();
        let mut a = Device::new();
        a.write("library.json", "{}");
        a.sync(&mut server);
        assert!(server.exists);
        assert_eq!(server.content("library.json"), Some("{}".into()));
    }

    #[test]
    fn passes_changes_on() {
        let mut server = FakeRemote::new();
        let (mut a, mut b) = (Device::new(), Device::new());
        a.write("nb/page_1.json", &page("a"));
        a.sync(&mut server);
        b.sync(&mut server);

        b.write("nb/page_1.json", &page("b"));
        assert_eq!(b.sync(&mut server).uploaded, 1);
        assert_eq!(a.sync(&mut server).downloaded, vec!["nb/page_1.json"]);
        assert_eq!(a.read("nb/page_1.json"), Some(page("b")));
    }

    #[test]
    fn keeps_both_versions_of_a_page_changed_on_both_sides() {
        let mut server = FakeRemote::new();
        let (mut a, mut b) = (Device::new(), Device::new());
        a.write("nb/page_1.json", &page("a"));
        a.sync(&mut server);
        b.sync(&mut server);

        a.write("nb/page_1.json", &page("from a"));
        b.write("nb/page_1.json", &page("from b"));
        a.sync(&mut server);
        let report = b.sync(&mut server);
        assert_eq!(report.conflicts, 1);
        // Ours stays and goes up, theirs is kept next to it (locally only)
        assert_eq!(b.read("nb/page_1.json"), Some(page("from b")));
        assert_eq!(server.content("nb/page_1.json"), Some(page("from b")));
        let copy = b
            .files()
            .into_iter()
            .find(|f| f.contains("(conflict"))
            .unwrap();
        assert_eq!(b.read(&copy), Some(page("from a")));
        assert_eq!(
            crate::disk_watch::conflict_copy_of(copy.rsplit('/').next().unwrap()),
            Some(1)
        );
        assert!(!server.files.keys().any(|f| f.contains("conflict")));
    }

    #[test]
    fn keeps_a_conflict_copy_only_once() {
        let mut server = FakeRemote::new();
        let (mut a, mut b) = (Device::new(), Device::new());
        a.write("nb/page_1.json", &page("a"));
        a.sync(&mut server);
        b.write("nb/page_1.json", &page("b"));
        // The upload after keeping the copy is refused (changed again meanwhile)
        server.refuse_puts = 1;
        assert_eq!(b.sync(&mut server).deferred, 1);
        b.sync(&mut server);
        let copies = b
            .files()
            .into_iter()
            .filter(|f| f.contains("(conflict"))
            .count();
        assert_eq!(copies, 1);
        assert_eq!(server.content("nb/page_1.json"), Some(page("b")));
    }

    #[test]
    fn an_empty_page_gives_way() {
        let empty = |id: u64| {
            serde_json::to_string(&storage::PageData::from(&crate::document::Page::new(
                id,
                crate::document::PageBackground::Grid5mm,
            )))
            .unwrap()
        };
        let mut server = FakeRemote::new();
        let (mut a, mut b) = (Device::new(), Device::new());
        a.write("default/page_1.json", &page("written"));
        a.write("default/page_2.json", &empty(2));
        a.sync(&mut server);
        // A new installation with its own empty default notebook
        b.write("default/page_1.json", &empty(1));
        b.write("default/page_2.json", &page("written on b"));
        let report = b.sync(&mut server);
        assert_eq!(report.conflicts, 0);
        assert_eq!(b.read("default/page_1.json"), Some(page("written")));
        assert_eq!(
            server.content("default/page_2.json"),
            Some(page("written on b"))
        );
    }

    #[test]
    fn same_content_on_both_sides_is_no_conflict() {
        let mut server = FakeRemote::new();
        let (mut a, mut b) = (Device::new(), Device::new());
        a.write("nb/page_1.json", &page("same"));
        b.write("nb/page_1.json", &page("same"));
        a.sync(&mut server);
        let report = b.sync(&mut server);
        assert_eq!(report.conflicts, 0);
        assert_eq!(report.uploaded, 0);
        assert_eq!(b.sync(&mut server), Report::default());
    }

    #[test]
    fn deletions_travel_but_changes_win() {
        let mut server = FakeRemote::new();
        let (mut a, mut b) = (Device::new(), Device::new());
        a.write("nb/page_1.json", &page("1"));
        a.write("nb/page_2.json", &page("2"));
        a.sync(&mut server);
        b.sync(&mut server);

        // Deleted on a: gone on the server, then on b (into its trash)
        a.remove("nb/page_1.json");
        assert_eq!(a.sync(&mut server).deleted_remote, 1);
        assert!(server.content("nb/page_1.json").is_none());
        assert_eq!(b.sync(&mut server).deleted_local, 1);
        assert!(b.read("nb/page_1.json").is_none());
        assert!(b
            .files()
            .iter()
            .any(|f| f.starts_with(".trash/") && f.ends_with("nb/page_1.json")));

        // Deleted on a, changed on b: comes back
        a.remove("nb/page_2.json");
        b.write("nb/page_2.json", &page("2 changed"));
        b.sync(&mut server);
        a.sync(&mut server);
        assert_eq!(a.read("nb/page_2.json"), Some(page("2 changed")));
        assert_eq!(server.content("nb/page_2.json"), Some(page("2 changed")));
    }

    #[test]
    fn deleted_notebooks_go_to_the_trash() {
        let mut server = FakeRemote::new();
        let (mut a, mut b) = (Device::new(), Device::new());
        a.write("nb/manifest.json", "{}");
        a.write("nb/page_1.json", &page("1"));
        a.sync(&mut server);
        b.sync(&mut server);

        // The app moves deleted notebooks into .trash
        fs::create_dir_all(a.dir.path().join(".trash")).unwrap();
        fs::rename(a.dir.path().join("nb"), a.dir.path().join(".trash/nb_1")).unwrap();
        assert_eq!(a.sync(&mut server).deleted_remote, 1);
        assert!(!server.dirs.contains("nb"));
        assert!(server.files.is_empty());

        b.write("nb/.lock", "");
        assert_eq!(b.sync(&mut server).deleted_local, 1);
        assert!(!b.dir.path().join("nb").exists());
        assert!(b
            .files()
            .iter()
            .any(|f| f.starts_with(".trash/nb_") && f.ends_with("page_1.json")));
    }

    #[test]
    fn an_open_notebook_deleted_elsewhere_comes_back() {
        let mut server = FakeRemote::new();
        let (mut a, mut b) = (Device::new(), Device::new());
        a.write("nb/manifest.json", "{}");
        a.write("nb/page_1.json", &page("1"));
        a.write("other.json", "");
        a.sync(&mut server);
        b.sync(&mut server);
        server.delete("nb", None).unwrap();
        server.set("library.json", "{}");

        b.ctx.open_notebook = Some("nb".into());
        b.sync(&mut server);
        assert_eq!(b.read("nb/page_1.json"), Some(page("1")));
        assert_eq!(server.content("nb/page_1.json"), Some(page("1")));
    }

    #[test]
    fn waits_with_changes_to_an_open_notebook_with_unsaved_changes() {
        let mut server = FakeRemote::new();
        let (mut a, mut b) = (Device::new(), Device::new());
        a.write("nb/page_1.json", &page("1"));
        a.write("nb2/page_1.json", &page("1"));
        a.sync(&mut server);
        b.sync(&mut server);

        a.write("nb/page_1.json", &page("new"));
        a.write("nb2/page_1.json", &page("new"));
        a.sync(&mut server);
        b.ctx = Context {
            open_notebook: Some("nb".into()),
            busy: true,
        };
        let report = b.sync(&mut server);
        assert_eq!(report.deferred, 1);
        assert_eq!(report.downloaded, vec!["nb2/page_1.json"]);
        assert_eq!(b.read("nb/page_1.json"), Some(page("1")));

        // The folder ETag must not hide the waiting change
        b.ctx.busy = false;
        assert_eq!(b.sync(&mut server).downloaded, vec!["nb/page_1.json"]);
        assert_eq!(b.read("nb/page_1.json"), Some(page("new")));
    }

    #[test]
    fn stops_instead_of_deleting_everything() {
        let mut server = FakeRemote::new();
        let mut a = Device::new();
        a.write("nb/page_1.json", &page("1"));
        a.sync(&mut server);

        // Server folder removed or renamed: nothing is deleted locally
        server.files.clear();
        server.dirs.clear();
        server.exists = false;
        assert!(run_pass(&mut server, a.dir.path(), &mut a.state, &a.ctx).is_err());
        assert_eq!(a.read("nb/page_1.json"), Some(page("1")));

        // A fresh state (after reconnecting) uploads again
        a.state.files.clear();
        a.state.dirs.clear();
        a.sync(&mut server);
        assert_eq!(server.content("nb/page_1.json"), Some(page("1")));

        // Storage folder missing (drive not mounted): nothing is deleted on the server
        let mut b = Device::new();
        b.sync(&mut server);
        let gone = b.dir.path().join("missing");
        let mut state = b.state.clone();
        state
            .files
            .insert("nb/page_1.json".into(), FileState::default());
        assert!(run_pass(&mut server, &gone, &mut state, &b.ctx).is_err());
        assert!(server.content("nb/page_1.json").is_some());
    }

    #[test]
    fn a_first_sync_with_both_sides_filled_deletes_nothing() {
        let mut server = FakeRemote::new();
        server.set("nb/page_1.json", &page("server"));
        let mut a = Device::new();
        a.write("nb/page_2.json", &page("local"));
        a.sync(&mut server);
        assert_eq!(a.read("nb/page_1.json"), Some(page("server")));
        assert_eq!(server.content("nb/page_2.json"), Some(page("local")));
    }

    #[test]
    fn merges_manifests_changed_on_both_sides() {
        let manifest = |pages: &[(u64, &str)], next: u64| {
            let mut m = storage::NotebookManifest::new("nb", "Title");
            m.pages = pages
                .iter()
                .map(|(id, title)| {
                    let mut meta = storage::PageMeta::new(*id, None);
                    meta.title = (!title.is_empty()).then(|| title.to_string());
                    meta
                })
                .collect();
            m.page_ids = pages.iter().map(|(id, _)| *id).collect();
            m.next_page_id = next;
            m.created_at = 1;
            m.modified_at = 1;
            serde_json::to_string(&m).unwrap()
        };
        let mut server = FakeRemote::new();
        let (mut a, mut b) = (Device::new(), Device::new());
        a.write("nb/manifest.json", &manifest(&[(1, ""), (2, "")], 3));
        a.sync(&mut server);
        b.sync(&mut server);

        // a adds page 3 after page 1 and titles page 2; b removes page 1 and adds page 4
        a.write(
            "nb/manifest.json",
            &manifest(&[(1, ""), (3, ""), (2, "Two")], 4),
        );
        b.write("nb/manifest.json", &manifest(&[(2, ""), (4, "")], 5));
        a.sync(&mut server);
        let report = b.sync(&mut server);
        assert_eq!(report.merged, 1);
        let merged: storage::NotebookManifest =
            serde_json::from_str(&b.read("nb/manifest.json").unwrap()).unwrap();
        let pages: Vec<(u64, Option<String>)> = merged
            .pages
            .iter()
            .map(|p| (p.id, p.title.clone()))
            .collect();
        assert_eq!(pages, vec![(3, None), (2, Some("Two".into())), (4, None)]);
        assert_eq!(merged.next_page_id, 5);
        a.sync(&mut server);
        assert_eq!(a.read("nb/manifest.json"), b.read("nb/manifest.json"));
    }

    #[test]
    fn merges_json_lists_and_values() {
        let base = serde_json::json!({"folders": ["A", "B"], "notebooks": {"x": "A"}, "n": 1});
        let ours = serde_json::json!({"folders": ["A", "B", "C"], "notebooks": {"x": "B"}, "n": 2});
        let theirs =
            serde_json::json!({"folders": ["B", "D"], "notebooks": {"x": "A", "y": "D"}, "n": 3});
        assert_eq!(
            merge_json(Some(&base), &ours, &theirs),
            serde_json::json!({"folders": ["B", "D", "C"], "notebooks": {"x": "B", "y": "D"}, "n": 3})
        );
        // Without a common ancestor nothing is removed
        assert_eq!(
            merge_json(None, &serde_json::json!(["A"]), &serde_json::json!(["B"])),
            serde_json::json!(["B", "A"])
        );
    }

    /// Against a real server: `INK_NOTES_NEXTCLOUD_TEST=<file>` with address, login name and app
    /// password on three lines; works in the folder `ink-notes-test` only.
    /// `cargo test --lib real_server -- --ignored`
    #[test]
    #[ignore]
    fn real_server() {
        let file = std::env::var("INK_NOTES_NEXTCLOUD_TEST").expect("INK_NOTES_NEXTCLOUD_TEST");
        let lines: Vec<String> = fs::read_to_string(file)
            .unwrap()
            .lines()
            .map(|l| l.trim().to_string())
            .collect();
        let server = nextcloud::normalize_server(&lines[0]).unwrap();
        let client = nextcloud::Client::new(None);
        nextcloud::check_server(&client, &server).unwrap();
        let user_id = nextcloud::user_id(&client, &server, &lines[1], &lines[2]).unwrap();
        let account = Account {
            server,
            login_name: lines[1].clone(),
            user_id,
            app_password: lines[2].clone(),
            pinned_cert: None,
        };
        let mut remote = nextcloud::WebDav::with_folder(&account, "ink-notes-test").unwrap();
        remote.delete("", None).unwrap();

        let sync = |device: &mut Device, remote: &mut nextcloud::WebDav| {
            run_pass(remote, device.dir.path(), &mut device.state, &device.ctx).unwrap()
        };
        let (mut a, mut b) = (Device::new(), Device::new());
        let asset = format!("Mathe ü 1/assets/{}.png", "ab".repeat(32));
        a.write("library.json", r#"{"folders":[],"notebooks":{}}"#);
        a.write("Mathe ü 1/page_1.json", &page("eins"));
        a.write("Mathe ü 1/page_2.json", &page("zwei"));
        a.write(&asset, "png");
        assert_eq!(sync(&mut a, &mut remote).uploaded, 4);
        assert_eq!(sync(&mut b, &mut remote).downloaded.len(), 4);
        assert_eq!(b.read(&asset), Some("png".into()));
        sync(&mut a, &mut remote);
        assert_eq!(sync(&mut a, &mut remote), Report::default());
        assert_eq!(sync(&mut b, &mut remote), Report::default());

        // Change, conflict, deletion
        a.write("Mathe ü 1/page_1.json", &page("eins von a"));
        b.write("Mathe ü 1/page_1.json", &page("eins von b"));
        a.remove("Mathe ü 1/page_2.json");
        sync(&mut a, &mut remote);
        let report = sync(&mut b, &mut remote);
        assert_eq!(report.conflicts, 1);
        assert_eq!(report.deleted_local, 1);
        sync(&mut a, &mut remote);
        assert_eq!(a.read("Mathe ü 1/page_1.json"), Some(page("eins von b")));
        assert!(b.read("Mathe ü 1/page_2.json").is_none());

        // Deleting the notebook
        fs::create_dir_all(a.dir.path().join(".trash")).unwrap();
        fs::rename(
            a.dir.path().join("Mathe ü 1"),
            a.dir.path().join(".trash/m"),
        )
        .unwrap();
        assert_eq!(sync(&mut a, &mut remote).deleted_remote, 1);
        assert_eq!(sync(&mut b, &mut remote).deleted_local, 1);
        assert!(!b.dir.path().join("Mathe ü 1").exists());
        remote.delete("", None).unwrap();
    }

    #[test]
    fn state_belongs_to_one_pair_of_folders() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let mut state = SyncState::load(&path, Path::new("/a"), "srv");
        state
            .files
            .insert("library.json".into(), FileState::default());
        state.save(&path).unwrap();
        assert_eq!(SyncState::load(&path, Path::new("/a"), "srv"), state);
        assert!(SyncState::load(&path, Path::new("/b"), "srv")
            .files
            .is_empty());
        assert!(SyncState::load(&path, Path::new("/a"), "other")
            .files
            .is_empty());
    }
}
