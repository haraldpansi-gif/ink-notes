//! In-App Updater für ink-notes mit Gitea-Releases-Integration.
//!
//! Sicherheits- und Zuverlässigkeitsmerkmale:
//! - Rust-nativer HTTP-Client (`ureq` mit rustls) ohne externe CLI-Abhängigkeit
//! - Erzwingen von HTTPS (unsicheres HTTP standardmäßig blockiert)
//! - Kein Leak von Authorization-Tokens bei HTTP-Redirects auf fremde Origins
//! - Sichere Asset-Namensbereinigung (Path Traversal Schutz)
//! - Isolierter Benutzer-Cache-Pfad (`$XDG_CACHE_HOME/ink-notes/updates` mit 0700 Rechten)
//! - Integritätsprüfung via SHA-256 (`SHA256SUMS.txt`) mit automatischem Löschen ungültiger Dateien
//! - Erweiterbare Schnittstelle für optionale Signaturprüfung (.sig / .minisig)
//! - Atomares AppImage-Self-Update via `.new`-Zwischendatei, Backup-Pflicht und atomarem `fs::rename`
//! - Echtes Fortschritts-Streaming (~10 Updates/s)
//! - Korrekte SemVer-Prüfung via `semver` Crate (Prerelease-Schutz standardmäßig aktiv)

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Konfiguration für den Gitea-Update-Server.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UpdaterConfig {
    /// Basis-URL der Gitea-Instanz, z. B. `https://gitea.pansi.eu`
    pub server_url: String,
    /// Repository-Owner, z. B. `harald`
    pub repo_owner: String,
    /// Repository-Name, z. B. `ink-notes`
    pub repo_name: String,
    /// Optionales Personal Access Token für private Repositories
    pub auth_token: Option<String>,
    /// Ob unsicheres HTTP ausnahmsweise erlaubt ist (Standard: false)
    #[serde(default)]
    pub allow_insecure_http: bool,
    /// Ob Prerelease-Versionen als Update angeboten werden sollen (Standard: false)
    #[serde(default)]
    pub include_prereleases: bool,
}

impl Default for UpdaterConfig {
    fn default() -> Self {
        let auth_token = std::env::var("INK_NOTES_GITEA_TOKEN").ok();
        Self {
            server_url: "https://gitea.pansi.eu".to_string(),
            repo_owner: "harald".to_string(),
            repo_name: "ink-notes".to_string(),
            auth_token,
            allow_insecure_http: false,
            include_prereleases: false,
        }
    }
}

impl UpdaterConfig {
    /// Validiert die Server-URL auf Protokollsicherheit (erzwingt HTTPS).
    pub fn validate_url(&self) -> Result<url::Url, String> {
        let parsed = url::Url::parse(&self.server_url)
            .map_err(|e| format!("Ungültige Server-URL '{}': {e}", self.server_url))?;

        if parsed.scheme() != "https" && !self.allow_insecure_http {
            return Err(format!(
                "Unsicheres Protokoll '{}' abgewiesen. Es wird zwingend HTTPS verlangt (z. B. https://gitea.pansi.eu).",
                parsed.scheme()
            ));
        }
        Ok(parsed)
    }
}

/// Ein Release-Asset aus der Gitea API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GiteaAsset {
    pub id: u64,
    pub name: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub download_count: u64,
    pub browser_download_url: String,
}

/// Gitea Release-Objekt gemäß Swagger /api/v1/repos/{owner}/{repo}/releases/latest.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GiteaRelease {
    pub id: u64,
    pub tag_name: String,
    pub name: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub prerelease: bool,
    #[serde(default)]
    pub html_url: String,
    #[serde(default)]
    pub published_at: Option<String>,
    #[serde(default)]
    pub assets: Vec<GiteaAsset>,
}

/// Erkannte Art der Installation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeEnvironment {
    /// Läuft als portables AppImage (`APPIMAGE` Umgebungsvariable gesetzt)
    AppImage { appimage_path: PathBuf },
    /// Systemweit als .deb installiert (z.B. in /usr/bin/ink-notes)
    DebPackage { binary_path: PathBuf },
    /// Aus Quellcode / Entwicklungs-Binary (z.B. target/debug oder target/release)
    PortableOrDev { binary_path: PathBuf },
}

