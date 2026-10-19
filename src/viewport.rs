//! Viewport- und Kameraverwaltung für stufenloses Zoomen und Pannen.
//!
//! Berechnet die affine Transformation zwischen Bildschirmkoordinaten (Window-Pixel),
//! virtuellen DIN-A4-Seitenkoordinaten ([0, 2100] x [0, 2970]) und dem GPU-NDC-Raum ([-1, 1]).

use crate::document::PageViewport;
use glam::{Mat4, Vec2, Vec4};

/// Minimaler und maximaler Zoomfaktor.
pub const MIN_ZOOM: f32 = 0.25;
pub const MAX_ZOOM: f32 = 10.0;

/// Höchstens so viele Nachbarseiten werden gleichzeitig dargestellt.
pub const MAX_NEIGHBOR_PAGES: usize = 12;

/// Durchgehende Ansicht: Die Seiten hängen lückenlos untereinander (die Grenze markiert eine
/// Linie der Oberfläche) und sind waagerecht zueinander zentriert. Lage jeder Seite (Ecke oben links) in Koordinaten der
/// aktuellen Seite.
#[derive(Debug, Clone, PartialEq)]
pub struct PageStack {
    pub origins: Vec<Vec2>,
    pub sizes: Vec<Vec2>,
    pub current: usize,
}

impl PageStack {
    /// Stapel aus den Seitengrößen; `current` ist die Bezugsseite.
    pub fn new(sizes: Vec<Vec2>, current: usize) -> Self {
        let current = current.min(sizes.len().saturating_sub(1));
        let width = sizes.get(current).map_or(0.0, |s| s.x);
        let mut origins = vec![Vec2::ZERO; sizes.len()];
        let x = |i: usize| (width - sizes[i].x) / 2.0;
        for i in current + 1..sizes.len() {
            let y = origins[i - 1].y + sizes[i - 1].y;
            origins[i] = Vec2::new(x(i), y);
        }
        for i in (0..current).rev() {
            let y = origins[i + 1].y - sizes[i].y;
            origins[i] = Vec2::new(x(i), y);
        }
        Self {
            origins,
            sizes,
            current,
        }
    }

    pub fn len(&self) -> usize {
        self.sizes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sizes.is_empty()
    }

    /// Seite, auf deren Höhe `y` liegt (an der Grenze die untere); `None` außerhalb.
    pub fn page_at(&self, y: f32) -> Option<usize> {
        // Seiten liegen nach Index sortiert untereinander
        let i = self.origins.partition_point(|o| o.y <= y).checked_sub(1)?;
        (y <= self.origins[i].y + self.sizes[i].y).then_some(i)
    }

    /// Seiten (außer der aktuellen), die zwischen den Höhen `top` und `bottom` sichtbar sind;
    /// sehr weit herausgezoomt nur die [`MAX_NEIGHBOR_PAGES`] nächstgelegenen.
    pub fn visible_neighbors(&self, top: f32, bottom: f32) -> Vec<usize> {
        if !(top.is_finite() && bottom.is_finite()) || bottom < top {
            return Vec::new();
        }
        let first = self
            .origins
            .iter()
            .zip(&self.sizes)
            .position(|(o, s)| o.y + s.y >= top);
        let Some(first) = first else {
            return Vec::new();
        };
        let mut pages: Vec<usize> = (first..self.len())
            .take_while(|&i| self.origins[i].y <= bottom)
            .filter(|&i| i != self.current)
            .collect();
        if pages.len() > MAX_NEIGHBOR_PAGES {
            pages.sort_by_key(|&i| i.abs_diff(self.current));
            pages.truncate(MAX_NEIGHBOR_PAGES);
            pages.sort_unstable();
        }
        pages
    }

    /// Kleinstes Rechteck um alle Seiten (min, max).
    pub fn bounds(&self) -> (Vec2, Vec2) {
        self.origins.iter().zip(&self.sizes).fold(
            (Vec2::splat(f32::INFINITY), Vec2::splat(f32::NEG_INFINITY)),
            |(min, max), (&o, &s)| (min.min(o), max.max(o + s)),
        )
    }
}

