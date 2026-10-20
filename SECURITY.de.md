# Sicherheit

[English](SECURITY.md) · **Deutsch**

## Unterstützte Versionen

Sicherheitskorrekturen gibt es nur für die jeweils **neueste Version** (siehe [Releases](https://gitea.pansi.eu/harald/ink-notes/releases)). Die App bietet Updates selbst an (☰ → Updates); als Flatpak kommen sie über die Software-Verwaltung.

## Sicherheitslücke melden

Bitte Sicherheitslücken **nicht** in einem öffentlichen Issue melden, sondern per E-Mail an **harald@pansi.eu** – möglichst mit:

- betroffener Version und Installationsart (.deb, AppImage, Flatpak, selbst gebaut),
- einer Beschreibung, was passieren kann,
- den Schritten oder einer Datei (z. B. ein präpariertes Notizbuch oder PDF), mit denen es sich nachstellen lässt.

Du bekommst innerhalb einer Woche eine Antwort. Ist die Lücke bestätigt, erscheint die Korrektur als neue Version; auf Wunsch wirst du in den Release-Notes genannt.

## Besonders interessant

- **Updates:** Prüfung der Minisign-Signatur und der SHA-256-Prüfsummen, Download und Austausch der Pakete (siehe „Sicherheit der Updates“ im [README](README.de.md#sicherheit-der-updates)).
- **Dateien öffnen:** importierte PDFs und Bilder, Notizbuch-Dateien ([Dateiformat](docs/FILE_FORMAT.de.md)) – z. B. Abstürze oder Schreiben außerhalb des Datenordners durch präparierte Dateien.
