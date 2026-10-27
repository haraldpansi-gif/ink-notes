# Dateiformat von ink-notes

[English](FILE_FORMAT.md) · **Deutsch**

Dieses Dokument beschreibt, wo und wie ink-notes Notizbücher speichert, und die Versionen des Dateiformats. Aktuell ist **Version 9**.

---

## 1. Ordner

```text
~/.local/share/ink-notes/              ($XDG_DATA_HOME/ink-notes)
├── notebooks/
│   ├── library.json                   Ordner der Notizbücher (siehe unten)
│   ├── <notizbuch-id>/
│   │   ├── manifest.json              Metadaten und Seitenreihenfolge
│   │   ├── manifest.json.bak          Kopie des vorherigen Manifests
│   │   ├── page_1.json                eine Datei je Seite
│   │   ├── page_2.json
│   │   ├── page_7.json.corrupt-<ts>   beiseitegelegte beschädigte Seite (falls vorgekommen)
│   │   ├── assets/                    eingefügte Bilder und importierte PDFs
│   │   │   ├── <sha256>.png
│   │   │   └── <sha256>.pdf
│   │   ├── .opened_by.json            Gerät, auf dem das Notizbuch geöffnet ist (siehe unten)
│   │   └── .lock                      Sperrdatei, solange das Notizbuch geöffnet ist
│   └── .trash/                        gelöschte Notizbücher (<id>_<zeitstempel>)
└── recovery/                          Notfall-Sicherungen ungespeicherter Seiten

~/.config/ink-notes/ui.json            ($XDG_CONFIG_HOME) Einstellungen der Oberfläche
```

Unter Windows liegt dieselbe Struktur in `%APPDATA%\ink-notes` (Notizbücher, `recovery`, `ui.json`); heruntergeladene Updates landen in `%LOCALAPPDATA%\ink-notes`.

Der Ordner `notebooks/` lässt sich in den Einstellungen verlegen (`storage_dir` in `ui.json`), z. B. nach `~/Nextcloud/ink-notes`; `recovery/` bleibt, wo es ist.

- **Notizbuch-IDs** werden aus dem Titel und dem Zeitpunkt gebildet (z. B. `mathematik_1775460000`); das erste Notizbuch heißt `default`.
- **Assets** sind nach dem SHA-256-Hash ihres Inhalts benannt; dasselbe Bild wird nur einmal gespeichert. Bilder werden als PNG oder (unverändert) JPEG abgelegt, PDFs unverändert.
- Alle Dateien werden mit Rechten `0600`, Ordner mit `0700` angelegt.

### Ordner (`library.json`)
Ordner wie „3. Semester › Mathe“ sind rein logisch: Die Notizbücher bleiben in ihren Verzeichnissen, `library.json` hält nur fest, welcher Ordner welches Notizbuch enthält. Umbenennen, Verschieben (samt Inhalt) und Löschen von Ordnern ändern deshalb nie eine Notizbuch-Datei.

```json
{
  "folders": ["3. Semester", "3. Semester/Mathe", "4. Semester"],
  "notebooks": { "analysis_1775460000": "3. Semester/Mathe" }
}
```

- Ordner sind Pfade mit `/` als Trenner; Namen enthalten selbst kein `/` und höchstens 80 Zeichen.
- Notizbücher ohne Eintrag liegen auf der obersten Ebene.
- Wird ein Ordner gelöscht, rücken seine Notizbücher und Unterordner eine Ebene nach oben.
- Fehlt die Datei oder ist sie unlesbar, liegen alle Notizbücher auf der obersten Ebene; fehlende übergeordnete Ordner werden beim Laden ergänzt.

---

## 2. Manifest (`manifest.json`)

```json
{
  "format_version": 9,
  "id": "vorlesung_1775460000",
  "title": "Vorlesung",
  "created_at": 1775460000,
  "modified_at": 1775463600,
  "current_page_index": 0,
  "pages": [
    { "id": 1, "background": "Blank", "title": "Einführung", "size": [2970.0, 1671.0] },
    { "id": 2, "background": "Grid5mm" }
  ],
  "page_ids": [1, 2],
  "next_stroke_id": 42,
  "next_page_id": 3,
  "layout": "Paged"
}
```

