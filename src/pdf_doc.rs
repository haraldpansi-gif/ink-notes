//! Lesen und Rendern importierter PDF-Dateien mit `hayro` (reines Rust, nur CPU).
//!
//! Das Rendern einer Seite kann einige hundert Millisekunden dauern; [`PdfRenderService`]
//! erledigt es deshalb in einem Hintergrund-Thread, samt Mipmaps für die Anzeige, und meldet
//! fertige Seiten zurück.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Condvar, Mutex};

use hayro::hayro_interpret::InterpreterSettings;
use hayro::hayro_syntax::{LoadPdfError, Pdf};
use hayro::vello_cpu::color::palette::css::WHITE;
use hayro::{PixmapSettings, RenderCache, RenderSettings};

use crate::assets::RgbaImage;
use crate::t;

/// Längste Kante einer gerenderten Seite für die Anzeige in Pixeln (A4 ≈ 220 dpi).
pub const DISPLAY_RENDER_PIXELS: u32 = 2560;
/// Längste Kante beim starken Hineinzoomen (A4 ≈ 440 dpi).
pub const ZOOMED_RENDER_PIXELS: u32 = 5120;

/// Texturschlüssel einer PDF-Seite: `<asset>#<seite>`, gerendert mit mehr als
/// [`DISPLAY_RENDER_PIXELS`] als `<asset>#<seite>@<pixel>`. Liefert Asset, Seite und Pixel.
pub fn parse_page_key(key: &str) -> Option<(&str, usize, u32)> {
    let (asset, rest) = key.split_once('#')?;
    let (page, pixels) = match rest.split_once('@') {
        Some((page, pixels)) => (page, pixels.parse().ok()?),
        None => (rest, DISPLAY_RENDER_PIXELS),
    };
    Some((asset, page.parse().ok()?, pixels))
}
/// Größte Seitenzahl, die importiert wird.
pub const MAX_IMPORT_PAGES: usize = 2000;

/// Öffnet ein PDF und liefert die angezeigte Größe jeder Seite in Punkt (Drehung
/// berücksichtigt).
pub fn page_sizes(bytes: Vec<u8>) -> Result<Vec<(f32, f32)>, String> {
    let pdf = open(bytes)?;
    let sizes: Vec<(f32, f32)> = pdf.pages().iter().map(|p| p.render_dimensions()).collect();
    if sizes.is_empty() {
        return Err(t!("Das PDF enthält keine Seiten").into());
    }
    if sizes.len() > MAX_IMPORT_PAGES {
        return Err(t!(
            "Das PDF hat {count} Seiten, höchstens {max} werden unterstützt",
            count = sizes.len(),
            max = MAX_IMPORT_PAGES
        ));
    }
    Ok(sizes)
}

fn open(bytes: Vec<u8>) -> Result<Pdf, String> {
    Pdf::new(bytes).map_err(|e| match e {
        LoadPdfError::Decryption(_) => {
            t!("Das PDF ist verschlüsselt oder passwortgeschützt und kann nicht geöffnet werden")
                .to_string()
        }
        LoadPdfError::Invalid => t!("Die Datei ist kein lesbares PDF").to_string(),
    })
}

/// Rendert Seite `index` so, dass die längste Kante höchstens `max_pixels` lang ist,
/// auf weißem Grund.
pub fn render_page(pdf: &Pdf, index: usize, max_pixels: u32) -> Result<RgbaImage, String> {
    let page = pdf
        .pages()
        .get(index)
        .ok_or_else(|| t!("Seite {number} fehlt im PDF", number = index + 1))?;
    let (w, h) = page.render_dimensions();
    let scale = (max_pixels as f32 / w.max(h).max(1.0)).min(16.0);
    let pixmap = hayro::render(
        page,
        &RenderCache::new(),
        &InterpreterSettings::default(),
        &RenderSettings::default(),
        &PixmapSettings {
            x_scale: scale,
            y_scale: scale,
            bg_color: WHITE,
        },
    );
    // Auf weißem Grund ist jedes Pixel deckend: vormultipliziert = gerade Farbe
    Ok(RgbaImage {
        width: pixmap.width() as u32,
        height: pixmap.height() as u32,
        pixels: pixmap.data_as_u8_slice().to_vec(),
    })
}

