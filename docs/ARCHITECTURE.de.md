# Architektur von ink-notes

[English](ARCHITECTURE.md) · **Deutsch**

Dieses Dokument beschreibt, wie ink-notes intern aufgebaut ist: Eingabe und Strichverarbeitung, Rendering, Seiten und Ansicht, Formerkennung, Speichern und Export. Das Speicherformat steht in [`FILE_FORMAT.de.md`](FILE_FORMAT.de.md).

---

## 1. Überblick

ink-notes ist ein einzelnes Programm mit einem Event-Loop (`winit`) im Haupt-Thread. Dazu kommen drei Hintergrund-Threads:

| Thread | Aufgabe |
| :--- | :--- |
| Haupt-Thread | Eingaben, Datenmodell, Rendering (`wgpu`), Oberfläche (`egui`) |
| Autosave | Seiten und Manifest auf die Festplatte schreiben (`autosave.rs`) |
| PDF-Rendering | Importierte PDF-Seiten zu Texturen rendern (`pdf_doc.rs`, `hayro`) |
| Export | PDF/PNG/SVG-Export, damit die Oberfläche nicht hängt (`app.rs`) |

Stifteingaben kommen über `octotablet` (Tablet-Protokoll von Wayland bzw. X11), Maus und Touch über `winit`. Die Oberfläche (Werkzeugleiste, Menüs, Dialoge, Textbearbeitung) ist mit `egui` gebaut und wird im selben Render-Durchgang über das Blatt gezeichnet.

---

## 2. Vom Stift zum Strich

```
[Stift / Touch / Maus]
          │  Bildschirmposition, Druck, Zeit
          ▼
[Bildschirm → Seitenkoordinaten]   (viewport.rs)
          │
          ▼
[Druckkurve]                        (settings.rs, vom Nutzer einstellbar)
          │
          ▼
[1€-Filter]                         (filter.rs)
          │
          ▼
[Dreiecksstreifen wie beim fertigen Strich]   (spline.rs) → Live-Puffer auf der GPU
          │  Stift abgehoben
          ▼
[Nachbearbeitung]                   (spline.rs: finalize_stroke)
          │
          ▼
[Strich auf der Seite + Undo-Eintrag]   (document.rs, history.rs)
```

### 2.1 1€-Filter
Langsame Bewegungen zittern, schnelle sollen nicht nachhinken. Der 1€-Filter (`filter.rs`) passt seine Grenzfrequenz an die Geschwindigkeit an:

$$f_c = f_{c,\min} + \beta \cdot |\dot{x}|$$

Bei langsamer Bewegung glättet er stark, bei schneller kaum.

### 2.2 Zentripetaler Catmull-Rom-Spline
Die gefilterten Punkte werden mit einem zentripetalen Catmull-Rom-Spline ($\alpha = 0{,}5$) interpoliert:

$$t_{i+1} = t_i + \lVert P_{i+1} - P_i \rVert^{\alpha}$$

Diese Parametrisierung erzeugt bei engen Kurven keine Schlaufen oder Spitzen und ist an den Stützstellen $C^1$-stetig.

### 2.3 Dreiecksstreifen
Aus den Spline-Punkten mit Radius $r_i$ (aus Druck und Strichstärke) entsteht ein Dreiecksstreifen:

$$V_{i,\text{links}} = P_i + N_i \cdot r_i, \qquad V_{i,\text{rechts}} = P_i - N_i \cdot r_i$$

$N_i$ ist die Normale der Laufrichtung; an inneren Punkten wird über benachbarte Segmente gemittelt.

### 2.4 Zwei Stufen
1. **Während des Schreibens:** Der Filter arbeitet nur mit bisherigen Punkten; der Strich wird bei jedem neuen Punkt mit derselben Tessellierung wie beim Absetzen (ausdünnen, Ecken, runde Kappen) in einen eigenen Live-Puffer geschrieben, damit er beim Abheben nicht springt.
2. **Beim Absetzen** wird der ganze Strich noch einmal bearbeitet (`finalize_stroke`):
   - Druckverlauf mit dem Fenster $[0{,}25;\ 0{,}5;\ 0{,}25]$ glätten,
   - Punkte mit Ramer-Douglas-Peucker ausdünnen ($\varepsilon = 0{,}75$ Einheiten $= 0{,}075$ mm),
   - an Knicken über 65° den Spline auftrennen, damit Ecken spitz bleiben, und die Teile rund verbinden (ein Kreis um den Knick), damit die Spitze nicht abgeschnitten wirkt,
   - runde Endkappen anfügen.

   Danach wird der Live-Puffer geleert und der Strich in die Seite übernommen.

