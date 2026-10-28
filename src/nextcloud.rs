//! Talking to a Nextcloud server (Issue #49, stage 2): finding the server, signing in through
//! the browser (Login Flow v2, the app gets its own app password), and WebDAV access to the
//! folder `ink-notes` in the user's files. The sync itself is in `cloud_sync.rs`.
//!
//! Private servers often use a self-signed certificate. Such a certificate is trusted once the
//! user has confirmed its SHA-256 fingerprint; it is then pinned for this server.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::t;

/// Name of the synced folder in the user's files.
pub const REMOTE_FOLDER: &str = "ink-notes";

/// Largest file the sync downloads (bigger ones are surely not ours).
const MAX_DOWNLOAD: u64 = 512 * 1024 * 1024;

/// A connected Nextcloud account, stored in `nextcloud.json` next to the settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Account {
    /// Server address including a subfolder, without trailing slash
    pub server: String,
    /// Login name for the app password
    pub login_name: String,
    /// User id in WebDAV paths (differs from the login name e.g. with e-mail logins)
    pub user_id: String,
    pub app_password: String,
    /// SHA-256 fingerprint of a certificate the user trusts although it isn't signed by a
    /// known authority
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned_cert: Option<String>,
}

impl Account {
    pub fn file_path() -> Option<PathBuf> {
        crate::paths::config_dir().map(|d| d.join("nextcloud.json"))
    }

    pub fn load_from(path: &Path) -> Option<Self> {
        serde_json::from_slice(&std::fs::read(path).ok()?).ok()
    }

    /// Stores the account; the file is only readable by the user.
    pub fn save_to(&self, path: &Path) -> Result<(), String> {
        if let Some(dir) = path.parent() {
            crate::storage::create_secure_dir_all(dir).map_err(|e| e.to_string())?;
        }
        crate::storage::atomic_write_json(path, self).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        }
        Ok(())
    }

    /// `user@server` for the interface.
    pub fn label(&self) -> String {
        let host = url::Url::parse(&self.server)
            .ok()
            .and_then(|u| {
                let port = u.port().map(|p| format!(":{p}")).unwrap_or_default();
                u.host_str()
                    .map(|h| format!("{h}{port}{}", u.path().trim_end_matches('/')))
            })
            .unwrap_or_else(|| self.server.clone());
        format!("{}@{host}", self.login_name)
    }

    pub fn is_insecure(&self) -> bool {
        self.server.starts_with("http://")
    }
}

/// Why talking to the server failed.
#[derive(Debug, Clone, PartialEq)]
pub enum Error {
    /// The certificate isn't signed by a known authority (or doesn't match the pinned one)
    Untrusted {
        fingerprint: String,
    },
    /// Server not reachable (offline, wrong address)
    Offline(String),
    /// App password revoked or wrong
    Unauthorized,
    NotFound,
    /// The file changed on the server since we last saw it
    PreconditionFailed,
    Other(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Untrusted { fingerprint } => write!(
                f,
                "{}",
                t!(
                    "Zertifikat nicht vertrauenswürdig (SHA-256 {fingerprint})",
                    fingerprint = fingerprint
                )
            ),
            Error::Offline(msg) => write!(f, "{}", t!("Server nicht erreichbar: {msg}", msg = msg)),
            Error::Unauthorized => write!(f, "{}", t!("Anmeldung abgelehnt – bitte neu verbinden")),
            Error::NotFound => write!(f, "{}", t!("Nicht gefunden")),
            Error::PreconditionFailed => write!(f, "{}", t!("Inzwischen geändert")),
            Error::Other(msg) => write!(f, "{msg}"),
        }
    }
}

/// Server address from what the user typed: `https://` is added when missing, a pasted link
/// into the web interface is cut back to the server (a subfolder such as `/nextcloud` stays).
pub fn normalize_server(input: &str) -> Result<String, String> {
    let input = input.trim();
    if input.is_empty() {
        return Err(t!("Bitte die Adresse des Nextcloud-Servers eingeben").to_string());
    }
    let with_scheme = if input.contains("://") {
        input.to_string()
    } else {
        format!("https://{input}")
    };
    let mut url = url::Url::parse(&with_scheme).map_err(|e| t!("Ungültige Adresse: {e}", e = e))?;
    if !matches!(url.scheme(), "https" | "http") || url.host_str().is_none() {
        return Err(t!("Die Adresse muss mit https:// beginnen").to_string());
    }
    url.set_query(None);
    url.set_fragment(None);
    let _ = url.set_username("");
    let _ = url.set_password(None);
    let mut path = url.path().to_string();
    for marker in [
        "/index.php",
        "/apps/",
        "/login",
        "/remote.php",
        "/status.php",
        "/s/",
    ] {
        if let Some(pos) = path.find(marker) {
            path.truncate(pos);
        }
    }
    url.set_path(path.trim_end_matches('/'));
    Ok(url.as_str().trim_end_matches('/').to_string())
}