/// Öffnet die Datei und rendert eine Seite (für den PDF-Export als Ersatzbild).
pub fn render_file_page(
    path: &std::path::Path,
    index: usize,
    max_pixels: u32,
) -> Result<RgbaImage, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    render_page(&open(bytes)?, index, max_pixels)
}

struct RenderRequest {
    key: String,
    path: PathBuf,
    page: usize,
    max_pixels: u32,
    /// Wichtigkeit: kleiner zuerst (Reihenfolge der Anfragen im aktuellen Bild)
    rank: usize,
}

/// Wichtigkeit einer vorab gerenderten Seite: nach allen sichtbaren.
const PREFETCH_RANK: usize = usize::MAX;

/// Warteschlange der noch nicht begonnenen Render-Aufträge, geteilt mit dem Render-Thread.
#[derive(Default)]
struct RenderQueue {
    requests: Vec<RenderRequest>,
    shutdown: bool,
}

impl RenderQueue {
    /// Fügt einen Auftrag hinzu oder macht einen wartenden wichtiger.
    fn push(&mut self, request: RenderRequest) {
        match self.requests.iter_mut().find(|r| r.key == request.key) {
            Some(queued) => queued.rank = queued.rank.min(request.rank),
            None => self.requests.push(request),
        }
    }

    /// Nimmt den wichtigsten Auftrag heraus.
    fn pop(&mut self) -> Option<RenderRequest> {
        let (index, _) = self
            .requests
            .iter()
            .enumerate()
            .min_by_key(|(_, r)| r.rank)?;
        Some(self.requests.swap_remove(index))
    }

    /// Verwirft Aufträge, für die `rank_of` keinen Rang mehr liefert, und ordnet die übrigen
    /// neu. Liefert die Schlüssel der verworfenen.
    fn retain(&mut self, rank_of: impl Fn(&str) -> Option<usize>) -> Vec<String> {
        let mut dropped = Vec::new();
        self.requests.retain_mut(|r| match rank_of(&r.key) {
            Some(rank) => {
                r.rank = rank;
                true
            }
            None => {
                dropped.push(r.key.clone());
                false
            }
        });
        dropped
    }
}

type SharedQueue = Arc<(Mutex<RenderQueue>, Condvar)>;

/// Ergebnis eines Render-Auftrags: die Mipmap-Stufen der Seite (siehe
/// [`crate::image_layer::mip_chain`]), Stufe 0 in voller Größe.
pub type RenderResult = Result<Vec<RgbaImage>, String>;

/// Rendert PDF-Seiten im Hintergrund. Fertige Seiten holt der Event-Loop mit
/// [`PdfRenderService::take`] ab; `on_ready` weckt ihn dafür auf.
///
/// Je Bild meldet der Event-Loop zwischen [`Self::begin_frame`] und [`Self::end_frame`] mit
/// `take` alle Seiten an, die er gerade braucht (die wichtigste zuerst). Gerendert wird die
/// wichtigste wartende Seite zuerst; Seiten, die beim schnellen Scrollen schon wieder aus dem
/// Bild sind, verwirft `end_frame`, bevor der Render-Thread sie beginnt.
pub struct PdfRenderService {
    queue: SharedQueue,
    worker: std::thread::JoinHandle<()>,
    results: Receiver<(String, RenderResult)>,
    /// Wartende oder gerade gerenderte Seiten
    pending: HashSet<String>,
    ready: HashMap<String, RenderResult>,
    /// Im aktuellen Bild angeforderte Seiten in ihrer Reihenfolge
    wanted: Vec<String>,
    /// Zuletzt vorab angeforderte Seite (bleibt erhalten, bis sie abgeholt wird)
    prefetched: Option<String>,
}

