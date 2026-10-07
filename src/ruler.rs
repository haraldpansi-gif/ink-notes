//! Virtuelles Lineal: ein verschieb- und drehbares Lineal über dem Blatt. Striche, die nahe
//! einer Längskante beginnen, rasten auf dieser Kante ein und werden zu geraden Linien.
//!
//! Alle Maße in Seiteneinheiten (10 Einheiten = 1 mm).

use glam::Vec2;

use crate::document::UNITS_PER_MM;

/// Länge des Lineals (26 cm).
pub const RULER_LENGTH: f32 = 260.0 * UNITS_PER_MM;
/// Breite des Lineals (3,2 cm).
pub const RULER_WIDTH: f32 = 32.0 * UNITS_PER_MM;
/// Abstand zu einer Längskante, innerhalb dessen ein Strich einrastet (innen wie außen).
pub const SNAP_DISTANCE: f32 = 6.0 * UNITS_PER_MM;
/// Radius des Dreh-Griffs.
pub const HANDLE_RADIUS: f32 = 8.0 * UNITS_PER_MM;
/// Winkel rasten in diesen Schritten ein …
pub const ANGLE_SNAP_STEP_DEG: f32 = 15.0;
/// … wenn sie höchstens so weit davon entfernt sind.
pub const ANGLE_SNAP_TOLERANCE_DEG: f32 = 2.0;

/// Lage des Lineals auf dem Blatt.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Ruler {
    /// Mittelpunkt
    pub center: Vec2,
    /// Drehwinkel der Längsachse in Bogenmaß (0 = waagerecht)
    pub angle: f32,
}

/// Was an einer Stelle des Lineals liegt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RulerHit {
    /// Fläche zum Verschieben
    Body,
    /// Griff zum Drehen
    RotateHandle,
}

/// Eine Kante, an der ein Strich eingerastet ist: Gerade durch `origin` in Richtung `dir`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EdgeLock {
    pub origin: Vec2,
    pub dir: Vec2,
}

impl EdgeLock {
    /// Fußpunkt von `p` auf der Geraden.
    pub fn project(&self, p: Vec2) -> Vec2 {
        self.origin + self.dir * (p - self.origin).dot(self.dir)
    }
}

impl Ruler {
    pub fn new(center: Vec2) -> Self {
        Self { center, angle: 0.0 }
    }

    /// Einheitsvektor entlang der Längsachse.
    pub fn axis(&self) -> Vec2 {
        Vec2::from_angle(self.angle)
    }

    /// Einheitsvektor quer zur Längsachse (zur „unteren“ Kante, bei 0° nach unten).
    pub fn normal(&self) -> Vec2 {
        self.axis().perp()
    }

    /// Lage von `p` im Lineal-Koordinatensystem: (entlang der Achse, quer dazu).
    pub fn local(&self, p: Vec2) -> Vec2 {
        let d = p - self.center;
        Vec2::new(d.dot(self.axis()), d.dot(self.normal()))
    }

    /// Ecken: oben links, oben rechts, unten rechts, unten links (bei 0°).
    pub fn corners(&self) -> [Vec2; 4] {
        let a = self.axis() * (RULER_LENGTH / 2.0);
        let n = self.normal() * (RULER_WIDTH / 2.0);
        [
            self.center - a - n,
            self.center + a - n,
            self.center + a + n,
            self.center - a + n,
        ]
    }

    /// Mittelpunkt des Dreh-Griffs (nahe dem rechten Ende).
    pub fn handle_center(&self) -> Vec2 {
        self.center + self.axis() * (RULER_LENGTH / 2.0 - HANDLE_RADIUS * 1.6)
    }

    /// Was liegt an `p`? Die Streifen entlang der Kanten gehören nicht dazu: dort beginnen
    /// eingerastete Striche.
    pub fn hit(&self, p: Vec2) -> Option<RulerHit> {
        if (p - self.handle_center()).length() <= HANDLE_RADIUS {
            return Some(RulerHit::RotateHandle);
        }
        let l = self.local(p);
        let inside_band = l.y.abs() <= RULER_WIDTH / 2.0 - SNAP_DISTANCE;
        (l.x.abs() <= RULER_LENGTH / 2.0 && inside_band).then_some(RulerHit::Body)
    }

    /// Kante, an der ein bei `p` beginnender Strich einrastet, verschoben um `offset` nach
    /// außen (halbe Strichbreite: die Tinte liegt dann neben dem Lineal, nicht darunter).
    pub fn edge_lock(&self, p: Vec2, offset: f32) -> Option<EdgeLock> {
        let l = self.local(p);
        if l.x.abs() > RULER_LENGTH / 2.0 + SNAP_DISTANCE {
            return None;
        }
        let half = RULER_WIDTH / 2.0;
        let side = if (l.y - half).abs() <= SNAP_DISTANCE {
            1.0
        } else if (l.y + half).abs() <= SNAP_DISTANCE {
            -1.0
        } else {
            return None;
        };
        Some(EdgeLock {
            origin: self.center + self.normal() * side * (half + offset),
            dir: self.axis(),
        })
    }