### 2.5 Lineal
Beginnt ein Strich nahe einer Linealkante (`ruler.rs`), werden alle seine Punkte auf diese Kante projiziert, bis der Stift abgehoben wird.

---

## 3. Koordinaten, Seiten und Ansicht

### 3.1 Seitenkoordinaten
Gerechnet wird in Seiteneinheiten: **10 Einheiten = 1 mm**. Eine DIN-A4-Seite ist 2100 × 2970 Einheiten groß. Jede Seite hat ihre eigene Größe (`Page::size`): Standard ist A4 hoch; beim PDF-Import bekommen Querformat-Seiten 2970 Einheiten Breite und eine Höhe nach Seitenverhältnis (`document::pdf_page_size`). Neue Seiten übernehmen die Größe der aktuellen.

### 3.2 Maßstab und Kamera
- `PageViewport` (`document.rs`) passt einen Rahmen, in den alle Seiten des Notizbuchs passen (`Notebook::fit_box`), ins Fenster ein. Daraus ergibt sich ein Maßstab (Bildschirmpixel je Einheit), der für alle Seiten gilt. Die aktuelle Seite liegt waagerecht zentriert und oben im Rahmen. Den Platz der Werkzeugleiste rechnet der Viewport heraus.
- `ViewportState` (`viewport.rs`) ergänzt Zoom (25 % bis 1000 %) und Verschiebung (Pan). Beim Zoomen bleibt der Punkt unter dem Mauszeiger stehen:

$$\vec{p}_{\text{neu}} = \vec{c} - (\vec{c} - \vec{p}_{\text{alt}}) \cdot \frac{z_{\text{neu}}}{z_{\text{alt}}}$$

- Im Shader wird jeder Punkt mit `transform_scale` und `transform_offset` (aus `CameraUniforms`) direkt in Gerätekoordinaten umgerechnet.

### 3.3 Durchgehende Ansicht
Ob ein Notizbuch Seite für Seite, durchgehend oder als endlose Fläche angezeigt wird, steht in seinem Manifest (`layout`, siehe [`FILE_FORMAT.de.md`](FILE_FORMAT.de.md)).

Alle Koordinaten beziehen sich auf die **aktuelle Seite**. `PageStack` (`viewport.rs`) legt die übrigen Seiten lückenlos darunter bzw. darüber (die Grenze zeichnet die Oberfläche als dunkle Linie), waagerecht zur aktuellen Seite zentriert. Jede sichtbare Nachbarseite wird mit einer eigenen Kamera gezeichnet, die um die Lage der Seite verschoben ist (höchstens 12 Nachbarseiten).

Nach dem Scrollen wird die Seite in der Fenstermitte zur aktuellen Seite; die Kamera wird dabei so verschoben, dass sich das Bild nicht bewegt. Während eines Strichs, einer Auswahl, beim Ziehen des Lineals oder beim Schreiben von Text wird nicht gewechselt. Wer auf einer sichtbaren Nachbarseite zu zeichnen beginnt, wechselt zuerst auf diese Seite.

Schnelles Wischen mit dem Finger gleitet nach (`Fling` in `viewport.rs`): Die Geschwindigkeit wird über die letzten 100 ms vor dem Abheben gemessen und nimmt danach exponentiell ab. Die Bildlaufleiste am Rand gegenüber der Werkzeugleiste springt direkt an eine Stelle im Notizbuch.

---

### 3.4 Endlose Fläche
Ein endloses Notizbuch hat eine einzige Seite von 10 × 10 m (`ENDLESS_PAGE_SIZE`). Damit bleibt alles wie bei jeder anderen Seite – Eingabe, Treffertests, Undo, Speichern –, nur der Rand ist praktisch unerreichbar. `PageViewport::calculate_endless` wählt Maßstab und Lage so, als läge eine DIN-A4-Seite am Startausschnitt (`ENDLESS_HOME`, ein A4-Rahmen in der Mitte); 100 % Zoom und „Ansicht zurücksetzen“ zeigen also diesen Rahmen. Bilder und Textfelder richten ihre Größe bzw. ihren Umbruch an DIN A4 aus (`Page::reference_size`, `Page::text_right_edge`).