/// Verwaltet den Viewport-Kamerazustand für stufenlosen Zoom und Pan.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewportState {
    /// Zoomfaktor (1.0 = 100%, 0.25 bis 5.0).
    pub zoom: f32,
    /// Verschiebung (Pan) in virtuellen DIN-A4-Seitenkoordinaten.
    pub pan_offset: [f32; 2],
}

impl Default for ViewportState {
    fn default() -> Self {
        Self {
            zoom: 1.0,
            pan_offset: [0.0, 0.0],
        }
    }
}

impl ViewportState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Setzt Zoom und Pan auf die Ausgangsposition (100% zentriert) zurück.
    pub fn reset(&mut self) {
        self.zoom = 1.0;
        self.pan_offset = [0.0, 0.0];
    }

    /// Durchgehende Ansicht: Die Seite mit der Ecke `origin` (Koordinaten der bisherigen
    /// Bezugsseite) wird zur Bezugsseite, ohne dass sich der Bildausschnitt bewegt. Waagerecht
    /// zentriert [`PageViewport`] die neue Seite bereits (ungezoomt); der Pan gleicht nur den
    /// Zoomanteil aus.
    pub fn shift_reference_page(&mut self, origin: Vec2) {
        self.pan_offset[0] += origin.x * (1.0 - 1.0 / self.zoom.max(1e-6));
        self.pan_offset[1] += origin.y;
    }

    /// Durchgehende Ansicht: Verschiebt den Ausschnitt so, dass der Bildschirmpunkt `center`
    /// über dem Notizbuch liegt (zwischen Oberkante der ersten und Unterkante der letzten
    /// Seite, waagerecht innerhalb der Seitenbreite). `true`, wenn korrigiert wurde.
    pub fn clamp_continuous_center(
        &mut self,
        center: [f32; 2],
        stack: &PageStack,
        page_viewport: &PageViewport,
    ) -> bool {
        if stack.is_empty() {
            return false;
        }
        let c = self.screen_to_page_unclamped(center[0], center[1], page_viewport);
        let (min, max) = stack.bounds();
        let clamped = c.clamp(min, max.max(min));
        if (clamped - c).length() < 1e-3 {
            return false;
        }
        // Bildschirm = Viewport + (Seite + Pan) · Maßstab: der Pan gleicht die Differenz aus
        self.pan_offset[0] += c.x - clamped.x;
        self.pan_offset[1] += c.y - clamped.y;
        true
    }

    /// Passt den Pan-Offset um eine Bildschirmverschiebung (in Fensterpixeln) an.
    pub fn pan_by_screen_delta(
        &mut self,
        delta_x: f32,
        delta_y: f32,
        page_viewport: &PageViewport,
    ) {
        let base_scale = page_viewport.scale;
        if base_scale * self.zoom > 1e-6 {
            self.pan_offset[0] += delta_x / (base_scale * self.zoom);
            self.pan_offset[1] += delta_y / (base_scale * self.zoom);
        }
    }

    /// Zoomt zentriert auf eine Cursor-Position in Bildschirmkoordinaten.
    ///
    /// Formel: offset_neu = cursor - (cursor - offset_alt) * (zoom_neu / zoom_alt)
    pub fn zoom_at_cursor(
        &mut self,
        cursor_screen: [f32; 2],
        zoom_factor: f32,
        page_viewport: &PageViewport,
    ) {
        let old_zoom = self.zoom;
        let new_zoom = (self.zoom * zoom_factor).clamp(MIN_ZOOM, MAX_ZOOM);
        if (new_zoom - old_zoom).abs() < 1e-6 {
            return;
        }

        let base_scale = page_viewport.scale;

        // Bildschirmposition des Seitenursprungs (0, 0)
        let old_offset_x = page_viewport.page_x + self.pan_offset[0] * base_scale * old_zoom;
        let old_offset_y = page_viewport.page_y + self.pan_offset[1] * base_scale * old_zoom;

        let ratio = new_zoom / old_zoom;
        let new_offset_x = cursor_screen[0] - (cursor_screen[0] - old_offset_x) * ratio;
        let new_offset_y = cursor_screen[1] - (cursor_screen[1] - old_offset_y) * ratio;

        self.zoom = new_zoom;
        self.pan_offset[0] = (new_offset_x - page_viewport.page_x) / (base_scale * new_zoom);
        self.pan_offset[1] = (new_offset_y - page_viewport.page_y) / (base_scale * new_zoom);
    }

    /// Transformiert einen Fenster-Pixelpunkt invers in DIN-A4-Seitenkoordinaten (ungeklemmt).
    pub fn screen_to_page_unclamped(
        &self,
        screen_x: f32,
        screen_y: f32,
        page_viewport: &PageViewport,
    ) -> Vec2 {
        let base_scale = page_viewport.scale;
        let effective_scale = base_scale * self.zoom;
        if effective_scale < 1e-6 {
            return Vec2::ZERO;
        }

        let page_x = (screen_x - page_viewport.page_x) / effective_scale - self.pan_offset[0];
        let page_y = (screen_y - page_viewport.page_y) / effective_scale - self.pan_offset[1];

        Vec2::new(page_x, page_y)
    }

    /// Transformiert einen Fenster-Pixelpunkt invers in DIN-A4-Seitenkoordinaten.
    ///
    /// Liefert `None`, falls der Punkt außerhalb der virtuellen Seite liegt.
    pub fn screen_to_page(
        &self,
        screen_x: f32,
        screen_y: f32,
        page_viewport: &PageViewport,
    ) -> Option<Vec2> {
        let pt = self.screen_to_page_unclamped(screen_x, screen_y, page_viewport);
        let size = page_viewport.page_size;
        if (0.0..=size.x).contains(&pt.x) && (0.0..=size.y).contains(&pt.y) {
            Some(pt)
        } else {
            None
        }
    }

    /// Transformiert einen Fenster-Pixelpunkt invers in DIN-A4-Seitenkoordinaten und klemmt auf die Seite.
    pub fn screen_to_page_clamped(
        &self,
        screen_x: f32,
        screen_y: f32,
        page_viewport: &PageViewport,
    ) -> Vec2 {
        let pt = self.screen_to_page_unclamped(screen_x, screen_y, page_viewport);
        pt.clamp(Vec2::ZERO, page_viewport.page_size)
    }

    /// Transformiert einen DIN-A4-Seitenpunkt in Fenster-Pixelkoordinaten.
    pub fn page_to_screen_vec2(&self, page_pt: Vec2, page_viewport: &PageViewport) -> [f32; 2] {
        self.page_to_screen(page_pt.x, page_pt.y, page_viewport)
    }

    /// Transformiert einen DIN-A4-Seitenkoordinatenpunkt in Fenster-Bildschirmkoordinaten.
    pub fn page_to_screen(
        &self,
        page_x: f32,
        page_y: f32,
        page_viewport: &PageViewport,
    ) -> [f32; 2] {
        let base_scale = page_viewport.scale;
        let effective_scale = base_scale * self.zoom;
        let screen_x = page_viewport.page_x + (page_x + self.pan_offset[0]) * effective_scale;
        let screen_y = page_viewport.page_y + (page_y + self.pan_offset[1]) * effective_scale;
        [screen_x, screen_y]
    }

    /// Berechnet die Shader-Transformationsparameter (Scale, Offset) für den Vertex-Shader,
    /// um DIN-A4-Seitenkoordinaten direkt in den GPU-NDC-Bereich [-1.0, 1.0] abzubilden.
    pub fn ndc_transform(&self, page_viewport: &PageViewport) -> ([f32; 2], [f32; 2]) {
        let base_scale = page_viewport.scale;
        let effective_scale = base_scale * self.zoom;

        // ndc_x = (screen_x / window_width) * 2.0 - 1.0
        // screen_x = page_viewport.page_x + (page_x + pan_offset[0]) * effective_scale
        //          = page_x * effective_scale + (page_viewport.page_x + pan_offset[0] * effective_scale)
        let scale_x = (2.0 * effective_scale) / page_viewport.window_width;
        let offset_x = (2.0 * (page_viewport.page_x + self.pan_offset[0] * effective_scale))
            / page_viewport.window_width
            - 1.0;

        // ndc_y = 1.0 - (screen_y / window_height) * 2.0
        // screen_y = page_viewport.page_y + (page_y + pan_offset[1]) * effective_scale
        //          = page_y * effective_scale + (page_viewport.page_y + pan_offset[1] * effective_scale)
        let scale_y = (-2.0 * effective_scale) / page_viewport.window_height;
        let offset_y = 1.0
            - (2.0 * (page_viewport.page_y + self.pan_offset[1] * effective_scale))
                / page_viewport.window_height;

        ([scale_x, scale_y], [offset_x, offset_y])
    }

    /// Berechnet die 2D-Projektionsmatrix (Ortho-Projektion kombiniert mit Pan und Zoom).
    pub fn projection_matrix(&self, page_viewport: &PageViewport) -> Mat4 {
        let (scale, offset) = self.ndc_transform(page_viewport);
        Mat4::from_cols(
            Vec4::new(scale[0], 0.0, 0.0, 0.0),
            Vec4::new(0.0, scale[1], 0.0, 0.0),
            Vec4::new(0.0, 0.0, 1.0, 0.0),
            Vec4::new(offset[0], offset[1], 0.0, 1.0),
        )
    }
}

