# 🖋️ ink-notes

Eine latenzoptimierte, hardwarebeschleunigte Inking- und Notizbuch-Anwendung für Linux, entwickelt in **Rust** mit **wgpu** und **winit**.

`ink-notes` kombiniert minimale Eingabelatenz beim Zeichnen mit Stift, Touchpad oder Maus mit einem flüssigen stufenlosen Viewport, automatischer Geometrie- und Formerkennung, Objekttransformationen, Multi-Dokumenten-Verwaltung und Vektor-PDF-Export.

---

## ✨ Features im Überblick

### ⚡ Latenzoptimierte Inking-Pipeline
- **Hardware-Rendering:** Direct-to-Screen-Rendering mit `wgpu` (Vulkan / X11 / Wayland) unter Nutzung von `PresentMode::Mailbox` für minimale Eingabeverzögerung.
- **Zwei-Stufen-Modell:**
  1. *Live-Phase:* Eingehende Stylus-Samples werden in Echtzeit durch einen 1€-Filter (One-Euro-Filter) entprellt und inkrementell als GPU-Meshes dargestellt.
  2. *Commit-Phase (Post-Stroke Finalisierung):* Akausale Ecken-Erkennung (Corner Preservation bei Knicken > 65°), gewichtetes 3-Punkt-Druckfenster, Ramer-Douglas-Peucker (RDP) Dezimierung und saubere runde Endkappen.
- **Centripetal Catmull-Rom Splines:** Weiche C1-stetige Kurveninterpolation ohne Überschwinger oder Schleifen.
- **Werkzeuge:**
  - **Stift (Pen):** Feine, variable Strichstärken (0.3 mm bis 1.2 mm) mit Notiz-Farben (Schwarz, Blau, Rot, Grün, Grau).
  - **Textmarker (Highlighter):** Breite, halbtransparente Striche (4.0 mm bis 8.0 mm) mit Multiplikations-Blending.
  - **Radierer (Eraser):** Segment- und abstandsbasierte Löschung einzelner Striche auf dem DIN-A4-Canvas.
  - **Text (`T`):** Ins Blatt tippen und mit der Tastatur schreiben – für Überschriften, Code-Schnipsel oder kopierten Text. Normale Schrift oder Code-Schrift (feste Zeichenbreite, Tab rückt ein), 8 bis 36 pt, Farben der Stiftpalette; Zeilen werden am rechten Rand des Textfelds umbrochen. Vorhandenen Text antippen zum Ändern, `Esc` beendet. Textfelder lassen sich wie Bilder verschieben, drehen und skalieren; im PDF-Export ist der Text echt (durchsuchbar und kopierbar). Aus anderen Programmen kopierter Text wird mit `Strg + V` zum Textfeld.

### 🔍 Stufenloser Zoom & Pan (Kamera)
- **Viewport-Transformation:** 2D-Projektionsmatrix im WGSL-Vertex-Shader.
- **Durchgehendes Scrollen:** Seiten hängen untereinander und werden mit Mausrad, Finger oder Touchpad fließend gescrollt; wer im unteren Fünftel der letzten Seite schreibt, bekommt automatisch eine neue Seite angehängt. Abschaltbar im ☰-Menü unter „Ansicht“ (dann Seite für Seite blättern).
- **Cursor-zentrierter Zoom:** Stufenloses Zoomen von **25 % bis 1000 %** (importierte PDF-Seiten werden beim Hineinzoomen schärfer nachgerendert) zentriert auf die Cursor-Position via `Strg + Mausrad` (ohne durchgehende Ansicht: Mausrad), Trackpad-Pinch oder Tastenkürzel (`Strg +` / `Strg -` / `Strg 0`).
- **Pan:** Verschieben des Canvas per mittlerer Maustaste, `Leertaste + Drag` oder Trackpad-Pan.
- **Inverse Koordinatentransformation:** Präzise Umrechnung von Bildschirm- in DIN-A4-Dokumentkoordinaten (`screen_to_page`).

### 📐 Geometrie- & Objekterkennung (Shape Recognition)
- **Erkannte Primitiven:**
  - **Geraden (Line):** Sehnen- zu Pfadlängenverhältnis > 0.92.
  - **Rechtecke (Rectangle):** 4 orthogonale Ecken via Douglas-Peucker-Polygonzug (RDP).
  - **Kreise & Ellipsen (Circle/Ellipse):** Geschlossene Pfade mit minimaler Varianz via Least-Squares Circle Fit.
  - **Koordinatensysteme (Axes):** Zwei orthogonale Achsen (80°–100° gesnappt auf exakt 90°) mit Richtungspfeilen.
  - **Dreiecke, Rhomben / Rauten & Oktaeder:** Regelmäßige Polygone mit Eckpunkt-Analyse.