Die Fläche wächst nur nach rechts und unten: Nach jedem Verschieben oder Zoomen hält `CanvasState::clamp_endless` die linke obere Ecke des sichtbaren Bereichs auf oder hinter `Page::endless_scroll_limit` – 10 mm links und oberhalb des Startausschnitts bzw. von Inhalt, der weiter oben oder links liegt (in Notizbüchern aus 0.29/0.30).

Beim Wechsel von und zu Seiten (`Notebook::make_endless`/`make_paged`) wird der Inhalt um `ENDLESS_HOME` verschoben und der Verlauf der Seite geleert. Zurück zu Seiten geht nur, solange alles im Startausschnitt liegt. Export und Bilder einer Auswahl rastern bzw. exportieren nur einen Ausschnitt (`Page::cropped`), beim Export auf das Musterraster gelegt (`Page::endless_export_bounds`).

## 4. Rendering

Ein Frame besteht aus einem Render-Durchgang auf die Fensterfläche (`canvas.rs`):

1. Für jede sichtbare Nachbarseite: Papier und Muster, Bilder/PDF-Seite/Text, Tinte.
2. Für die aktuelle Seite: Papier, Muster (Karo, Linien, Punkte), Bildebene, gespeicherte Striche, Live-Strich.
3. Die `egui`-Oberfläche: Werkzeugleiste, Menüs, Seitengrenzen, Auswahlrahmen und Griffe, Lineal, Textbearbeitung.

Jeder Strich ist ein eigener Draw-Call mit eigener Nummer im Stencil-Puffer: Ein Pixel, das der Strich schon gefärbt hat, wird kein zweites Mal gefärbt. So bleibt halbtransparente Tinte (Textmarker) gleichmäßig, auch wo sich der Strich selbst überlappt (Anfangskappe, Kehren, langsames Hin- und Herfahren); erst ein neuer Strich deckt stärker. Nach 255 Strichen setzt ein bildschirmfüllendes Dreieck den Stencil-Puffer zurück.

### 4.1 Bildebene
`image_layer.rs` zeichnet texturierte Vierecke unter der Tinte: importierte PDF-Seiten, eingefügte Bilder und Textfelder. Texturen werden beim ersten Bedarf geladen, bekommen Mipmaps und werden freigegeben, wenn mehr als 384 MB belegt sind.
- **PDF-Seiten** rendert ein Hintergrund-Thread mit `hayro` (längste Kante 2560 Pixel, beim starken Hineinzoomen 5120 Pixel). Bis die schärfere Fassung fertig ist, bleibt die vorhandene sichtbar.
- **Textfelder** werden mit `ab_glyph` gerastert, in einer Auflösung, die dem Zoom folgt (in Vierteln einer Zweierpotenz, höchstens 8 Pixel je Einheit). Der Texturschlüssel enthält einen Hash von Inhalt, Schrift und Farbe; jede Änderung erzeugt eine neue Textur.

### 4.2 Papier- und Musterfarben
Papierfarbe (`PaperTheme`) und Musterfarbe (`PatternColor`) stehen in `background.rs`; sie sind linear angegeben und werden im Shader nach sRGB umgerechnet. Tintenfarben (Striche, Text) sind sRGB-Werte – dieselben Zahlen landen im PDF und in den Farbpunkten der Oberfläche. Gezeichnet wird ohne sRGB-Umrechnung der GPU (`render_format`), Texturen sind `Rgba8Unorm`: So wird halbtransparente Tinte wie im PDF in sRGB gemischt und sieht am Bildschirm genauso aus wie im Export. Auf dunklem Papier kann der Shader dunkle Tinte aufhellen (`ink_params` in `CameraUniforms`), gespeichert werden immer die Originalfarben.

---

## 5. Objekte auf der Seite