impl PdfRenderService {
    pub fn new(on_ready: impl Fn() + Send + 'static) -> Self {
        let queue: SharedQueue = Arc::default();
        let (res_tx, res_rx) = mpsc::channel();
        let worker_queue = Arc::clone(&queue);
        let worker = std::thread::Builder::new()
            .name("pdf-render".into())
            .spawn(move || {
                // Zuletzt geöffnetes PDF behalten: Blättern rendert meist dieselbe Datei
                let mut open_pdf: Option<(PathBuf, Pdf)> = None;
                loop {
                    let request = {
                        let (lock, ready) = &*worker_queue;
                        let mut queue = lock.lock().unwrap_or_else(|e| e.into_inner());
                        loop {
                            if queue.shutdown {
                                return;
                            }
                            if let Some(request) = queue.pop() {
                                break request;
                            }
                            queue = ready.wait(queue).unwrap_or_else(|e| e.into_inner());
                        }
                    };
                    if open_pdf.as_ref().map(|(p, _)| p) != Some(&request.path) {
                        open_pdf = std::fs::read(&request.path)
                            .map_err(|e| e.to_string())
                            .and_then(open)
                            .map_err(|e| eprintln!("[PDF] '{}': {e}", request.path.display()))
                            .ok()
                            .map(|pdf| (request.path.clone(), pdf));
                    }
                    let result = match &open_pdf {
                        // Mipmaps hier statt im Haupt-Thread: sonst stockt das Scrollen bei
                        // jeder neu gerenderten Seite (Issue #28)
                        Some((_, pdf)) => render_page(pdf, request.page, request.max_pixels)
                            .map(crate::image_layer::mip_chain),
                        None => Err(t!("PDF kann nicht geöffnet werden").into()),
                    };
                    if res_tx.send((request.key, result)).is_err() {
                        return;
                    }
                    on_ready();
                }
            })
            .expect("PDF-Render-Thread konnte nicht gestartet werden");
        Self {
            queue,
            worker,
            results: res_rx,
            pending: HashSet::new(),
            ready: HashMap::new(),
            wanted: Vec::new(),
            prefetched: None,
        }
    }

    /// Beginnt die Anfragen eines neuen Bildes (siehe [`Self::end_frame`]).
    pub fn begin_frame(&mut self) {
        self.wanted.clear();
    }

    /// Verwirft wartende Aufträge und fertige Ergebnisse für Seiten, die in diesem Bild nicht
    /// mehr angefordert wurden (außer der vorab angeforderten), und ordnet die übrigen nach
    /// der Reihenfolge der Anfragen.
    pub fn end_frame(&mut self) {
        let rank_of = |key: &str| {
            self.wanted
                .iter()
                .position(|k| k == key)
                .or_else(|| (self.prefetched.as_deref() == Some(key)).then_some(PREFETCH_RANK))
        };
        let dropped = {
            let mut queue = self.queue.0.lock().unwrap_or_else(|e| e.into_inner());
            queue.retain(rank_of)
        };
        for key in dropped {
            self.pending.remove(&key);
        }
        self.ready.retain(|key, _| rank_of(key).is_some());
    }

    /// Rendert eine Seite vorab (z. B. die nächste), ohne auf das Ergebnis zu warten;
    /// [`Self::take`] liefert es später sofort.
    pub fn prefetch(&mut self, key: &str, path: PathBuf, page: usize) {
        self.collect();
        self.prefetched = Some(key.to_string());
        if self.ready.contains_key(key) {
            return;
        }
        self.request(key, path, page, DISPLAY_RENDER_PIXELS, PREFETCH_RANK);
    }

    fn collect(&mut self) {
        while let Ok((done, result)) = self.results.try_recv() {
            self.pending.remove(&done);
            self.ready.insert(done, result);
        }
    }

    /// Erteilt den Auftrag bzw. macht einen wartenden wichtiger. `false`, wenn der
    /// Render-Thread beendet ist.
    fn request(
        &mut self,
        key: &str,
        path: PathBuf,
        page: usize,
        max_pixels: u32,
        rank: usize,
    ) -> bool {
        if self.worker.is_finished() {
            self.pending.remove(key);
            return false;
        }
        let (lock, ready) = &*self.queue;
        let mut queue = lock.lock().unwrap_or_else(|e| e.into_inner());
        // Steht ein angefragter Auftrag nicht mehr in der Warteschlange, rendert ihn der
        // Thread gerade: nicht doppelt erteilen
        let waiting = queue.requests.iter().any(|r| r.key == key);
        if self.pending.contains(key) && !waiting {
            return true;
        }
        self.pending.insert(key.to_string());
        queue.push(RenderRequest {
            key: key.to_string(),
            path,
            page,
            max_pixels,
            rank,
        });
        ready.notify_one();
        true
    }