/// Schwung beim Scrollen mit dem Finger (Issue #28): Nach dem Abheben gleitet der
/// Ausschnitt mit der zuletzt gewischten Geschwindigkeit weiter und bremst langsam ab.
/// Geschwindigkeiten in Bildschirmpixeln je Sekunde.
#[derive(Debug, Clone, Copy, Default)]
pub struct Fling {
    velocity: Vec2,
    /// Zeit der letzten Fingerbewegung bzw. des letzten Gleitschritts
    last: Option<std::time::Instant>,
    active: bool,
}

impl Fling {
    /// Abbremsen je Sekunde (Anteil der Geschwindigkeit, der nach 1 s bleibt: e^-FRICTION)
    const FRICTION: f32 = 2.5;
    /// Langsamer wischen gleitet nicht nach
    const MIN_START_SPEED: f32 = 400.0;
    /// Darunter hört das Gleiten auf
    const MIN_SPEED: f32 = 40.0;
    /// Liegt der Finger vor dem Abheben so lange still, gleitet nichts nach
    const MAX_REST: std::time::Duration = std::time::Duration::from_millis(80);

    /// Finger hat sich um `delta` verschoben.
    pub fn track(&mut self, delta: Vec2, now: std::time::Instant) {
        self.active = false;
        let dt = self
            .last
            .map_or(0.0, |last| now.duration_since(last).as_secs_f32());
        self.last = Some(now);
        if dt <= 0.0 {
            return;
        }
        // Geglättet: einzelne ungleichmäßige Touch-Ereignisse sollen nicht zählen
        let sample = delta / dt.max(0.004);
        self.velocity = self.velocity.lerp(sample, 0.4);
    }