/// SHA-256 fingerprint as colon-separated hex pairs, as browsers show it.
pub fn fingerprint(cert: &[u8]) -> String {
    Sha256::digest(cert)
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}

/// Accepts what the usual authorities sign and, besides, the one pinned certificate. Remembers
/// the fingerprint of a rejected certificate so the user can be asked about it.
#[derive(Debug)]
struct Verifier {
    webpki: Arc<rustls::client::WebPkiServerVerifier>,
    provider: Arc<rustls::crypto::CryptoProvider>,
    pinned: Option<String>,
    rejected: Arc<Mutex<Option<String>>>,
}

impl ServerCertVerifier for Verifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        match self.webpki.verify_server_cert(
            end_entity,
            intermediates,
            server_name,
            ocsp_response,
            now,
        ) {
            Ok(ok) => Ok(ok),
            Err(err) => {
                let print = fingerprint(end_entity);
                if self.pinned.as_deref() == Some(print.as_str()) {
                    return Ok(ServerCertVerified::assertion());
                }
                if let Ok(mut slot) = self.rejected.lock() {
                    *slot = Some(print);
                }
                Err(err)
            }
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// HTTP client for one server, with the pinned certificate if any.
pub struct Client {
    agent: ureq::Agent,
    rejected: Arc<Mutex<Option<String>>>,
    user_agent: String,
}

impl Client {
    pub fn new(pinned: Option<String>) -> Self {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let rejected = Arc::new(Mutex::new(None));
        let mut agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(10))
            .timeout_read(Duration::from_secs(120))
            .timeout_write(Duration::from_secs(120))
            .redirects(0);
        let webpki = rustls::client::WebPkiServerVerifier::builder_with_provider(
            Arc::new(roots),
            provider.clone(),
        )
        .build();
        let config = webpki.map_err(|e| e.to_string()).and_then(|webpki| {
            let verifier = Verifier {
                webpki,
                provider: provider.clone(),
                pinned,
                rejected: rejected.clone(),
            };
            rustls::ClientConfig::builder_with_provider(provider)
                .with_safe_default_protocol_versions()
                .map(|b| {
                    b.dangerous()
                        .with_custom_certificate_verifier(Arc::new(verifier))
                        .with_no_client_auth()
                })
                .map_err(|e| e.to_string())
        });
        match config {
            Ok(config) => agent = agent.tls_config(Arc::new(config)),
            Err(err) => eprintln!("[Nextcloud] TLS-Einrichtung fehlgeschlagen: {err}"),
        }
        Self {
            agent: agent.build(),
            rejected,
            user_agent: format!("ink-notes ({})", crate::storage::device_name()),
        }
    }

    fn request(&self, method: &str, url: &str) -> ureq::Request {
        if let Ok(mut slot) = self.rejected.lock() {
            *slot = None;
        }
        self.agent
            .request(method, url)
            .set("User-Agent", &self.user_agent)
    }

    /// Turns a failed request into an [`Error`].
    fn error(&self, err: ureq::Error) -> Error {
        match err {
            ureq::Error::Status(401, _) => Error::Unauthorized,
            ureq::Error::Status(404, _) => Error::NotFound,
            ureq::Error::Status(412, _) => Error::PreconditionFailed,
            ureq::Error::Status(code, resp) => {
                let location = resp.header("Location").map(str::to_string);
                match location {
                    Some(to) if (300..400).contains(&code) => Error::Other(t!(
                        "Der Server leitet nach {to} weiter – bitte diese Adresse verwenden",
                        to = to
                    )),
                    _ => Error::Other(t!("Server-Fehler {code}", code = code)),
                }
            }
            ureq::Error::Transport(transport) => {
                if let Some(print) = self.rejected.lock().ok().and_then(|s| s.clone()) {
                    return Error::Untrusted { fingerprint: print };
                }
                Error::Offline(transport.to_string())
            }
        }
    }
}