| Feld | Bedeutung |
| :--- | :--- |
| `format_version` | Formatversion (siehe Abschnitt 5) |
| `id`, `title` | ID (= Ordnername) und Anzeigename |
| `created_at`, `modified_at` | UNIX-Zeit in Sekunden |
| `current_page_index` | zuletzt geöffnete Seite |
| `pages` | Seiten in ihrer Reihenfolge: `id`, `background`, optional `title` und `size` (siehe unten) – damit Seitenübersicht und Ansicht sie kennen, ohne jede Seite zu laden |
| `page_ids` | dieselbe Reihenfolge als reine ID-Liste (für ältere Versionen) |
| `next_stroke_id`, `next_page_id` | nächste freie IDs |
| `layout` | Anordnung der Seiten: `Paged` (Seite für Seite), `Continuous` (durchgehend untereinander) oder `Endless` (endlose Fläche, siehe unten). Seit 0.28; fehlt das Feld, gilt die frühere globale Einstellung `continuous_scroll` aus `ui.json`. Seit 0.30 werden neue Notizbücher als `Continuous` angelegt |

**Endlose Notizbücher** (Version 9) haben genau eine Seite der Größe `[100000.0, 100000.0]` (10 × 10 m). Der Startausschnitt ist ein DIN-A4-Rahmen in ihrer Mitte (ab `[48950, 48515]`); Koordinaten bleiben positiv. Beim Export wird nur der beschriebene Bereich mit 10 mm Rand zur PDF-Seite.

---

## 3. Seite (`page_<id>.json`)

Jede Seite liegt in einer eigenen Datei (kompaktes JSON, hier zur Lesbarkeit umbrochen):

```json
{
  "format_version": 8,
  "id": 1,
  "background": "Blank",
  "title": "Einführung",
  "size": [2970.0, 1671.0],
  "pdf_background": { "asset": "<sha256>.pdf", "page": 0, "width_pt": 960.0, "height_pt": 540.0 },
  "strokes": [ … ]
}
```

| Feld | Bedeutung |
| :--- | :--- |
| `background` | Muster: `Blank`, `Grid5mm`, `Lined`, `DotGrid` |
| `title` | Seitentitel (optional) |
| `size` | Seitengröße `[breite, höhe]` in Seiteneinheiten (10 = 1 mm); fehlt bei DIN A4 hoch (`[2100, 2970]`). Höchstens 2 × 2 m, außer der endlosen Seite `[100000, 100000]` |
| `pdf_background` | importierte PDF-Seite als Hintergrund (optional): Datei, Seitennummer ab 0, Größe in Punkt |
| `strokes` | alle Objekte der Seite, von unten nach oben |

### 3.1 Objekte (`strokes`)

Jedes Objekt hat eine `id`, eine `color` (RGBA, sRGB, Werte 0–1) und `samples`: Punkte mit `x`, `y` (Seiteneinheiten), `p` (Druck 0–1) und `t` (Zeit in Sekunden). Die Dreiecke für die Anzeige werden beim Laden aus den Punkten berechnet und nicht gespeichert.

**Tinte**

```json
{ "id": 7, "color": [0.12, 0.12, 0.14, 1.0],
  "samples": [{ "x": 350.2, "y": 420.5, "p": 0.65, "t": 12.345 }, …],
  "tool": "pen", "width": 5.0, "finalized": true }
```

- `tool`: `pen`, `highlighter` oder `shape` (ein zu einer Form eingerasteter Textmarker bleibt `highlighter` und hat zusätzlich `shape`)
- `width`: Strichbreite in Seiteneinheiten (5.0 = 0,5 mm)
- `finalized`: Der Strich wurde nach dem Absetzen nachbearbeitet (Glättung, Ecken, Endkappen)

**Formen** haben zusätzlich `shape`, z. B.:

```json
"shape": { "Rectangle": { "corners": [[500, 600], [800, 600], [800, 800], [500, 800]] } }
```

Mögliche Formen: `Line { start, end }`, `Arrow { start, end, double }`, `Circle { center, radius }`, `Ellipse { center, radius_x, radius_y, rotation }`, `Rectangle { corners }`, `Triangle { corners }`, `Rhombus { corners }`, `Polygon { vertices }`, `CoordinateSystem { origin, x_axis_end, y_axis_end, with_arrows, negative_ends? }`, `Octahedron { … }`. Punkte sind `[x, y]`. Beim Achsenkreuz enthält `negative_ends` die Enden der x- und der y-Achse auf der anderen Seite des Ursprungs; ältere Versionen ignorieren das Feld und zeigen ein L-förmiges Koordinatensystem.

**Bilder** haben `image` (Dateiname in `assets/`); die vier `samples` sind die Ecken oben links, oben rechts, unten rechts, unten links:

```json
{ "id": 9, "color": [1, 1, 1, 1], "samples": [ …4 Ecken… ], "width": 0.0, "image": "<sha256>.png" }
```

**Textfelder** haben `text`; die vier `samples` sind wie bei Bildern die Ecken des Felds:

