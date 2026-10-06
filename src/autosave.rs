//! Asynchroner Autosave-Mechanismus mit Debounce-Logik und Hintergrund-Worker-Thread.
//!
//! Stellt sicher, dass Datei-I/O niemals den 120/240 Hz Render-Loop blockiert.
//! Schnelle aufeinanderfolgende Änderungen werden gebündelt (debounced).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::document::{Notebook, Page};
use crate::storage::{self, NotebookManifest, PageData};

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
            AutosaveStatus::Saved => "Gespeichert",
            AutosaveStatus::Pending => "Ungespeichert",
            AutosaveStatus::Saving => "Speichert...",
            AutosaveStatus::Error => "Speicherfehler!",
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
        result: Result<(), String>,
    },
    ManifestSaved {
        result: Result<(), String>,
    },
}

/// Controller zur Verwaltung des asynchronen Autosave-Ablaufs.
pub struct AutosaveController {
    notebook_dir: PathBuf,
    debounce_duration: Duration,
    last_change_time: Option<Instant>,
    pending_pages: HashMap<u64, PageData>,
    pending_manifest: Option<NotebookManifest>,
    status: AutosaveStatus,
    in_flight_count: usize,
    total_page_writes: Arc<AtomicUsize>,
    cmd_tx: Sender<AutosaveCommand>,
    resp_rx: Receiver<AutosaveResponse>,
    worker_handle: Option<JoinHandle<()>>,
}

impl AutosaveController {
    /// Erzeugt einen neuen Controller und startet den Hintergrund-Worker-Thread.
    pub fn new(notebook_dir: PathBuf, debounce_duration: Duration) -> Self {
        let (cmd_tx, cmd_rx) = mpsc::channel::<AutosaveCommand>();
        let (resp_tx, resp_rx) = mpsc::channel::<AutosaveResponse>();
        let total_page_writes = Arc::new(AtomicUsize::new(0));
        let worker_writes_counter = Arc::clone(&total_page_writes);

        let worker_handle = thread::spawn(move || {
            while let Ok(cmd) = cmd_rx.recv() {
                match cmd {
                    AutosaveCommand::SavePage {
                        notebook_dir,
                        page_data,
                    } => {
                        let page_id = page_data.id;
                        let res = storage::save_page(&notebook_dir, &page_data);
                        if res.is_ok() {
                            worker_writes_counter.fetch_add(1, Ordering::SeqCst);
                        }
                        let _ = resp_tx.send(AutosaveResponse::PageSaved {
                            page_id,
                            result: res.map_err(|e| e.to_string()),
                        });
                    }
                    AutosaveCommand::SaveManifest {
                        notebook_dir,
                        manifest,
                    } => {
                        let res = storage::save_manifest(&notebook_dir, &manifest);
                        let _ = resp_tx.send(AutosaveResponse::ManifestSaved {
                            result: res.map_err(|e| e.to_string()),
                        });
                    }
                    AutosaveCommand::Flush(ack_tx) => {
                        let _ = ack_tx.send(());
                    }
                    AutosaveCommand::Shutdown => {
                        break;
                    }
                }
            }
        });

        Self {
            notebook_dir,
            debounce_duration,
            last_change_time: None,
            pending_pages: HashMap::new(),
            pending_manifest: None,
            status: AutosaveStatus::Saved,
            in_flight_count: 0,
            total_page_writes,
            cmd_tx,
            resp_rx,
            worker_handle: Some(worker_handle),
        }
    }

    /// Markiert eine Seite als geändert und startet/erneuert das Debounce-Fenster.
    ///
    /// Nicht geladene oder fehlgeschlagene Ersatzseiten werden nie gespeichert, damit ihre
    /// Originaldatei auf der Festplatte nicht überschrieben wird.
    pub fn mark_page_dirty(&mut self, page: &Page, manifest: &NotebookManifest) {
        if page.is_loaded && !page.load_failed {
            self.pending_pages.insert(page.id, PageData::from(page));
        }
        self.pending_manifest = Some(manifest.clone());
        self.last_change_time = Some(Instant::now());
        self.status = AutosaveStatus::Pending;
    }

    /// Markiert die aktuelle Seite als geändert und gleicht das Manifest (Seitenliste,
    /// aktuelle Seite, ID-Zähler) mit dem Notizbuch ab.
    pub fn mark_dirty(&mut self, notebook: &Notebook, manifest: &NotebookManifest) {
        let mut synced = manifest.clone();
        synced.sync_from_notebook(notebook);
        self.mark_page_dirty(notebook.current_page(), &synced);
    }

    /// Sichert sofort alle geladenen Seiten und das Manifest (Strg+S) und meldet,
    /// ob wirklich alles geschrieben wurde.
    pub fn save_now(
        &mut self,
        notebook: &Notebook,
        manifest: &NotebookManifest,
    ) -> Result<(), String> {
        for page in &notebook.pages {
            if page.is_loaded && !page.load_failed {
                self.pending_pages.insert(page.id, PageData::from(page));
            }
        }
        let mut synced = manifest.clone();
        synced.sync_from_notebook(notebook);
        self.pending_manifest = Some(synced);

        let errors = self.flush_collect_errors();
        if errors.is_empty() {
            // Der komplette Zustand liegt jetzt auf der Festplatte – frühere Fehler sind überholt
            self.status = AutosaveStatus::Saved;
            Ok(())
        } else {
            self.status = AutosaveStatus::Error;
            Err(errors.join("; "))
        }
    }

