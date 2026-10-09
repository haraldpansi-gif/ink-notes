//! Asynchroner Autosave-Mechanismus mit Debounce- und Max-Wait-Logik,
//! austauschbarem PageStore-Backend und resilientem Hintergrund-Worker.
//!
//! Stellt sicher, dass Datei-I/O niemals den 120/240 Hz Render-Loop blockiert.
//! Schnelle aufeinanderfolgende Änderungen werden gebündelt (debounced).

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::document::{Notebook, Page, PersistState};
use crate::storage::{self, NotebookManifest, PageData, StorageError};
use crate::t;

/// Abstraktion für die Zeitmessung (für deterministische Tests ohne Thread-Sleep).
pub trait Clock: Send + Sync + 'static {
    fn now(&self) -> Instant;
}

/// Standard-Uhr basierend auf der Systemzeit.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// Austauschbares Speicher-Backend für Unit-Tests und alternative Speicherstrategien.
pub trait PageStore: Send + Sync + 'static {
    fn save_page(&self, notebook_dir: &Path, page: &PageData) -> Result<(), StorageError>;
    fn save_manifest(
        &self,
        notebook_dir: &Path,
        manifest: &NotebookManifest,
    ) -> Result<(), StorageError>;
}

/// Standard-Implementierung über das lokale Dateisystem (`src/storage.rs`).
#[derive(Debug, Clone, Copy, Default)]
pub struct DiskPageStore;

impl PageStore for DiskPageStore {
    fn save_page(&self, notebook_dir: &Path, page: &PageData) -> Result<(), StorageError> {
        storage::save_page(notebook_dir, page)
    }

    fn save_manifest(
        &self,
        notebook_dir: &Path,
        manifest: &NotebookManifest,
    ) -> Result<(), StorageError> {
        storage::save_manifest(notebook_dir, manifest)
    }
}

/// Fehler beim Autosave oder Flush.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutosaveError {
    pub failed_page_ids: Vec<u64>,
    pub messages: Vec<String>,
}

impl fmt::Display for AutosaveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&t!(
            "Autosave fehlgeschlagen für Seiten {pages}: {messages}",
            pages = format!("{:?}", self.failed_page_ids),
            messages = self.messages.join("; ")
        ))
    }
}

impl std::error::Error for AutosaveError {}

/// Aktueller Status des Autosave-Controllers für OSD und UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutosaveStatus {
    /// Alle Änderungen sind dauerhaft auf der Festplatte gesichert.
    Saved,
    /// Es liegen ungespeicherte Änderungen vor (im Debounce-Fenster).
    Pending,
    /// Ein Hintergrund-Schreibvorgang wird gerade ausgeführt.
    Saving,
    /// Ein Fehler ist beim Schreiben aufgetreten.
    Error,
}

impl AutosaveStatus {
    pub fn display_text(&self) -> &'static str {
        match self {
            AutosaveStatus::Saved => t!("Gespeichert"),
            AutosaveStatus::Pending => t!("Ungespeichert"),
            AutosaveStatus::Saving => t!("Speichert..."),
            AutosaveStatus::Error => t!("Speicherfehler!"),
        }
    }
}

/// Befehle vom Haupt-Thread an den Hintergrund-Worker.
pub enum AutosaveCommand {
    SavePage {
        notebook_dir: PathBuf,
        page_data: PageData,
    },
    SaveManifest {
        notebook_dir: PathBuf,
        manifest: NotebookManifest,
    },
    Flush(Sender<()>),
    Shutdown,
}

/// Antworten des Hintergrund-Workers an den Haupt-Thread.
pub enum AutosaveResponse {
    PageSaved {
        page_id: u64,
        result: Result<(), (String, Box<PageData>)>,
    },
    ManifestSaved {
        result: Result<(), (String, Box<NotebookManifest>)>,
    },
}