    /// Dreht das Lineal so, dass die Achse vom Mittelpunkt auf `toward` zeigt; Vielfache von
    /// 15° rasten ein.
    pub fn rotate_toward(&mut self, toward: Vec2) {
        let d = toward - self.center;
        if d.length_squared() < 1e-6 {
            return;
        }
        self.angle = snap_angle(d.y.atan2(d.x));
    }

    /// Winkel für die Anzeige in Grad, 0–179 (ein Lineal hat keine Richtung).
    pub fn display_degrees(&self) -> u32 {
        (self.angle.to_degrees().round() as i32).rem_euclid(180) as u32
    }
}

/// Rastet einen Winkel (Bogenmaß) auf das nächste Vielfache von 15° ein, wenn er nahe genug ist.
pub fn snap_angle(angle: f32) -> f32 {
    let deg = angle.to_degrees();
    let nearest = (deg / ANGLE_SNAP_STEP_DEG).round() * ANGLE_SNAP_STEP_DEG;
    if (deg - nearest).abs() <= ANGLE_SNAP_TOLERANCE_DEG {
        nearest.to_radians()
    } else {
        angle
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: Vec2, b: Vec2) -> bool {
        (a - b).length() < 1e-2
    }

    #[test]
    fn test_geometry_and_hits() {
        let ruler = Ruler::new(Vec2::new(1000.0, 1000.0));
        let [tl, tr, br, bl] = ruler.corners();
        assert!(close(
            tl,
            Vec2::new(1000.0 - RULER_LENGTH / 2.0, 1000.0 - RULER_WIDTH / 2.0)
        ));
        assert!(close(
            br,
            Vec2::new(1000.0 + RULER_LENGTH / 2.0, 1000.0 + RULER_WIDTH / 2.0)
        ));
        assert!(close(
            tr - tl,
            bl - tl + Vec2::new(RULER_LENGTH, -RULER_WIDTH)
        ));

        assert_eq!(ruler.hit(ruler.center), Some(RulerHit::Body));
        assert_eq!(
            ruler.hit(ruler.handle_center()),
            Some(RulerHit::RotateHandle)
        );
        // Kantenstreifen und außerhalb: kein Treffer (dort wird gezeichnet)
        assert_eq!(
            ruler.hit(Vec2::new(1000.0, 1000.0 - RULER_WIDTH / 2.0 + 10.0)),
            None
        );
        assert_eq!(ruler.hit(Vec2::new(1000.0, 1300.0)), None);
        assert_eq!(ruler.hit(Vec2::new(1000.0 + RULER_LENGTH, 1000.0)), None);
    }

    #[test]
    fn test_edge_lock_projects_onto_offset_edge() {
        let ruler = Ruler::new(Vec2::new(1000.0, 1000.0));
        let edge_y = 1000.0 - RULER_WIDTH / 2.0;
        // Knapp oberhalb der oberen Kante: rastet ein, Linie liegt um den Versatz außerhalb
        let lock = ruler
            .edge_lock(Vec2::new(900.0, edge_y - 20.0), 3.0)
            .unwrap();
        let p = lock.project(Vec2::new(1300.0, edge_y - 55.0));
        assert!(close(p, Vec2::new(1300.0, edge_y - 3.0)));
        // Knapp innerhalb der unteren Kante
        let lock = ruler
            .edge_lock(Vec2::new(900.0, 1000.0 + RULER_WIDTH / 2.0 - 20.0), 0.0)
            .unwrap();
        assert!(close(
            lock.project(Vec2::new(0.0, 0.0)),
            Vec2::new(0.0, 1000.0 + RULER_WIDTH / 2.0)
        ));
        // Mitte oder weit weg: kein Einrasten
        assert!(ruler.edge_lock(ruler.center, 0.0).is_none());
        assert!(ruler.edge_lock(Vec2::new(1000.0, 500.0), 0.0).is_none());
        assert!(ruler
            .edge_lock(Vec2::new(1000.0 + RULER_LENGTH, edge_y), 0.0)
            .is_none());

        // Gedrehtes Lineal: die Linie folgt der Drehung
        let mut tilted = ruler;
        tilted.angle = 30f32.to_radians();
        let near_edge = tilted.center - tilted.normal() * (RULER_WIDTH / 2.0 + 10.0);
        let lock = tilted.edge_lock(near_edge, 0.0).unwrap();
        let a = lock.project(near_edge);
        let b = lock.project(near_edge + Vec2::new(300.0, 0.0));
        let dir = (b - a).normalize();
        assert!((dir.y.atan2(dir.x).to_degrees() - 30.0).abs() < 0.01);
    }

    #[test]
    fn test_rotation_snaps_to_15_degrees() {
        let mut ruler = Ruler::new(Vec2::ZERO);
        ruler.rotate_toward(Vec2::from_angle(44.0f32.to_radians()) * 100.0);
        assert_eq!(ruler.display_degrees(), 45);
        ruler.rotate_toward(Vec2::from_angle(37.0f32.to_radians()) * 100.0);
        assert_eq!(ruler.display_degrees(), 37);
        // Gegenrichtung zeigt denselben Winkel
        ruler.rotate_toward(Vec2::from_angle(-150.0f32.to_radians()) * 100.0);
        assert_eq!(ruler.display_degrees(), 30);
        assert_eq!(snap_angle(1.0f32.to_radians()), 0.0);
    }
}
