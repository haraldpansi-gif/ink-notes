//! Viewport- und Kameraverwaltung für stufenloses Zoomen und Pannen.
//!
//! Berechnet die affine Transformation zwischen Bildschirmkoordinaten (Window-Pixel),
//! virtuellen DIN-A4-Seitenkoordinaten ([0, 2100] x [0, 2970]) und dem GPU-NDC-Raum ([-1, 1]).

use crate::document::{PageViewport, PAGE_HEIGHT, PAGE_WIDTH, UNITS_PER_MM};
use glam::{Mat4, Vec2, Vec4};

/// Minimaler und maximaler Zoomfaktor.
pub const MIN_ZOOM: f32 = 0.25;
pub const MAX_ZOOM: f32 = 10.0;

/// Abstand zwischen zwei Seiten in der durchgehenden Ansicht (8 mm).
pub const PAGE_GAP: f32 = 8.0 * UNITS_PER_MM;
/// Vertikaler Abstand gleicher Punkte zweier aufeinanderfolgender Seiten.
pub const PAGE_PITCH: f32 = PAGE_HEIGHT + PAGE_GAP;
/// Höchstens so viele Nachbarseiten werden gleichzeitig dargestellt.
pub const MAX_NEIGHBOR_PAGES: usize = 12;

/// Durchgehende Ansicht: Die Seiten hängen untereinander, Koordinaten beziehen sich auf die
/// aktuelle Seite. Liefert den Versatz (in Seiten) der Seite, auf der die Höhe `y` liegt,
/// oder `None` im Zwischenraum zweier Seiten.
pub fn page_offset_at(y: f32) -> Option<i64> {
    let k = (y / PAGE_PITCH).floor();
    (y - k * PAGE_PITCH <= PAGE_HEIGHT).then_some(k as i64)
}

