# Architektur & Technische Dokumentation von ink-notes

Dieses Dokument beschreibt die interne Architektur, mathematische Modelle und Rendering-Pipelines von `ink-notes`.

---

## 1. Die Inking-Pipeline

```
[Hardware Stylus / Touch / Maus]
              │
              ▼
       [winit Event-Loop]
              │ (tx, ty, pressure, timestamp)
              ▼
   [Viewport Invertierung]  (screen_to_page: Umrechnung in DIN-A4-Einheiten)
              │
              ▼
       [1€-Filter]          (Jitter-Beseitigung bei langsamen Bewegungen)
              │
              ▼
 [Centripetal Catmull-Rom]  (C1-stetige Spline-Interpolation, alpha = 0.5)
              │
              ▼
   [Mesh-Tessellierung]     (Drucksensitive Dreiecksstreifen via Normalen)
              │
              ▼
       [wgpu GPU-Buffer]    (Vertex- & Index-Buffer im VRAM)
```

### 1.1. 1€-Filter (One-Euro-Filter)
Stift- und Touch-Eingaben unterliegen bei niedrigen Geschwindigkeiten oft Mikrozittern (Jitter). Standard-Tiefpassfilter verursachen jedoch bei schnellen Schwüngen eine störende Verzögerung (Latenz).

Der 1€-Filter (`filter.rs`) löst dieses Dilemma mit einer adaptiven Grenzfrequenz $f_c$:
$$f_c = f_{c,\min} + \beta \cdot |\dot{x}|$$
- Bei geringer Geschwindigkeit ($|\dot{x}| \approx 0$) dominiert $f_{c,\min}$, wodurch Zittern maximal geglättet wird.
- Bei schnellen Zügen wächst $\beta \cdot |\dot{x}|$ an, wodurch der Filter durchlässig wird und Latenz eliminiert.

### 1.2. Centripetal Catmull-Rom Splines
Um Ecken zu vermeiden und eine organische Schriftführung zu erzielen, werden Abtastpunkte mit einem zentripetalen Catmull-Rom-Spline (`spline.rs`) interpoliert ($\alpha = 0.5$):
$$t_{i+1} = t_i + ||P_{i+1} - P_i||^\alpha$$
Diese Parametrisierung garantiert:
- Keine Selbstüberschneidungen bei scharfen Wendungen.
- Keine unerwünschten Cusp-Schleifen.
- Perfekte $C^1$-Stetigkeit an den Stützstellen.

### 1.3. Drucksensitive Dreiecksstreifen (Tessellierung)
Aus den interpolierten Spline-Punkten mit Radius $r_i = f(\text{pressure}_i)$ wird ein Triangle-Strip erzeugt:
1. Richtungsvektor $T_i = P_{i+1} - P_i$.
2. Senkrechter Normalenvektor $N_i = (-T_{i,y}, T_{i,x}) / ||T_i||$.
3. An inneren Stützstellen werden Normalen benachbarter Segmente gemittelt (Winkelhalbierende).
4. Eckpunkte des Dreiecksstreifens:
   $$V_{i,\text{left}} = P_i + N_i \cdot r_i, \quad V_{i,\text{right}} = P_i - N_i \cdot r_i$$

---

## 2. Rendering & Kamera-Pipeline (`wgpu`)

### 2.1. Virtuelles Koordinatensystem
Die Anwendung rechnet intern in virtuellen DIN-A4-Koordinaten:
- Breite: **2100 Einheiten** (210 mm)
- Höhe: **2970 Einheiten** (297 mm)
- 1 mm entspricht exakt **10 Einheiten**.

### 2.2. Viewport & Projektionsmatrix (`viewport.rs`)
Der Vertex-Shader transformiert DIN-A4-Koordinaten über eine Orthogonal-Projektion kombiniert mit Pan und Zoom in Normalized Device Coordinates (NDC, $[-1, 1]$):

```wgsl
struct CameraUniform {
    transform: mat4x4<f32>,
};
```

