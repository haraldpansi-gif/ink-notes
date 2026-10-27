# ink-notes architecture

**English** · [Deutsch](ARCHITECTURE.de.md)

This document describes how ink-notes is built internally: input and stroke processing, rendering, pages and view, shape recognition, saving and export. The storage format is described in [`FILE_FORMAT.md`](FILE_FORMAT.md).

---

## 1. Overview

ink-notes is a single program with an event loop (`winit`) on the main thread, plus three background threads:

| Thread | Task |
| :--- | :--- |
| Main thread | Input, data model, rendering (`wgpu`), interface (`egui`) |
| Autosave | Writes pages and manifest to disk (`autosave.rs`) |
| PDF rendering | Renders imported PDF pages to textures (`pdf_doc.rs`, `hayro`) |
| Export | PDF/PNG/SVG export, so the interface does not freeze (`app.rs`) |

Pen input arrives via `octotablet` (the Wayland or X11 tablet protocol), mouse and touch via `winit`. The interface (toolbar, menus, dialogs, text editing) is built with `egui` and drawn over the page in the same render pass.

---

## 2. From pen to stroke

```
[Pen / touch / mouse]
          │  screen position, pressure, time
          ▼
[Screen → page coordinates]         (viewport.rs)
          │
          ▼
[Pressure curve]                    (settings.rs, user-adjustable)
          │
          ▼
[1€ filter]                         (filter.rs)
          │
          ▼
[Triangle strip as for the final stroke]   (spline.rs) → live buffer on the GPU
          │  pen lifted
          ▼
[Post-processing]                   (spline.rs: finalize_stroke)
          │
          ▼
[Stroke on the page + undo entry]   (document.rs, history.rs)
```

### 2.1 1€ filter
Slow movements jitter, fast ones should not lag behind. The 1€ filter (`filter.rs`) adapts its cutoff frequency to the speed:

$$f_c = f_{c,\min} + \beta \cdot |\dot{x}|$$

It smooths strongly during slow movement and hardly at all during fast movement.

### 2.2 Centripetal Catmull-Rom spline
The filtered points are interpolated with a centripetal Catmull-Rom spline ($\alpha = 0.5$):

$$t_{i+1} = t_i + \lVert P_{i+1} - P_i \rVert^{\alpha}$$

This parametrization does not create loops or cusps in tight curves and is $C^1$-continuous at the control points.

### 2.3 Triangle strip
From the spline points with radius $r_i$ (from pressure and stroke width), a triangle strip is built:

$$V_{i,\text{left}} = P_i + N_i \cdot r_i, \qquad V_{i,\text{right}} = P_i - N_i \cdot r_i$$

$N_i$ is the normal of the direction of travel; at inner points it is averaged over neighboring segments.

### 2.4 Two stages
1. **While writing:** The filter only works with the points so far; with every new point the stroke is written into its own live buffer using the same tessellation as when the pen is lifted (thinning, corners, round caps), so it does not jump when the pen lifts.
2. **When the pen is lifted**, the whole stroke is processed once more (`finalize_stroke`):
   - smooth the pressure curve with the window $[0.25;\ 0.5;\ 0.25]$,
   - thin out points with Ramer-Douglas-Peucker ($\varepsilon = 0.75$ units $= 0.075$ mm),
   - split the spline at bends over 65° so corners stay sharp, and join the parts round (a circle around the bend) so the tip does not look cut off,
   - add round end caps.

   Then the live buffer is cleared and the stroke is added to the page.

### 2.5 Ruler
If a stroke starts near a ruler edge (`ruler.rs`), all its points are projected onto that edge until the pen is lifted.

---

## 3. Coordinates, pages and view

### 3.1 Page coordinates
Everything is computed in page units: **10 units = 1 mm**. An A4 page is 2100 × 2970 units. Each page has its own size (`Page::size`): the default is A4 portrait; on PDF import, landscape pages get a width of 2970 units and a height according to the aspect ratio (`document::pdf_page_size`). New pages take over the size of the current one.

### 3.2 Scale and camera
- `PageViewport` (`document.rs`) fits a box that holds all pages of the notebook (`Notebook::fit_box`) into the window. This yields one scale (screen pixels per unit) that applies to all pages. The current page is centered horizontally and placed at the top of the box. The viewport subtracts the space taken by the toolbar.
- `ViewportState` (`viewport.rs`) adds zoom (25 % to 1000 %) and panning. When zooming, the point under the mouse cursor stays in place:

$$\vec{p}_{\text{new}} = \vec{c} - (\vec{c} - \vec{p}_{\text{old}}) \cdot \frac{z_{\text{new}}}{z_{\text{old}}}$$

- In the shader, every point is converted directly to device coordinates with `transform_scale` and `transform_offset` (from `CameraUniforms`).

### 3.3 Continuous view
Whether a notebook is shown page by page, continuously or as an endless canvas is stored in its manifest (`layout`, see [`FILE_FORMAT.md`](FILE_FORMAT.md)).