    /// Prüft den Worker-Kanal und stößt fällige Schreibvorgänge nach Ablauf des Debounce-Timers an.
    ///
    /// Sollte regelmäßig im Event-Loop aufgerufen werden.
    pub fn update(&mut self) -> AutosaveStatus {
        // 1. Antworten des Hintergrund-Workers verarbeiten
        while let Ok(response) = self.resp_rx.try_recv() {
            if self.in_flight_count > 0 {
                self.in_flight_count -= 1;
            }

            match response {
                AutosaveResponse::PageSaved { page_id: _, result } => {
                    if let Err(e) = result {
                        eprintln!("[Autosave-Fehler] Seite konnte nicht gespeichert werden: {e}");
                        self.status = AutosaveStatus::Error;
                    }
                }
                AutosaveResponse::ManifestSaved { result } => {
                    if let Err(e) = result {
                        eprintln!(
                            "[Autosave-Fehler] Manifest konnte nicht gespeichert werden: {e}"
                        );
                        self.status = AutosaveStatus::Error;
                    }
                }
            }

            if self.in_flight_count == 0
                && self.pending_pages.is_empty()
                && self.status != AutosaveStatus::Error
            {
                self.status = AutosaveStatus::Saved;
            }
        }

        // 2. Debounce-Timer prüfen: Wenn Zeit abgelaufen, an Worker übergeben
        if !self.pending_pages.is_empty() {
            if let Some(last_change) = self.last_change_time {
                if last_change.elapsed() >= self.debounce_duration {
                    self.dispatch_pending();
                }
            }
        }

        self.status
    }

    /// Übergibt alle anstehenden Änderungen sofort an den Worker-Kanal.
    fn dispatch_pending(&mut self) {
        for (_page_id, page_data) in self.pending_pages.drain() {
            let cmd = AutosaveCommand::SavePage {
                notebook_dir: self.notebook_dir.clone(),
                page_data,
            };
            if self.cmd_tx.send(cmd).is_ok() {
                self.in_flight_count += 1;
            }
        }

        if let Some(manifest) = self.pending_manifest.take() {
            let cmd = AutosaveCommand::SaveManifest {
                notebook_dir: self.notebook_dir.clone(),
                manifest,
            };
            if self.cmd_tx.send(cmd).is_ok() {
                self.in_flight_count += 1;
            }
        }

        self.status = AutosaveStatus::Saving;
        self.last_change_time = None;
    }

    /// Führt einen synchronen Flush aller noch ausstehenden Änderungen durch (z. B. vor Beenden).
    pub fn flush(&mut self) -> Result<(), String> {
        let errors = self.flush_collect_errors();
        if errors.is_empty() {
            if self.status != AutosaveStatus::Error {
                self.status = AutosaveStatus::Saved;
            }
            Ok(())
        } else {
            self.status = AutosaveStatus::Error;
            Err(errors.join("; "))
        }
    }

    /// Schickt alle anstehenden Änderungen los, wartet, bis der Worker sie abgearbeitet hat,
    /// und liefert alle dabei aufgetretenen Schreibfehler.
    fn flush_collect_errors(&mut self) -> Vec<String> {
        // Falls noch Änderungen im Debounce-Puffer liegen, sofort losschicken
        if !self.pending_pages.is_empty() || self.pending_manifest.is_some() {
            self.dispatch_pending();
        }

        let mut errors = Vec::new();
        let (ack_tx, ack_rx) = mpsc::channel();
        if self.cmd_tx.send(AutosaveCommand::Flush(ack_tx)).is_err() || ack_rx.recv().is_err() {
            errors.push("Autosave-Worker ist nicht erreichbar".to_string());
        }

        // Restliche Antworten leeren (der Worker hat alle vor dem Flush-Befehl abgearbeitet)
        while let Ok(response) = self.resp_rx.try_recv() {
            if self.in_flight_count > 0 {
                self.in_flight_count -= 1;
            }
            match response {
                AutosaveResponse::PageSaved {
                    page_id,
                    result: Err(e),
                } => {
                    eprintln!("[Autosave-Flush] Seite {page_id}: {e}");
                    errors.push(format!("Seite {page_id}: {e}"));
                }
                AutosaveResponse::ManifestSaved { result: Err(e) } => {
                    eprintln!("[Autosave-Flush] Manifest: {e}");
                    errors.push(format!("Manifest: {e}"));
                }
                _ => {}
            }
        }
        errors
    }

    /// Wechselt das Zielverzeichnis für zukünftige Speicheroperationen (z. B. beim Dokumentenwechsel),
    /// nachdem alle noch anstehenden Schreibvorgänge des vorherigen Notizbuchs sicher geflusht wurden.
    /// Liefert die Schreibfehler des vorherigen Notizbuchs zurück; der Status gilt danach
    /// für das neue Notizbuch.
    pub fn switch_notebook_dir(&mut self, new_notebook_dir: PathBuf) -> Result<(), String> {
        let result = self.flush();
        self.notebook_dir = new_notebook_dir;
        self.status = AutosaveStatus::Saved;
        result
    }

    /// Beendet den Hintergrund-Worker ordnungsgemäß nach einem Flush.
    pub fn shutdown(&mut self) {
        if let Err(e) = self.flush() {
            eprintln!("[Shutdown] Nicht alle Änderungen konnten gespeichert werden: {e}");
        }
        let _ = self.cmd_tx.send(AutosaveCommand::Shutdown);
        if let Some(handle) = self.worker_handle.take() {
            let _ = handle.join();
        }
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

impl Drop for AutosaveController {
    fn drop(&mut self) {
        if self.worker_handle.is_some() {
            self.shutdown();
        }
    }
}
