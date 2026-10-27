# ink-notes file format

**English** · [Deutsch](FILE_FORMAT.de.md)

This document describes where and how ink-notes stores notebooks, and the versions of the file format. The current version is **9**.

---

## 1. Folders

```text
~/.local/share/ink-notes/              ($XDG_DATA_HOME/ink-notes)
├── notebooks/
│   ├── library.json                   notebook folders (see below)
│   ├── <notebook-id>/
│   │   ├── manifest.json              metadata and page order
│   │   ├── manifest.json.bak          copy of the previous manifest
│   │   ├── page_1.json                one file per page
│   │   ├── page_2.json
│   │   ├── page_7.json.corrupt-<ts>   damaged page set aside (if it ever happened)
│   │   ├── assets/                    inserted images and imported PDFs
│   │   │   ├── <sha256>.png
│   │   │   └── <sha256>.pdf
│   │   ├── .opened_by.json            device that has the notebook open (see below)
│   │   └── .lock                      lock file while the notebook is open
│   └── .trash/                        deleted notebooks (<id>_<timestamp>)
└── recovery/                          emergency backups of unsaved pages

~/.config/ink-notes/ui.json            ($XDG_CONFIG_HOME) interface settings
```

The folder `notebooks/` can be moved elsewhere in the settings (`storage_dir` in `ui.json`), e.g. to `~/Nextcloud/ink-notes`; `recovery/` stays where it is.

- **Notebook IDs** are built from the title and the time (e.g. `mathematik_1775460000`); the first notebook is called `default`.
- **Assets** are named after the SHA-256 hash of their content; the same image is stored only once. Images are stored as PNG or (unchanged) JPEG, PDFs unchanged.
- All files are created with permissions `0600`, folders with `0700`.

### Folders (`library.json`)
Folders such as "Semester 3 › Math" are purely logical: notebooks stay in their directories, and `library.json` only records which folder contains which notebook. Renaming, moving (with content) and deleting folders therefore never changes a notebook file.

```json
{
  "folders": ["3. Semester", "3. Semester/Mathe", "4. Semester"],
  "notebooks": { "analysis_1775460000": "3. Semester/Mathe" }
}
```

- Folders are paths with `/` as separator; names themselves contain no `/` and at most 80 characters.
- Notebooks without an entry are on the top level.
- When a folder is deleted, its notebooks and subfolders move up one level.
- If the file is missing or unreadable, all notebooks are on the top level; missing parent folders are added when loading.

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

| Field | Meaning |
| :--- | :--- |
| `format_version` | format version (see section 5) |
| `id`, `title` | ID (= folder name) and display name |
| `created_at`, `modified_at` | UNIX time in seconds |
| `current_page_index` | page opened last |
| `pages` | pages in their order: `id`, `background`, optional `title` and `size` (see below) – so the page overview and the view know them without loading every page |
| `page_ids` | the same order as a plain ID list (for older versions) |
| `next_stroke_id`, `next_page_id` | next free IDs |
| `layout` | page arrangement: `Paged` (page by page), `Continuous` (seamlessly below each other) or `Endless` (endless canvas, see below). Since 0.28; if the field is missing, the former global setting `continuous_scroll` from `ui.json` applies. Since 0.30, new notebooks are created as `Continuous` |

**Endless notebooks** (version 9) have exactly one page of size `[100000.0, 100000.0]` (10 × 10 m). The starting area is an A4 frame in its middle (from `[48950, 48515]`); coordinates stay positive. On export, only the used area plus a 10 mm margin becomes the PDF page.

---

## 3. Page (`page_<id>.json`)

Each page is stored in its own file (compact JSON, wrapped here for readability):

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

| Field | Meaning |
| :--- | :--- |
| `background` | pattern: `Blank`, `Grid5mm`, `Lined`, `DotGrid` |
| `title` | page title (optional) |
| `size` | page size `[width, height]` in page units (10 = 1 mm); omitted for A4 portrait (`[2100, 2970]`). At most 2 × 2 m, except for the endless page `[100000, 100000]` |
| `pdf_background` | imported PDF page as background (optional): file, page number starting at 0, size in points |
| `strokes` | all objects on the page, from bottom to top |

### 3.1 Objects (`strokes`)

Every object has an `id`, a `color` (RGBA, sRGB, values 0–1) and `samples`: points with `x`, `y` (page units), `p` (pressure 0–1) and `t` (time in seconds). The triangles for display are computed from the points when loading and are not stored.

**Ink**

```json
{ "id": 7, "color": [0.12, 0.12, 0.14, 1.0],
  "samples": [{ "x": 350.2, "y": 420.5, "p": 0.65, "t": 12.345 }, …],
  "tool": "pen", "width": 5.0, "finalized": true }
```

