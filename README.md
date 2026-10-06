# 🖋️ ink-notes

Eine latenzoptimierte, hardwarebeschleunigte Inking- und Notizbuch-Anwendung für Linux, entwickelt in **Rust** mit **wgpu** und **winit**.

`ink-notes` kombiniert minimale Eingabelatenz beim Zeichnen mit Stift, Touchpad oder Maus mit einem flüssigen stufenlosen Viewport, automatischer Geometrie- und Formerkennung, Objekttransformationen, Multi-Dokumenten-Verwaltung und Vektor-PDF-Export.

---

## ✨ Features im Überblick

### ⚡ Latenzoptimierte Inking-Pipeline
- **Hardware-Rendering:** Direct-to-Screen-Rendering mit `wgpu` (Vulkan / X11 / Wayland) unter Nutzung von `PresentMode::Mailbox` für minimale Eingabeverzögerung.
- **1€-Filter (One-Euro-Filter):** Dynamischer Tiefpassfilter zur Glättung von Zittern bei langsamen Bewegungen ohne spürbare Latenz bei schnellen Schwüngen.
- **Centripetal Catmull-Rom Splines:** Weiche C1-stetige Kurveninterpolation ohne Überschwinger oder Schleifen.
- **Drucksensitive Dreiecksstreifen-Tessellierung:** Rechnerische Generierung von Triangle-Meshes mit winkelhalbierenden Normalenvektoren entlang des Pfads.
- **Werkzeuge:**
  - **Stift (Pen):** Feine, variable Strichstärken (0.3 mm bis 1.2 mm) mit Notiz-Farben (Schwarz, Blau, Rot, Grün, Grau).
  - **Textmarker (Highlighter):** Breite, halbtransparente Striche (4.0 mm bis 8.0 mm) mit Multiplikations-Blending.
  - **Radierer (Eraser):** Segment- und abstandsbasierte Löschung einzelner Striche auf dem DIN-A4-Canvas.

### 🔍 Stufenloser Zoom & Pan (Kamera)
- **Viewport-Transformation:** 2D-Projektionsmatrix im WGSL-Vertex-Shader.
- **Cursor-zentrierter Zoom:** Stufenloses Zoomen von **25 % bis 500 %** zentriert auf die Cursor-Position via Mausrad, Trackpad-Pinch oder Tastenkürzel (`Strg +` / `Strg -` / `Strg 0`).
- **Pan:** Verschieben des Canvas per mittlerer Maustaste, `Leertaste + Drag` oder Trackpad-Pan.
- **Inverse Koordinatentransformation:** Präzise Umrechnung von Bildschirm- in DIN-A4-Dokumentkoordinaten (`screen_to_page`).

### 📐 Geometrie- & Objekterkennung (Shape Recognition)
- **Erkannte Primitiven:**
  - **Geraden (Line):** Sehnen- zu Pfadlängenverhältnis > 0.92.
  - **Rechtecke (Rectangle):** 4 orthogonale Ecken via Douglas-Peucker-Polygonzug (RDP).
  - **Kreise & Ellipsen (Circle/Ellipse):** Geschlossene Pfade mit minimaler Varianz via Least-Squares Circle Fit.
  - **Koordinatensysteme (Axes):** Zwei orthogonale Achsen (80°–100° gesnappt auf exakt 90°) mit optionalen Richtungspfeilen.
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
- **Dokumenten-Manager:** Dunkles Glassmorphism-Modal direkt in der Toolbar:
  - Neues Notizbuch anlegen.
  - Zwischen Notizbüchern mit einem Klick wechseln.
  - Dokumente direkt umbenennen (`✏`).
  - Dokumente löschen (`🗑`).
- **Verlustfreier Wechsel:** Beim Dokumentenwechsel sichert ein synchroner Disk-Flush alle anstehenden Änderungen garantiert ab.

### 📄 Seiten & Hintergründe
- **Virtuelles DIN-A4-Raster:** 2100 × 2970 Koordinateneinheiten (10 Einheiten = 1 mm).
- **Hintergrundmuster:**
  - Blanko
  - Kariert (5 mm Raster)
  - Liniert (7.5 mm Zeilenabstand)
  - Punktraster (5 mm Dot-Grid)
- **Seitenverwaltung:** Vor- und Zurückblättern, neue Seiten anfügen, Seitensprung-Menü.

### 💾 Autosave & Persistenz
- **Asynchroner Autosave-Controller:** Sichert Änderungen im Hintergrund-Thread nach 1.5 s Ruhezeit (Debounce), ohne das Zeichnen zu blockieren.
- **Atomares Schreiben:** Temp-Dateien mit atomarem Rename (`manifest.json.tmp` -> `manifest.json`) schützen vor Datenkorruption bei Abstürzen.
- **Sicherer Shutdown:** `ESC` oder Schließen des Fensters führt vor Programmende einen vollständigen synchronen Flush aus.

### 🖨️ Vektor-PDF-Export
- Exportiert das gesamte Dokument via `printpdf` nach `./export.pdf`.
- Striche und geometrische Formen werden als skalierbare Vektoren ausgegeben.

---