/// Checks that a Nextcloud server answers at `server`.
pub fn check_server(client: &Client, server: &str) -> Result<(), Error> {
    let resp = client
        .request("GET", &format!("{server}/status.php"))
        .call()
        .map_err(|e| client.error(e))?;
    let status: serde_json::Value = resp.into_json().map_err(|_| not_nextcloud())?;
    if status.get("installed").and_then(|v| v.as_bool()) == Some(true) {
        Ok(())
    } else {
        Err(not_nextcloud())
    }
}

fn not_nextcloud() -> Error {
    Error::Other(t!("Unter dieser Adresse antwortet kein Nextcloud-Server").to_string())
}

/// A started sign-in: the user approves it in the browser, the app polls for the result.
#[derive(Debug, Clone, PartialEq)]
pub struct LoginFlow {
    pub login_url: String,
    poll_url: String,
    token: String,
}

/// Puts a link the server built with its own idea of its address (wrong behind a proxy) onto
/// the address the user gave.
fn onto_server(link: &str, server: &str, marker: &str) -> String {
    let same_origin = match (url::Url::parse(link), url::Url::parse(server)) {
        (Ok(a), Ok(b)) => a.origin() == b.origin(),
        _ => false,
    };
    match link.find(marker) {
        Some(pos) if !same_origin => format!("{server}/index.php{}", &link[pos..]),
        _ => link.to_string(),
    }
}

/// Starts Login Flow v2; the app password gets the device name as its name.
pub fn start_login(client: &Client, server: &str) -> Result<LoginFlow, Error> {
    #[derive(Deserialize)]
    struct Poll {
        token: String,
        endpoint: String,
    }
    #[derive(Deserialize)]
    struct Start {
        poll: Poll,
        login: String,
    }
    let resp = client
        .request("POST", &format!("{server}/index.php/login/v2"))
        .call()
        .map_err(|e| client.error(e))?;
    let start: Start = resp.into_json().map_err(|_| not_nextcloud())?;
    Ok(LoginFlow {
        login_url: onto_server(&start.login, server, "/login/v2"),
        poll_url: onto_server(&start.poll.endpoint, server, "/login/v2"),
        token: start.poll.token,
    })
}

/// Login name and app password once the user has approved the sign-in.
pub fn poll_login(client: &Client, flow: &LoginFlow) -> Result<Option<(String, String)>, Error> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Done {
        login_name: String,
        app_password: String,
    }
    match client
        .request("POST", &flow.poll_url)
        .send_form(&[("token", flow.token.as_str())])
    {
        Ok(resp) => {
            let done: Done = resp.into_json().map_err(|e| Error::Other(e.to_string()))?;
            Ok(Some((done.login_name, done.app_password)))
        }
        // Not approved yet
        Err(ureq::Error::Status(404, _)) => Ok(None),
        Err(err) => Err(client.error(err)),
    }
}

fn basic_auth(login_name: &str, password: &str) -> String {
    let raw = format!("{login_name}:{password}");
    format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(raw)
    )
}

/// User id of the signed-in user (the name in WebDAV paths).
pub fn user_id(
    client: &Client,
    server: &str,
    login_name: &str,
    password: &str,
) -> Result<String, Error> {
    let resp = client
        .request(
            "GET",
            &format!("{server}/ocs/v2.php/cloud/user?format=json"),
        )
        .set("OCS-APIRequest", "true")
        .set("Authorization", &basic_auth(login_name, password))
        .call()
        .map_err(|e| client.error(e))?;
    let value: serde_json::Value = resp.into_json().map_err(|e| Error::Other(e.to_string()))?;
    value["ocs"]["data"]["id"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| Error::Other(t!("Unerwartete Antwort des Servers").to_string()))
}

/// Revokes the app password on the server (when disconnecting).
pub fn revoke(account: &Account) -> Result<(), Error> {
    let client = Client::new(account.pinned_cert.clone());
    client
        .request(
            "DELETE",
            &format!("{}/ocs/v2.php/core/apppassword", account.server),
        )
        .set("OCS-APIRequest", "true")
        .set(
            "Authorization",
            &basic_auth(&account.login_name, &account.app_password),
        )
        .call()
        .map(|_| ())
        .map_err(|e| client.error(e))
}

