//! Crash-Simulationstesthelfer für ink-notes.
//! Wird von scripts/crash-test.sh verwendet, um unvollständige Schreibvorgänge
//! und SIGKILL (kill -9) Robustheit zu verifizieren.

use std::env;
use std::path::Path;
use std::process;
use std::time::Duration;

use glam::Vec2;
use ink_notes::document::{Notebook, PageBackground, StylusSample};
use ink_notes::spline::{finalize_stroke_with_config, SplineConfig};
use ink_notes::storage::{
    cleanup_orphaned_temp_files, load_notebook, load_page, save_manifest, save_notebook, save_page,
    NotebookManifest, PageData,
};

fn run_init(dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let nb = Notebook::new();
    let id = dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("crash_test");
    let mut m = NotebookManifest::new(id, "Crash Test Notebook");
    m.sync_from_notebook(&nb);
    save_notebook(dir, &nb, &mut m)?;
    println!("[init] Initialized clean notebook in {:?}", dir);
    Ok(())
}

fn run_worker(dir: &Path) {
    let (mut notebook, mut manifest) = match load_notebook(dir) {
        Ok((nb, m)) => (nb, m),
        Err(_) => {
            let nb = Notebook::new();
            let id = dir
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("crash_test");
            let mut m = NotebookManifest::new(id, "Crash Test Notebook");
            m.sync_from_notebook(&nb);
            let _ = save_notebook(dir, &nb, &mut m);
            (nb, m)
        }
    };

    let spline_cfg = SplineConfig::default();
    let mut stroke_counter = manifest.next_stroke_id;

    println!("[worker] Started writing loop in {:?}", dir);

    loop {
        // Erzeuge neuen Strich mit einigen Samples
        let base_x = (stroke_counter % 500) as f32;
        let samples = vec![
            StylusSample {
                position: Vec2::new(base_x, 100.0),
                pressure: 0.5,
                timestamp: 0.0,
            },
            StylusSample {
                position: Vec2::new(base_x + 10.0, 150.0),
                pressure: 0.7,
                timestamp: 0.01,
            },
            StylusSample {
                position: Vec2::new(base_x + 20.0, 200.0),
                pressure: 0.6,
                timestamp: 0.02,
            },
        ];

        let stroke = finalize_stroke_with_config(
            stroke_counter,
            &samples,
            [0.0, 0.0, 0.0, 1.0],
            &spline_cfg,
        );
        stroke_counter += 1;
        manifest.next_stroke_id = stroke_counter;

        let page = notebook.current_page_mut();
        page.add_stroke(stroke);

        let page_data = PageData::from(&*page);

        // 1. Schreibe Seite atomar
        if let Err(e) = save_page(dir, &page_data) {
            eprintln!("[worker] Error saving page: {:?}", e);
        }

        // 2. Aktualisiere Manifest atomar
        manifest.sync_from_notebook(&notebook);
        if let Err(e) = save_manifest(dir, &manifest) {
            eprintln!("[worker] Error saving manifest: {:?}", e);
        }

        // 3. Gelegentlich Gesamtsave (wie Autosave-Flush)
        if stroke_counter % 5 == 0 {
            if let Err(e) = save_notebook(dir, &notebook, &mut manifest) {
                eprintln!("[worker] Error saving notebook: {:?}", e);
            }
        }

        // 4. Evtl. neue Seite anlegen und sofort sichern, bevor Manifest aktualisiert wird
        if stroke_counter % 25 == 0 {
            let page_idx = notebook.add_page(PageBackground::DotGrid);
            notebook.current_page_index = page_idx;
            let new_page_data = PageData::from(notebook.current_page());
            if let Err(e) = save_page(dir, &new_page_data) {
                eprintln!("[worker] Error saving new page: {:?}", e);
            }
            manifest.sync_from_notebook(&notebook);
            if let Err(e) = save_manifest(dir, &manifest) {
                eprintln!("[worker] Error saving manifest: {:?}", e);
            }
        }
    }
}

fn run_verify(dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    println!("[verify] Checking notebook in {:?}", dir);

    // 1. Bereinige eventuelle verwaiste .tmp-Dateien
    let _ = cleanup_orphaned_temp_files(dir, Duration::from_secs(0));

    // Prüfe, ob das Verzeichnis noch gar kein Notizbuch enthält (Prozess vor erstem Flush getötet)
    let manifest_path = dir.join("manifest.json");
    let manifest_bak = dir.join("manifest.json.bak");
    if !manifest_path.exists() && !manifest_bak.exists() {
        println!("[verify] Notebook not yet created on disk (killed prior to first save). Valid empty state.");
        return Ok(());
    }

    // 2. Notizbuch laden
    let (_notebook, manifest) = match load_notebook(dir) {
        Ok(res) => res,
        Err(e) => {
            eprintln!("[verify] FAILED to load notebook: {:?}", e);
            return Err(Box::new(e));
        }
    };

    println!(
        "[verify] Manifest loaded successfully: title='{}', pages={}, next_stroke_id={}",
        manifest.title,
        manifest.pages.len(),
        manifest.next_stroke_id
    );

    // 3. Alle im Manifest registrierten Seiten laden und prüfen
    for page_meta in &manifest.pages {
        match load_page(dir, page_meta.id) {
            Ok(page_data) => {
                let page = page_data.to_page();
                println!(
                    "[verify] Page {} verified: {} strokes, background={:?}",
                    page_meta.id,
                    page.strokes.len(),
                    page.background
                );
            }
            Err(e) => {
                eprintln!(
                    "[verify] FAILED to load page {} referenced by manifest: {:?}",
                    page_meta.id, e
                );
                return Err(Box::new(e));
            }
        }
    }

    // 4. Prüfen ob Quarantäne-Dateien existieren
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let file_name = entry.file_name();
            let name = file_name.to_string_lossy();
            if name.contains(".corrupt-") {
                println!("[verify] Notice: Found quarantined file: {}", name);
            }
        }
    }

    println!("[verify] SUCCESS: All pages and manifest are fully consistent!");
    Ok(())
}

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 3 {
        eprintln!("Usage: crash_sim <init|worker|verify> <notebook_dir>");
        process::exit(2);
    }

    let command = &args[1];
    let notebook_dir = Path::new(&args[2]);

    match command.as_str() {
        "init" => match run_init(notebook_dir) {
            Ok(_) => process::exit(0),
            Err(e) => {
                eprintln!("[init] Error: {}", e);
                process::exit(1);
            }
        },
        "worker" => {
            run_worker(notebook_dir);
        }
        "verify" => match run_verify(notebook_dir) {
            Ok(_) => process::exit(0),
            Err(e) => {
                eprintln!("[verify] Error: {}", e);
                process::exit(1);
            }
        },
        other => {
            eprintln!("Unknown command: {}", other);
            process::exit(2);
        }
    }
}