/// Startet einen isolierten Hintergrund-Worker-Thread.
fn spawn_worker(
    cmd_rx: Receiver<AutosaveCommand>,
    resp_tx: Sender<AutosaveResponse>,
    store: Arc<dyn PageStore>,
    worker_writes_counter: Arc<AtomicUsize>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        while let Ok(cmd) = cmd_rx.recv() {
            match cmd {
                AutosaveCommand::SavePage {
                    notebook_dir,
                    page_data,
                } => {
                    let page_id = page_data.id;
                    let store_clone = Arc::clone(&store);
                    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        store_clone.save_page(&notebook_dir, &page_data)
                    }));
                    match res {
                        Ok(Ok(())) => {
                            worker_writes_counter.fetch_add(1, Ordering::SeqCst);
                            let _ = resp_tx.send(AutosaveResponse::PageSaved {
                                page_id,
                                result: Ok(()),
                            });
                        }
                        Ok(Err(e)) => {
                            let _ = resp_tx.send(AutosaveResponse::PageSaved {
                                page_id,
                                result: Err((e.to_string(), Box::new(page_data))),
                            });
                        }
                        Err(_panic) => {
                            let _ = resp_tx.send(AutosaveResponse::PageSaved {
                                page_id,
                                result: Err((
                                    t!("Worker panic beim Speichern der Seite").to_string(),
                                    Box::new(page_data),
                                )),
                            });
                        }
                    }
                }
                AutosaveCommand::SaveManifest {
                    notebook_dir,
                    manifest,
                } => {
                    let store_clone = Arc::clone(&store);
                    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        store_clone.save_manifest(&notebook_dir, &manifest)
                    }));
                    match res {
                        Ok(Ok(())) => {
                            let _ =
                                resp_tx.send(AutosaveResponse::ManifestSaved { result: Ok(()) });
                        }
                        Ok(Err(e)) => {
                            let _ = resp_tx.send(AutosaveResponse::ManifestSaved {
                                result: Err((e.to_string(), Box::new(manifest))),
                            });
                        }
                        Err(_panic) => {
                            let _ = resp_tx.send(AutosaveResponse::ManifestSaved {
                                result: Err((
                                    t!("Worker panic beim Speichern des Manifests").to_string(),
                                    Box::new(manifest),
                                )),
                            });
                        }
                    }
                }
                AutosaveCommand::Flush(ack_tx) => {
                    let _ = ack_tx.send(());
                }
                AutosaveCommand::Shutdown => {
                    break;
                }
            }
        }
    })
}

#[derive(Debug, Clone)]
struct PageRetry {
    next_retry: Instant,
    backoff: Duration,
}

/// Controller zur Verwaltung des asynchronen Autosave-Ablaufs.
pub struct AutosaveController {
    notebook_dir: PathBuf,
    debounce_duration: Duration,
    max_wait_duration: Duration,
    first_change_time: Option<Instant>,
    last_change_time: Option<Instant>,
    pending_pages: HashMap<u64, PageData>,
    pending_manifest: Option<NotebookManifest>,
    retries: HashMap<u64, PageRetry>,
    last_errors: Vec<String>,
    status: AutosaveStatus,
    in_flight_count: usize,
    total_page_writes: Arc<AtomicUsize>,
    store: Arc<dyn PageStore>,
    clock: Arc<dyn Clock>,
    cmd_tx: Sender<AutosaveCommand>,
    resp_rx: Receiver<AutosaveResponse>,
    read_only: bool,
    worker_handle: Option<JoinHandle<()>>,
    worker_restart_count: usize,
}

impl AutosaveController {
    /// Erzeugt einen neuen Controller mit Standardeinstellungen (1.5s Debounce, 10s MaxWait, DiskStore).
    pub fn new(notebook_dir: PathBuf, debounce_duration: Duration) -> Self {
        Self::with_options(
            notebook_dir,
            debounce_duration,
            Duration::from_secs(10),
            Arc::new(DiskPageStore),
            Arc::new(SystemClock),
        )
    }

    /// Voll konfigurierbarer Konstruktor für Tests mit MockClock und FakeStore.
    pub fn with_options(
        notebook_dir: PathBuf,
        debounce_duration: Duration,
        max_wait_duration: Duration,
        store: Arc<dyn PageStore>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        let (cmd_tx, cmd_rx) = mpsc::channel::<AutosaveCommand>();
        let (resp_tx, resp_rx) = mpsc::channel::<AutosaveResponse>();
        let total_page_writes = Arc::new(AtomicUsize::new(0));

        let worker_handle = spawn_worker(
            cmd_rx,
            resp_tx,
            Arc::clone(&store),
            Arc::clone(&total_page_writes),
        );

        Self {
            notebook_dir,
            debounce_duration,
            max_wait_duration,
            first_change_time: None,
            last_change_time: None,
            pending_pages: HashMap::new(),
            pending_manifest: None,
            retries: HashMap::new(),
            last_errors: Vec::new(),
            status: AutosaveStatus::Saved,
            in_flight_count: 0,
            total_page_writes,
            store,
            clock,
            cmd_tx,
            resp_rx,
            read_only: false,
            worker_handle: Some(worker_handle),
            worker_restart_count: 0,
        }
    }

    /// Aktiviert oder deaktiviert den Nur-Lese-Modus (z. B. wenn das Notizbuch von einer anderen Instanz gelockt ist).
    pub fn set_read_only(&mut self, read_only: bool) {
        self.read_only = read_only;
        if read_only {
            self.pending_pages.clear();
            self.pending_manifest = None;
            self.status = AutosaveStatus::Saved;
        }
    }