/// One entry of a folder listing on the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Path relative to the synced folder, `/`-separated
    pub path: String,
    pub is_dir: bool,
    pub etag: String,
}

/// How an upload must find the file on the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Expect<'a> {
    /// Must not exist yet
    New,
    /// Must still have this ETag
    Etag(&'a str),
}

/// The folder `ink-notes` on the server, as the sync sees it.
pub trait Remote {
    /// Entries directly in `dir` (`""` is the synced folder); `None` if it doesn't exist.
    fn list(&mut self, dir: &str) -> Result<Option<Vec<Entry>>, Error>;
    /// Content and ETag of a file.
    fn get(&mut self, path: &str) -> Result<(Vec<u8>, String), Error>;
    /// Uploads a file and returns its new ETag.
    fn put(&mut self, path: &str, data: &[u8], expect: Expect<'_>) -> Result<String, Error>;
    /// Creates a folder (fine if it exists).
    fn mkdir(&mut self, path: &str) -> Result<(), Error>;
    /// Deletes a file or folder (into the server's trash bin); with an ETag only if unchanged.
    fn delete(&mut self, path: &str, etag: Option<&str>) -> Result<(), Error>;
}

/// [`Remote`] over WebDAV.
pub struct WebDav {
    client: Client,
    /// `…/remote.php/dav/files/<user>/ink-notes/`
    root: url::Url,
    auth: String,
}

impl WebDav {
    pub fn new(account: &Account) -> Result<Self, Error> {
        Self::with_folder(account, REMOTE_FOLDER)
    }

    /// Like [`Self::new`], with another folder (for tests against a real server).
    pub fn with_folder(account: &Account, folder: &str) -> Result<Self, Error> {
        let mut root = url::Url::parse(&format!("{}/", account.server))
            .map_err(|e| Error::Other(e.to_string()))?;
        root.path_segments_mut()
            .map_err(|_| Error::Other(account.server.clone()))?
            .pop_if_empty()
            .extend(["remote.php", "dav", "files", &account.user_id, folder, ""]);
        Ok(Self {
            client: Client::new(account.pinned_cert.clone()),
            root,
            auth: basic_auth(&account.login_name, &account.app_password),
        })
    }

    fn url(&self, path: &str, dir: bool) -> String {
        let mut url = self.root.clone();
        if let Ok(mut segments) = url.path_segments_mut() {
            segments.pop_if_empty();
            if !path.is_empty() {
                segments.extend(path.split('/'));
            }
            if dir {
                segments.push("");
            }
        }
        url.to_string()
    }

