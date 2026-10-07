//! Eingebettete Bilder eines Notizbuchs (`<Notizbuch>/assets/`).
//!
//! Jedes Bild wird einmal unter dem SHA-256 seines Inhalts gespeichert (`<hash>.png` bzw.
//! `<hash>.jpg`). Seiten verweisen nur auf diesen Namen; dasselbe Bild mehrfach einzufügen
//! kostet keinen zusätzlichen Platz. Bilddateien werden nie gelöscht, damit Undo und
//! Kopien auf anderen Seiten immer funktionieren.
//!
//! JPEG wird unverändert übernommen (verlustfrei, auch im PDF-Export); alle anderen Formate
//! werden als PNG gespeichert.

use std::io::Cursor;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::storage;

/// Name des Unterordners mit den Bilddateien.
pub const ASSETS_DIR: &str = "assets";
/// Größte Kantenlänge eines eingefügten Bildes in Pixeln; größere Bilder werden verkleinert
/// (8000 px ≈ 300 dpi über die doppelte A4-Breite – mehr ist auf einer Seite nie sichtbar).
pub const MAX_IMPORT_DIMENSION: u32 = 8000;
/// Größte Kantenlänge der Bildschirmdarstellung (GPU-Textur).
pub const MAX_DISPLAY_DIMENSION: u32 = 2048;
/// Größte Dateigröße beim Einfügen.
pub const MAX_IMPORT_BYTES: usize = 64 * 1024 * 1024;

/// Fehler beim Einfügen oder Laden eines Bildes.
#[derive(Debug)]
pub enum AssetError {
    /// Format nicht unterstützt oder Datei beschädigt
    Decode(String),
    /// Datei zu groß
    TooLarge(usize),
    /// Ungültiger Asset-Name (z. B. manipulierte Seitendatei)
    InvalidName(String),
    Io(std::io::Error),
}

impl std::fmt::Display for AssetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AssetError::Decode(msg) => write!(f, "Bild kann nicht gelesen werden: {msg}"),
            AssetError::TooLarge(bytes) => write!(
                f,
                "Bild ist zu groß ({} MB, höchstens {} MB)",
                bytes / (1024 * 1024),
                MAX_IMPORT_BYTES / (1024 * 1024)
            ),
            AssetError::InvalidName(name) => write!(f, "Ungültiger Bildverweis '{name}'"),
            AssetError::Io(e) => write!(f, "Ein-/Ausgabefehler: {e}"),
        }
    }
}

impl std::error::Error for AssetError {}

impl From<std::io::Error> for AssetError {
    fn from(e: std::io::Error) -> Self {
        AssetError::Io(e)
    }
}

/// Ergebnis eines erfolgreichen Imports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedImage {
    /// Dateiname im Asset-Ordner, z. B. `3f…a9.png`
    pub asset: String,
    pub width: u32,
    pub height: u32,
}