impl RuntimeEnvironment {
    /// Erkennt die aktuelle Laufzeitumgebung anhand der Umgebung und des Binärpfads.
    pub fn detect() -> Self {
        let appimage_var = std::env::var("APPIMAGE").ok();
        let current_exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("ink-notes"));
        Self::detect_from(appimage_var.as_deref(), &current_exe)
    }

    /// Testbare Kernlogik zur Erkennung der Laufzeitumgebung.
    pub fn detect_from(appimage_var: Option<&str>, current_exe: &Path) -> Self {
        if let Some(appimage_path) = appimage_var {
            let trimmed = appimage_path.trim();
            if !trimmed.is_empty() {
                return RuntimeEnvironment::AppImage {
                    appimage_path: PathBuf::from(trimmed),
                };
            }
        }

        let exe_str = current_exe.to_string_lossy();
        if exe_str.starts_with("/usr/bin/") || exe_str.starts_with("/usr/local/bin/") {
            RuntimeEnvironment::DebPackage {
                binary_path: current_exe.to_path_buf(),
            }
        } else {
            RuntimeEnvironment::PortableOrDev {
                binary_path: current_exe.to_path_buf(),
            }
        }
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            RuntimeEnvironment::AppImage { .. } => "AppImage (Portabel)",
            RuntimeEnvironment::DebPackage { .. } => "System-Paket (.deb)",
            RuntimeEnvironment::PortableOrDev { .. } => "Entwicklung / Lokal",
        }
    }
}

/// Bereinigt und validiert einen Asset-Dateinamen gegen Path Traversal Angriffe.
pub fn sanitize_asset_name(raw_name: &str) -> Result<String, String> {
    let trimmed = raw_name.trim();
    if trimmed.is_empty() {
        return Err("Asset-Name darf nicht leer sein.".to_string());
    }
    if trimmed.contains('/') || trimmed.contains('\\') || trimmed.contains("..") {
        return Err(format!(
            "Ungültiger Asset-Name (enthält unzulässige Pfadkomponenten): '{raw_name}'"
        ));
    }
    let path = Path::new(trimmed);
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| format!("Ungültiger Dateiname: '{raw_name}'"))?;

    if file_name != trimmed || file_name.starts_with('.') {
        return Err(format!(
            "Ungültiger oder versteckter Asset-Name: '{raw_name}'"
        ));
    }
    Ok(file_name.to_string())
}

/// Liefert das isolierte Cache-Verzeichnis für Downloads pro Nutzer mit Rechten 0700.
pub fn get_updates_cache_dir() -> Result<PathBuf, String> {
    let base = if let Ok(xdg) = std::env::var("XDG_CACHE_HOME") {
        let trimmed = xdg.trim();
        if !trimmed.is_empty() {
            PathBuf::from(trimmed)
        } else {
            default_user_cache_dir()?
        }
    } else {
        default_user_cache_dir()?
    };

    let dir = base.join("ink-notes").join("updates");
    std::fs::create_dir_all(&dir).map_err(|e| {
        format!(
            "Konnte Update-Cache-Verzeichnis nicht anlegen ({}): {e}",
            dir.display()
        )
    })?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }

    Ok(dir)
}

fn default_user_cache_dir() -> Result<PathBuf, String> {
    let home = std::env::var("HOME")
        .map_err(|_| "HOME-Umgebungsvariable ist nicht gesetzt.".to_string())?;
    Ok(PathBuf::from(home).join(".cache"))
}

/// Löscht alte temporäre Downloads aus dem Cache-Verzeichnis.
pub fn cleanup_old_updates() {
    if let Ok(dir) = get_updates_cache_dir() {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() {
                    let _ = std::fs::remove_file(path);
                }
            }
        }
    }
}

/// Zerlegt einen Versionstag wie "v0.1.2" oder "0.1.2" in ein `semver::Version`.
pub fn parse_semver(tag: &str) -> Option<semver::Version> {
    let clean = tag.trim().trim_start_matches(['v', 'V']);
    semver::Version::parse(clean).ok()
}

/// Prüft, ob `remote_tag` numerisch neuer ist als `current_tag`.
/// Prerelease-Versionen (z. B. `-rc1`) werden standardmäßig ausgeschlossen.
pub fn is_newer_version(remote_tag: &str, current_tag: &str, include_prereleases: bool) -> bool {
    match (parse_semver(remote_tag), parse_semver(current_tag)) {
        (Some(remote), Some(current)) => {
            if !include_prereleases && !remote.pre.is_empty() {
                return false;
            }
            remote > current
        }
        _ => false,
    }
}

/// Berechnet den hex-kodierten SHA-256 Hash einer Datei.
pub fn compute_sha256(path: &Path) -> Result<String, String> {
    let mut file = std::fs::File::open(path).map_err(|e| {
        format!(
            "Konnte Datei '{}' zur Prüfsummenberechnung nicht öffnen: {e}",
            path.display()
        )
    })?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 16384];
    loop {
        let n = file
            .read(&mut buffer)
            .map_err(|e| format!("Fehler beim Lesen der Prüfsummendatei: {e}"))?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Parst eine `SHA256SUMS.txt` Datei in ein Mapping von Dateiname -> Hash.
pub fn parse_checksums(content: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 2 {
            let hash = parts[0].to_lowercase();
            let raw_file = parts[1].trim_start_matches('*');
            if let Ok(safe_name) = sanitize_asset_name(raw_file) {
                map.insert(safe_name, hash);
            }
        }
    }
    map
}