- `tool`: `pen`, `highlighter` or `shape` (a highlighter snapped to a shape stays `highlighter` and also has `shape`)
- `width`: stroke width in page units (5.0 = 0.5 mm)
- `finalized`: the stroke was post-processed after the pen was lifted (smoothing, corners, end caps)

**Shapes** additionally have `shape`, e.g.:

```json
"shape": { "Rectangle": { "corners": [[500, 600], [800, 600], [800, 800], [500, 800]] } }
```

Possible shapes: `Line { start, end }`, `Arrow { start, end, double }`, `Circle { center, radius }`, `Ellipse { center, radius_x, radius_y, rotation }`, `Rectangle { corners }`, `Triangle { corners }`, `Rhombus { corners }`, `Polygon { vertices }`, `CoordinateSystem { origin, x_axis_end, y_axis_end, with_arrows, negative_ends? }`, `Octahedron { … }`. Points are `[x, y]`. For an axis cross, `negative_ends` contains the ends of the x and y axes on the other side of the origin; older versions ignore the field and show an L-shaped coordinate system.

**Images** have `image` (file name in `assets/`); the four `samples` are the corners top left, top right, bottom right, bottom left:

```json
{ "id": 9, "color": [1, 1, 1, 1], "samples": [ …4 corners… ], "width": 0.0, "image": "<sha256>.png" }
```

**Text boxes** have `text`; like for images, the four `samples` are the corners of the box:

```json
"text": { "content": "Heading\nsecond line", "size": 42.3, "font": "Sans", "wrap_width": 1500.0 }
```

- `size`: font size (em) in page units (12 pt ≈ 42.3)
- `font`: `Sans` (Ubuntu Light) or `Mono` (Hack)
- `wrap_width`: wrap width in page units; the corners map the box of this width (and the height of the laid-out text), rotation and scaling are contained in the corners

---

## 4. Safe saving

### Atomic writes
1. The content is written to `<file>.tmp.<pid>.<nanoseconds>` and flushed to disk with `fsync`.
2. `rename` replaces the old file in one step.
3. Afterwards the folder is synced with `fsync` too.

A crash while saving therefore leaves either the old or the new file, never a half-written one.

### Damaged files
- If a page cannot be read, it is moved to `page_<id>.json.corrupt-<timestamp>` (not deleted) and the page is shown read-only.
- If `manifest.json` is damaged or missing, `manifest.json.bak` is used.

### Deleted pages
A deleted page disappears from `manifest.json`; its file `page_<id>.json` stays in the folder. Page IDs are never reused (`next_page_id`), so the file cannot collide with a new page. If the page is restored in the same session, the app writes it again.

### Syncing between devices
If the notebooks are kept in a folder synced by Nextcloud, Google Drive, Syncthing or similar:
- `.opened_by.json` (`{"device": "<host name>", "since": <unix time>}`) is written when a notebook is opened and removed when it is closed. A marker from another device leads to a warning; the lock file `.lock` only protects against a second instance on the same computer.
- While all local changes are saved, the app checks the folder every 3 seconds (and when the window gets the focus). Changed page files are reloaded, a changed `manifest.json` brings in new, removed or reordered pages and titles.
- Conflict copies of sync clients (`page_<id> (….json`, `page_<id>_….json`, `page_<id>.sync-conflict-….json`) are added as new pages after the original and renamed to `<name>.imported`.

### Newer versions
Files with a higher `format_version` than supported are not opened – this way an older app version cannot lose content it does not know.

### Migration
Before the first save of an unversioned file (version 0), a copy `*.v0.bak` is created.

---

## 5. Versions

| Version | App | Change |
| :--- | :--- | :--- |
| 0 | – | without `format_version` |
| 1 | – | `format_version`, backgrounds in the manifest |
| 2 | – | `tool`, `width`, `finalized` on strokes; compact JSON |
| 3 | 0.3.7 | shapes `Ellipse` and `Arrow` |
| 4 | 0.5 | images (`image`, folder `assets/`) |
| 5 | 0.6.1 | imported PDF pages (`pdf_background`) |
| 6 | 0.7 | page titles (`title` in page and manifest) |
| 7 | 0.10 | text boxes (`text`) |
| 8 | 0.11 | page size per page (`size` in page and manifest) |
| 9 | 0.29 | page arrangement per notebook (`layout` in the manifest), endless notebooks |

Each new version only adds things; older files are read without conversion and written with the current version number on the next save.

---

## 6. Why JSON

A binary format (e.g. `postcard`) would make files another 20–30 % smaller. JSON stays anyway, because it can be inspected, diffed and repaired by hand in an emergency, and saving a page takes well under a millisecond even with many strokes.