    /// Prüft, ob der Nur-Lese-Modus aktiv ist.
    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    /// Markiert eine Seite als geändert und startet/erneuert das Debounce-Fenster.
    pub fn mark_page_dirty(&mut self, page: &Page, manifest: &NotebookManifest) {
        // Nicht geladene Platzhalter nie schreiben: Sie sind leer und würden den Inhalt
        // der Seitendatei überschreiben
        if self.read_only
            || !page.is_loaded
            || page.persist == crate::document::PersistState::ReadOnlyAfterLoadError
        {
            return;
        }
        let page_data = PageData::from(page);
        self.pending_pages.insert(page.id, page_data);
        self.pending_manifest = Some(manifest.clone());

        let now = self.clock.now();
        if self.first_change_time.is_none() {
            self.first_change_time = Some(now);
        }
        self.last_change_time = Some(now);

        if self.status != AutosaveStatus::Error {
            self.status = AutosaveStatus::Pending;
        }
    }

    /// Gleicht das Manifest (Seitenliste, aktuelle Seite, ID-Zähler) mit dem Notizbuch ab
    /// und markiert die aktuelle Seite als geändert. So landen auch neu angelegte Seiten
    /// im gespeicherten Manifest.
    pub fn mark_dirty(&mut self, notebook: &Notebook, manifest: &mut NotebookManifest) {
        manifest.sync_from_notebook(notebook);
        self.mark_page_dirty(notebook.current_page(), manifest);
    }

    /// Markiert das Manifest als geändert (z. B. bei Seitenwechsel oder Umbenennen).
    pub fn mark_manifest_dirty(&mut self, manifest: &NotebookManifest) {
        if self.read_only {
            return;
        }
        self.pending_manifest = Some(manifest.clone());

        let now = self.clock.now();
        if self.first_change_time.is_none() {
            self.first_change_time = Some(now);
        }
        self.last_change_time = Some(now);

        if self.status != AutosaveStatus::Error {
            self.status = AutosaveStatus::Pending;
        }
    }

    /// Startet den Worker-Thread neu, falls er unerwartet beendet wurde.
    fn ensure_worker_alive(&mut self) -> bool {
        // Prüfen, ob Worker noch reagiert
        let is_alive = self
            .worker_handle
            .as_ref()
            .map(|h| !h.is_finished())
            .unwrap_or(false);

        if !is_alive && self.worker_restart_count < 3 {
            self.worker_restart_count += 1;
            eprintln!(
                "[Autosave] Worker beendet! Starte Worker neu (Versuch {}/3)...",
                self.worker_restart_count
            );
            let (cmd_tx, cmd_rx) = mpsc::channel();
            let (resp_tx, resp_rx) = mpsc::channel();
            let handle = spawn_worker(
                cmd_rx,
                resp_tx,
                Arc::clone(&self.store),
                Arc::clone(&self.total_page_writes),
            );
            self.cmd_tx = cmd_tx;
            self.resp_rx = resp_rx;
            self.worker_handle = Some(handle);
            return true;
        }
        is_alive
    }