/// Verifiziert die Integrität einer Datei anhand von `SHA256SUMS.txt`.
/// Bei fehlender oder nicht übereinstimmender Prüfsumme wird die Datei gelöscht!
pub fn verify_checksum(
    downloaded_path: &Path,
    asset_name: &str,
    checksums_content: &str,
) -> Result<(), String> {
    let checksums = parse_checksums(checksums_content);
    let expected_hash = checksums.get(asset_name).ok_or_else(|| {
        let _ = std::fs::remove_file(downloaded_path);
        format!("Kein Eintrag für '{asset_name}' in SHA256SUMS.txt gefunden. Heruntergeladene Datei wurde gelöscht.")
    })?;

    let actual_hash = compute_sha256(downloaded_path)?;
    if !actual_hash.eq_ignore_ascii_case(expected_hash) {
        let _ = std::fs::remove_file(downloaded_path);
        return Err(format!(
            "Integritätsprüfung fehlgeschlagen für '{asset_name}': Erwartet {expected_hash}, berechnet {actual_hash}. Datei wurde gelöscht."
        ));
    }
    Ok(())
}

/// Status einer Signaturverifikation (.sig / .minisig).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignatureVerificationStatus {
    NotPresent,
    Verified { signer: String },
    Failed(String),
}

/// Erweiterbare Schnittstelle für kryptografische Signaturprüfung.
pub fn verify_signature_if_present(
    _asset_file: &Path,
    _sig_bytes: Option<&[u8]>,
) -> Result<SignatureVerificationStatus, String> {
    // Erweiterungspunkt: Sobald ein Signaturschlüssel / Minisign-Public-Key
    // im Projekt konfiguriert ist, kann hier die kryptografische Verifikation erfolgen.
    Ok(SignatureVerificationStatus::NotPresent)
}

/// Event-Typen für den asynchronen Worker.
#[derive(Debug)]
pub enum UpdaterEvent {
    CheckSuccess(GiteaRelease),
    CheckUpToDate(String),
    CheckError(String),
    DownloadProgress {
        percent: f32,
        message: String,
    },
    DownloadSuccess {
        file_path: PathBuf,
        asset_name: String,
    },
    DownloadError(String),
}

/// Status des In-App Updaters für die egui-Oberfläche.
#[derive(Debug, Clone, PartialEq)]
pub enum UpdaterStatus {
    Idle,
    Checking,
    UpdateAvailable(GiteaRelease),
    UpToDate {
        current_version: String,
    },
    Downloading {
        asset_name: String,
        percent: f32,
    },
    DownloadFinished {
        file_path: PathBuf,
        asset_name: String,
    },
    Error(String),
}

/// Erstellt einen ureq Agent mit sauberen Timeouts.
fn create_http_agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(10))
        .timeout_read(Duration::from_secs(180))
        .redirects(0) // Manuelle Redirect-Behandlung zum Schutz des Authorization-Tokens
        .build()
}

/// Führt einen GET-Request mit Redirect-Schutz aus:
/// Bei Weiterleitungen auf andere Hosts (z. B. S3, Fremd-CDNs) wird der Authorization-Header verworfen.
fn http_get_with_auth_guard(
    agent: &ureq::Agent,
    initial_url_str: &str,
    token: Option<&str>,
    max_redirects: usize,
) -> Result<ureq::Response, String> {
    let mut current_url = url::Url::parse(initial_url_str)
        .map_err(|e| format!("Ungültige URL '{initial_url_str}': {e}"))?;
    let initial_origin = current_url.origin().ascii_serialization();

    for _ in 0..=max_redirects {
        let mut req = agent.get(current_url.as_str());

        // Token NUR anhängen, wenn Origin (Scheme, Host, Port) identisch ist
        if let Some(tok) = token {
            let tok_trimmed = tok.trim();
            if !tok_trimmed.is_empty()
                && current_url.origin().ascii_serialization() == initial_origin
            {
                req = req.set("Authorization", &format!("token {tok_trimmed}"));
            }
        }

        match req.call() {
            Ok(resp) => return Ok(resp),
            Err(ureq::Error::Status(code, resp)) if (301..=308).contains(&code) => {
                let loc = resp.header("Location").ok_or_else(|| {
                    format!("HTTP {code} Weiterleitung ohne Location-Header erhalten")
                })?;
                current_url = current_url
                    .join(loc)
                    .map_err(|e| format!("Ungültiges Weiterleitungsziel '{loc}': {e}"))?;
                continue;
            }
            Err(ureq::Error::Status(code, resp)) => {
                let body = resp.into_string().unwrap_or_default();
                return Err(format!("Gitea-Server antwortete mit HTTP {code}: {body}"));
            }
            Err(ureq::Error::Transport(t)) => {
                return Err(format!("Netzwerk-Transportfehler: {t}"));
            }
        }
    }

    Err("Zu viele Weiterleitungen (Redirect-Schleife abgebrochen).".to_string())
}