- **Interaktionsmodelle:**
  - **Draw-and-Hold:** Verweilt der Stift am Ende eines Strichs für > 400 ms regungslos (< 3 px), snappt die Skizze sofort automatisch in eine perfekte Vektorform ein.
  - **Formen-Werkzeug (Shape Mode):** Direkte Erkennung und Ersetzung beim Loslassen.

### 🖐️ Objektauswahl & Transformation (Lasso / Select)
- **Lasso & Klick-Auswahl:** Schnelles Markieren einzelner Striche oder Gruppen per Rahmen (`V`).
- **Echtzeit-Verschieben:** Freies Verschieben ausgewählter Objekte über die Seite.
- **Stufenlose Skalierung:** Größenänderung über interaktive Eck-Griffe.
- **Drehung:** Stufenlose Rotation um das Objektzentrum über den oberen Dreh-Griff oder exakte 90°-Schritte mit Taste `R`.
- **Multicolor-Umfärbung:** Ausgewählte Objekte direkt über die Farbpalette umfärben.
- **Clipboard & Editieren:** Kopieren (`Strg + C`), Einfügen (`Strg + V`) und Löschen (`Entf` / `Backspace`).
- **Undo / Redo:** Vollständige Transaktionshistorie (`Strg + Z` / `Strg + Y`) für alle Aktionen inklusive Transformationen.

### 📚 Multi-Dokumenten- & Notizbuch-Verwaltung
- **Dateisystem-Architektur:** Jedes Dokument wird in einem separaten Verzeichnis unter `~/.local/share/ink-notes/notebooks/<id>/` mit `manifest.json` und Einzelseiten (`page_<id>.json`) gespeichert.
- **Dokumenten-Manager:** Toolbar-Modal zum Anlegen, Wechseln, Umbenennen und Löschen von Notizbüchern.
- **Verlustfreier Wechsel:** Beim Dokumentenwechsel sichert ein synchroner Disk-Flush alle anstehenden Änderungen garantiert ab.

### 📄 Seiten & Hintergründe
- **Virtuelles DIN-A4-Raster:** 2100 × 2970 Koordinateneinheiten (10 Einheiten = 1 mm).
- **Hintergrundmuster:** Blanko, Kariert (5 mm), Liniert (7.5 mm), Punktraster (5 mm).
- **Seitenverwaltung:** Vor- und Zurückblättern, neue Seiten anfügen, Seitensprung-Menü.

### 💾 Autosave & Persistenz
- **Asynchroner Autosave-Controller:** Sichert Änderungen im Hintergrund nach 1.5 s Ruhezeit (Debounce), ohne das Zeichnen zu blockieren.
- **Atomares Schreiben:** Temp-Dateien mit atomarem Rename (`manifest.json.tmp` -> `manifest.json`) schützen vor Datenkorruption bei Abstürzen.
- **Dediziertes Speichern (`Strg + S`):** Sofortiger, synchroner Disk-Flush mit visueller Toast-Rückmeldung.
- **Sicherer Shutdown:** `Strg + Q` oder Schließen des Fensters führt vor Programmende einen vollständigen synchronen Flush aus.

### 🖨️ Vektor-PDF-Export
- Exportiert das Dokument via `printpdf` nach `~/Documents/<Titel>.pdf` (mit automatischer Kollisionsvermeidung `_1`, `_2`).
- Striche und geometrische Formen werden als skalierbare Vektoren ausgegeben. Kürzel: `Strg + E`.

---

## 📦 Release, Packaging & Updater

- **Natives .deb-Paket**: Optimiert für Zorin OS 18 (Ubuntu 24.04 LTS Basis) mit vollständiger XDG Desktop-Integration (`.desktop`, Hicolor-Icons SVG & PNG, AppStream Metainfo, Debian Copyright & Changelog).
- **Portables .AppImage**: Eigenständiges x86_64 Bundle.
- **In-App Updater (`src/updater.rs`)**: Automatischer Prüflauf gegen Gitea Releases, SemVer-Filterung, Minisign-Signaturprüfung, SHA-256-Prüfung und atomarer In-Place Austausch.

