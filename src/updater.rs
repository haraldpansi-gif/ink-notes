//! In-App Updater für ink-notes mit Gitea-Releases-Integration.
//!
//! Unterstützt:
//! - Versionsabfrage gegen Gitea API (`/api/v1/repos/{owner}/{repo}/releases/latest`)
//! - SemVer-Vergleich (`remote > current`)
//! - Erkennung der Laufzeitumgebung (AppImage, .deb-Paket, Entwicklungs-Binary)
//! - Asynchroner Download ohne Blockierung der Rendering-Schleife
//! - Direkte AppImage-Aktualisierung bzw. Debian-Paket-Installation via xdg-open

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread;

use serde::{Deserialize, Serialize};

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
}

impl Default for UpdaterConfig {
    fn default() -> Self {
        // Falls Umgebungsvariable gesetzt ist, wird das Token automatisch übernommen
        let auth_token = std::env::var("INK_NOTES_GITEA_TOKEN").ok();
        Self {
            server_url: "https://gitea.pansi.eu".to_string(),
            repo_owner: "harald".to_string(),
            repo_name: "ink-notes".to_string(),
            auth_token,
        }
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
    /// Erkennt die aktuelle Laufzeitumgebung automatisch.
    pub fn detect() -> Self {
        if let Ok(appimage_path) = std::env::var("APPIMAGE") {
            if !appimage_path.is_empty() {
                return RuntimeEnvironment::AppImage {
                    appimage_path: PathBuf::from(appimage_path),
                };
            }
        }

        let current_exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("ink-notes"));
        let exe_str = current_exe.to_string_lossy();

        if exe_str.starts_with("/usr/bin/") || exe_str.starts_with("/usr/local/bin/") {
            RuntimeEnvironment::DebPackage {
                binary_path: current_exe,
            }
        } else {
            RuntimeEnvironment::PortableOrDev {
                binary_path: current_exe,
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

/// Zerlegt einen Versionstag wie "v0.1.2" oder "0.1.2-beta" in numerische Komponenten.
pub fn parse_semver(tag: &str) -> Option<(u64, u64, u64)> {
    let clean = tag.trim().trim_start_matches('v').trim_start_matches('V');
    let version_part = clean.split('-').next()?; // Prerelease-Suffix abtrennen
    let mut parts = version_part.split('.');

    let major: u64 = parts.next()?.parse().ok()?;
    let minor: u64 = parts.next()?.parse().ok()?;
    let patch: u64 = parts.next()?.parse().ok()?;

    Some((major, minor, patch))
}

/// Prüft, ob `remote_tag` numerisch neuer ist als `current_tag`.
pub fn is_newer_version(remote_tag: &str, current_tag: &str) -> bool {
    match (parse_semver(remote_tag), parse_semver(current_tag)) {
        (Some((r_maj, r_min, r_pat)), Some((c_maj, c_min, c_pat))) => {
            (r_maj, r_min, r_pat) > (c_maj, c_min, c_pat)
        }
        _ => false,
    }
}

/// Event-Typen für den asynchronen Worker.
#[derive(Debug)]
pub enum UpdaterEvent {
    CheckSuccess(GiteaRelease),
    CheckUpToDate(String),
    CheckError(String),
    DownloadProgress { percent: f32, message: String },
    DownloadSuccess { file_path: PathBuf, asset_name: String },
    DownloadError(String),
}

/// Status des In-App Updaters für die egui-Oberfläche.
#[derive(Debug, Clone, PartialEq)]
pub enum UpdaterStatus {
    Idle,
    Checking,
    UpdateAvailable(GiteaRelease),
    UpToDate { current_version: String },
    Downloading { asset_name: String, percent: f32 },
    DownloadFinished { file_path: PathBuf, asset_name: String },
    Error(String),
}

/// Asynchroner Controller für Update-Prüfung und Asset-Download.
pub struct UpdaterController {
    config: UpdaterConfig,
    runtime: RuntimeEnvironment,
    status: UpdaterStatus,
    receiver: Receiver<UpdaterEvent>,
    sender: Sender<UpdaterEvent>,
}

impl UpdaterController {
    pub fn new() -> Self {
        let (sender, receiver) = channel();
        Self {
            config: UpdaterConfig::default(),
            runtime: RuntimeEnvironment::detect(),
            status: UpdaterStatus::Idle,
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
        if matches!(self.status, UpdaterStatus::Checking | UpdaterStatus::Downloading { .. }) {
            return;
        }

        self.status = UpdaterStatus::Checking;
        let config = self.config.clone();
        let tx = self.sender.clone();
        let current_version = env!("CARGO_PKG_VERSION").to_string();

        thread::spawn(move || {
            let api_url = format!(
                "{}/api/v1/repos/{}/{}/releases/latest",
                config.server_url.trim_end_matches('/'),
                config.repo_owner,
                config.repo_name
            );

            let mut cmd = Command::new("curl");
            cmd.arg("-fsSL")
                .arg("-m")
                .arg("10")
                .arg("-H")
                .arg("Accept: application/json");

            if let Some(token) = &config.auth_token {
                if !token.is_empty() {
                    cmd.arg("-H").arg(format!("Authorization: token {token}"));
                }
            }

            cmd.arg(&api_url);

            match cmd.output() {
                Ok(output) => {
                    if output.status.success() {
                        match serde_json::from_slice::<GiteaRelease>(&output.stdout) {
                            Ok(release) => {
                                if is_newer_version(&release.tag_name, &current_version) {
                                    let _ = tx.send(UpdaterEvent::CheckSuccess(release));
                                } else {
                                    let _ = tx.send(UpdaterEvent::CheckUpToDate(current_version));
                                }
                            }
                            Err(e) => {
                                let _ = tx.send(UpdaterEvent::CheckError(format!(
                                    "JSON-Parsefehler der Gitea API: {e}"
                                )));
                            }
                        }
                    } else {
                        let stderr = String::from_utf8_lossy(&output.stderr);
                        let _ = tx.send(UpdaterEvent::CheckError(format!(
                            "Gitea API antwortete mit Fehler (HTTP {}): {}",
                            output.status.code().unwrap_or(-1),
                            stderr.trim()
                        )));
                    }
                }
                Err(e) => {
                    let _ = tx.send(UpdaterEvent::CheckError(format!(
                        "curl konnte nicht ausgeführt werden: {e}"
                    )));
                }
            }
        });
    }

    /// Startet den asynchronen Download eines Release-Assets.
    pub fn download_asset(&mut self, asset: GiteaAsset) {
        if matches!(self.status, UpdaterStatus::Downloading { .. }) {
            return;
        }

        self.status = UpdaterStatus::Downloading {
            asset_name: asset.name.clone(),
            percent: 0.0,
        };

        let config = self.config.clone();
        let tx = self.sender.clone();

        thread::spawn(move || {
            // Download in temporäres Verzeichnis
            let cache_dir = std::env::temp_dir().join("ink-notes-updates");
            if let Err(e) = std::fs::create_dir_all(&cache_dir) {
                let _ = tx.send(UpdaterEvent::DownloadError(format!(
                    "Konnte Download-Verzeichnis nicht anlegen: {e}"
                )));
                return;
            }

            let target_file = cache_dir.join(&asset.name);

            let mut cmd = Command::new("curl");
            cmd.arg("-fsSL")
                .arg("-m")
                .arg("180") // Bis zu 3 Minuten Timeout für größere Pakete
                .arg("-o")
                .arg(&target_file);

            if let Some(token) = &config.auth_token {
                if !token.is_empty() {
                    cmd.arg("-H").arg(format!("Authorization: token {token}"));
                }
            }

            cmd.arg(&asset.browser_download_url);

            match cmd.output() {
                Ok(output) => {
                    if output.status.success() && target_file.exists() {
                        // Wenn es ein AppImage ist, Executable-Bit setzen
                        if asset.name.ends_with(".AppImage") {
                            #[cfg(unix)]
                            {
                                use std::os::unix::fs::PermissionsExt;
                                if let Ok(metadata) = std::fs::metadata(&target_file) {
                                    let mut perms = metadata.permissions();
                                    perms.set_mode(0o755);
                                    let _ = std::fs::set_permissions(&target_file, perms);
                                }
                            }
                        }

                        let _ = tx.send(UpdaterEvent::DownloadSuccess {
                            file_path: target_file,
                            asset_name: asset.name,
                        });
                    } else {
                        let stderr = String::from_utf8_lossy(&output.stderr);
                        let _ = tx.send(UpdaterEvent::DownloadError(format!(
                            "Download fehlgeschlagen: {}",
                            stderr.trim()
                        )));
                    }
                }
                Err(e) => {
                    let _ = tx.send(UpdaterEvent::DownloadError(format!(
                        "curl-Fehler beim Download: {e}"
                    )));
                }
            }
        });
    }

    /// Startet die Installation des heruntergeladenen Pakets oder öffnet es im System.
    pub fn install_or_open(&self, file_path: &Path) -> Result<(), String> {
        if !file_path.exists() {
            return Err("Die heruntergeladene Datei existiert nicht mehr.".to_string());
        }

        match &self.runtime {
            RuntimeEnvironment::AppImage { appimage_path } => {
                // Bei AppImage: Die aktuelle Datei ersetzen oder neue Datei ausführen
                // Sicherer Modus: Zielpfad überschreiben mit Backup
                if let Some(parent) = appimage_path.parent() {
                    let backup_path = parent.join(format!(
                        "{}.bak",
                        appimage_path.file_name().unwrap().to_string_lossy()
                    ));
                    let _ = std::fs::copy(appimage_path, &backup_path);
                }
                match std::fs::copy(file_path, appimage_path) {
                    Ok(_) => Ok(()),
                    Err(e) => Err(format!(
                        "Konnte laufendes AppImage nicht direkt überschreiben: {e}. Die neue Datei liegt unter: {}",
                        file_path.display()
                    )),
                }
            }
            RuntimeEnvironment::DebPackage { .. } => {
                // Öffne mit Standardanwendung (auf Zorin OS: Zorin Software Store oder gdebi)
                match Command::new("xdg-open").arg(file_path).spawn() {
                    Ok(_) => Ok(()),
                    Err(e) => Err(format!("Konnte xdg-open nicht ausführen: {e}")),
                }
            }
            RuntimeEnvironment::PortableOrDev { .. } => {
                // Im Dateimanager anzeigen
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
        assert_eq!(parse_semver("v0.1.0"), Some((0, 1, 0)));
        assert_eq!(parse_semver("0.1.0"), Some((0, 1, 0)));
        assert_eq!(parse_semver("v1.2.3-rc1"), Some((1, 2, 3)));
        assert_eq!(parse_semver("invalid"), None);
    }

    #[test]
    fn test_is_newer_version() {
        assert!(is_newer_version("v0.2.0", "0.1.0"));
        assert!(is_newer_version("0.1.1", "0.1.0"));
        assert!(is_newer_version("1.0.0", "0.9.9"));
        assert!(!is_newer_version("0.1.0", "0.1.0"));
        assert!(!is_newer_version("v0.1.0", "0.1.0"));
        assert!(!is_newer_version("0.0.9", "0.1.0"));
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

        let release: GiteaRelease = serde_json::from_str(json_data).expect("Sollte Release deserialisieren");
        assert_eq!(release.tag_name, "v0.2.0");
        assert_eq!(release.assets.len(), 2);
        assert_eq!(release.assets[0].name, "ink-notes_0.2.0_amd64.deb");
        assert_eq!(release.assets[1].name, "ink-notes_0.2.0_x86_64.AppImage");
        assert!(is_newer_version(&release.tag_name, "0.1.0"));
    }

    #[test]
    fn test_runtime_environment_detection() {
        let runtime = RuntimeEnvironment::detect();
        assert!(!runtime.display_name().is_empty());
    }
}