/// Asynchroner Controller für Update-Prüfung und Asset-Download.
pub struct UpdaterController {
    config: UpdaterConfig,
    runtime: RuntimeEnvironment,
    status: UpdaterStatus,
    active_release: Option<GiteaRelease>,
    receiver: Receiver<UpdaterEvent>,
    sender: Sender<UpdaterEvent>,
}

impl UpdaterController {
    pub fn new() -> Self {
        // Alten Download-Cache beim Start aufräumen
        cleanup_old_updates();

        let (sender, receiver) = channel();
        Self {
            config: UpdaterConfig::default(),
            runtime: RuntimeEnvironment::detect(),
            status: UpdaterStatus::Idle,
            active_release: None,
            receiver,
            sender,
        }
    }

    pub fn status(&self) -> &UpdaterStatus {
        &self.status
    }

    pub fn runtime(&self) -> &RuntimeEnvironment {
        &self.runtime
    }

    pub fn config(&self) -> &UpdaterConfig {
        &self.config
    }

    pub fn set_auth_token(&mut self, token: Option<String>) {
        self.config.auth_token = token;
    }

    pub fn set_server_url(&mut self, url: String) {
        self.config.server_url = url;
    }

    /// Fragt im Hintergrund die Gitea-API ab.
    pub fn check_for_updates(&mut self) {
        if matches!(
            self.status,
            UpdaterStatus::Checking | UpdaterStatus::Downloading { .. }
        ) {
            return;
        }

        self.status = UpdaterStatus::Checking;
        let config = self.config.clone();
        let tx = self.sender.clone();
        let current_version = env!("CARGO_PKG_VERSION").to_string();

        thread::spawn(move || {
            if let Err(e) = config.validate_url() {
                let _ = tx.send(UpdaterEvent::CheckError(e));
                return;
            }

            let api_url = format!(
                "{}/api/v1/repos/{}/{}/releases/latest",
                config.server_url.trim_end_matches('/'),
                config.repo_owner,
                config.repo_name
            );

            let agent = create_http_agent();
            match http_get_with_auth_guard(&agent, &api_url, config.auth_token.as_deref(), 5) {
                Ok(resp) => match resp.into_json::<GiteaRelease>() {
                    Ok(release) => {
                        if release.prerelease && !config.include_prereleases {
                            let _ = tx.send(UpdaterEvent::CheckUpToDate(current_version));
                        } else if is_newer_version(
                            &release.tag_name,
                            &current_version,
                            config.include_prereleases,
                        ) {
                            let _ = tx.send(UpdaterEvent::CheckSuccess(release));
                        } else {
                            let _ = tx.send(UpdaterEvent::CheckUpToDate(current_version));
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(UpdaterEvent::CheckError(format!(
                            "Gitea Release-JSON konnte nicht verarbeitet werden: {e}"
                        )));
                    }
                },
                Err(e) => {
                    let _ = tx.send(UpdaterEvent::CheckError(e));
                }
            }
        });
    }

    /// Startet den asynchronen Download eines Release-Assets inklusive Integritätsprüfung.
    pub fn download_asset(&mut self, asset: GiteaAsset) {
        if matches!(self.status, UpdaterStatus::Downloading { .. }) {
            return;
        }

        let safe_asset_name = match sanitize_asset_name(&asset.name) {
            Ok(n) => n,
            Err(e) => {
                self.status = UpdaterStatus::Error(e);
                return;
            }
        };

        self.status = UpdaterStatus::Downloading {
            asset_name: safe_asset_name.clone(),
            percent: 0.0,
        };

        let config = self.config.clone();
        let tx = self.sender.clone();
        let active_release = self.active_release.clone();

        thread::spawn(move || {
            let cache_dir = match get_updates_cache_dir() {
                Ok(d) => d,
                Err(e) => {
                    let _ = tx.send(UpdaterEvent::DownloadError(e));
                    return;
                }
            };

            let target_file = cache_dir.join(&safe_asset_name);
            let agent = create_http_agent();

            // 1. Download des Release-Assets mit Fortschritts-Streaming
            let resp = match http_get_with_auth_guard(
                &agent,
                &asset.browser_download_url,
                config.auth_token.as_deref(),
                5,
            ) {
                Ok(r) => r,
                Err(e) => {
                    let _ = tx.send(UpdaterEvent::DownloadError(format!(
                        "Download-Verbindungsfehler: {e}"
                    )));
                    return;
                }
            };

            let content_length: Option<u64> =
                resp.header("Content-Length").and_then(|s| s.parse().ok());
            let mut reader = resp.into_reader();
            let mut file = match std::fs::File::create(&target_file) {
                Ok(f) => f,
                Err(e) => {
                    let _ = tx.send(UpdaterEvent::DownloadError(format!(
                        "Konnte Zieldatei '{}' nicht erstellen: {e}",
                        target_file.display()
                    )));
                    return;
                }
            };

            let mut downloaded: u64 = 0;
            let mut buffer = [0u8; 16384];
            let mut last_progress_time = Instant::now();
            let throttle = Duration::from_millis(100); // Max. 10 Progress-Events pro Sekunde

            loop {
                let n = match reader.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(count) => count,
                    Err(e) => {
                        let _ = std::fs::remove_file(&target_file);
                        let _ = tx.send(UpdaterEvent::DownloadError(format!(
                            "Fehler beim Lesen des Download-Streams: {e}"
                        )));
                        return;
                    }
                };

                if let Err(e) = file.write_all(&buffer[..n]) {
                    let _ = std::fs::remove_file(&target_file);
                    let _ = tx.send(UpdaterEvent::DownloadError(format!(
                        "Fehler beim Schreiben auf die Festplatte: {e}"
                    )));
                    return;
                }

                downloaded += n as u64;

                if last_progress_time.elapsed() >= throttle {
                    let percent = match content_length {
                        Some(total) if total > 0 => {
                            (downloaded as f32 / total as f32).clamp(0.0, 1.0)
                        }
                        _ => 0.0,
                    };
                    let msg = match content_length {
                        Some(total) => format!(
                            "{:.1} / {:.1} MB",
                            downloaded as f64 / 1_048_576.0,
                            total as f64 / 1_048_576.0
                        ),
                        None => format!("{:.1} MB", downloaded as f64 / 1_048_576.0),
                    };
                    let _ = tx.send(UpdaterEvent::DownloadProgress {
                        percent,
                        message: msg,
                    });
                    last_progress_time = Instant::now();
                }
            }

            drop(file);

            // 2. Integritätsprüfung gegen SHA256SUMS.txt
            let checksum_asset = active_release
                .as_ref()
                .and_then(|r| r.assets.iter().find(|a| a.name == "SHA256SUMS.txt"));

            let checksums_content = match checksum_asset {
                Some(ca) => {
                    match http_get_with_auth_guard(
                        &agent,
                        &ca.browser_download_url,
                        config.auth_token.as_deref(),
                        5,
                    ) {
                        Ok(c_resp) => match c_resp.into_string() {
                            Ok(text) => text,
                            Err(e) => {
                                let _ = std::fs::remove_file(&target_file);
                                let _ = tx.send(UpdaterEvent::DownloadError(format!(
                                    "Konnte SHA256SUMS.txt nicht dekodieren: {e}. Datei gelöscht."
                                )));
                                return;
                            }
                        },
                        Err(e) => {
                            let _ = std::fs::remove_file(&target_file);
                            let _ = tx.send(UpdaterEvent::DownloadError(format!(
                                "Konnte SHA256SUMS.txt nicht laden: {e}. Datei aus Sicherheitsgründen gelöscht."
                            )));
                            return;
                        }
                    }
                }
                None => {
                    let _ = std::fs::remove_file(&target_file);
                    let _ = tx.send(UpdaterEvent::DownloadError(
                        "Release enthält keine SHA256SUMS.txt-Prüfsummen. Download abgelehnt und gelöscht.".to_string(),
                    ));
                    return;
                }
            };

            if let Err(err) = verify_checksum(&target_file, &safe_asset_name, &checksums_content) {
                let _ = tx.send(UpdaterEvent::DownloadError(err));
                return;
            }

            // 3. Wenn es ein AppImage ist, Executable-Bit auf 0755 setzen
            if safe_asset_name.ends_with(".AppImage") {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if let Ok(meta) = std::fs::metadata(&target_file) {
                        let mut perms = meta.permissions();
                        perms.set_mode(0o755);
                        let _ = std::fs::set_permissions(&target_file, perms);
                    }
                }
            }

            let _ = tx.send(UpdaterEvent::DownloadSuccess {
                file_path: target_file,
                asset_name: safe_asset_name,
            });
        });
    }

    /// Startet die sichere Installation des heruntergeladenen Pakets oder öffnet es im System.
    pub fn install_or_open(&self, file_path: &Path) -> Result<(), String> {
        if !file_path.exists() {
            return Err("Die heruntergeladene Datei existiert nicht mehr.".to_string());
        }

        match &self.runtime {
            RuntimeEnvironment::AppImage { appimage_path } => {
                let parent_dir = appimage_path.parent().ok_or_else(|| {
                    "Konnte übergeordnetes Verzeichnis des AppImages nicht ermitteln.".to_string()
                })?;

                let file_name = appimage_path
                    .file_name()
                    .ok_or_else(|| "Ungültiger AppImage-Dateiname.".to_string())?
                    .to_string_lossy();

                let temp_new_file =
                    parent_dir.join(format!("{}.new.{}", file_name, std::process::id()));
                let backup_file = parent_dir.join(format!("{}.bak", file_name));

                // 1. Neue Datei im SELBEN Verzeichnis anlegen (garantiert atomares rename)
                if let Err(e) = std::fs::copy(file_path, &temp_new_file) {
                    let _ = std::fs::remove_file(&temp_new_file);
                    return Err(format!(
                        "Konnte temporäre Update-Datei nicht schreiben: {e}"
                    ));
                }

                // 2. Executable-Bit setzen
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if let Ok(meta) = std::fs::metadata(&temp_new_file) {
                        let mut perms = meta.permissions();
                        perms.set_mode(0o755);
                        let _ = std::fs::set_permissions(&temp_new_file, perms);
                    }
                }

                // 3. Backup des alten AppImages anlegen – Fehler NIEMALS ignorieren!
                if let Err(e) = std::fs::copy(appimage_path, &backup_file) {
                    let _ = std::fs::remove_file(&temp_new_file);
                    return Err(format!(
                        "Konnte Backup ({}) nicht anlegen: {e}. Update abgebrochen, altes AppImage unverändert.",
                        backup_file.display()
                    ));
                }

                // 4. Atomar ersetzen via rename
                if let Err(e) = std::fs::rename(&temp_new_file, appimage_path) {
                    let _ = std::fs::remove_file(&temp_new_file);
                    return Err(format!(
                        "Konnte AppImage nicht atomar ersetzen: {e}. Altes AppImage bleibt unverändert."
                    ));
                }

                // Heruntergeladene Cache-Datei aufräumen
                let _ = std::fs::remove_file(file_path);
                Ok(())
            }
            RuntimeEnvironment::DebPackage { .. } => {
                match Command::new("xdg-open").arg(file_path).spawn() {
                    Ok(_) => Ok(()),
                    Err(e) => Err(format!("Konnte xdg-open nicht ausführen: {e}")),
                }
            }
            RuntimeEnvironment::PortableOrDev { .. } => {
                if let Some(dir) = file_path.parent() {
                    let _ = Command::new("xdg-open").arg(dir).spawn();
                }
                Ok(())
            }
        }
    }

    /// Fragt periodisch ohne Blockieren eingehende Events vom Worker-Thread ab.
    pub fn update(&mut self) {
        while let Ok(event) = self.receiver.try_recv() {
            match event {
                UpdaterEvent::CheckSuccess(release) => {
                    self.active_release = Some(release.clone());
                    self.status = UpdaterStatus::UpdateAvailable(release);
                }
                UpdaterEvent::CheckUpToDate(ver) => {
                    self.status = UpdaterStatus::UpToDate {
                        current_version: ver,
                    };
                }
                UpdaterEvent::CheckError(err) => {
                    self.status = UpdaterStatus::Error(err);
                }
                UpdaterEvent::DownloadProgress { percent, .. } => {
                    if let UpdaterStatus::Downloading { asset_name, .. } = &self.status {
                        self.status = UpdaterStatus::Downloading {
                            asset_name: asset_name.clone(),
                            percent,
                        };
                    }
                }
                UpdaterEvent::DownloadSuccess {
                    file_path,
                    asset_name,
                } => {
                    self.status = UpdaterStatus::DownloadFinished {
                        file_path,
                        asset_name,
                    };
                }
                UpdaterEvent::DownloadError(err) => {
                    self.status = UpdaterStatus::Error(err);
                }
            }
        }
    }
}