### 1. Release-Pakete bauen

```bash
# Baut .deb, .AppImage und berechnet SHA256SUMS.txt in target/
./scripts/build-release.sh
```

Ergebnis in `target/`:
- `ink-notes_0.1.0_amd64.deb`
- `ink-notes_0.1.0_x86_64.AppImage`
- `SHA256SUMS.txt`
- `SHA256SUMS.txt.minisig` (nur mit gesetztem `MINISIGN_SECRET_KEY`, siehe unten)

### Release-Signatur einrichten (einmalig)

In-App-Updates werden nur mit gültiger Minisign-Signatur installiert. Signiert wird mit [rsign2](https://crates.io/crates/rsign2), einer Minisign-kompatiblen Implementierung (`cargo install rsign2 --locked`). Einmalig ein Schlüsselpaar erzeugen (ohne Passwort, da es nur als CI-Secret verwendet wird):

```bash
rsign generate -W -p ink-notes.pub -s ink-notes.key
```

> **Wichtig:** Schlüssel und Signierwerkzeug müssen zusammenpassen. `rsign generate -W` verschlüsselt den Schlüssel mit einem leeren Passwort; das `minisign`-CLI (z. B. Ubuntu-Paket 0.11) kann ihn ohne Terminal nicht laden. `build-release.sh` und die CI signieren deshalb mit `rsign`.

1. Die zweite Zeile aus `ink-notes.pub` (beginnt mit `RW`) in `src/updater.rs` als `UPDATE_SIGNING_PUBLIC_KEY` eintragen und committen.
2. Den kompletten Inhalt von `ink-notes.key` in Gitea unter *Repository → Einstellungen → Actions → Secrets* als `MINISIGN_SECRET_KEY` anlegen.
3. `ink-notes.key` sicher offline aufbewahren und vom Rechner löschen. Geht der Schlüssel verloren, können bestehende Installationen keine Updates mehr prüfen.

Lokal signiert `build-release.sh` nur, wenn `MINISIGN_SECRET_KEY` gesetzt und `rsign` installiert ist. In der CI ist die Signatur Pflicht (`INK_NOTES_REQUIRE_SIGNATURE=1`), und das Skript prüft, dass der geheime Schlüssel zum eingebauten öffentlichen Schlüssel passt.

### 2. Installation auf Zorin OS 18 / Ubuntu 24.04

```bash
# Natives Debian-Paket installieren
sudo dpkg -i target/ink-notes_0.1.0_amd64.deb

# Oder portables AppImage starten
chmod +x target/ink-notes_0.1.0_x86_64.AppImage
./target/ink-notes_0.1.0_x86_64.AppImage
```

#### Hinweise zu AppImage, glibc & FUSE:
- **glibc-Mindestversion:** `ink-notes` benötigt `glibc >= 2.39` (Standard unter Ubuntu 24.04 LTS und Zorin OS 18).
- **Host-Grafikstack:** Das AppImage bündelt die Anwendungslogik, nutzt für maximale Performance und Hardwarebeschleunigung jedoch den Treiber- und Grafikstack des Host-Systems (Vulkan ICD-Treiber, Wayland bzw. X11).
- **FUSE 2 / FUSE 3 Kompatibilität:** Ubuntu 24.04-basierte Distributionen nutzen standardmäßig FUSE 3 (`fuse3`). Sollte das AppImage beim Start einen Fehler bezüglich `libfuse.so.2` ausgeben, kann entweder das Kompatibilitätspaket installiert werden:
  ```bash
  sudo apt install libfuse2t64
  ```
  oder das AppImage direkt ohne FUSE entpackt und ausgeführt werden:
  ```bash
  ./ink-notes_0.1.0_x86_64.AppImage --appimage-extract-and-run
  ```

### 3. Release auf Gitea veröffentlichen

Das Skript arbeitet vollkommen idempotent und aktualisiert bei wiederholter Ausführung Release-Notizen und Assets:

```bash
export GITEA_TOKEN="<DEIN_GITEA_TOKEN>"
./scripts/publish-gitea-release.sh
```

---

## 🛡️ Updater-Sicherheit

Der In-App-Updater (`src/updater.rs`) folgt strengen Sicherheitsrichtlinien:

1. **Minisign-Signatur (Pflicht):**
   - Jedes Release enthält `SHA256SUMS.txt.minisig`, eine Minisign-Signatur der Prüfsummendatei.
   - Der Updater lädt `SHA256SUMS.txt` und die Signatur **vor** dem Paket und prüft sie gegen den in die App eingebauten öffentlichen Schlüssel (`UPDATE_SIGNING_PUBLIC_KEY` in `src/updater.rs`, Crate `minisign-verify`).
   - Fehlt die Signatur, ist sie ungültig oder ist kein Schlüssel eingebaut, wird das Update abgelehnt. Ein kompromittierter Release-Server kann so keine manipulierten Pakete ausliefern.
2. **SHA-256-Integritätsprüfung:**
   - Der SHA-256-Hash des heruntergeladenen Pakets wird mit dem Eintrag in der signierten `SHA256SUMS.txt` verglichen (`sha2`).
   - Weicht die Prüfsumme ab oder fehlt ein Eintrag, wird die Datei sofort gelöscht und der Installationsvorgang mit Fehlermeldung abgebrochen.
3. **Redirect-Schutz & HTTPS:**
   - Jede Anfrage erzwingt HTTPS – auch Download-Links aus der Release-API und jedes Weiterleitungsziel.
   - Bei HTTP-Redirects werden sensible Autorisierungs-Header (`Authorization: token ...`) verworfen, sobald die Weiterleitung auf eine andere Origin oder ein externes CDN verweist.
4. **Isolierter Download-Pfad & Path-Traversal-Schutz:**
   - Downloads werden in einem isolierten Benutzerverzeichnis abgelegt (`$XDG_CACHE_HOME/ink-notes/updates` bzw. `~/.cache/ink-notes/updates`, Berechtigung `0700`).
   - Asset-Namen werden sanitisiert: Pfadtrenner (`/`, `\`), Pfadüberquerungen (`..`), Steuerzeichen und leere Dateinamen werden strikt abgewiesen.
5. **Atomarer AppImage-Tausch:**
   - Kein direktes Überschreiben laufender Programmdateien per `fs::copy` (verhindert den Linux-Kernel-Fehler *"Text file busy"*).
   - Die neue Datei wird temporär im selben Verzeichnis angelegt (`<name>.new.<pid>`), mit Ausführungsrechten (`0755`) versehen, das bestehende AppImage als `<name>.bak` gesichert (dessen Fehler nicht ignoriert werden) und anschließend per atomarem `fs::rename` ausgetauscht.

---

## ⚙️ CI-Runner einrichten (Gitea Actions / act_runner)

Der Veröffentlichungs-Workflow liegt unter [`.gitea/workflows/release.yaml`](.gitea/workflows/release.yaml).

### Runner-Label konfigurieren
Der Workflow nutzt standardmäßig das Runner-Label:
```yaml
runs-on: ubuntu-latest
```

In der Konfigurationsdatei Ihres Gitea `act_runner` (`config.yaml`) muss dieses Label auf ein passendes Container-Image oder den Host gemappt sein:
```yaml
runner:
  labels:
    - "ubuntu-latest:docker://catthehacker/ubuntu:act-latest"
    # oder für native Host-Ausführung:
    # - "ubuntu-latest:host"
```

### Diagnose & Fehlerbehebung
Bleibt ein Release-Job auf **"Waiting to run"** hängen:
1. **Runner online?** Prüfen Sie in Gitea unter *Einstellungen -> Aktionen -> Runner*, ob der `act_runner` aktiv und mit einem grünen Punkt als *Online* markiert ist.
2. **Label-Übereinstimmung:** Stimmen die im Runner registrierten Labels exakt mit dem `runs-on`-Feld in `.gitea/workflows/release.yaml` überein?
3. **Rechte:** Der Workflow prüft `if [ "$(id -u)" -ne 0 ]` und setzt `sudo` nur ein, wenn der Runner nicht als `root` läuft. Das verhindert Fehler sowohl in unprivilegierten Host-Umgebungen als auch in Root-Containern.

---

## 🚀 Schnellstart (Entwicklung)

### Voraussetzungen
- **Rust & Cargo:** Version 1.75 oder neuer (`rustup update stable`)
- **System-Bibliotheken (Linux):**
  - Vulkan-Treiber (z. B. `mesa-vulkan-drivers` oder proprietäre Treiber)
  - X11 / Wayland Bibliotheken (`libx11-dev`, `libxcursor-dev`, `libxrandr-dev`, `libxi-dev`, `libxkbcommon-dev`)

### Bauen & Ausführen

```bash
# Repository klonen
git clone https://gitea.pansi.eu/harald/ink-notes.git
cd ink-notes

# Kompilieren und starten via Starter-Skript
./run.sh
```

Alternativ direkt mit Cargo:

```bash
cargo run --release
```

---

## ⌨️ Tastatur- & Maus-Shortcuts

| Aktion | Tastenkürzel / Mausgeste |
| :--- | :--- |
| **Zeichnen** | Linke Maustaste / Stiftspitze |
| **Canvas verschieben (Pan)** | Mittlere Maustaste **oder** `Leertaste + Drag` |
| **Scrollen (durchgehende Ansicht)** | Mausrad (`Umschalt` = waagerecht) / ein Finger |
| **Zoom zentriert auf Cursor** | `Strg + Mausrad` / Touchpad-Pinch (Einzelseiten-Ansicht: Mausrad) |
| **Zoom zurücksetzen (100 %)** | `Strg + 0` |
| **Zoom In / Out** | `Strg + +` / `Strg + -` |
| **Auswahlwerkzeug (Lasso)** | `V` |
| **Text-Werkzeug** | `T`, dann ins Blatt klicken; `Esc` beendet die Eingabe |
| **Auswahl um 90° drehen** | `R` |
| **Kopieren & Einfügen** | `Strg + C` / `Strg + V` |
| **Auswahl löschen** | `Entf` oder `Backspace` |
| **Undo / Redo** | `Strg + Z` / `Strg + Y` (oder `Strg + Shift + Z`) |
| **Nächste Seite** | `Bild ab (PageDown)` oder `N` |
| **Vorherige Seite** | `Bild auf (PageUp)` |
| **Erste / Letzte Seite** | `Pos 1 (Home)` / `Ende (End)` |
| **Jetzt speichern (Disk Flush)** | `Strg + S` |
| **Vektor-PDF-Export** | `Strg + E` |
| **Dialoge / Menüs schließen** | `Esc` |
| **Beenden (mit Disk-Flush)** | `Strg + Q` (oder Fenster schließen) |

---

## 🏗️ Projekt- und Modul-Architektur

```text
src/
├── main.rs         # Event-Loop (winit), Input-Routing, App-Orchestrierung
├── lib.rs          # Bibliotheks-Exporte für Tests und Komponenten
├── canvas.rs       # wgpu Render-Pipeline, GPU-Buffer, Zeichenzustand
├── viewport.rs     # Kamera-Pan, Zoom, 2D-Transformationsmatrizen
├── shader.wgsl     # WGSL Vertex- & Fragment-Shader für Inking-Meshes
├── filter.rs       # One-Euro-Filter gegen Eingabe-Jitter
├── spline.rs       # Catmull-Rom, Corner Preservation & Dreiecksstreifen
├── geometry.rs     # RDP-Algorithmus, Least-Squares Circle Fit, Shape Recognition
├── selection.rs    # Lasso-Auswahl, Bounding-Box, Griffe, Rotation & Skalierung
├── background.rs   # Tessellierung für Kariert, Liniert, Punktraster
├── document.rs     # Notizbuch-Datenmodell, Seiten, DIN-A4-Koordinatensystem
├── history.rs      # Undo/Redo-Transaktionsstapel (Add, Erase, Transform)
├── storage.rs      # Atomares Laden/Speichern von Manifest & Seiten, Multi-Notebooks
├── autosave.rs     # Debounced Hintergrund-Autosave mit MPSC-Kanal
├── updater.rs      # In-App Gitea Updater (SemVer, HTTPS, SHA256-Integrität, Self-Update)
├── export.rs       # Skalierbarer Vektor-PDF-Export via printpdf
└── ui.rs           # Floating Glassmorphism-Toolbar, Dokumenten- & Update-Manager (egui)
```

Ausführliche Details zur Architektur und mathematischen Modellen findest du unter [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md).

---

## 🧪 Tests ausführen

Die gesamte Inking-Pipeline, Geometrieerkennung, Speicherlogik und der Updater sind mit umfassenden Unit-Tests abgesichert:

```bash
cargo test
```

Alle Unit-Tests prüfen Berechnungen, Hit-Testing, Undo/Redo, Persistenz, Updater-Sicherheit und Versionsvergleiche konsistent ab.

---

## 📄 Lizenz

Dieses Projekt ist unter der [MIT-Lizenz](LICENSE) lizenziert (Copyright © 2026 Harald Pansi).