    /// Finger abgehoben: gleitet weiter, wenn zuletzt schnell genug gewischt wurde.
    pub fn release(&mut self, now: std::time::Instant) {
        let resting = self
            .last
            .is_none_or(|last| now.duration_since(last) > Self::MAX_REST);
        self.active = !resting && self.velocity.length() >= Self::MIN_START_SPEED;
        self.last = Some(now);
        if !self.active {
            self.stop();
        }
    }

    /// Hält an (neue Berührung, Mausklick, Mausrad).
    pub fn stop(&mut self) {
        *self = Self::default();
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Verschiebung (Bildschirmpixel) seit dem letzten Schritt; `None`, wenn nichts gleitet.
    pub fn step(&mut self, now: std::time::Instant) -> Option<Vec2> {
        if !self.active {
            return None;
        }
        let dt = self
            .last
            .map_or(0.0, |last| now.duration_since(last).as_secs_f32())
            .min(0.1);
        self.last = Some(now);
        // Exakt integriert, damit die Strecke nicht von der Bildrate abhängt
        let decay = (-Self::FRICTION * dt).exp();
        let delta = self.velocity * (1.0 - decay) / Self::FRICTION;
        self.velocity *= decay;
        if self.velocity.length() < Self::MIN_SPEED {
            self.stop();
        }
        Some(delta)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A4: Vec2 = crate::document::A4_SIZE;
    const PITCH: f32 = A4.y;

    fn a4_stack(count: usize, current: usize) -> PageStack {
        PageStack::new(vec![A4; count], current)
    }

    #[test]
    fn test_fling_glides_and_slows_down() {
        use std::time::{Duration, Instant};
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        let mut fling = Fling::default();
        // Schnell nach oben wischen: 30 px alle 10 ms = 3000 px/s
        for i in 0..=10 {
            fling.track(Vec2::new(0.0, -30.0), ms(i * 10));
        }
        fling.release(ms(105));
        assert!(fling.is_active());
        let first = fling.step(ms(121)).unwrap();
        assert!(first.y < -20.0, "{first:?}");
        let mut total = first;
        let mut t = 121;
        while let Some(d) = fling.step(ms(t)) {
            total += d;
            t += 16;
            assert!(t < 10_000, "gleitet endlos");
        }
        // Gleitet ungefähr v / FRICTION = 1200 px weiter, dann ist Schluss
        assert!(total.y < -800.0 && total.y > -1300.0, "{total:?}");
        assert!(!fling.is_active());

        // Langsam schieben oder vor dem Abheben stillhalten: kein Nachgleiten
        let mut slow = Fling::default();
        for i in 0..=10 {
            slow.track(Vec2::new(0.0, -2.0), ms(i * 10));
        }
        slow.release(ms(105));
        assert!(!slow.is_active());
        let mut rested = Fling::default();
        for i in 0..=10 {
            rested.track(Vec2::new(0.0, -30.0), ms(i * 10));
        }
        rested.release(ms(400));
        assert!(!rested.is_active());
    }

    #[test]
    fn test_continuous_layout() {
        let stack = a4_stack(5, 1);
        assert_eq!(stack.page_at(100.0), Some(1));
        // Lückenlos: direkt unter dem Seitenende beginnt die nächste Seite
        assert_eq!(stack.page_at(A4.y - 1.0), Some(1));
        assert_eq!(stack.page_at(A4.y + 1.0), Some(2));
        assert_eq!(stack.page_at(PITCH + 5.0), Some(2));
        assert_eq!(stack.page_at(-5.0), Some(0));
        assert_eq!(stack.page_at(-PITCH - 5.0), None, "vor der ersten Seite");

        // Ende von Seite 2 und Anfang von Seite 3 sichtbar (aktuell: Seite 2 von 5)
        assert_eq!(
            stack.visible_neighbors(A4.y - 500.0, PITCH + 500.0),
            vec![2]
        );
        // Ganz oben im Notizbuch: keine Seite vor der ersten
        assert!(a4_stack(5, 0)
            .visible_neighbors(-PITCH * 3.0, 500.0)
            .is_empty());
        // Hinter der letzten Seite: keine weiteren
        assert_eq!(
            a4_stack(5, 3).visible_neighbors(-PITCH + 1.0, PITCH * 4.0),
            vec![2, 4]
        );
        // Weit herausgezoomt: auf die nächstgelegenen Seiten begrenzt
        let many = a4_stack(200, 50).visible_neighbors(-PITCH * 50.0, PITCH * 50.0);
        assert_eq!(many.len(), MAX_NEIGHBOR_PAGES);
        assert!(many.iter().all(|i| i.abs_diff(50) <= 6));
    }

    #[test]
    fn test_stack_of_different_page_sizes() {
        let slide = Vec2::new(2970.0, 1671.0);
        let stack = PageStack::new(vec![slide, A4, slide], 1);
        // Folien waagerecht zur A4-Seite zentriert, direkt darüber und darunter
        assert_eq!(stack.origins[0], Vec2::new(-435.0, -slide.y));
        assert_eq!(stack.origins[2], Vec2::new(-435.0, A4.y));
        assert_eq!(stack.page_at(A4.y + 10.0), Some(2));
        let (min, max) = stack.bounds();
        assert_eq!(min, Vec2::new(-435.0, -slide.y));
        assert_eq!(max, Vec2::new(2535.0, A4.y + slide.y));
    }

    #[test]
    fn test_shift_reference_page_keeps_view() {
        let mut vp = ViewportState::new();
        let page_vp = PageViewport::calculate(1920.0, 1080.0, 24.0);
        vp.pan_by_screen_delta(0.0, -900.0, &page_vp);
        // Punkt oben auf der nächsten Seite, in Koordinaten der aktuellen Seite
        let before = vp.page_to_screen(300.0, PITCH + 200.0, &page_vp);
        vp.shift_reference_page(a4_stack(2, 0).origins[1]);
        let after = vp.page_to_screen(300.0, 200.0, &page_vp);
        assert!((before[0] - after[0]).abs() < 0.01 && (before[1] - after[1]).abs() < 0.01);
    }

    #[test]
    fn test_shift_to_page_of_other_size_keeps_view_when_zoomed() {
        let slide = Vec2::new(2970.0, 1671.0);
        let fit = Vec2::new(2970.0, 2970.0);
        let view_a4 = PageViewport::calculate_for(1600.0, 900.0, 20.0, (0.0, 0.0), fit, A4);
        let view_slide = PageViewport::calculate_for(1600.0, 900.0, 20.0, (0.0, 0.0), fit, slide);
        let stack = PageStack::new(vec![A4, slide], 0);
        let mut vp = ViewportState::new();
        vp.zoom_at_cursor([500.0, 300.0], 2.5, &view_a4);
        vp.pan_by_screen_delta(-40.0, -700.0, &view_a4);
        let point = Vec2::new(900.0, 300.0);
        let before = vp.page_to_screen_vec2(stack.origins[1] + point, &view_a4);
        vp.shift_reference_page(stack.origins[1]);
        let after = vp.page_to_screen_vec2(point, &view_slide);
        assert!((before[0] - after[0]).abs() < 0.01 && (before[1] - after[1]).abs() < 0.01);
    }

    #[test]
    fn test_continuous_scroll_stays_on_the_notebook() {
        let page_vp = PageViewport::calculate(1280.0, 850.0, 24.0);
        let center = [640.0, 425.0];
        let stack = a4_stack(3, 1);
        let mut vp = ViewportState::new();
        // Mitte der Seite: nichts zu korrigieren
        assert!(!vp.clamp_continuous_center(center, &stack, &page_vp));
        // Weit über die erste Seite hinaus nach oben gescrollt (aktuell: Seite 2 von 3)
        vp.pan_by_screen_delta(0.0, 5000.0, &page_vp);
        assert!(vp.clamp_continuous_center(center, &stack, &page_vp));
        let c = vp.screen_to_page_unclamped(center[0], center[1], &page_vp);
        assert!(
            (c.y + PITCH).abs() < 0.1,
            "Oberkante der ersten Seite: {c:?}"
        );
        // Weit nach unten: Unterkante der letzten Seite
        vp.pan_by_screen_delta(-3000.0, -20000.0, &page_vp);
        assert!(vp.clamp_continuous_center(center, &stack, &page_vp));
        let c = vp.screen_to_page_unclamped(center[0], center[1], &page_vp);
        assert!((c.y - (PITCH + A4.y)).abs() < 0.1, "{c:?}");
        assert!((c.x - A4.x).abs() < 0.1, "{c:?}");
    }

    #[test]
    fn test_viewport_zoom_limits() {
        let mut vp = ViewportState::new();
        let page_vp = PageViewport::calculate(1920.0, 1080.0, 24.0);

        vp.zoom_at_cursor([960.0, 540.0], 10.0, &page_vp);
        assert!((vp.zoom - MAX_ZOOM).abs() < 1e-4);

        vp.zoom_at_cursor([960.0, 540.0], 0.01, &page_vp);
        assert!((vp.zoom - MIN_ZOOM).abs() < 1e-4);
    }

    #[test]
    fn test_viewport_zoom_centered_at_cursor() {
        let mut vp = ViewportState::new();
        let page_vp = PageViewport::calculate(1920.0, 1080.0, 24.0);
        let cursor = [960.0, 540.0]; // Mitten im Fenster und auf der Seite

        // Punkt vor Zoom berechnen
        let page_pt_before = vp.screen_to_page_unclamped(cursor[0], cursor[1], &page_vp);

        // Zoom verdoppeln
        vp.zoom_at_cursor(cursor, 2.0, &page_vp);

        // Nach Zoom muss der Punkt unter dem Cursor unverändert sein
        let screen_pt_after = vp.page_to_screen(page_pt_before.x, page_pt_before.y, &page_vp);
        assert!((screen_pt_after[0] - cursor[0]).abs() < 0.1);
        assert!((screen_pt_after[1] - cursor[1]).abs() < 0.1);
    }

    #[test]
    fn test_viewport_pan() {
        let mut vp = ViewportState::new();
        let page_vp = PageViewport::calculate(1920.0, 1080.0, 24.0);

        let pt0 = vp.screen_to_page_unclamped(960.0, 540.0, &page_vp);
        vp.pan_by_screen_delta(50.0, 30.0, &page_vp);
        let pt1 = vp.page_to_screen(pt0.x, pt0.y, &page_vp);

        assert!((pt1[0] - 1010.0).abs() < 0.1);
        assert!((pt1[1] - 570.0).abs() < 0.1);
    }

    #[test]
    fn test_ndc_transform_consistency_at_default() {
        let vp = ViewportState::new();
        let page_vp = PageViewport::calculate(1920.0, 1080.0, 24.0);

        let (scale1, offset1) = vp.ndc_transform(&page_vp);
        let (scale2, offset2) = page_vp.ndc_transform();

        assert!((scale1[0] - scale2[0]).abs() < 1e-5);
        assert!((scale1[1] - scale2[1]).abs() < 1e-5);
        assert!((offset1[0] - offset2[0]).abs() < 1e-5);
        assert!((offset1[1] - offset2[1]).abs() < 1e-5);
    }

    #[test]
    fn test_pinch_to_zoom_math() {
        let mut vp = ViewportState::new();
        let page_vp = PageViewport::calculate(1920.0, 1080.0, 24.0);

        // Simulation von 2 Fingern:
        // Finger 1 bei (400, 500), Finger 2 bei (600, 500) -> Distanz: 200, Zentrum: (500, 500)
        let p1_start = (400.0_f32, 500.0_f32);
        let p2_start = (600.0_f32, 500.0_f32);
        let dist_start =
            ((p2_start.0 - p1_start.0).powi(2) + (p2_start.1 - p1_start.1).powi(2)).sqrt();
        let center_start = (
            (p1_start.0 + p2_start.0) * 0.5,
            (p1_start.1 + p2_start.1) * 0.5,
        );
        assert_eq!(dist_start, 200.0);
        assert_eq!(center_start, (500.0, 500.0));

        // Finger auseinanderziehen und leicht verschieben:
        // Finger 1 bei (350, 520), Finger 2 bei (650, 520) -> Distanz: 300, Zentrum: (500, 520)
        let p1_end = (350.0_f32, 520.0_f32);
        let p2_end = (650.0_f32, 520.0_f32);
        let dist_end = ((p2_end.0 - p1_end.0).powi(2) + (p2_end.1 - p1_end.1).powi(2)).sqrt();
        let center_end = ((p1_end.0 + p2_end.0) * 0.5, (p1_end.1 + p2_end.1) * 0.5);
        assert_eq!(dist_end, 300.0);
        assert_eq!(center_end, (500.0, 520.0));

        let zoom_factor = dist_end / dist_start; // 1.5x Vergrößerung
        let pan_delta = (center_end.0 - center_start.0, center_end.1 - center_start.1); // (0, +20)

        vp.pan_by_screen_delta(pan_delta.0, pan_delta.1, &page_vp);
        vp.zoom_at_cursor([center_end.0, center_end.1], zoom_factor, &page_vp);

        assert!((vp.zoom - 1.5).abs() < 1e-4);
    }
}