impl Default for UpdaterController {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_semver_parsing() {
        assert_eq!(parse_semver("v0.1.0"), Some(semver::Version::new(0, 1, 0)));
        assert_eq!(parse_semver("0.1.0"), Some(semver::Version::new(0, 1, 0)));
        assert_eq!(
            parse_semver("v1.2.3-rc1"),
            Some(semver::Version::parse("1.2.3-rc1").unwrap())
        );
        assert_eq!(parse_semver("invalid"), None);
    }

    #[test]
    fn test_is_newer_version_semver_rules() {
        // Standardmäßig keine Prereleases anbieten
        assert!(is_newer_version("v0.2.0", "0.1.0", false));
        assert!(is_newer_version("0.1.1", "0.1.0", false));
        assert!(is_newer_version("1.0.0", "0.9.9", false));
        assert!(!is_newer_version("0.1.0", "0.1.0", false));
        assert!(!is_newer_version("v0.1.0", "0.1.0", false));
        assert!(!is_newer_version("0.0.9", "0.1.0", false));

        // SemVer Prerelease-Reihenfolge: 1.0.0-rc1 ist kleiner als 1.0.0
        assert!(is_newer_version("1.0.0", "1.0.0-rc1", false));
        assert!(!is_newer_version("1.0.0-rc1", "1.0.0", false));
        assert!(!is_newer_version("1.0.0-rc1", "1.0.0", true));

        // Prerelease-Filterung
        assert!(!is_newer_version("1.0.0-rc1", "0.9.0", false));
        assert!(is_newer_version("1.0.0-rc1", "0.9.0", true));
    }