    /// Fertiges Ergebnis für `key` abholen; sonst den Auftrag (einmalig) erteilen und `None`.
    /// Die Seite wird so gerendert, dass die längste Kante `max_pixels` lang ist.
    pub fn take(
        &mut self,
        key: &str,
        path: PathBuf,
        page: usize,
        max_pixels: u32,
    ) -> Option<RenderResult> {
        self.collect();
        if self.prefetched.as_deref() == Some(key) {
            self.prefetched = None;
        }
        if let Some(result) = self.ready.remove(key) {
            return Some(result);
        }
        let rank = self.wanted.len();
        self.wanted.push(key.to_string());
        if !self.request(key, path, page, max_pixels, rank) {
            return Some(Err(t!("PDF-Render-Thread beendet").into()));
        }
        None
    }
}

impl Drop for PdfRenderService {
    fn drop(&mut self) {
        let (lock, ready) = &*self.queue;
        lock.lock().unwrap_or_else(|e| e.into_inner()).shutdown = true;
        ready.notify_all();
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Minimales gültiges PDF mit Seiten der angegebenen Größen (Punkt); jede Seite zeigt ein
    /// rotes Rechteck in der linken oberen Ecke. Optional mit `/Rotate`.
    pub(crate) fn sample_pdf(pages: &[(f32, f32, i32)]) -> Vec<u8> {
        let mut objects: Vec<String> = Vec::new();
        let n = pages.len();
        // 1: Katalog, 2: Seitenbaum, danach je Seite: Seite + Inhalt
        objects.push("<< /Type /Catalog /Pages 2 0 R >>".into());
        let kids: Vec<String> = (0..n).map(|i| format!("{} 0 R", 3 + 2 * i)).collect();
        objects.push(format!(
            "<< /Type /Pages /Kids [{}] /Count {n} >>",
            kids.join(" ")
        ));
        for (i, (w, h, rotate)) in pages.iter().enumerate() {
            let content_id = 4 + 2 * i;
            objects.push(format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {w} {h}] /Rotate {rotate} /Contents {content_id} 0 R /Resources << >> >>"
            ));
            let content = format!("1 0 0 rg 0 {} 40 40 re f", h - 40.0);
            objects.push(format!(
                "<< /Length {} >>\nstream\n{content}\nendstream",
                content.len()
            ));
        }
        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (i, obj) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n{obj}\nendobj\n", i + 1).as_bytes());
        }
        let xref_pos = out.len();
        out.extend_from_slice(
            format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
        );
        for off in offsets {
            out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_pos}\n%%EOF\n",
                objects.len() + 1
            )
            .as_bytes(),
        );
        out
    }

    #[test]
    fn test_page_sizes_and_errors() {
        let pdf = sample_pdf(&[(595.0, 842.0, 0), (842.0, 595.0, 0), (595.0, 842.0, 90)]);
        let sizes = page_sizes(pdf).unwrap();
        assert_eq!(sizes, vec![(595.0, 842.0), (842.0, 595.0), (842.0, 595.0)]);
        assert!(page_sizes(b"kein pdf".to_vec()).is_err());
    }

    #[test]
    fn test_render_page_draws_content() {
        let pdf = open(sample_pdf(&[(200.0, 100.0, 0)])).unwrap();
        let image = render_page(&pdf, 0, 400).unwrap();
        assert_eq!((image.width, image.height), (400, 200));
        let px = |x: u32, y: u32| {
            let i = ((y * image.width + x) * 4) as usize;
            &image.pixels[i..i + 4]
        };
        // Rotes Quadrat oben links, sonst weiß
        assert_eq!(px(20, 20), &[255, 0, 0, 255]);
        assert_eq!(px(300, 150), &[255, 255, 255, 255]);
        assert!(render_page(&pdf, 5, 400).is_err());
    }

    #[test]
    fn test_render_service_delivers_in_background() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.pdf");
        std::fs::write(&path, sample_pdf(&[(400.0, 300.0, 0)])).unwrap();
        let (wake_tx, wake_rx) = mpsc::channel();
        let mut service = PdfRenderService::new(move || {
            let _ = wake_tx.send(());
        });
        let px = DISPLAY_RENDER_PIXELS;
        assert!(service.take("a#0", path.clone(), 0, px).is_none());
        // Zweite Anfrage erteilt keinen weiteren Auftrag
        assert!(service.take("a#0", path.clone(), 0, px).is_none());
        wake_rx
            .recv_timeout(std::time::Duration::from_secs(20))
            .unwrap();
        let levels = service.take("a#0", path.clone(), 0, px).unwrap().unwrap();
        assert_eq!(levels[0].width, DISPLAY_RENDER_PIXELS);
        assert_eq!(
            (levels.last().unwrap().width, levels.last().unwrap().height),
            (1, 1)
        );

        // Vorab rendern: danach sofort verfügbar
        service.prefetch("a#0", path.clone(), 0);
        wake_rx
            .recv_timeout(std::time::Duration::from_secs(20))
            .unwrap();
        assert!(service.take("a#0", path, 0, px).unwrap().is_ok());
    }

    fn request(key: &str, rank: usize) -> RenderRequest {
        RenderRequest {
            key: key.into(),
            path: PathBuf::new(),
            page: 0,
            max_pixels: 1,
            rank,
        }
    }

    #[test]
    fn test_render_queue_priorities_and_stale_requests() {
        let mut queue = RenderQueue::default();
        queue.push(request("a", 2));
        queue.push(request("b", 0));
        queue.push(request("c", 1));
        // Erneute Anfrage macht einen wartenden Auftrag wichtiger, ohne ihn zu verdoppeln
        queue.push(request("a", 0));
        assert_eq!(queue.requests.len(), 3);
        assert_eq!(queue.pop().map(|r| r.key), Some("a".into()));

        // Nach dem Scrollen wird nur noch „c“ gebraucht: „b“ fällt weg
        let dropped = queue.retain(|key| (key == "c").then_some(0));
        assert_eq!(dropped, vec!["b".to_string()]);
        assert_eq!(queue.pop().map(|r| r.key), Some("c".into()));
        assert!(queue.pop().is_none());
    }

    #[test]
    fn test_end_frame_drops_unwanted_results_but_keeps_prefetch() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.pdf");
        std::fs::write(&path, sample_pdf(&[(400.0, 300.0, 0), (400.0, 300.0, 0)])).unwrap();
        let (wake_tx, wake_rx) = mpsc::channel();
        let mut service = PdfRenderService::new(move || {
            let _ = wake_tx.send(());
        });
        let px = DISPLAY_RENDER_PIXELS;
        let wait = || {
            wake_rx
                .recv_timeout(std::time::Duration::from_secs(20))
                .unwrap()
        };

        // Bild 1 braucht Seite 1; vorab gerendert wird Seite 2
        service.begin_frame();
        assert!(service.take("a#0", path.clone(), 0, px).is_none());
        service.prefetch("a#1", path.clone(), 1);
        service.end_frame();
        wait();
        wait();

        // Bild 2 braucht keine Seite mehr (weggescrollt): Seite 1 wird verworfen,
        // die vorab gerenderte Seite 2 bleibt
        service.begin_frame();
        service.collect();
        service.end_frame();
        assert!(!service.ready.contains_key("a#0"));
        assert!(service.ready.contains_key("a#1"));
        assert!(service.pending.is_empty());

        service.begin_frame();
        assert!(service.take("a#1", path.clone(), 1, px).unwrap().is_ok());
        // Seite 1 wird bei Bedarf neu gerendert
        assert!(service.take("a#0", path.clone(), 0, px).is_none());
        service.end_frame();
        wait();
        assert!(service.take("a#0", path, 0, px).unwrap().is_ok());
    }

    #[test]
    fn test_parse_page_key() {
        assert_eq!(
            parse_page_key("abc.pdf#3"),
            Some(("abc.pdf", 3, DISPLAY_RENDER_PIXELS))
        );
        assert_eq!(parse_page_key("abc.pdf#3@5120"), Some(("abc.pdf", 3, 5120)));
        assert_eq!(parse_page_key("abc.png"), None);
        assert_eq!(parse_page_key("abc.pdf#x"), None);
    }
}