    /// Prüft den Worker-Kanal und stößt fällige Schreibvorgänge nach Ablauf von Debounce oder MaxWait an.
    pub fn update(&mut self) -> AutosaveStatus {
        if self.read_only {
            return AutosaveStatus::Saved;
        }
        let now = self.clock.now();

        // 1. Antworten des Hintergrund-Workers verarbeiten
        while let Ok(response) = self.resp_rx.try_recv() {
            if self.in_flight_count > 0 {
                self.in_flight_count -= 1;
            }

            match response {
                AutosaveResponse::PageSaved { page_id, result } => match result {
                    Ok(()) => {
                        self.retries.remove(&page_id);
                    }
                    Err((err_msg, failed_page)) => {
                        eprintln!("[Autosave-Fehler] Seite {page_id} fehlgeschlagen: {err_msg}");
                        self.status = AutosaveStatus::Error;
                        self.last_errors
                            .push(t!("Seite {id}: {msg}", id = page_id, msg = err_msg));

                        // Wieder einreihen, wenn nicht bereits eine neuere Version vorliegt
                        self.pending_pages.entry(page_id).or_insert(*failed_page);

                        // Exponentieller Backoff
                        let retry = self.retries.entry(page_id).or_insert(PageRetry {
                            next_retry: now,
                            backoff: Duration::from_secs(1),
                        });
                        retry.next_retry = now + retry.backoff;
                        retry.backoff = (retry.backoff * 2).min(Duration::from_secs(30));
                    }
                },
                AutosaveResponse::ManifestSaved { result } => {
                    if let Err((err_msg, failed_manifest)) = result {
                        eprintln!("[Autosave-Fehler] Manifest fehlgeschlagen: {err_msg}");
                        self.status = AutosaveStatus::Error;
                        self.last_errors.push(t!("Manifest: {msg}", msg = err_msg));
                        if self.pending_manifest.is_none() {
                            self.pending_manifest = Some(*failed_manifest);
                        }
                    }
                }
            }

            if self.in_flight_count == 0
                && self.pending_pages.is_empty()
                && self.pending_manifest.is_none()
            {
                self.last_errors.clear();
                self.status = AutosaveStatus::Saved;
            }
        }

        // 2. Prüfen, ob anstehende Änderungen fällig sind (Debounce ODER Max-Wait ODER Backoff-Retry)
        let has_pending = !self.pending_pages.is_empty() || self.pending_manifest.is_some();
        if has_pending {
            let max_wait_hit = self
                .first_change_time
                .map(|t| now.duration_since(t) >= self.max_wait_duration)
                .unwrap_or(false);

            let debounce_hit = self
                .last_change_time
                .map(|t| now.duration_since(t) >= self.debounce_duration)
                .unwrap_or(false);

            let retry_ready = !self.retries.is_empty()
                && self.pending_pages.keys().any(|id| {
                    self.retries
                        .get(id)
                        .map(|r| now >= r.next_retry)
                        .unwrap_or(false)
                });

            if max_wait_hit || debounce_hit || retry_ready {
                self.dispatch_pending();
            }
        }

        self.status
    }

    /// Übergibt alle fälligen Änderungen sofort an den Worker-Kanal.
    fn dispatch_pending(&mut self) {
        let now = self.clock.now();
        self.ensure_worker_alive();

        // Nur Seiten dispatchen, deren Backoff abgelaufen ist
        let mut to_dispatch = Vec::new();
        let page_ids: Vec<u64> = self.pending_pages.keys().copied().collect();
        for id in page_ids {
            let ready = self
                .retries
                .get(&id)
                .map(|r| now >= r.next_retry)
                .unwrap_or(true);
            if ready {
                if let Some(page_data) = self.pending_pages.remove(&id) {
                    to_dispatch.push(page_data);
                }
            }
        }

        for page_data in to_dispatch {
            let page_id = page_data.id;
            let cmd = AutosaveCommand::SavePage {
                notebook_dir: self.notebook_dir.clone(),
                page_data: page_data.clone(),
            };
            if self.cmd_tx.send(cmd).is_ok() {
                self.in_flight_count += 1;
            } else {
                // Senden fehlgeschlagen -> zurück in den Puffer
                self.status = AutosaveStatus::Error;
                self.pending_pages.insert(page_id, page_data);
                self.ensure_worker_alive();
            }
        }

        if let Some(manifest) = self.pending_manifest.take() {
            let cmd = AutosaveCommand::SaveManifest {
                notebook_dir: self.notebook_dir.clone(),
                manifest: manifest.clone(),
            };
            if self.cmd_tx.send(cmd).is_ok() {
                self.in_flight_count += 1;
            } else {
                self.status = AutosaveStatus::Error;
                self.pending_manifest = Some(manifest);
                self.ensure_worker_alive();
            }
        }

        if self.in_flight_count > 0 {
            if self.status != AutosaveStatus::Error {
                self.status = AutosaveStatus::Saving;
            }
            self.first_change_time = None;
            self.last_change_time = None;
        }
    }