    #[test]
    fn test_sanitize_asset_name() {
        assert_eq!(
            sanitize_asset_name("ink-notes_0.1.0_amd64.deb").unwrap(),
            "ink-notes_0.1.0_amd64.deb"
        );
        assert_eq!(
            sanitize_asset_name("ink-notes_0.1.0_x86_64.AppImage").unwrap(),
            "ink-notes_0.1.0_x86_64.AppImage"
        );

        // Bösartige Pfade abfangen
        assert!(sanitize_asset_name("../../etc/passwd").is_err());
        assert!(sanitize_asset_name("/etc/shadow").is_err());
        assert!(sanitize_asset_name("..").is_err());
        assert!(sanitize_asset_name("").is_err());
        assert!(sanitize_asset_name("   ").is_err());
        assert!(sanitize_asset_name("subdir/file.deb").is_err());
        assert!(sanitize_asset_name(".hidden_file").is_err());
    }

    #[test]
    fn test_checksum_parsing_and_verification() {
        let checksums_txt = "\
# SHA256 Prüfsummen
dcdeb7ac87d9e6cb8022e5a71bc7f2c56d779b4fce4efdd5bae5be3b0214884a  ink-notes_0.1.0_amd64.deb
53383dcdb49651dcd15358b9d03c7d0482dcbe4dae9f8f9b28e24c5900192229  ink-notes_0.1.0_x86_64.AppImage
";

        let parsed = parse_checksums(checksums_txt);
        assert_eq!(
            parsed.get("ink-notes_0.1.0_amd64.deb").unwrap(),
            "dcdeb7ac87d9e6cb8022e5a71bc7f2c56d779b4fce4efdd5bae5be3b0214884a"
        );

        // Testdatei anlegen
        let temp_dir = tempfile::tempdir().unwrap();
        let test_file = temp_dir.path().join("test_asset.bin");
        std::fs::write(&test_file, b"ink-notes test payload").unwrap();

        let real_hash = compute_sha256(&test_file).unwrap();
        let valid_sums = format!("{real_hash}  test_asset.bin\n");

        // 1. Gültige Prüfsumme
        assert!(verify_checksum(&test_file, "test_asset.bin", &valid_sums).is_ok());
        assert!(test_file.exists());

        // 2. Ungültige Prüfsumme -> Muss fehlschlagen UND Datei löschen
        let invalid_sums =
            "0000000000000000000000000000000000000000000000000000000000000000  test_asset.bin\n";
        assert!(verify_checksum(&test_file, "test_asset.bin", invalid_sums).is_err());
        assert!(
            !test_file.exists(),
            "Manipulierte Datei muss gelöscht werden"
        );

        // 3. Fehlender Eintrag -> Muss fehlschlagen
        let test_file2 = temp_dir.path().join("missing.bin");
        std::fs::write(&test_file2, b"other data").unwrap();
        assert!(verify_checksum(&test_file2, "missing.bin", &valid_sums).is_err());
        assert!(
            !test_file2.exists(),
            "Datei ohne Prüfsumme muss gelöscht werden"
        );
    }