```json
"text": { "content": "Überschrift\nzweite Zeile", "size": 42.3, "font": "Sans", "wrap_width": 1500.0 }
```

- `size`: Schriftgröße (Em) in Seiteneinheiten (12 pt ≈ 42,3)
- `font`: `Sans` (Ubuntu Light) oder `Mono` (Hack)
- `wrap_width`: Umbruchbreite in Seiteneinheiten; die Ecken bilden das Feld dieser Breite (und der Höhe des gesetzten Texts) ab, Drehung und Skalierung stecken in den Ecken

---

## 4. Sicherheit beim Speichern

### Atomares Schreiben
1. Der Inhalt wird in `<datei>.tmp.<pid>.<nanosekunden>` geschrieben und mit `fsync` auf den Datenträger gebracht.
2. `rename` ersetzt die alte Datei in einem Schritt.
3. Danach wird auch der Ordner mit `fsync` gesichert.

Ein Absturz während des Speicherns hinterlässt deshalb entweder die alte oder die neue Datei, nie eine halbe.

### Beschädigte Dateien
- Lässt sich eine Seite nicht lesen, wird sie nach `page_<id>.json.corrupt-<zeitstempel>` verschoben (nicht gelöscht) und die Seite schreibgeschützt angezeigt.
- Ist `manifest.json` beschädigt oder fehlt, wird `manifest.json.bak` verwendet.

### Gelöschte Seiten
Eine gelöschte Seite verschwindet aus `manifest.json`; ihre Datei `page_<id>.json` bleibt im Ordner liegen. Seitenkennungen werden nie wiederverwendet (`next_page_id`), die Datei kann also mit keiner neuen Seite kollidieren. Wird die Seite in derselben Sitzung wiederhergestellt, schreibt die App sie neu.

### Abgleich zwischen Geräten
Liegen die Notizbücher in einem Ordner, den Nextcloud, Google Drive, Syncthing o. Ä. abgleichen:
- `.opened_by.json` (`{"device": "<Rechnername>", "since": <Unix-Zeit>}`) wird beim Öffnen eines Notizbuchs geschrieben und beim Schließen entfernt. Eine Markierung eines anderen Geräts führt zu einer Warnung; die Sperrdatei `.lock` schützt nur vor einer zweiten Instanz auf demselben Rechner.
- Solange alle lokalen Änderungen gespeichert sind, prüft die App den Ordner alle 3 Sekunden (und wenn das Fenster den Fokus bekommt). Geänderte Seitendateien werden neu geladen, ein geändertes `manifest.json` bringt neue, entfernte oder umsortierte Seiten und Titel mit.
- Konfliktkopien der Sync-Clients (`page_<id> (….json`, `page_<id>_….json`, `page_<id>.sync-conflict-….json`) werden als neue Seiten hinter dem Original eingefügt und in `<name>.imported` umbenannt.

### Neuere Versionen
Dateien mit einer höheren `format_version` als der unterstützten werden nicht geöffnet – so kann eine ältere App-Version keine Inhalte verlieren, die sie nicht kennt.

### Migration
Beim ersten Speichern einer unversionierten Datei (Version 0) wird vorher eine Kopie `*.v0.bak` angelegt.

---

## 5. Versionen

| Version | App | Änderung |
| :--- | :--- | :--- |
| 0 | – | ohne `format_version` |
| 1 | – | `format_version`, Hintergründe im Manifest |
| 2 | – | `tool`, `width`, `finalized` an Strichen; kompaktes JSON |
| 3 | 0.3.7 | Formen `Ellipse` und `Arrow` |
| 4 | 0.5 | Bilder (`image`, Ordner `assets/`) |
| 5 | 0.6.1 | importierte PDF-Seiten (`pdf_background`) |
| 6 | 0.7 | Seitentitel (`title` in Seite und Manifest) |
| 7 | 0.10 | Textfelder (`text`) |
| 8 | 0.11 | Seitengröße je Seite (`size` in Seite und Manifest) |
| 9 | 0.29 | Seitenanordnung je Notizbuch (`layout` im Manifest), endlose Notizbücher |

Jede neue Version ändert nur Ergänzungen; ältere Dateien werden ohne Umwandlung gelesen und beim nächsten Speichern mit der aktuellen Versionsnummer geschrieben.

---

## 6. Warum JSON

Ein Binärformat (z. B. `postcard`) würde Dateien um weitere 20–30 % verkleinern. JSON bleibt trotzdem, weil es sich ansehen, vergleichen und im Notfall von Hand reparieren lässt und das Speichern einer Seite auch bei vielen Strichen deutlich unter einer Millisekunde dauert.