    /// Führt einen synchronen Flush aller anstehenden Änderungen durch.
    /// Gibt `Result<(), AutosaveError>` mit allen Fehlermeldungen zurück.
    pub fn flush(&mut self) -> Result<(), AutosaveError> {
        if self.read_only {
            return Ok(());
        }
        // Backoff für den Flush ignorieren, damit sofort geschrieben wird
        self.retries.clear();

        if !self.pending_pages.is_empty() || self.pending_manifest.is_some() {
            self.dispatch_pending();
        }

        let (ack_tx, ack_rx) = mpsc::channel();
        if self.cmd_tx.send(AutosaveCommand::Flush(ack_tx)).is_ok() {
            let _ = ack_rx.recv();
        }

        // Restliche Antworten leeren
        while let Ok(response) = self.resp_rx.try_recv() {
            if self.in_flight_count > 0 {
                self.in_flight_count -= 1;
            }
            match response {
                AutosaveResponse::PageSaved { page_id, result } => match result {
                    Ok(()) => {
                        self.retries.remove(&page_id);
                    }
                    Err((e, failed_page)) => {
                        self.status = AutosaveStatus::Error;
                        self.last_errors
                            .push(t!("Seite {id}: {msg}", id = page_id, msg = e));
                        self.pending_pages.insert(page_id, *failed_page);
                        let now = self.clock.now();
                        let retry = self.retries.entry(page_id).or_insert(PageRetry {
                            next_retry: now,
                            backoff: Duration::from_secs(1),
                        });
                        retry.next_retry = now + retry.backoff;
                        retry.backoff = (retry.backoff * 2).min(Duration::from_secs(30));
                    }
                },
                AutosaveResponse::ManifestSaved { result } => {
                    if let Err((e, failed_manifest)) = result {
                        self.status = AutosaveStatus::Error;
                        self.last_errors.push(t!("Manifest: {msg}", msg = e));
                        self.pending_manifest = Some(*failed_manifest);
                    }
                }
            }
        }

        if self.in_flight_count == 0
            && self.pending_pages.is_empty()
            && self.pending_manifest.is_none()
        {
            self.last_errors.clear();
            self.status = AutosaveStatus::Saved;
            Ok(())
        } else {
            self.status = AutosaveStatus::Error;
            let failed_page_ids: Vec<u64> = self.pending_pages.keys().copied().collect();
            let messages = std::mem::take(&mut self.last_errors);
            Err(AutosaveError {
                failed_page_ids,
                messages,
            })
        }
    }

    /// Wartet synchron darauf, dass der Worker alle bereits in der Warteschlange befindlichen Aufgaben ausführt.
    /// Startet keine neuen Schreibvorgänge (im Unterschied zu `flush()`).
    pub fn sync_worker(&self) {
        let (ack_tx, ack_rx) = mpsc::channel();
        if self.cmd_tx.send(AutosaveCommand::Flush(ack_tx)).is_ok() {
            let _ = ack_rx.recv();
        }
    }

    /// Wechselt das Zielverzeichnis für zukünftige Speicheroperationen (z. B. beim Dokumentenwechsel).
    pub fn switch_notebook_dir(&mut self, new_notebook_dir: PathBuf) {
        let _ = self.flush();
        self.notebook_dir = new_notebook_dir;
    }

    /// Beendet den Hintergrund-Worker ordnungsgemäß nach einem Flush.
    pub fn shutdown(&mut self) -> Result<(), AutosaveError> {
        let res = self.flush();
        let _ = self.cmd_tx.send(AutosaveCommand::Shutdown);
        if let Some(handle) = self.worker_handle.take() {
            let _ = handle.join();
        }
        res
    }

    /// Schreibt alle ungespeicherten/ausstehenden Seiten als Notfall-Sicherung
    /// in $XDG_DATA_HOME/ink-notes/recovery/ (Dateiname: recovery_<page_id>_<unix_ts>.json).
    pub fn export_recovery(
        &self,
        fallback_notebook: Option<&Notebook>,
    ) -> Result<Vec<PathBuf>, std::io::Error> {
        let rec_dir = recovery_dir();
        fs::create_dir_all(&rec_dir)?;
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let mut written = Vec::new();
        let mut exported_ids = HashSet::new();

        // 1. Alle in pending_pages wartenden Seiten sichern
        for (&page_id, page_data) in &self.pending_pages {
            let filename = format!("recovery_{}_{}.json", page_id, ts);
            let file_path = rec_dir.join(filename);
            let json = serde_json::to_vec_pretty(page_data)?;
            fs::write(&file_path, json)?;
            exported_ids.insert(page_id);
            written.push(file_path);
        }

        // 2. Falls ein Notizbuch übergeben wurde, alle dirty-Seiten sichern, die noch nicht erfasst wurden
        if let Some(nb) = fallback_notebook {
            for page in &nb.pages {
                if !exported_ids.contains(&page.id)
                    && (page.is_dirty || page.persist == PersistState::Normal)
                {
                    let page_data = PageData::from(page);
                    let filename = format!("recovery_{}_{}.json", page.id, ts);
                    let file_path = rec_dir.join(filename);
                    let json = serde_json::to_vec_pretty(&page_data)?;
                    fs::write(&file_path, json)?;
                    written.push(file_path);
                }
            }
        }

        Ok(written)
    }

    /// Gibt die Gesamtzahl aller tatsächlich auf die Disk geschriebenen Seiten zurück.
    pub fn total_page_writes(&self) -> usize {
        self.total_page_writes.load(Ordering::SeqCst)
    }

    /// Gibt den aktuellen Status zurück.
    pub fn status(&self) -> AutosaveStatus {
        self.status
    }
}