Alles auf einer Seite ist ein `Stroke` (`document.rs`):
- **Tinte:** Abtastpunkte, Werkzeug (Stift, Textmarker, Form), Breite, Farbe; die Dreiecke werden beim Laden aus den Punkten neu berechnet.
- **Formen:** zusätzlich eine `RecognizedShape` (siehe Abschnitt 6).
- **Bilder und Textfelder:** vier Eckpunkte (oben links, oben rechts, unten rechts, unten links) plus Verweis auf die Bilddatei bzw. ein `TextBox`. Weil die Ecken normale Punkte sind, wirken Verschieben, Drehen und Skalieren der Auswahl ohne Sonderfälle.

### 5.1 Textfelder
`text.rs` setzt den Text selbst: Zeilenumbruch an Leerzeichen (zu lange Wörter zeichenweise), Unterschneidung, Tabulator = 4 Leerzeichen. Bildschirm-Textur, PDF-Export und Bearbeitung verwenden dieselben Schriften (Ubuntu Light und Hack aus den eingebauten `egui`-Schriften) und denselben Umbruch. Bearbeitet wird in einem `egui`-Textfeld an der Stelle des Felds; danach wird die Höhe des Felds an den Text angepasst, Lage, Drehung und Skalierung bleiben.

### 5.2 Rückgängig
Jede Seite hat einen eigenen Verlauf (`history.rs`): Hinzufügen, Entfernen, Ersetzen und Ändern von Strichen. Verschieben/Drehen/Skalieren einer Auswahl, ein Radierzug mit dem Teilstrich-Radierer und eine Textbearbeitung sind jeweils ein Schritt.

**Platz schaffen** (`Notebook::make_space`) betrifft mehrere Seiten: Die Seite, auf der angesetzt wurde, und die folgenden bis vor die nächste PDF-Seite gelten als durchgehende Spalte. Alles unterhalb der Ansatzhöhe rutscht um denselben Betrag; ein Objekt, das über ein Seitenende ragen würde, beginnt 10 mm unter dem Anfang der nächsten Seite, und alles danach rutscht entsprechend weiter. Fehlende Seiten werden eingefügt. Der Schritt (`SpaceShift`: jeder verschobene Strich vorher/nachher mit Seite) landet im Verlauf der Seite, auf der angesetzt wurde; `Notebook::undo`/`redo` buchen ihn über alle Seiten zurück (später geänderte Striche bleiben unberührt) und entfernen dafür angelegte, leer gebliebene Seiten wieder.

---

## 6. Formerkennung

`geometry.rs` erkennt Formen rein geometrisch, ohne Machine Learning. Zuerst wird auf ein Achsenkreuz geprüft (alle Teilstücke liegen auf einer waagerechten und einer senkrechten Linie, beide reichen auf beiden Seiten über den Schnittpunkt hinaus). Geschlossene Formen (Kreis, Ellipse, Dreieck, Rechteck, Raute) treten gegeneinander an; gewählt wird die, deren Umriss am besten zu den Punkten passt. Danach folgen Vielecke, Pfeile, Koordinatensysteme und Linien. Übernommen wird eine Form nur, wenn der Strich im Mittel höchstens 4 % seiner Diagonale von ihr abweicht – sonst bleibt er Handschrift.

- **Linie:** Verhältnis von Abstand Anfang–Ende zur Strichlänge über 0,92.
- **Kreis, Ellipse:** Kreis- bzw. Ellipsen-Fit über die Punkte.
- **Rechteck, Dreieck, Raute, Vieleck, Pfeil:** Analyse der mit Douglas-Peucker vereinfachten Kontur (Eckenzahl, Winkel, Seitenverhältnisse).
- **Koordinatensystem:** zwei etwa rechtwinklige Achsen; der Winkel wird auf 90° gesetzt.
- **Achsenkreuz:** ein „+“ in einem Zug; die Achsen werden waagerecht und senkrecht ausgerichtet und bekommen an allen vier Enden Pfeilspitzen.
- **Oktaeder:** Skizze einer Doppelpyramide als 3D-Kantenmodell; nur, wenn kein einfacher Umriss passt.

**Halten am Strichende:** Bleibt der Stift über 400 ms innerhalb von 3 Pixeln ruhig, wird die erkannte Form schon vor dem Absetzen angezeigt und beim Absetzen übernommen – in der Breite des gewählten Stifts bzw. Textmarkers; ein eingerasteter Textmarker bleibt Textmarker. Mit dem Formen-Werkzeug wird jeder Strich beim Absetzen geprüft.

