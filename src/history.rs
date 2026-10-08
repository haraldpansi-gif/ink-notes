//! Transaktionales Undo / Redo mit History-Stack pro Seite oder Notizbuch.

use crate::document::Stroke;

/// Eine rückgängig machbare Benutzeraktion auf einer Seite.
// Der Verlauf hält nur wenige hundert Einträge; die Größe des Ersetzen-Eintrags (zwei Striche)
// fällt nicht ins Gewicht, eine Box würde nur die Aufrufe umständlicher machen
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
pub enum HistoryAction {
    /// Ein Strich wurde neu gezeichnet.
    AddStroke(Stroke),
    /// Mehrere Striche wurden auf einmal hinzugefügt (z. B. Einfügen/Duplizieren).
    AddStrokes(Vec<Stroke>),
    /// Ein oder mehrere Striche wurden durch den Radierer oder Selektion gelöscht.
    RemoveStrokes(Vec<Stroke>),
    /// Ein Strich wurde durch eine geometrische Form ersetzt.
    ReplaceStroke { old: Stroke, new: Stroke },
    /// Ein oder mehrere Striche wurden modifiziert (z. B. verschoben, gedreht, skaliert, umgefärbt).
    ModifyStrokes {
        before: Vec<Stroke>,
        after: Vec<Stroke>,
    },
    /// Teilradierer: Striche wurden durch ihre übrig gebliebenen Stücke ersetzt.
    SplitStrokes {
        removed: Vec<Stroke>,
        added: Vec<Stroke>,
        /// Zeitpunkt des letzten Schnitts (zum Zusammenfassen eines Radierzugs)
        at: std::time::Instant,
    },
}

/// History-Stack für Transaktionsverwaltung (Undo / Redo).
#[derive(Debug, Clone, Default)]
pub struct HistoryStack {
    pub undo_stack: Vec<HistoryAction>,
    pub redo_stack: Vec<HistoryAction>,
}

impl HistoryStack {
    pub fn new() -> Self {
        Self {
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
        }
    }

    /// Registriert eine neue Aktion. Leert den Redo-Stack.
    pub fn push(&mut self, action: HistoryAction) {
        self.undo_stack.push(action);
        self.redo_stack.clear();
    }

    /// Registriert einen Teilradier-Schnitt. Folgt er innerhalb von
    /// [`crate::document::PARTIAL_ERASE_MERGE_WINDOW`] auf einen vorherigen, werden beide zu
    /// einem Undo-Schritt zusammengefasst: Stücke, die im selben Zug wieder zerschnitten
    /// wurden, verschwinden aus der Bilanz.
    pub fn push_split(&mut self, removed: Vec<Stroke>, added: Vec<Stroke>) {
        let now = std::time::Instant::now();
        if let Some(HistoryAction::SplitStrokes {
            removed: prev_removed,
            added: prev_added,
            at,
        }) = self.undo_stack.last_mut()
        {
            if now.duration_since(*at) <= crate::document::PARTIAL_ERASE_MERGE_WINDOW
                && self.redo_stack.is_empty()
            {
                for stroke in removed {
                    if let Some(pos) = prev_added.iter().position(|s| s.id == stroke.id) {
                        prev_added.remove(pos);
                    } else {
                        prev_removed.push(stroke);
                    }
                }
                prev_added.extend(added);
                *at = now;
                return;
            }
        }
        self.push(HistoryAction::SplitStrokes {
            removed,
            added,
            at: now,
        });
    }

    /// Gibt zurück, ob Undo verfügbar ist.
    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    /// Gibt zurück, ob Redo verfügbar ist.
    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    /// Führt Undo auf der Liste der Striche aus.
    ///
    /// Gibt `true` zurück, wenn sich die Strichliste geändert hat.
    pub fn undo(&mut self, strokes: &mut Vec<Stroke>) -> bool {
        let Some(action) = self.undo_stack.pop() else {
            return false;
        };

        match action {
            HistoryAction::AddStroke(stroke) => {
                // Den hinzugefügten Strich aus der Seite entfernen
                if let Some(pos) = strokes.iter().position(|s| s.id == stroke.id) {
                    strokes.remove(pos);
                }
                self.redo_stack.push(HistoryAction::AddStroke(stroke));
            }
            HistoryAction::AddStrokes(added) => {
                let ids: Vec<u64> = added.iter().map(|s| s.id).collect();
                strokes.retain(|s| !ids.contains(&s.id));
                self.redo_stack.push(HistoryAction::AddStrokes(added));
            }
            HistoryAction::RemoveStrokes(removed) => {
                // Die gelöschten Striche wiederherstellen
                for s in &removed {
                    strokes.push(s.clone());
                }
                self.redo_stack.push(HistoryAction::RemoveStrokes(removed));
            }
            HistoryAction::ReplaceStroke { old, new } => {
                if let Some(pos) = strokes.iter().position(|s| s.id == new.id) {
                    strokes[pos] = old.clone();
                }
                self.redo_stack
                    .push(HistoryAction::ReplaceStroke { old, new });
            }
            HistoryAction::ModifyStrokes { before, after } => {
                for b in &before {
                    if let Some(pos) = strokes.iter().position(|s| s.id == b.id) {
                        strokes[pos] = b.clone();
                    }
                }
                self.redo_stack
                    .push(HistoryAction::ModifyStrokes { before, after });
            }
            HistoryAction::SplitStrokes { removed, added, at } => {
                strokes.retain(|s| !added.iter().any(|a| a.id == s.id));
                strokes.extend(removed.iter().cloned());
                self.redo_stack
                    .push(HistoryAction::SplitStrokes { removed, added, at });
            }
        }

        true
    }

