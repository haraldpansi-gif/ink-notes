# Dateiformat & Speicherstruktur von ink-notes

Dieses Dokument beschreibt die Speicherstruktur, die JSON-Dateiformate und die Versionshistorie, mit denen `ink-notes` Dokumente auf dem Dateisystem ablegt.

---

## 1. Verzeichnisstruktur

Notizbücher werden standardmäßig unter dem XDG-Datenpfad gespeichert:

```text
~/.local/share/ink-notes/
└── notebooks/
    ├── <notebook_id_1>/
    │   ├── manifest.json
    │   ├── manifest.json.v0.bak (optional, Migrations-Backup)
    │   ├── page_1.json
    │   ├── page_1.json.corrupt-1775460000 (optional, Quarantäne bei Parsefehlern)
    │   ├── page_2.json
    │   └── ...
    └── <notebook_id_2>/
        ├── manifest.json
        └── page_1.json
```

---

## 2. Dokumenten-Manifest (`manifest.json`)

Das Manifest enthält Metadaten über das gesamte Dokument sowie die Reihenfolge und IDs aller Seiten:

```json
{
  "format_version": 2,
  "id": "notebook-a1b2c3d4",
  "title": "Mathematik Vorlesung",
  "pages": [
    {
      "id": 1,
      "background": "Grid5mm"
    },
    {
      "id": 2,
      "background": "DotGrid"
    }
  ],
  "page_ids": [1, 2],
  "current_page_index": 0,
  "next_page_id": 3,
  "next_stroke_id": 42,
  "created_at": 1775460000,
  "modified_at": 1775463600
}
```

### Felder:
- `format_version`: Format-Versionsnummer (aktuell `2`, bei unversionierten Legacy-Dateien implizit `0`).
- `id`: Eindeutige ID des Notizbuchs (z. B. UUID oder `"default"`).
- `title`: Vom Nutzer vergebener Anzeigename des Notizbuchs.
- `pages`: Geordnete Liste von Seitenmetadaten (`id`, optional `background`).
- `page_ids`: Abwärtskompatible Liste der Seiten-IDs.
- `current_page_index`: Index der zuletzt aktiven Seite für Session-Restore.
- `next_page_id`: Inkrementierender ID-Zähler für neue Seiten.
- `next_stroke_id`: Globaler ID-Zähler für neue Striche.
- `created_at` / `modified_at`: UNIX-Zeitstempel in Sekunden.

---

## 3. Seitendatei (`page_<id>.json`)

Jede Seite wird in einer eigenen Datei gespeichert, um Lazy-Loading und schnelles inkrementelles Speichern zu ermöglichen.
Die Speicherung erfolgt ab Version 2 als kompaktes JSON (`serde_json::to_vec`), wodurch gegenüber Pretty-Printed JSON über 50 % Speicherplatz eingespart wird.

```json
{"format_version":2,"id":1,"background":"Grid5mm","strokes":[{"id":1,"color":[0.12,0.12,0.14,1.0],"samples":[{"x":350.2,"y":420.5,"p":0.65,"t":12.345},{"x":352.1,"y":425.8,"p":0.70,"t":12.355}],"shape":null,"tool":"pen","width":5.0,"finalized":true},{"id":2,"color":[0.15,0.35,0.85,1.0],"samples":[],"shape":{"Rectangle":{"top_left":{"x":500.0,"y":600.0},"width":300.0,"height":200.0}},"tool":"shape","width":20.0,"finalized":true}]}
```

### Felder:
- `format_version`: Format-Versionsnummer (`2`).
- `id`: Eindeutige ID der Seite.
- `background`: Papiermuster (`Blank`, `Grid5mm`, `Lined`, `DotGrid`).
- `strokes`: Liste der auf der Seite gezeichneten Striche und Vektorformen.
  - `id`: Eindeutige ID des Strichs.
  - `color`: RGBA-Farbwerte im Bereich `[0.0, 1.0]`.
  - `samples`: Rohabtastpunkte entlang des Pfads (`x`, `y` in DIN-A4-Einheiten, `p` Druckwert in `[0.0, 1.0]`, `t` Zeitstempel in Sekunden).
  - `shape`: Optionales Enum für geometrische Vektorformen:
    - `Line { start, end }`
    - `Rectangle { top_left, width, height }`
    - `Circle { center, radius }`
    - `Axes { origin, x_end, y_end, has_arrows }`
    - `Triangle { p1, p2, p3 }`
    - `Rhombus { center, width, height }`
    - `Octahedron { center, radius }`
  - `tool`: Zeichenwerkzeug (`"pen"`, `"highlighter"`, `"shape"`).
  - `width`: Strichbreite in internen DIN-A4-Einheiten (10 Einheiten = 1 mm; z. B. `5.0` für 0.5 mm Stift, `60.0` für 6.0 mm Marker).
  - `finalized`: Boolesches Flag für Striche, die das Zwei-Stufen-Inking (Eckenerkennung, Glättung, runde Endkappen) durchlaufen haben.