---

## 7. Speichern

```
[Änderung]
    │  mark_page_dirty / mark_manifest_dirty
    ▼
[AutosaveController]  ── 1,5 s nach der letzten Änderung, spätestens nach 10 s ──►  [Autosave-Thread]
                                                                                        │
                                                                                        ▼
                                                                 [temporäre Datei schreiben + fsync]
                                                                                        │
                                                                                        ▼
                                                                 [rename über die alte Datei, Ordner-fsync]
```

- Der Haupt-Thread schreibt nie selbst; er übergibt fertige Seitendaten an den Autosave-Thread.
- Schlägt ein Schreibvorgang fehl, wird die Seite mit wachsendem Abstand (1 s bis 30 s) erneut versucht; der Status wird rot.
- `flush()` schreibt alles Ausstehende und wartet darauf. Es läuft bei `Strg + S`, beim Wechsel des Notizbuchs und beim Beenden. Schlägt es beim Beenden fehl, bleibt die App offen und bietet Wiederholen oder einen Notfall-Export der ungespeicherten Seiten an (`~/.local/share/ink-notes/recovery/`).
- Seiten werden erst beim Anzeigen geladen (Lazy Loading); Titel, Hintergrund und Größe stehen im Manifest, damit Seitenübersicht und durchgehende Ansicht sie vorher kennen.
- Ein zweites Fenster mit demselben Notizbuch öffnet es schreibgeschützt (Sperrdatei).

---

## 8. Export

`export.rs` erzeugt das PDF mit `printpdf`. Jede Seite bekommt ihre eigene Größe und diese Ebenen:

1. **Background:** Karo, Linien oder Punkte in der gewählten Musterfarbe (immer auf weißem Papier).
2. **PDF** (nur im Notfall): importierte PDF-Seite als Bild, falls sie sich nicht als Vektor übernehmen lässt.
3. **Images:** eingefügte Bilder (höchstens 300 dpi).
4. **Text:** Textfelder als echter Text mit eingebetteter Schrift (durchsuchbar, kopierbar). Jedes Feld liegt in einem eigenen Koordinatensystem, das eine Transformationsmatrix auf seine Ecken abbildet.
5. **Ink:** Striche als gefüllte Polygone, Formen als Pfade. Halbtransparente Striche (Textmarker) bekommen einen Grafikzustand mit Deckkraft (`ca`/`CA`); printpdf kann solche Zustände nicht anlegen, deshalb trägt `add_alpha_states` sie nach dem Speichern mit `lopdf` in die Seitenressourcen ein. Jeder Strich ist ein einziger Pfad und deckt sich daher nicht selbst doppelt. Der Umriss eines Strichs wird vor dem Export vereinfacht (`geometry::simplify_closed_outline`, höchstens 0,01 mm vom gezeichneten entfernt): Das Netz hat etwa 16 Punkte je Stiftpunkt, und mit allen dauerte der Export einer vollen Seite Sekunden. `INK_NOTES_EXACT_OUTLINES=1` exportiert wie bisher jeden Punkt.

Importierte PDF-Seiten werden anschließend als Form-XObject unter die Seite gelegt (`pdf_merge.rs`), bleiben also Vektorgrafik. Seitentitel werden zu Lesezeichen.

PNG und SVG einer Seite (`page_export.rs`) entstehen aus diesem PDF: `hayro` rastert es bzw. wandelt es in SVG um. So sehen alle Exporte gleich aus.

---

## 9. Updates

`updater.rs` fragt die Gitea-Releases ab, vergleicht die Version (SemVer) und lädt auf Wunsch das passende Paket. Vor der Installation prüft er die Minisign-Signatur von `SHA256SUMS.txt` gegen den eingebauten Schlüssel und danach die SHA-256-Prüfsumme des Pakets. Ein AppImage wird per `rename` ersetzt; ein `.deb`-Paket wird mit `xdg-open` an die Softwareinstallation des Systems übergeben. Details im [README](../README.de.md#sicherheit-der-updates) unter „Sicherheit der Updates“.
