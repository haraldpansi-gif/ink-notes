# Dateiformat & Speicherstruktur von ink-notes

Dieses Dokument beschreibt die Speicherstruktur und die JSON-Dateiformate, mit denen `ink-notes` Dokumente auf dem Dateisystem ablegt.

---

## 1. Verzeichnisstruktur

Notizbücher werden standardmäßig unter dem XDG-Datenpfad gespeichert:

```text
~/.local/share/ink-notes/
└── notebooks/
    ├── <notebook_id_1>/
    │   ├── manifest.json
    │   ├── page_1.json
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
  "id": "notebook-a1b2c3d4",
  "title": "Mathematik Vorlesung",
  "version": 1,
  "pages": [
    {
      "id": 1,
      "file_name": "page_1.json"
    },
    {
      "id": 2,
      "file_name": "page_2.json"
    }
  ],
  "current_page_index": 0,
  "next_page_id": 3,
  "next_stroke_id": 42,
  "created_at": 1775460000,
  "modified_at": 1775463600
}
```

### Felder:
- `id`: Eindeutige ID des Notizbuchs (z. B. UUID oder `"default"`).
- `title`: Vom Nutzer vergebener Anzeigename des Notizbuchs.
- `version`: Format-Versionsnummer (aktuell `1`).
- `pages`: Geordnete Liste von Seitenreferenzen.
- `current_page_index`: Index der zuletzt aktiven Seite für Session-Restore.
- `next_page_id`: Inkrementierender ID-Zähler für neue Seiten.
- `next_stroke_id`: Globaler ID-Zähler für neue Striche.
- `created_at` / `modified_at`: UNIX-Zeitstempel in Sekunden.

---

## 3. Seitendatei (`page_<id>.json`)

Jede Seite wird in einer eigenen Datei gespeichert, um Lazy-Loading und schnelles inkrementelles Speichern zu ermöglichen.

```json
{
  "id": 1,
  "background": "Grid5mm",
  "strokes": [
    {
      "id": 1,
      "color": [0.12, 0.12, 0.14, 1.0],
      "width_mm": 0.5,
      "samples": [
        {
          "x": 350.2,
          "y": 420.5,
          "pressure": 0.65,
          "timestamp": 12.345
        },
        {
          "x": 352.1,
          "y": 425.8,
          "pressure": 0.70,
          "timestamp": 12.355
        }
      ],
      "recognized_shape": null
    },
    {
      "id": 2,
      "color": [0.15, 0.35, 0.85, 1.0],
      "width_mm": 0.8,
      "samples": [],
      "recognized_shape": {
        "Rectangle": {
          "top_left": { "x": 500.0, "y": 600.0 },
          "width": 300.0,
          "height": 200.0
        }
      }
    }
  ]
}
```

### Felder:
- `id`: Eindeutige ID der Seite.
- `background`: Papiermuster (`Blank`, `Grid5mm`, `Lined`, `DotGrid`).
- `strokes`: Liste der auf der Seite gezeichneten Striche und Vektorformen.
  - `id`: Eindeutige ID des Strichs.
  - `color`: RGBA-Farbwerte im Bereich `[0.0, 1.0]`.
  - `width_mm`: Basis-Strichbreite in Millimetern.
  - `samples`: Rohabtastpunkte entlang des Pfads (`x`, `y` in DIN-A4-Einheiten, `pressure` in `[0.0, 1.0]`, `timestamp` in Sekunden).
  - `recognized_shape`: Optionales Enum für geometrische Vektorformen:
    - `Line { start, end }`
    - `Rectangle { top_left, width, height }`
    - `Circle { center, radius }`
    - `Axes { origin, x_end, y_end, has_arrows }`
    - `Triangle { p1, p2, p3 }`
    - `Rhombus { center, width, height }`
    - `Octahedron { center, radius }`

---

## 4. Atomares Schreibverfahren

Um Datenverlust bei Stromausfall oder plötzlichem Prozessabbruch zu verhindern:
1. Daten werden in `<ziel>.tmp` serialisiert und per `fsync` auf das Speichermedium geschrieben.
2. Mittels `std::fs::rename("<ziel>.tmp", "<ziel>")` wird die Zieldatei auf POSIX-Systemen atomar ersetzt.