/// Dekodiertes Bild in 8-Bit-RGBA (sRGB).
#[derive(Debug, Clone, PartialEq)]
pub struct RgbaImage {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

/// Prüft einen Asset-Namen: genau 64 Hex-Zeichen und `.png`, `.jpg` oder `.pdf`. Verhindert, dass
/// eine manipulierte Seitendatei auf Dateien außerhalb des Asset-Ordners verweist.
pub fn is_valid_asset_name(name: &str) -> bool {
    let Some((hash, ext)) = name.split_once('.') else {
        return false;
    };
    hash.len() == 64
        && hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        && matches!(ext, "png" | "jpg" | "pdf")
}

/// Bilddateien eines Notizbuchs.
#[derive(Debug, Clone)]
pub struct AssetStore {
    dir: PathBuf,
}

impl AssetStore {
    /// Asset-Ordner des Notizbuchs in `notebook_dir`.
    pub fn new(notebook_dir: &Path) -> Self {
        Self {
            dir: notebook_dir.join(ASSETS_DIR),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Pfad einer Bilddatei (nur für gültige Namen).
    pub fn path_of(&self, asset: &str) -> Result<PathBuf, AssetError> {
        if is_valid_asset_name(asset) {
            Ok(self.dir.join(asset))
        } else {
            Err(AssetError::InvalidName(asset.to_string()))
        }
    }

    /// Übernimmt eine Bilddatei (Inhalt einer PNG-, JPEG-, GIF-, WebP- oder BMP-Datei).
    pub fn import_bytes(&self, bytes: &[u8]) -> Result<ImportedImage, AssetError> {
        if bytes.len() > MAX_IMPORT_BYTES {
            return Err(AssetError::TooLarge(bytes.len()));
        }
        let format = image::guess_format(bytes).map_err(|e| AssetError::Decode(e.to_string()))?;
        let decoded = image::load_from_memory_with_format(bytes, format)
            .map_err(|e| AssetError::Decode(e.to_string()))?;

        let fits =
            decoded.width() <= MAX_IMPORT_DIMENSION && decoded.height() <= MAX_IMPORT_DIMENSION;
        if format == image::ImageFormat::Jpeg && fits {
            // JPEG unverändert übernehmen (kein erneuter Qualitätsverlust)
            return self.store(bytes, "jpg", decoded.width(), decoded.height());
        }
        let decoded = if fits {
            decoded
        } else {
            decoded.resize(
                MAX_IMPORT_DIMENSION,
                MAX_IMPORT_DIMENSION,
                image::imageops::FilterType::Triangle,
            )
        };
        let png = encode_png(&decoded)?;
        self.store(&png, "png", decoded.width(), decoded.height())
    }

    /// Übernimmt eine PDF-Datei unverändert (der Inhalt muss vorher geprüft sein).
    pub fn import_pdf(&self, bytes: &[u8]) -> Result<String, AssetError> {
        let hash = Sha256::digest(bytes);
        let asset = format!("{}.pdf", hex(&hash));
        let path = self.path_of(&asset)?;
        if !path.exists() {
            storage::atomic_write_file(&path, bytes)?;
        }
        Ok(asset)
    }

    /// Übernimmt rohe RGBA-Pixel (z. B. aus der Zwischenablage) als PNG.
    pub fn import_rgba(
        &self,
        width: u32,
        height: u32,
        pixels: Vec<u8>,
    ) -> Result<ImportedImage, AssetError> {
        let buffer = image::RgbaImage::from_raw(width, height, pixels)
            .ok_or_else(|| AssetError::Decode("Pixeldaten passen nicht zur Bildgröße".into()))?;
        let mut decoded = image::DynamicImage::ImageRgba8(buffer);
        if width > MAX_IMPORT_DIMENSION || height > MAX_IMPORT_DIMENSION {
            decoded = decoded.resize(
                MAX_IMPORT_DIMENSION,
                MAX_IMPORT_DIMENSION,
                image::imageops::FilterType::Triangle,
            );
        }
        let png = encode_png(&decoded)?;
        self.store(&png, "png", decoded.width(), decoded.height())
    }

    /// Kopiert ein Bild aus dem Asset-Ordner eines anderen Notizbuchs (Einfügen über
    /// Notizbuchgrenzen hinweg). Ist es schon vorhanden, passiert nichts.
    pub fn copy_from(&self, other: &AssetStore, asset: &str) -> Result<(), AssetError> {
        let target = self.path_of(asset)?;
        if target.exists() || other.dir == self.dir {
            return Ok(());
        }
        let bytes = std::fs::read(other.path_of(asset)?)?;
        storage::atomic_write_file(&target, &bytes)?;
        Ok(())
    }

    /// Liest die gespeicherte Bilddatei unverändert.
    pub fn read_bytes(&self, asset: &str) -> Result<Vec<u8>, AssetError> {
        Ok(std::fs::read(self.path_of(asset)?)?)
    }

    /// Lädt ein Bild für die Bildschirmdarstellung, verkleinert auf höchstens
    /// `max_dimension` Pixel Kantenlänge.
    pub fn load_rgba(&self, asset: &str, max_dimension: u32) -> Result<RgbaImage, AssetError> {
        let bytes = self.read_bytes(asset)?;
        let mut decoded =
            image::load_from_memory(&bytes).map_err(|e| AssetError::Decode(e.to_string()))?;
        if decoded.width() > max_dimension || decoded.height() > max_dimension {
            decoded = decoded.resize(
                max_dimension,
                max_dimension,
                image::imageops::FilterType::Triangle,
            );
        }
        let rgba = decoded.into_rgba8();
        Ok(RgbaImage {
            width: rgba.width(),
            height: rgba.height(),
            pixels: rgba.into_raw(),
        })
    }

    fn store(
        &self,
        bytes: &[u8],
        ext: &str,
        width: u32,
        height: u32,
    ) -> Result<ImportedImage, AssetError> {
        let hash = Sha256::digest(bytes);
        let asset = format!("{}.{ext}", hex(&hash));
        let path = self.path_of(&asset)?;
        if !path.exists() {
            storage::atomic_write_file(&path, bytes)?;
        }
        Ok(ImportedImage {
            asset,
            width,
            height,
        })
    }
}

fn encode_png(image: &image::DynamicImage) -> Result<Vec<u8>, AssetError> {
    let mut png = Vec::new();
    image
        .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(|e| AssetError::Decode(e.to_string()))?;
    Ok(png)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Ist die Datei dem Namen nach ein unterstütztes Bild?
pub fn is_supported_image_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| {
            matches!(
                e.to_ascii_lowercase().as_str(),
                "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp"
            )
        })
        .unwrap_or(false)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Kleines Testbild als Dateiinhalt im gewünschten Format.
    pub(crate) fn sample_image_bytes(
        width: u32,
        height: u32,
        format: image::ImageFormat,
    ) -> Vec<u8> {
        let img = image::RgbImage::from_fn(width, height, |x, y| {
            image::Rgb([
                (x * 255 / width.max(1)) as u8,
                (y * 255 / height.max(1)) as u8,
                90,
            ])
        });
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut Cursor::new(&mut bytes), format)
            .unwrap();
        bytes
    }