    fn request(&self, method: &str, url: &str) -> ureq::Request {
        self.client
            .request(method, url)
            .set("Authorization", &self.auth)
    }
}

/// Decodes `%xx` escapes of a URL path.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
            if let Ok(b) = u8::from_str_radix(hex, 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Entries of a PROPFIND answer: (decoded href path, is folder, ETag).
pub fn parse_multistatus(xml: &str) -> Result<Vec<(String, bool, String)>, String> {
    use quick_xml::events::Event;
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut entries = Vec::new();
    let mut href = String::new();
    let mut etag = String::new();
    let mut is_dir = false;
    let mut ok = false;
    let mut in_tag: Option<&'static str> = None;
    loop {
        match reader.read_event().map_err(|e| e.to_string())? {
            Event::Start(e) => {
                let name = e.local_name();
                match name.as_ref() {
                    b"response" => {
                        href.clear();
                        etag.clear();
                        is_dir = false;
                        ok = false;
                    }
                    b"href" => in_tag = Some("href"),
                    b"getetag" => in_tag = Some("etag"),
                    b"status" => in_tag = Some("status"),
                    b"collection" => is_dir = true,
                    _ => {}
                }
            }
            Event::Empty(e) => {
                if e.local_name().as_ref() == b"collection" {
                    is_dir = true;
                }
            }
            event @ (Event::Text(_) | Event::GeneralRef(_)) => {
                let text = match event {
                    Event::Text(t) => t.decode().map_err(|e| e.to_string())?.into_owned(),
                    Event::GeneralRef(r) => match r.resolve_char_ref() {
                        Ok(Some(c)) => c.to_string(),
                        _ => r
                            .decode()
                            .ok()
                            .and_then(|name| quick_xml::escape::resolve_predefined_entity(&name))
                            .unwrap_or_default()
                            .to_string(),
                    },
                    _ => String::new(),
                };
                match in_tag {
                    Some("href") => href.push_str(&text),
                    Some("etag") => etag.push_str(&text),
                    Some("status") => ok |= text.contains(" 200 "),
                    _ => {}
                }
            }
            Event::End(e) => {
                let name = e.local_name();
                match name.as_ref() {
                    b"href" | b"getetag" | b"status" => in_tag = None,
                    b"response" if ok => {
                        entries.push((percent_decode(href.trim()), is_dir, etag.trim().to_string()))
                    }
                    _ => {}
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(entries)
}

/// ETag of a file in an answer, as PROPFIND lists it. Nextcloud's own `OC-ETag` is left alone
/// by the web server; a plain `ETag` may carry a mark of the compression.
fn etag_of(resp: &ureq::Response) -> Option<String> {
    if let Some(etag) = resp.header("OC-ETag") {
        return Some(etag.to_string());
    }
    let etag = resp.header("ETag")?;
    let plain = etag.strip_suffix('"').and_then(|unquoted| {
        ["-gzip", "-br", "-zstd", "-df"]
            .iter()
            .find_map(|mark| unquoted.strip_suffix(mark))
    });
    Some(plain.map_or(etag.to_string(), |tag| format!("{tag}\"")))
}

const PROPFIND_BODY: &str = r#"<?xml version="1.0"?><d:propfind xmlns:d="DAV:"><d:prop><d:getetag/><d:resourcetype/></d:prop></d:propfind>"#;

impl Remote for WebDav {
    fn list(&mut self, dir: &str) -> Result<Option<Vec<Entry>>, Error> {
        let url = self.url(dir, true);
        let resp = match self
            .request("PROPFIND", &url)
            .set("Depth", "1")
            .set("Content-Type", "application/xml; charset=utf-8")
            .send_string(PROPFIND_BODY)
        {
            Ok(resp) => resp,
            Err(ureq::Error::Status(404, _)) => return Ok(None),
            Err(err) => return Err(self.client.error(err)),
        };
        let body = resp
            .into_string()
            .map_err(|e| Error::Offline(e.to_string()))?;
        let root_path = percent_decode(self.root.path());
        let entries = parse_multistatus(&body).map_err(Error::Other)?;
        Ok(Some(
            entries
                .into_iter()
                .filter_map(|(href, is_dir, etag)| {
                    // Hrefs are paths, sometimes full URLs
                    let path = match href.find("/remote.php/") {
                        Some(pos) if href.contains("://") => href[pos..].to_string(),
                        _ => href,
                    };
                    let path = path.strip_prefix(&root_path)?.trim_end_matches('/');
                    (path != dir && !path.is_empty()).then(|| Entry {
                        path: path.to_string(),
                        is_dir,
                        etag,
                    })
                })
                .collect(),
        ))
    }

    fn get(&mut self, path: &str) -> Result<(Vec<u8>, String), Error> {
        // Uncompressed: Apache marks the ETag of a compressed answer (`"…-gzip"`)
        let resp = self
            .request("GET", &self.url(path, false))
            .set("Accept-Encoding", "identity")
            .call()
            .map_err(|e| self.client.error(e))?;
        let etag = etag_of(&resp).unwrap_or_default();
        let mut data = Vec::new();
        resp.into_reader()
            .take(MAX_DOWNLOAD)
            .read_to_end(&mut data)
            .map_err(|e| Error::Offline(e.to_string()))?;
        Ok((data, etag))
    }

    fn put(&mut self, path: &str, data: &[u8], expect: Expect<'_>) -> Result<String, Error> {
        let mut req = self.request("PUT", &self.url(path, false));
        req = match expect {
            Expect::New => req.set("If-None-Match", "*"),
            Expect::Etag(etag) => req.set("If-Match", etag),
        };
        let resp = req.send_bytes(data).map_err(|e| self.client.error(e))?;
        match etag_of(&resp) {
            Some(etag) => Ok(etag),
            None => {
                // Look it up when the server didn't say
                let parent = path.rsplit_once('/').map_or("", |(p, _)| p);
                self.list(parent)?
                    .unwrap_or_default()
                    .into_iter()
                    .find(|e| e.path == path)
                    .map(|e| e.etag)
                    .ok_or(Error::NotFound)
            }
        }
    }

    fn mkdir(&mut self, path: &str) -> Result<(), Error> {
        match self.request("MKCOL", &self.url(path, true)).call() {
            Ok(_) | Err(ureq::Error::Status(405, _)) => Ok(()),
            Err(err) => Err(self.client.error(err)),
        }
    }

    fn delete(&mut self, path: &str, etag: Option<&str>) -> Result<(), Error> {
        let mut req = self.request("DELETE", &self.url(path, false));
        if let Some(etag) = etag {
            req = req.set("If-Match", etag);
        }
        match req.call() {
            Ok(_) | Err(ureq::Error::Status(404, _)) => Ok(()),
            Err(err) => Err(self.client.error(err)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_server_addresses() {
        assert_eq!(
            normalize_server("cloud.example.org").unwrap(),
            "https://cloud.example.org"
        );
        assert_eq!(
            normalize_server(" https://example.org/nextcloud/ ").unwrap(),
            "https://example.org/nextcloud"
        );
        assert_eq!(
            normalize_server("https://example.org/nextcloud/index.php/apps/files/?dir=/").unwrap(),
            "https://example.org/nextcloud"
        );
        assert_eq!(
            normalize_server("http://192.168.1.5:8080/apps/dashboard/").unwrap(),
            "http://192.168.1.5:8080"
        );
        assert!(normalize_server("").is_err());
        assert!(normalize_server("ftp://example.org").is_err());
    }

    #[test]
    fn moves_links_onto_the_given_server() {
        let server = "https://cloud.example.org/nc";
        assert_eq!(
            onto_server("http://localhost/login/v2/poll", server, "/login/v2"),
            "https://cloud.example.org/nc/index.php/login/v2/poll"
        );
        let same = "https://cloud.example.org/nc/login/v2/flow/abc";
        assert_eq!(onto_server(same, server, "/login/v2"), same);
    }

    #[test]
    fn parses_propfind_answers() {
        let xml = r#"<?xml version="1.0"?>
<d:multistatus xmlns:d="DAV:"><d:response><d:href>/remote.php/dav/files/pia/ink-notes/</d:href><d:propstat><d:prop><d:getetag>&quot;abc&quot;</d:getetag><d:resourcetype><d:collection/></d:resourcetype></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
<d:response><d:href>/remote.php/dav/files/pia/ink-notes/library.json</d:href><d:propstat><d:prop><d:getetag>&quot;e1&quot;</d:getetag><d:resourcetype/></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
<d:response><d:href>/remote.php/dav/files/pia/ink-notes/Mathe%20%C3%BC/</d:href><d:propstat><d:prop><d:getetag>"e2"</d:getetag><d:resourcetype><d:collection/></d:resourcetype></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#;
        let entries = parse_multistatus(xml).unwrap();
        assert_eq!(
            entries,
            vec![
                (
                    "/remote.php/dav/files/pia/ink-notes/".to_string(),
                    true,
                    "\"abc\"".to_string()
                ),
                (
                    "/remote.php/dav/files/pia/ink-notes/library.json".to_string(),
                    false,
                    "\"e1\"".to_string()
                ),
                (
                    "/remote.php/dav/files/pia/ink-notes/Mathe ü/".to_string(),
                    true,
                    "\"e2\"".to_string()
                ),
            ]
        );
    }

    #[test]
    fn builds_webdav_urls() {
        let account = Account {
            server: "https://example.org/nc".into(),
            login_name: "pia@example.org".into(),
            user_id: "pia".into(),
            app_password: "x".into(),
            pinned_cert: None,
        };
        let dav = WebDav::new(&account).unwrap();
        assert_eq!(
            dav.url("nb 1/page_1.json", false),
            "https://example.org/nc/remote.php/dav/files/pia/ink-notes/nb%201/page_1.json"
        );
        assert_eq!(
            dav.url("", true),
            "https://example.org/nc/remote.php/dav/files/pia/ink-notes/"
        );
        assert_eq!(account.label(), "pia@example.org@example.org/nc");
    }

    #[test]
    fn account_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cfg").join("nextcloud.json");
        let account = Account {
            server: "https://example.org".into(),
            login_name: "pia".into(),
            user_id: "pia".into(),
            app_password: "secret".into(),
            pinned_cert: Some("AA:BB".into()),
        };
        account.save_to(&path).unwrap();
        assert_eq!(Account::load_from(&path), Some(account));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }
}