    /// Führt Redo auf der Liste der Striche aus.
    ///
    /// Gibt `true` zurück, wenn sich die Strichliste geändert hat.
    pub fn redo(&mut self, strokes: &mut Vec<Stroke>) -> bool {
        let Some(action) = self.redo_stack.pop() else {
            return false;
        };

        match action {
            HistoryAction::AddStroke(stroke) => {
                // Strich erneut hinzufügen
                strokes.push(stroke.clone());
                self.undo_stack.push(HistoryAction::AddStroke(stroke));
            }
            HistoryAction::AddStrokes(added) => {
                strokes.extend(added.clone());
                self.undo_stack.push(HistoryAction::AddStrokes(added));
            }
            HistoryAction::RemoveStrokes(removed) => {
                // Striche erneut entfernen
                let ids: Vec<u64> = removed.iter().map(|s| s.id).collect();
                strokes.retain(|s| !ids.contains(&s.id));
                self.undo_stack.push(HistoryAction::RemoveStrokes(removed));
            }
            HistoryAction::ReplaceStroke { old, new } => {
                if let Some(pos) = strokes.iter().position(|s| s.id == old.id) {
                    strokes[pos] = new.clone();
                }
                self.undo_stack
                    .push(HistoryAction::ReplaceStroke { old, new });
            }
            HistoryAction::ModifyStrokes { before, after } => {
                for a in &after {
                    if let Some(pos) = strokes.iter().position(|s| s.id == a.id) {
                        strokes[pos] = a.clone();
                    }
                }
                self.undo_stack
                    .push(HistoryAction::ModifyStrokes { before, after });
            }
            HistoryAction::SplitStrokes { removed, added, at } => {
                strokes.retain(|s| !removed.iter().any(|r| r.id == s.id));
                strokes.extend(added.iter().cloned());
                self.undo_stack
                    .push(HistoryAction::SplitStrokes { removed, added, at });
            }
        }

        true
    }

    /// Setzt den History-Stack zurück.
    pub fn clear(&mut self) {
        self.undo_stack.clear();
        self.redo_stack.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::StylusSample;

    fn make_dummy_stroke(id: u64) -> Stroke {
        Stroke::new(
            id,
            vec![StylusSample {
                position: glam::Vec2::new(0.0, 0.0),
                pressure: 0.5,
                timestamp: 0.0,
            }],
            Vec::new(),
            [0.0, 0.0, 0.0, 1.0],
        )
    }

    #[test]
    fn test_undo_redo_add_stroke() {
        let mut history = HistoryStack::new();
        let mut strokes = Vec::new();

        let s1 = make_dummy_stroke(1);
        strokes.push(s1.clone());
        history.push(HistoryAction::AddStroke(s1));

        assert_eq!(strokes.len(), 1);
        assert!(history.can_undo());
        assert!(!history.can_redo());

        // Undo
        assert!(history.undo(&mut strokes));
        assert_eq!(strokes.len(), 0);
        assert!(!history.can_undo());
        assert!(history.can_redo());

        // Redo
        assert!(history.redo(&mut strokes));
        assert_eq!(strokes.len(), 1);
        assert_eq!(strokes[0].id, 1);
        assert!(history.can_undo());
        assert!(!history.can_redo());
    }

    #[test]
    fn test_undo_redo_remove_strokes() {
        let mut history = HistoryStack::new();
        let mut strokes = vec![make_dummy_stroke(1), make_dummy_stroke(2)];

        // Löschen von Stroke 2
        let removed = vec![strokes.remove(1)];
        history.push(HistoryAction::RemoveStrokes(removed));

        assert_eq!(strokes.len(), 1);

        // Undo: Stroke 2 wird wiederhergestellt
        assert!(history.undo(&mut strokes));
        assert_eq!(strokes.len(), 2);

        // Redo: Stroke 2 wird wieder entfernt
        assert!(history.redo(&mut strokes));
        assert_eq!(strokes.len(), 1);
        assert_eq!(strokes[0].id, 1);
    }

    #[test]
    fn test_undo_redo_replace_stroke() {
        let mut history = HistoryStack::new();
        let s_old = make_dummy_stroke(1);
        let mut s_new = make_dummy_stroke(1);
        s_new.color = [1.0, 0.0, 0.0, 1.0]; // Modifizierter Strich (z. B. erkannte Form)

        let mut strokes = vec![s_old.clone()];
        strokes[0] = s_new.clone();
        history.push(HistoryAction::ReplaceStroke {
            old: s_old.clone(),
            new: s_new.clone(),
        });

        assert_eq!(strokes[0].color, [1.0, 0.0, 0.0, 1.0]);

        // Undo: stellt den alten Strich wieder her
        assert!(history.undo(&mut strokes));
        assert_eq!(strokes[0].color, s_old.color);

        // Redo: setzt den neuen Strich wieder ein
        assert!(history.redo(&mut strokes));
        assert_eq!(strokes[0].color, [1.0, 0.0, 0.0, 1.0]);
    }
}