/// Versätze der Seiten, die zwischen den Höhen `top` und `bottom` (Koordinaten der aktuellen
/// Seite) sichtbar sind, begrenzt auf die Seitenindizes `0..page_count` und ohne die aktuelle
/// Seite selbst.
pub fn visible_neighbor_offsets(
    top: f32,
    bottom: f32,
    current: usize,
    page_count: usize,
) -> Vec<i64> {
    if !(top.is_finite() && bottom.is_finite()) || bottom < top {
        return Vec::new();
    }
    let first = (top / PAGE_PITCH).floor().max(-(current as f32)) as i64;
    let last = (bottom / PAGE_PITCH)
        .floor()
        .min((page_count as f32) - 1.0 - current as f32) as i64;
    let mut offsets: Vec<i64> = (first..=last).filter(|&k| k != 0).collect();
    // Sehr weit herausgezoomt: nur die nächstgelegenen Seiten
    if offsets.len() > MAX_NEIGHBOR_PAGES {
        offsets.sort_by_key(|k| k.unsigned_abs());
        offsets.truncate(MAX_NEIGHBOR_PAGES);
        offsets.sort_unstable();
    }
    offsets
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

    /// Durchgehende Ansicht: Die Seite `k` Seiten weiter wird zur Bezugsseite, ohne dass sich
    /// der Bildausschnitt bewegt.
    pub fn shift_reference_page(&mut self, k: i64) {
        self.pan_offset[1] += k as f32 * PAGE_PITCH;
    }

    /// Durchgehende Ansicht: Verschiebt den Ausschnitt so, dass der Bildschirmpunkt `center`
    /// über dem Notizbuch liegt (zwischen Oberkante der ersten und Unterkante der letzten
    /// Seite, waagerecht innerhalb der Seitenbreite). `true`, wenn korrigiert wurde.
    pub fn clamp_continuous_center(
        &mut self,
        center: [f32; 2],
        current: usize,
        page_count: usize,
        page_viewport: &PageViewport,
    ) -> bool {
        let c = self.screen_to_page_unclamped(center[0], center[1], page_viewport);
        let top = -(current as f32) * PAGE_PITCH;
        let bottom = page_count.saturating_sub(1 + current) as f32 * PAGE_PITCH + PAGE_HEIGHT;
        let clamped = Vec2::new(c.x.clamp(0.0, PAGE_WIDTH), c.y.clamp(top, bottom.max(top)));
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
        let base_scale = page_viewport.page_width / PAGE_WIDTH;
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

        let base_scale = page_viewport.page_width / PAGE_WIDTH;

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
        let base_scale = page_viewport.page_width / PAGE_WIDTH;
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
        if (0.0..=PAGE_WIDTH).contains(&pt.x) && (0.0..=PAGE_HEIGHT).contains(&pt.y) {
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
        Vec2::new(pt.x.clamp(0.0, PAGE_WIDTH), pt.y.clamp(0.0, PAGE_HEIGHT))
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
        let base_scale = page_viewport.page_width / PAGE_WIDTH;
        let effective_scale = base_scale * self.zoom;
        let screen_x = page_viewport.page_x + (page_x + self.pan_offset[0]) * effective_scale;
        let screen_y = page_viewport.page_y + (page_y + self.pan_offset[1]) * effective_scale;
        [screen_x, screen_y]
    }

    /// Berechnet die Shader-Transformationsparameter (Scale, Offset) für den Vertex-Shader,
    /// um DIN-A4-Seitenkoordinaten direkt in den GPU-NDC-Bereich [-1.0, 1.0] abzubilden.
    pub fn ndc_transform(&self, page_viewport: &PageViewport) -> ([f32; 2], [f32; 2]) {
        let base_scale = page_viewport.page_width / PAGE_WIDTH;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_continuous_layout() {
        assert_eq!(page_offset_at(100.0), Some(0));
        assert_eq!(page_offset_at(PAGE_HEIGHT + PAGE_GAP / 2.0), None);
        assert_eq!(page_offset_at(PAGE_PITCH + 5.0), Some(1));
        assert_eq!(page_offset_at(-5.0), None);
        assert_eq!(page_offset_at(-PAGE_GAP - 5.0), Some(-1));

        // Ende von Seite 2 und Anfang von Seite 3 sichtbar (aktuell: Seite 2 von 5)
        assert_eq!(
            visible_neighbor_offsets(PAGE_HEIGHT - 500.0, PAGE_PITCH + 500.0, 1, 5),
            vec![1]
        );
        // Ganz oben im Notizbuch: keine Seite vor der ersten
        assert!(visible_neighbor_offsets(-PAGE_PITCH * 3.0, 500.0, 0, 5).is_empty());
        // Hinter der letzten Seite: keine weiteren
        assert_eq!(
            visible_neighbor_offsets(-PAGE_PITCH, PAGE_PITCH * 4.0, 3, 5),
            vec![-1, 1]
        );
        // Weit herausgezoomt: auf die nächstgelegenen Seiten begrenzt
        let many = visible_neighbor_offsets(-PAGE_PITCH * 50.0, PAGE_PITCH * 50.0, 50, 200);
        assert_eq!(many.len(), MAX_NEIGHBOR_PAGES);
        assert!(many.iter().all(|k| k.unsigned_abs() <= 6));
    }

    #[test]
    fn test_shift_reference_page_keeps_view() {
        let mut vp = ViewportState::new();
        let page_vp = PageViewport::calculate(1920.0, 1080.0, 24.0);
        vp.pan_by_screen_delta(0.0, -900.0, &page_vp);
        // Punkt oben auf der nächsten Seite, in Koordinaten der aktuellen Seite
        let before = vp.page_to_screen(300.0, PAGE_PITCH + 200.0, &page_vp);
        vp.shift_reference_page(1);
        let after = vp.page_to_screen(300.0, 200.0, &page_vp);
        assert!((before[0] - after[0]).abs() < 0.01 && (before[1] - after[1]).abs() < 0.01);
    }

    #[test]
    fn test_continuous_scroll_stays_on_the_notebook() {
        let page_vp = PageViewport::calculate(1280.0, 850.0, 24.0);
        let center = [640.0, 425.0];
        let mut vp = ViewportState::new();
        // Mitte der Seite: nichts zu korrigieren
        assert!(!vp.clamp_continuous_center(center, 1, 3, &page_vp));
        // Weit über die erste Seite hinaus nach oben gescrollt (aktuell: Seite 2 von 3)
        vp.pan_by_screen_delta(0.0, 5000.0, &page_vp);
        assert!(vp.clamp_continuous_center(center, 1, 3, &page_vp));
        let c = vp.screen_to_page_unclamped(center[0], center[1], &page_vp);
        assert!(
            (c.y + PAGE_PITCH).abs() < 0.1,
            "Oberkante der ersten Seite: {c:?}"
        );
        // Weit nach unten: Unterkante der letzten Seite
        vp.pan_by_screen_delta(-3000.0, -20000.0, &page_vp);
        assert!(vp.clamp_continuous_center(center, 1, 3, &page_vp));
        let c = vp.screen_to_page_unclamped(center[0], center[1], &page_vp);
        assert!((c.y - (PAGE_PITCH + PAGE_HEIGHT)).abs() < 0.1, "{c:?}");
        assert!((c.x - PAGE_WIDTH).abs() < 0.1, "{c:?}");
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