### 🚀 Release, Packaging & In-App Auto-Update (Zorin OS 18 / Ubuntu)
- **Natives .deb-Paket**: Optimiert für Zorin OS 18 (Ubuntu 24.04 LTS Noble-Basis) mit vollständiger XDG Desktop-Integration (`.desktop`, Hicolor-Icons SVG & 256x256 PNG, AppStream Metainfo).
- **Portables .AppImage**: Eigenständiges, distributionsunabhängiges Bundle (x86_64) mit integrierten Laufzeitabhängigkeiten.
- **In-App Gitea Updater (`src/updater.rs`)**:
  - Fragt im Hintergrund die Gitea Releases API (`https://gitea.pansi.eu/api/v1/repos/harald/ink-notes/releases/latest`) ab.
  - Zeigt bei neuen Versionen ein Update-Badge in der Toolbar an.
  - Release-Modal mit Versionsvergleich, Changelog und 1-Click-Download / In-Place-Aktualisierung.
- **Gitea Actions CI/CD (`.gitea/workflows/release.yaml`)**: Automatisches Paketieren und Veröffentlichen auf Gitea bei Git-Tags (`v*`).

---

## 📦 Paketierung & Installation (Zorin OS 18)

### 1. Release-Pakete bauen

```bash
# Baut sowohl .deb als auch .AppImage und berechnet SHA256SUMS.txt in target/
./scripts/build-release.sh
```

Ergebnis in `target/`:
- `ink-notes_0.1.0_amd64.deb` (~4.2 MB)
- `ink-notes_0.1.0_x86_64.AppImage` (~5.6 MB)
- `SHA256SUMS.txt`

### 2. Installation auf Zorin OS 18

```bash
# Natives Debian-Paket installieren
sudo dpkg -i target/ink-notes_0.1.0_amd64.deb

# Oder portables AppImage direkt starten
chmod +x target/ink-notes_0.1.0_x86_64.AppImage
./target/ink-notes_0.1.0_x86_64.AppImage
```

Nach der `.deb`-Installation ist `ink-notes` nahtlos im Zorin OS Anwendungsmenü unter *Büro* und *Grafik* integriert.

### 3. Release auf Gitea veröffentlichen

```bash
# Veröffentlicht die Artefakte auf https://gitea.pansi.eu/harald/ink-notes
./scripts/publish-gitea-release.sh <DEIN_GITEA_TOKEN>
```

---

## 🚀 Schnellstart (Entwicklung)

### Voraussetzungen
- **Rust & Cargo:** Version 1.75 oder neuer (`rustup update stable`)
- **System-Bibliotheken (Linux):**
  - Vulkan-Treiber (z. B. `mesa-vulkan-drivers` oder proprietäre Treiber)
  - X11 / XWayland Bibliotheken (`libx11-dev`, `libxcursor-dev`, `libxrandr-dev`, `libxi-dev`, `libxkbcommon-dev`)

### Bauen & Ausführen

```bash
# Repository klonen
git clone gitea@farmersdog.pansi.eu:harald/ink-notes.git
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
| **Zoom zentriert auf Cursor** | Mausrad / Touchpad-Pinch |
| **Zoom zurücksetzen (100 %)** | `Strg + 0` |
| **Zoom In / Out** | `Strg + +` / `Strg + -` |
| **Auswahlwerkzeug (Lasso)** | `V` |
| **Auswahl um 90° drehen** | `R` |
| **Kopieren & Einfügen** | `Strg + C` / `Strg + V` |
| **Auswahl löschen** | `Entf` oder `Backspace` |
| **Undo / Redo** | `Strg + Z` / `Strg + Y` (oder `Strg + Shift + Z`) |
| **Nächste Seite** | `Bild ab (PageDown)` oder `N` |
| **Vorherige Seite** | `Bild auf (PageUp)` |
| **Erste / Letzte Seite** | `Pos 1 (Home)` / `Ende (End)` |
| **PDF-Export** | `Strg + E` oder `Strg + S` |
| **Beenden (mit Disk-Flush)** | `Esc` oder `Q` |

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
├── spline.rs       # Centripetal Catmull-Rom & Dreiecksstreifen-Tessellierung
├── geometry.rs     # RDP-Algorithmus, Least-Squares Circle Fit, Shape Recognition
├── selection.rs    # Lasso-Auswahl, Bounding-Box, Griffe, Rotation & Skalierung
├── background.rs   # Tessellierung für Kariert, Liniert, Punktraster
├── document.rs     # Notizbuch-Datenmodell, Seiten, DIN-A4-Koordinatensystem
├── history.rs      # Undo/Redo-Transaktionsstapel (Add, Erase, Transform)
├── storage.rs      # Atomares Laden/Speichern von Manifest & Seiten, Multi-Notebooks
├── autosave.rs     # Debounced Hintergrund-Autosave mit MPSC-Kanal
├── updater.rs      # In-App Gitea Updater (SemVer, Release-API, Download, Self-Update)
├── export.rs       # Skalierbarer Vektor-PDF-Export via printpdf
└── ui.rs           # Floating Glassmorphism-Toolbar, Dokumenten- & Update-Manager (egui)
```

Ausführliche Details zur Architektur und mathematischen Modellen findest du unter [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md).

---

## 🧪 Tests ausführen

Die gesamte Inking-Pipeline, Geometrieerkennung, Speicherlogik und der Updater sind mit Unit-Tests abgesichert:

```bash
cargo test
```

Alle 61 Unit-Tests prüfen Berechnungen, Hit-Testing, Undo/Redo, Persistenz und Versionsvergleiche konsistent ab.

---

## 📄 Lizenz

MIT License / Open Source Inking Project for Linux.