/// Liefert das Verzeichnis für Notfall-Wiederherstellungsdateien ($XDG_DATA_HOME/ink-notes/recovery).
pub fn recovery_dir() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_DATA_HOME") {
        if !xdg.is_empty() {
            return PathBuf::from(xdg).join("ink-notes").join("recovery");
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() {
            return PathBuf::from(home)
                .join(".local")
                .join("share")
                .join("ink-notes")
                .join("recovery");
        }
    }
    PathBuf::from(".").join(".recovery")
}

impl Drop for AutosaveController {
    fn drop(&mut self) {
        if self.worker_handle.is_some() {
            if let Err(e) = self.shutdown() {
                eprintln!("[Autosave] Fehler beim geordneten Beenden in Drop: {e}");
            }
        }
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::canvas::InkVertex;
    use crate::document::{PageBackground, Stroke, StylusSample};
    use std::sync::Mutex;

    /// Mock-Uhr für deterministische, sofortige Tests ohne thread::sleep.
    pub struct MockClock {
        now: Mutex<Instant>,
    }

    impl MockClock {
        pub fn new(start: Instant) -> Self {
            Self {
                now: Mutex::new(start),
            }
        }

        pub fn advance(&self, duration: Duration) {
            let mut guard = self.now.lock().unwrap();
            *guard += duration;
        }
    }

    impl Clock for MockClock {
        fn now(&self) -> Instant {
            *self.now.lock().unwrap()
        }
    }

    /// Fake-PageStore zum Simulieren von I/O-Fehlern und Panics.
    pub struct FakePageStore {
        pub fail_page_writes: Mutex<usize>,
        pub panic_on_page: Mutex<bool>,
        pub page_writes: Arc<AtomicUsize>,
        pub manifest_writes: Arc<AtomicUsize>,
    }

    impl FakePageStore {
        pub fn new() -> Self {
            Self {
                fail_page_writes: Mutex::new(0),
                panic_on_page: Mutex::new(false),
                page_writes: Arc::new(AtomicUsize::new(0)),
                manifest_writes: Arc::new(AtomicUsize::new(0)),
            }
        }
    }

    impl Default for FakePageStore {
        fn default() -> Self {
            Self::new()
        }
    }

    impl PageStore for FakePageStore {
        fn save_page(&self, _dir: &Path, _page: &PageData) -> Result<(), StorageError> {
            if *self.panic_on_page.lock().unwrap() {
                panic!("Simulierte Panic im Worker!");
            }
            let mut fails = self.fail_page_writes.lock().unwrap();
            if *fails > 0 {
                *fails -= 1;
                return Err(StorageError::InvalidManifest(
                    "Simulierter Store-Fehler".to_string(),
                ));
            }
            self.page_writes.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        fn save_manifest(
            &self,
            _dir: &Path,
            _manifest: &NotebookManifest,
        ) -> Result<(), StorageError> {
            self.manifest_writes.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[test]
    fn test_debounce_bundles_changes() {
        let clock = Arc::new(MockClock::new(Instant::now()));
        let store = Arc::new(FakePageStore::new());
        let page_writes_counter = Arc::clone(&store.page_writes);

        let debounce = Duration::from_millis(1500);
        let max_wait = Duration::from_secs(10);
        let mut controller = AutosaveController::with_options(
            PathBuf::from("/tmp/test"),
            debounce,
            max_wait,
            store,
            Arc::clone(&clock) as Arc<dyn Clock>,
        );

        let manifest = NotebookManifest::new("nb1", "Test");
        let mut page = Page::new(1, PageBackground::Grid5mm);

        // 10 schnelle Änderungen, jeweils 100 ms Abstand (unter 1500 ms Debounce)
        for i in 0..10 {
            page.add_stroke(Stroke::new(
                i + 1,
                vec![StylusSample {
                    position: glam::Vec2::new(i as f32, 0.0),
                    pressure: 0.5,
                    timestamp: i as f64 * 0.1,
                }],
                vec![InkVertex::new(i as f32, 0.0, [0.0, 0.0, 0.0, 1.0])],
                [0.0, 0.0, 0.0, 1.0],
            ));
            controller.mark_page_dirty(&page, &manifest);
            clock.advance(Duration::from_millis(100));
            controller.update();
        }

        // Während der schnellen Änderungen darf noch nichts geschrieben sein
        assert_eq!(page_writes_counter.load(Ordering::SeqCst), 0);

        // Zeit um 1600 ms voranschreiten lassen (> 1500 ms Debounce)
        clock.advance(Duration::from_millis(1600));
        controller.update();
        let _ = controller.flush();

        // Genau 1 gebündelter Schreibvorgang
        assert_eq!(page_writes_counter.load(Ordering::SeqCst), 1);
        assert_eq!(controller.status(), AutosaveStatus::Saved);
    }

    #[test]
    fn test_max_wait_forces_flush_during_continuous_edits() {
        let clock = Arc::new(MockClock::new(Instant::now()));
        let store = Arc::new(FakePageStore::new());
        let page_writes_counter = Arc::clone(&store.page_writes);

        let debounce = Duration::from_millis(1500);
        let max_wait = Duration::from_secs(10);
        let mut controller = AutosaveController::with_options(
            PathBuf::from("/tmp/test"),
            debounce,
            max_wait,
            store,
            Arc::clone(&clock) as Arc<dyn Clock>,
        );

        let manifest = NotebookManifest::new("nb2", "Test");
        let mut page = Page::new(1, PageBackground::Grid5mm);

        // Kontinuierliche Änderungen alle 500 ms über insgesamt 12 Sekunden (Debounce 1500ms würde nie greifen!)
        for i in 0..24 {
            page.add_stroke(Stroke::new(i + 1, vec![], vec![], [0.0, 0.0, 0.0, 1.0]));
            controller.mark_page_dirty(&page, &manifest);
            clock.advance(Duration::from_millis(500));
            controller.update();
        }

        // Nach 12s (> 10s max_wait) MUSS mindestens ein Schreibvorgang erzwungen worden sein!
        controller.sync_worker();
        let writes = page_writes_counter.load(Ordering::SeqCst);
        assert!(
            writes >= 1,
            "MaxWait muss Schreiben erzwingen, selbst wenn Debounce nie abläuft"
        );
    }

    #[test]
    fn test_error_retry_with_backoff_and_recovery() {
        let clock = Arc::new(MockClock::new(Instant::now()));
        let store = Arc::new(FakePageStore::new());
        *store.fail_page_writes.lock().unwrap() = 2; // Die ersten 2 Versuche schlagen fehl

        let mut controller = AutosaveController::with_options(
            PathBuf::from("/tmp/test"),
            Duration::from_millis(500),
            Duration::from_secs(10),
            store,
            Arc::clone(&clock) as Arc<dyn Clock>,
        );

        let manifest = NotebookManifest::new("nb3", "Test");
        let mut page = Page::new(1, PageBackground::Grid5mm);
        page.add_stroke(Stroke::new(1, vec![], vec![], [0.0, 0.0, 0.0, 1.0]));

        controller.mark_page_dirty(&page, &manifest);
        clock.advance(Duration::from_millis(600));
        controller.update();

        // Erster Versuch fehlgeschlagen -> Status Error
        let flush_err = controller.flush();
        assert!(
            flush_err.is_err(),
            "Flush muss fehlschlagen, solange Store fehlschlägt"
        );
        assert_eq!(controller.status(), AutosaveStatus::Error);

        // Nach Ablauf des Backoffs erneuter Versuch (Versuch 2 schlägt fehl)
        clock.advance(Duration::from_secs(2));
        controller.update();
        controller.sync_worker();
        controller.update();
        assert_eq!(controller.status(), AutosaveStatus::Error);

        // Nach Ablauf des neuen Backoffs dritter Versuch (Store fehlerfrei -> gelingt):
        clock.advance(Duration::from_secs(4));
        let flush_ok = controller.flush();
        assert!(
            flush_ok.is_ok(),
            "Dritter Versuch muss gelingen und Status auf Saved setzen"
        );
        assert_eq!(controller.status(), AutosaveStatus::Saved);
    }

    #[test]
    fn test_worker_panic_catch_and_restart() {
        let clock = Arc::new(MockClock::new(Instant::now()));
        let store = Arc::new(FakePageStore::new());
        *store.panic_on_page.lock().unwrap() = true; // Panic auslösen

        let mut controller = AutosaveController::with_options(
            PathBuf::from("/tmp/test"),
            Duration::from_millis(500),
            Duration::from_secs(10),
            store,
            Arc::clone(&clock) as Arc<dyn Clock>,
        );

        let manifest = NotebookManifest::new("nb4", "Test");
        let mut page = Page::new(1, PageBackground::Grid5mm);
        page.add_stroke(Stroke::new(1, vec![], vec![], [0.0, 0.0, 0.0, 1.0]));

        controller.mark_page_dirty(&page, &manifest);
        clock.advance(Duration::from_millis(600));
        controller.update();

        // Worker fängt Panic ab und meldet Fehler zurück
        let res = controller.flush();
        assert!(res.is_err());
        assert_eq!(controller.status(), AutosaveStatus::Error);
    }

    #[test]
    fn test_manifest_only_dirty_triggers_autosave() {
        let clock = Arc::new(MockClock::new(Instant::now()));
        let store = Arc::new(FakePageStore::new());
        let manifest_writes_counter = Arc::clone(&store.manifest_writes);

        let mut controller = AutosaveController::with_options(
            PathBuf::from("/tmp/test"),
            Duration::from_millis(500),
            Duration::from_secs(10),
            store,
            Arc::clone(&clock) as Arc<dyn Clock>,
        );

        let manifest = NotebookManifest::new("nb5", "Nur Manifest Geändert");
        controller.mark_manifest_dirty(&manifest);

        clock.advance(Duration::from_millis(600));
        controller.update();
        let _ = controller.flush();

        assert_eq!(manifest_writes_counter.load(Ordering::SeqCst), 1);
        assert_eq!(controller.status(), AutosaveStatus::Saved);
    }

    #[test]
    fn test_export_recovery_on_failure() {
        let clock = Arc::new(MockClock::new(Instant::now()));
        let store = FakePageStore::new();
        // Alle Schreibvorgänge fehlschlagen lassen
        *store.fail_page_writes.lock().unwrap() = 10;

        let temp_recovery_dir = std::env::temp_dir().join(format!(
            "ink_notes_rec_test_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::env::set_var("XDG_DATA_HOME", &temp_recovery_dir);

        let mut controller = AutosaveController::with_options(
            PathBuf::from("/tmp/test_rec"),
            Duration::from_millis(100),
            Duration::from_secs(10),
            Arc::new(store),
            Arc::clone(&clock) as Arc<dyn Clock>,
        );

        let page = Page::new(42, PageBackground::Grid5mm);
        let manifest = NotebookManifest::new("rec_nb", "Recovery Test");
        controller.mark_page_dirty(&page, &manifest);

        // Flush schlägt fehl
        let flush_res = controller.flush();
        assert!(flush_res.is_err(), "Flush muss fehlschlagen");
        assert_eq!(controller.status(), AutosaveStatus::Error);

        // Notfall-Export ausführen
        let written_files = controller
            .export_recovery(None)
            .expect("Notfall-Export muss erfolgreich sein");
        assert_eq!(written_files.len(), 1);
        let file = &written_files[0];
        assert!(file.exists());

        let content = std::fs::read_to_string(file).expect("Datei lesbar");
        let parsed: PageData = serde_json::from_str(&content).expect("Gültiges JSON");
        assert_eq!(parsed.id, 42);

        // Aufräumen
        let _ = std::fs::remove_dir_all(&temp_recovery_dir);
    }

    #[test]
    fn test_snapshot_performance() {
        let store = Arc::new(FakePageStore::new());
        let clock = Arc::new(MockClock::new(Instant::now()));
        let mut controller = AutosaveController::with_options(
            PathBuf::from("/tmp/test_perf"),
            Duration::from_millis(1500),
            Duration::from_secs(10),
            store,
            Arc::clone(&clock) as Arc<dyn Clock>,
        );

        let manifest = NotebookManifest::new("perf_nb", "Perf Test");

        fn make_page(num_samples: usize) -> Page {
            let mut page = Page::new(1, PageBackground::Blank);
            let samples_per_stroke = 100;
            let num_strokes = num_samples / samples_per_stroke;
            for s in 0..num_strokes {
                let mut samples = Vec::with_capacity(samples_per_stroke);
                for i in 0..samples_per_stroke {
                    samples.push(StylusSample {
                        position: glam::Vec2::new(s as f32 * 10.0 + i as f32, i as f32),
                        pressure: 0.5,
                        timestamp: i as f64 * 0.01,
                    });
                }
                page.add_stroke(Stroke::new(
                    (s + 1) as u64,
                    samples,
                    Vec::new(),
                    [0.0, 0.0, 0.0, 1.0],
                ));
            }
            page
        }

        // 1. 1 000 Samples
        let p_1k = make_page(1_000);
        let start = Instant::now();
        controller.mark_page_dirty(&p_1k, &manifest);
        let dur_1k = start.elapsed();

        // 2. 10 000 Samples
        let p_10k = make_page(10_000);
        let start = Instant::now();
        controller.mark_page_dirty(&p_10k, &manifest);
        let dur_10k = start.elapsed();

        // 3. 50 000 Samples
        let p_50k = make_page(50_000);
        let start = Instant::now();
        controller.mark_page_dirty(&p_50k, &manifest);
        let dur_50k = start.elapsed();

        println!(
            "\n[Benchmark Snapshot] 1k Samples: {:?} | 10k Samples: {:?} | 50k Samples: {:?}",
            dur_1k, dur_10k, dur_50k
        );
    }
}