---

## 4. Datensicherheit & Migration

### Atomares Schreiben & Verzeichnis-Sync
1. Daten werden in `<ziel>.tmp.<pid>.<nanos>` serialisiert und per `fsync` auf das Speichermedium geschrieben (Dateirechte `0600` auf Unix).
2. Mittels `std::fs::rename("<ziel>.tmp", "<ziel>")` wird die Zieldatei auf POSIX-Systemen atomar ersetzt.
3. Nach dem Rename wird das übergeordnete Verzeichnis synchronisiert (`sync_all()`), um Persistenz bei Stromausfall zu garantieren. Verzeichnisse erhalten Berechtigung `0700`.

### Quarantäne bei Parsefehlern
Wird eine Seitendatei beim Laden als fehlerhaft erkannt (z. B. unvollständiges oder korruptes JSON), wird sie nicht stillschweigend gelöscht oder überschrieben, sondern nach `page_<id>.json.corrupt-<unix_ts>` verschoben. Die Ersatzseite wird als schreibgeschützt markiert.

### Migrations-Backups & Rolling Backups
Beim ersten Speichern einer Datei im Legacy-Format (Version 0) wird vor dem Überschreiben ein Backup `*.v0.bak` angelegt (z. B. `manifest.json.v0.bak`, `page_1.json.v0.bak`).
Zusätzlich existiert ein Rolling-Backup `manifest.json.bak`, auf das bei beschädigtem Manifest automatisch zurückgegriffen wird.

---

## 5. Versionshistorie

| Version | Status | Beschreibung |
|---|---|---|
| **0** | Legacy | Unversioniertes Format. `format_version` fehlt in JSON. Implizit als Version 0 interpretiert. |
| **1** | Überholt | Einführung von `format_version: u32` in `manifest.json` und `page_<id>.json`. Seiten-Hintergrund im Manifest gecached (`pages: Vec<PageMeta>`). Strenges Versionsmatching (neuere Versionen werden abgelehnt). Automatische Migration mit `*.v0.bak`. |
| **2** | **Aktuell** | Stroke-Treue: Ergänzung von `tool`, `width` und `finalized` in `StrokeData` für exakte Rekonstruktion aller Vertizes und Splines. Umstellung auf kompaktes JSON (`serde_json::to_vec`, ca. 54% Platzerparnis). |

---

## 6. Analyse & Bewertung: Binärformat (z. B. `postcard`)

Eine Umstellung auf ein Binärformat wie `postcard` wurde evaluiert:
- **Vorteile**:
  - Weitere 20–30 % Reduktion der Dateigröße gegenüber kompaktem JSON.
  - Minimal schnellere Serialisierung auf leistungsschwachen Systemen.
- **Nachteile**:
  - Verlust der Text-Transparenz (JSON lässt sich direkt inspizieren, mit `git`/Tools diffen und bei Korruption manuell reparieren).
  - Snapshotting und JSON-Verarbeitung liegen im Benchmark bereits bei **0,02 ms (1k Samples)** bis **0,70 ms (50k Samples)** – also weit unter dem Zielwert von 1 ms.
  - Kompaktes JSON spart bereits über 53 % Dateigröße ein.
- **Entscheidung**:
  Kompaktes JSON bietet aktuell das optimale Verhältnis aus Ausfallsicherheit, Benutzerfreundlichkeit, Performance und Portabilität. Ein Binärformat wie `postcard` bleibt als Option für spätere Riesen-Notizbücher (> 500 Seiten) denkbar, wird aktuell jedoch nicht eingeführt.