All coordinates refer to the **current page**. `PageStack` (`viewport.rs`) places the other pages seamlessly below and above it (the interface draws the boundary as a dark line), centered horizontally on the current page. Each visible neighboring page is drawn with its own camera, shifted by the page's position (at most 12 neighboring pages).

After scrolling, the page in the middle of the window becomes the current page; the camera is shifted so that the picture does not move. There is no switch during a stroke, a selection, while dragging the ruler or while typing text. Starting to draw on a visible neighboring page switches to that page first.

A quick finger swipe keeps gliding (`Fling` in `viewport.rs`): the speed is measured over the last 100 ms before the finger lifts and then decays exponentially. The scrollbar on the edge opposite the toolbar jumps directly to a position in the notebook.

---

### 3.4 Endless canvas
An endless notebook has a single page of 10 × 10 m (`ENDLESS_PAGE_SIZE`). This keeps everything the same as with any other page – input, hit tests, undo, saving – only the edge is practically unreachable. `PageViewport::calculate_endless` chooses scale and position as if an A4 page lay at the starting area (`ENDLESS_HOME`, an A4 frame in the middle); 100 % zoom and "Reset view" therefore show this frame. Images and text boxes base their size and wrapping on A4 (`Page::reference_size`, `Page::text_right_edge`).

The canvas only grows to the right and down: after every pan or zoom, `CanvasState::clamp_endless` keeps the top left corner of the visible area at or beyond `Page::endless_scroll_limit` – 10 mm above and left of the starting area, or of content lying further up or left (in notebooks from 0.29/0.30).

When switching to and from pages (`Notebook::make_endless`/`make_paged`), the content is shifted by `ENDLESS_HOME` and the page's history is cleared. Switching back to pages is only possible while everything lies within the starting area. Export and images of a selection only rasterize or export a section (`Page::cropped`), aligned to the pattern grid on export (`Page::endless_export_bounds`).

## 4. Rendering

A frame consists of one render pass onto the window surface (`canvas.rs`):

1. For each visible neighboring page: paper and pattern, images/PDF page/text, ink.
2. For the current page: paper, pattern (squares, lines, dots), image layer, stored strokes, live stroke.
3. The `egui` interface: toolbar, menus, page boundaries, selection frame and handles, ruler, text editing.

Each stroke is its own draw call with its own number in the stencil buffer: a pixel the stroke has already colored is not colored a second time. This keeps semi-transparent ink (highlighter) even where the stroke overlaps itself (start cap, turns, slow back-and-forth); only a new stroke covers more strongly. After 255 strokes, a full-screen triangle resets the stencil buffer.

### 4.1 Image layer
`image_layer.rs` draws textured quads below the ink: imported PDF pages, inserted images and text boxes. Textures are loaded when first needed, get mipmaps, and are released when more than 384 MB are in use.
- **PDF pages** are rendered by a background thread with `hayro` (longest edge 2560 pixels, 5120 pixels when zoomed in strongly). Until the sharper version is ready, the existing one stays visible.
- **Text boxes** are rasterized with `ab_glyph` at a resolution that follows the zoom (in quarters of a power of two, at most 8 pixels per unit). The texture key contains a hash of content, font and color; every change creates a new texture.

### 4.2 Paper and pattern colors
Paper color (`PaperTheme`) and pattern color (`PatternColor`) are defined in `background.rs`; they are given in linear space and converted to sRGB in the shader. Ink colors (strokes, text) are sRGB values – the same numbers end up in the PDF and in the color dots of the interface. Drawing happens without the GPU's sRGB conversion (`render_format`), and textures are `Rgba8Unorm`: this way semi-transparent ink is blended in sRGB like in the PDF and looks the same on screen as in the export. On dark paper the shader can brighten dark ink (`ink_params` in `CameraUniforms`); the original colors are always what gets saved.

---

## 5. Objects on the page

Everything on a page is a `Stroke` (`document.rs`):
- **Ink:** sample points, tool (pen, highlighter, shape), width, color; the triangles are recomputed from the points when loading.
- **Shapes:** additionally a `RecognizedShape` (see section 6).
- **Images and text boxes:** four corner points (top left, top right, bottom right, bottom left) plus a reference to the image file or a `TextBox`. Because the corners are ordinary points, moving, rotating and scaling a selection work without special cases.

### 5.1 Text boxes
`text.rs` lays out the text itself: line wrapping at spaces (overlong words character by character), kerning, tab = 4 spaces. The screen texture, the PDF export and editing use the same fonts (Ubuntu Light and Hack from the fonts built into `egui`) and the same wrapping. Editing happens in an `egui` text field at the position of the box; afterwards the box height is adapted to the text, while position, rotation and scaling stay.

### 5.2 Undo
Each page has its own history (`history.rs`): adding, removing, replacing and modifying strokes. Moving/rotating/scaling a selection, one stroke of the partial-stroke eraser and one text edit are each a single step.