    #[test]
    fn test_runtime_environment_detection_logic() {
        // AppImage gesetzt
        let r1 = RuntimeEnvironment::detect_from(
            Some("/home/user/Applications/ink-notes.AppImage"),
            Path::new("/tmp/.mount_123/usr/bin/ink-notes"),
        );
        assert_eq!(
            r1,
            RuntimeEnvironment::AppImage {
                appimage_path: PathBuf::from("/home/user/Applications/ink-notes.AppImage")
            }
        );

        // Systemweites Debian-Paket (/usr/bin)
        let r2 = RuntimeEnvironment::detect_from(None, Path::new("/usr/bin/ink-notes"));
        assert_eq!(
            r2,
            RuntimeEnvironment::DebPackage {
                binary_path: PathBuf::from("/usr/bin/ink-notes")
            }
        );

        // Lokales Entwicklungs-Binary
        let r3 = RuntimeEnvironment::detect_from(
            None,
            Path::new("/home/user/ink-notes/target/release/ink-notes"),
        );
        assert_eq!(
            r3,
            RuntimeEnvironment::PortableOrDev {
                binary_path: PathBuf::from("/home/user/ink-notes/target/release/ink-notes")
            }
        );
    }

    #[test]
    fn test_https_enforcement() {
        let config = UpdaterConfig {
            server_url: "https://gitea.pansi.eu".to_string(),
            allow_insecure_http: false,
            ..Default::default()
        };
        assert!(config.validate_url().is_ok());

        let insecure_config = UpdaterConfig {
            server_url: "http://gitea.pansi.eu".to_string(),
            allow_insecure_http: false,
            ..Default::default()
        };
        assert!(insecure_config.validate_url().is_err());

        let allowed_insecure = UpdaterConfig {
            server_url: "http://gitea.pansi.eu".to_string(),
            allow_insecure_http: true,
            ..Default::default()
        };
        assert!(allowed_insecure.validate_url().is_ok());
    }