Die Projektionsmatrix $M$ wird berechnet aus:
1. Skalierung des DIN-A4-Papiers auf den Viewport unter Berücksichtigung des Seitenverhältnisses.
2. Anwenden des Zoom-Faktors $z \in [0.25, 5.0]$.
3. Translation durch den Pan-Offset $(p_x, p_y)$.

### 2.3. Cursor-zentrierter Zoom
Beim Zoomen mit dem Mausrad wird der neue Pan-Offset so korrigiert, dass der Punkt unter dem Cursor fixiert bleibt:
$$\vec{p}_{\text{neu}} = \vec{c} - (\vec{c} - \vec{p}_{\text{alt}}) \cdot \frac{z_{\text{neu}}}{z_{\text{alt}}}$$

### 2.4. Render-Passes
In jedem Frame werden zwei Hauptpässe durchlaufen:
1. **Pass 1 (Canvas):**
   - Hintergrund-Quad und Linien/Gitter (Instanced oder Batch Vertex-Buffer).
   - Gespeicherte Striche und Geometrie-Meshes.
   - Aktiver Live-Strich (Ephemeral Buffer).
   - Auswahl-Overlay & Transformationsgriffe.
2. **Pass 2 (egui UI):**
   - Floating Glassmorphism-Toolbar.
   - Farb- und Werkzeug-Popovers.
   - Modal-Dokumentenmanager.

---

## 3. Geometrieerkennung & Shape-Fitting (`geometry.rs`)

Die Erkennung erfolgt rein analytisch und deterministisch ohne schwere ML-Frameworks:

### 3.1. Ramer-Douglas-Peucker (RDP)
Reduziert den Rohpfad auf signifikante Eckpunkte mit konfigurierbarem Toleranzabstand $\epsilon$.

### 3.2. Erkennungsregeln:
- **Gerade:** Verhältnis von Sehnenlänge ($||P_{\text{end}} - P_{\text{start}}||$) zur Pfad-Bogenlänge $> 0.92$.
- **Rechteck:** RDP liefert 4 dominante Ecken; Kantenwinkel liegen zwischen $75^\circ$ und $105^\circ$.
- **Kreis / Ellipse:** Pfad ist geschlossen ($||P_{\text{start}} - P_{\text{end}}|| < 0.1 \times \text{Umfang}$); Standardabweichung der Radien vom Mittelpunkt ist minimal ($< 0.12 \times \bar{r}$).
- **Koordinatensystem:** Zwei orthogonale Schenkel mit Schnittpunkt nahe dem Start/Ende; Winkel $80^\circ$ bis $100^\circ$ wird exakt auf $90^\circ$ eingerastet; positive Achsenenden erhalten Pfeilspitzen.
- **Dreiecke / Rhomben / Oktaeder:** Spezifische Eckpunkt- und Symmetrieanalysen der RDP-Kontur.

### 3.3. Draw-and-Hold Erkennung
Hält der Anwender den Stift am Ende eines Strichs für $> 400\text{ ms}$ innerhalb eines Radius von $< 3\text{ px}$ ruhig, wird die Form im fliegenden Wechsel als Vektorform visualisiert und beim Abheben des Stifts fixiert.

---

## 4. Persistenz & Autosave-Architektur (`storage.rs`, `autosave.rs`)

```
[UI / Inking Event]
       │
       ▼
 [mark_page_dirty]
       │
       ▼
[AutosaveController] ──(1.5s Debounce)──► [Hintergrund-Thread (MPSC)]
                                                   │
                                                   ▼
                                         [Temp-Datei *.tmp schreiben]
                                                   │
                                                   ▼
                                         [Atomares std::fs::rename]
                                                   │
                                                   ▼
                                         [manifest.json / page_<id>.json]
```

- **Keine UI-Blockierung:** Disk-I/O erfolgt vollständig in einem dedizierten Worker-Thread.
- **Atomarität:** Daten werden stets zuerst in `.json.tmp` geschrieben und via `fs::rename` atomar ersetzt.
- **Verlustfreier Notizbuchwechsel:** Bei `switch_notebook_dir` wird synchron `flush()` aufgerufen, um alle ungespeicherten Daten des aktuellen Dokuments sicherzustellen, bevor der Pfad gewechselt wird.
