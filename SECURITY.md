# Sicherheit / Security Policy

## Unterstützte Versionen / Supported versions

Sicherheitskorrekturen gibt es nur für die jeweils **neueste Version** (siehe [Releases](https://gitea.pansi.eu/harald/ink-notes/releases)). Die App bietet Updates selbst an (☰ → Updates); als Flatpak kommen sie über die Software-Verwaltung.

Security fixes are only provided for the **latest release**. The app offers updates itself (☰ → Updates); the Flatpak is updated through the software manager.

## Sicherheitslücke melden / Reporting a vulnerability

Bitte Sicherheitslücken **nicht** in einem öffentlichen Issue melden, sondern per E-Mail an **harald@pansi.eu** – möglichst mit:

- betroffener Version und Installationsart (.deb, AppImage, Flatpak, selbst gebaut),
- einer Beschreibung, was passieren kann,
- den Schritten oder einer Datei (z. B. ein präpariertes Notizbuch oder PDF), mit denen es sich nachstellen lässt.

Du bekommst innerhalb einer Woche eine Antwort. Ist die Lücke bestätigt, erscheint die Korrektur als neue Version; auf Wunsch wirst du in den Release-Notes genannt.

Please do **not** report vulnerabilities in a public issue. Send an e-mail to **harald@pansi.eu** instead, ideally with the affected version and installation type, a description of the impact and steps or a file (e.g. a crafted notebook or PDF) to reproduce it. You will get an answer within a week. Confirmed issues are fixed in a new release; on request you will be credited in the release notes.

## Besonders interessant / Areas of interest

- **Updates:** Prüfung der Minisign-Signatur und der SHA-256-Prüfsummen, Download und Austausch der Pakete (siehe „Sicherheit der Updates“ im [README](README.de.md#sicherheit-der-updates)).
- **Dateien öffnen:** importierte PDFs und Bilder, Notizbuch-Dateien ([Dateiformat](docs/FILE_FORMAT.md)) – z. B. Abstürze oder Schreiben außerhalb des Datenordners durch präparierte Dateien.
- **Updates:** verification of the Minisign signature and SHA-256 checksums, downloading and replacing packages.
- **Opening files:** imported PDFs and images, notebook files – e.g. crashes or writes outside the data folder caused by crafted files.