    #[test]
    fn test_appimage_atomic_self_update() {
        let temp_dir = tempfile::tempdir().unwrap();
        let appimage_path = temp_dir.path().join("ink-notes.AppImage");
        std::fs::write(&appimage_path, b"original-appimage-v1").unwrap();

        let new_download = temp_dir.path().join("downloaded.AppImage");
        std::fs::write(&new_download, b"updated-appimage-v2").unwrap();

        let controller = UpdaterController {
            config: UpdaterConfig::default(),
            runtime: RuntimeEnvironment::AppImage {
                appimage_path: appimage_path.clone(),
            },
            status: UpdaterStatus::Idle,
            active_release: None,
            receiver: channel().1,
            sender: channel().0,
        };

        // Update durchführen
        let res = controller.install_or_open(&new_download);
        assert!(res.is_ok(), "Update sollte erfolgreich sein: {res:?}");

        // Verifizieren: AppImage hat neuen Inhalt
        let updated_content = std::fs::read(&appimage_path).unwrap();
        assert_eq!(updated_content, b"updated-appimage-v2");

        // Backup existiert mit altem Inhalt
        let backup_path = temp_dir.path().join("ink-notes.AppImage.bak");
        assert!(backup_path.exists(), "Backup-Datei muss existieren");
        assert_eq!(
            std::fs::read(&backup_path).unwrap(),
            b"original-appimage-v1"
        );

        // Download-Cache-Datei wurde aufgeräumt
        assert!(
            !new_download.exists(),
            "Download-Datei muss nach Erfolg aufgeräumt werden"
        );
    }

    #[test]
    fn test_gitea_release_deserialization() {
        let json_data = r#"{
            "id": 42,
            "tag_name": "v0.2.0",
            "name": "ink-notes v0.2.0 Release",
            "body": "Neuerungen:\n- Zorin OS 18 Optimierung\n- Gitea Auto-Update",
            "prerelease": false,
            "html_url": "https://gitea.pansi.eu/harald/ink-notes/releases/tag/v0.2.0",
            "published_at": "2026-10-06T12:00:00Z",
            "assets": [
                {
                    "id": 101,
                    "name": "ink-notes_0.2.0_amd64.deb",
                    "size": 4313296,
                    "download_count": 5,
                    "browser_download_url": "https://gitea.pansi.eu/harald/ink-notes/releases/download/v0.2.0/ink-notes_0.2.0_amd64.deb"
                },
                {
                    "id": 102,
                    "name": "ink-notes_0.2.0_x86_64.AppImage",
                    "size": 5872000,
                    "download_count": 8,
                    "browser_download_url": "https://gitea.pansi.eu/harald/ink-notes/releases/download/v0.2.0/ink-notes_0.2.0_x86_64.AppImage"
                }
            ]
        }"#;

        let release: GiteaRelease =
            serde_json::from_str(json_data).expect("Sollte Release deserialisieren");
        assert_eq!(release.tag_name, "v0.2.0");
        assert_eq!(release.assets.len(), 2);
        assert_eq!(release.assets[0].name, "ink-notes_0.2.0_amd64.deb");
        assert_eq!(release.assets[1].name, "ink-notes_0.2.0_x86_64.AppImage");
        assert!(is_newer_version(&release.tag_name, "0.1.0", false));
    }
}