**Make space** (`Notebook::make_space`) affects several pages: the page where you started and the following ones up to the next PDF page count as one continuous column. Everything below the starting height moves by the same amount; an object that would extend past the end of a page starts 10 mm below the top of the next page, and everything after it moves further accordingly. Missing pages are inserted. The step (`SpaceShift`: every moved stroke before/after, with its page) goes into the history of the page where you started; `Notebook::undo`/`redo` apply it back across all pages (strokes changed later stay untouched) and remove pages that were created for it and stayed empty.

---

## 6. Shape recognition

`geometry.rs` recognizes shapes purely geometrically, without machine learning. It first checks for an axis cross (all segments lie on one horizontal and one vertical line, both extending past the intersection on both sides). Closed shapes (circle, ellipse, triangle, rectangle, rhombus) compete with each other; the one whose outline fits the points best wins. Then polygons, arrows, coordinate systems and lines follow. A shape is only accepted if the stroke deviates from it by at most 4 % of its diagonal on average – otherwise it stays handwriting.

- **Line:** ratio of start–end distance to stroke length above 0.92.
- **Circle, ellipse:** circle or ellipse fit through the points.
- **Rectangle, triangle, rhombus, polygon, arrow:** analysis of the contour simplified with Douglas-Peucker (number of corners, angles, aspect ratios).
- **Coordinate system:** two roughly perpendicular axes; the angle is set to 90°.
- **Axis cross:** a "+" in one stroke; the axes are aligned horizontally and vertically and get arrowheads at all four ends.
- **Octahedron:** sketch of a double pyramid as a 3D edge model; only if no simple outline fits.

**Holding at the end of a stroke:** If the pen stays within 3 pixels for more than 400 ms, the recognized shape is shown before lifting and taken over when the pen is lifted. It uses the width of the selected pen or highlighter; a snapped highlighter stays a highlighter. With the shape tool, every stroke is checked when the pen is lifted.

---

## 7. Saving

```
[Change]
    │  mark_page_dirty / mark_manifest_dirty
    ▼
[AutosaveController]  ── 1.5 s after the last change, at the latest after 10 s ──►  [Autosave thread]
                                                                                        │
                                                                                        ▼
                                                                 [write temporary file + fsync]
                                                                                        │
                                                                                        ▼
                                                                 [rename over the old file, folder fsync]
```

- The main thread never writes itself; it hands finished page data to the autosave thread.
- If a write fails, the page is retried with increasing delay (1 s to 30 s); the status turns red.
- `flush()` writes everything pending and waits for it. It runs on `Ctrl + S`, when switching notebooks and when quitting. If it fails when quitting, the app stays open and offers to retry or to make an emergency export of the unsaved pages (`~/.local/share/ink-notes/recovery/`).
- Pages are only loaded when shown (lazy loading); title, background and size are in the manifest, so the page overview and the continuous view know them in advance.
- A second window with the same notebook opens it read-only (lock file).

---

## 8. Export

`export.rs` creates the PDF with `pdf_writer.rs`, a small writer on top of `lopdf` (layers as optional content groups, embedded fonts with a ToUnicode map, images, bookmarks). Each page gets its own size and these layers:

1. **Background:** squares, lines or dots in the chosen pattern color (always on white paper).
2. **PDF** (fallback only): imported PDF page as an image, if it cannot be taken over as vectors.
3. **Images:** inserted images (at most 300 dpi).
4. **Text:** text boxes as real text with embedded font (searchable, copyable). Each box lives in its own coordinate system that a transformation matrix maps onto its corners.
5. **Ink:** strokes as filled polygons, shapes as paths. Semi-transparent strokes (highlighter) get a graphics state with opacity (`ca`/`CA`); `add_alpha_states` adds them to the page resources with `lopdf` after saving. Each stroke is a single path and therefore does not cover itself twice. The outline of a stroke is simplified before export (`geometry::simplify_closed_outline`, at most 0.01 mm away from the drawn one): the mesh has about 16 points per pen sample, and with all of them a full page took seconds to export. `INK_NOTES_EXACT_OUTLINES=1` exports every point as before.

Imported PDF pages are then placed under the page as a form XObject (`pdf_merge.rs`), so they stay vector graphics. Page titles become bookmarks.

PNG and SVG of a page (`page_export.rs`) are created from this PDF: `hayro` rasterizes it or converts it to SVG. This way all exports look the same.

---

## 9. Updates

`updater.rs` queries the Gitea releases, compares the version (SemVer) and downloads the matching package on request. Before installing, it verifies the Minisign signature of `SHA256SUMS.txt` against the built-in key and then the SHA-256 checksum of the package. An AppImage is replaced via `rename`; a `.deb` package is handed to the system's software installer with `xdg-open`. Details in the [README](../README.md#update-security) under "Update security".