    #[test]
    fn test_asset_names_are_validated() {
        let hash = "a".repeat(64);
        assert!(is_valid_asset_name(&format!("{hash}.png")));
        assert!(is_valid_asset_name(&format!("{hash}.jpg")));
        assert!(!is_valid_asset_name(&format!("{hash}.exe")));
        assert!(!is_valid_asset_name("../../etc/passwd"));
        assert!(!is_valid_asset_name(&format!("../{}.png", "a".repeat(61))));
        assert!(!is_valid_asset_name(&format!("{}.png", "A".repeat(64))));
        assert!(!is_valid_asset_name(""));

        let store = AssetStore::new(Path::new("/tmp/nb"));
        assert!(store.path_of("../manifest.json").is_err());
    }

    #[test]
    fn test_import_png_jpeg_and_deduplicate() {
        let dir = tempfile::tempdir().unwrap();
        let store = AssetStore::new(dir.path());

        let png = sample_image_bytes(40, 20, image::ImageFormat::Png);
        let a = store.import_bytes(&png).unwrap();
        assert_eq!((a.width, a.height), (40, 20));
        assert!(a.asset.ends_with(".png"));
        assert!(store.path_of(&a.asset).unwrap().exists());

        // Gleiches Bild nochmals: gleicher Name, keine zweite Datei
        let b = store.import_bytes(&png).unwrap();
        assert_eq!(a, b);
        assert_eq!(std::fs::read_dir(store.dir()).unwrap().count(), 1);

        // JPEG bleibt byte-identisch
        let jpg = sample_image_bytes(30, 30, image::ImageFormat::Jpeg);
        let c = store.import_bytes(&jpg).unwrap();
        assert!(c.asset.ends_with(".jpg"));
        assert_eq!(store.read_bytes(&c.asset).unwrap(), jpg);

        // BMP wird als PNG gespeichert
        let bmp = sample_image_bytes(8, 4, image::ImageFormat::Bmp);
        let d = store.import_bytes(&bmp).unwrap();
        assert!(d.asset.ends_with(".png"));

        let rgba = store.load_rgba(&a.asset, MAX_DISPLAY_DIMENSION).unwrap();
        assert_eq!((rgba.width, rgba.height), (40, 20));
        assert_eq!(rgba.pixels.len(), 40 * 20 * 4);
        // Verkleinerung für die Anzeige behält das Seitenverhältnis
        let small = store.load_rgba(&a.asset, 10).unwrap();
        assert_eq!((small.width, small.height), (10, 5));
    }

    #[test]
    fn test_import_rejects_garbage_and_copies_between_notebooks() {
        let dir = tempfile::tempdir().unwrap();
        let store = AssetStore::new(&dir.path().join("a"));
        assert!(matches!(
            store.import_bytes(b"kein Bild"),
            Err(AssetError::Decode(_))
        ));
        assert!(store.import_rgba(4, 4, vec![0; 3]).is_err());

        let imported = store.import_rgba(2, 2, vec![255; 16]).unwrap();
        let other = AssetStore::new(&dir.path().join("b"));
        assert!(other.read_bytes(&imported.asset).is_err());
        other.copy_from(&store, &imported.asset).unwrap();
        assert_eq!(
            other.read_bytes(&imported.asset).unwrap(),
            store.read_bytes(&imported.asset).unwrap()
        );
    }

    #[test]
    fn test_supported_image_paths() {
        assert!(is_supported_image_path(Path::new("/x/Foto.JPG")));
        assert!(is_supported_image_path(Path::new("a.webp")));
        assert!(!is_supported_image_path(Path::new("a.pdf")));
        assert!(!is_supported_image_path(Path::new("png")));
    }
}
