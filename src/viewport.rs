//! Viewport- und Kameraverwaltung für stufenloses Zoomen und Pannen.
//!
//! Berechnet die affine Transformation zwischen Bildschirmkoordinaten (Window-Pixel),
//! virtuellen DIN-A4-Seitenkoordinaten ([0, 2100] x [0, 2970]) und dem GPU-NDC-Raum ([-1, 1]).

use crate::document::{PageViewport, PAGE_HEIGHT, PAGE_WIDTH};
use glam::{Mat4, Vec2, Vec4};

/// Minimaler und maximaler Zoomfaktor.
pub const MIN_ZOOM: f32 = 0.25;
pub const MAX_ZOOM: f32 = 5.0;

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
}
