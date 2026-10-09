//! Lesen und Rendern importierter PDF-Dateien mit `hayro` (reines Rust, nur CPU).
//!
//! Das Rendern einer Seite kann einige hundert Millisekunden dauern; [`PdfRenderService`]
//! erledigt es deshalb in einem Hintergrund-Thread und meldet fertige Seiten zurück.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};

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
}

/// Ergebnis eines Render-Auftrags.
pub type RenderResult = Result<RgbaImage, String>;

/// Rendert PDF-Seiten im Hintergrund. Fertige Seiten holt der Event-Loop mit
/// [`PdfRenderService::take`] ab; `on_ready` weckt ihn dafür auf.
pub struct PdfRenderService {
    requests: Sender<RenderRequest>,
    results: Receiver<(String, RenderResult)>,
    pending: HashSet<String>,
    ready: HashMap<String, RenderResult>,
}

impl PdfRenderService {
    pub fn new(on_ready: impl Fn() + Send + 'static) -> Self {
        let (req_tx, req_rx) = mpsc::channel::<RenderRequest>();
        let (res_tx, res_rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("pdf-render".into())
            .spawn(move || {
                // Zuletzt geöffnetes PDF behalten: Blättern rendert meist dieselbe Datei
                let mut open_pdf: Option<(PathBuf, Pdf)> = None;
                while let Ok(request) = req_rx.recv() {
                    if open_pdf.as_ref().map(|(p, _)| p) != Some(&request.path) {
                        open_pdf = std::fs::read(&request.path)
                            .map_err(|e| e.to_string())
                            .and_then(open)
                            .map_err(|e| eprintln!("[PDF] '{}': {e}", request.path.display()))
                            .ok()
                            .map(|pdf| (request.path.clone(), pdf));
                    }
                    let result = match &open_pdf {
                        Some((_, pdf)) => render_page(pdf, request.page, request.max_pixels),
                        None => Err(t!("PDF kann nicht geöffnet werden").into()),
                    };
                    if res_tx.send((request.key, result)).is_err() {
                        break;
                    }
                    on_ready();
                }
            })
            .expect("PDF-Render-Thread konnte nicht gestartet werden");
        Self {
            requests: req_tx,
            results: res_rx,
            pending: HashSet::new(),
            ready: HashMap::new(),
        }
    }

    /// Rendert eine Seite vorab (z. B. die nächste), ohne auf das Ergebnis zu warten;
    /// [`Self::take`] liefert es später sofort.
    pub fn prefetch(&mut self, key: &str, path: PathBuf, page: usize) {
        self.collect();
        if self.ready.contains_key(key) || self.pending.contains(key) {
            return;
        }
        // Vorab gerenderte Seiten nicht unbegrenzt aufheben
        if self.ready.len() >= 2 {
            self.ready.clear();
        }
        self.request(key, path, page, DISPLAY_RENDER_PIXELS);
    }

    fn collect(&mut self) {
        while let Ok((done, result)) = self.results.try_recv() {
            self.pending.remove(&done);
            self.ready.insert(done, result);
        }
    }

    /// Erteilt den Auftrag, falls noch keiner läuft. `false`, wenn der Thread beendet ist.
    fn request(&mut self, key: &str, path: PathBuf, page: usize, max_pixels: u32) -> bool {
        if !self.pending.insert(key.to_string()) {
            return true;
        }
        let request = RenderRequest {
            key: key.to_string(),
            path,
            page,
            max_pixels,
        };
        if self.requests.send(request).is_err() {
            self.pending.remove(key);
            return false;
        }
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
        if let Some(result) = self.ready.remove(key) {
            return Some(result);
        }
        if !self.request(key, path, page, max_pixels) {
            return Some(Err(t!("PDF-Render-Thread beendet").into()));
        }
        None
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
        let image = service.take("a#0", path.clone(), 0, px).unwrap().unwrap();
        assert_eq!(image.width, DISPLAY_RENDER_PIXELS);

        // Vorab rendern: danach sofort verfügbar
        service.prefetch("a#0", path.clone(), 0);
        wake_rx
            .recv_timeout(std::time::Duration::from_secs(20))
            .unwrap();
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
