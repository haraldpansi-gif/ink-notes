//! Bildschirmtastatur unter Wayland anfordern.
//!
//! GNOME (mutter) blendet die Bildschirmtastatur nicht schon beim ersten `enable` eines
//! Texteingabefelds (`zwp_text_input_v3`) ein, sondern erst, wenn ein bereits aktives Feld
//! `enable` erneut meldet – so macht es GTK beim Antippen. winit meldet `enable` nur einmal,
//! sobald egui ein Textfeld fokussiert. Daher schickt ink-notes über ein eigenes
//! text-input-Objekt auf derselben Wayland-Verbindung ein weiteres `enable`. Die getippten
//! Zeichen kommen weiterhin über winit an.
//!
//! Die Rücktaste der GNOME-Bildschirmtastatur löscht über `delete_surrounding_text` im zuvor
//! gemeldeten Umgebungstext; ohne Umgebungstext passiert nichts, und winit wertet das Löschen
//! ohnehin nicht aus. Daher meldet das eigene Objekt einen Platzhalter als Umgebungstext und
//! übersetzt die Löschwünsche in Tastendrücke (Rücktaste/Entf) für egui.

#[cfg(target_os = "linux")]
mod wayland;
#[cfg(target_os = "linux")]
pub use wayland::KeyboardRequester;

/// Android: the soft keyboard is shown only when asked to (Issue #42). Its text comes through
/// the activity's input connection (`android_bridge`); keys like Enter or Backspace may also
/// arrive as key events through winit.
#[cfg(target_os = "android")]
pub struct KeyboardRequester;

#[cfg(target_os = "android")]
impl KeyboardRequester {
    /// # Safety
    /// Nothing to uphold; `unsafe` like the Wayland version.
    pub unsafe fn for_window(_window: &winit::window::Window) -> Option<Self> {
        Some(Self)
    }

    pub fn request(&mut self) {
        crate::android_bridge::show_keyboard(true);
    }

    pub fn release(&mut self) {
        crate::android_bridge::show_keyboard(false);
    }

    /// Typed text and deletions; taking them is left to the app's frame (see `app.rs`)
    pub fn take_deletions(&mut self) -> Vec<egui::Event> {
        Vec::new()
    }
}

/// Elsewhere the system shows the on-screen keyboard by itself; nothing to request.
#[cfg(not(any(target_os = "linux", target_os = "android")))]
pub struct KeyboardRequester;

#[cfg(not(any(target_os = "linux", target_os = "android")))]
impl KeyboardRequester {
    /// # Safety
    /// Nothing to uphold; `unsafe` like the Wayland version.
    pub unsafe fn for_window(_window: &winit::window::Window) -> Option<Self> {
        None
    }

    pub fn request(&mut self) {}

    pub fn release(&mut self) {}

    pub fn take_deletions(&mut self) -> Vec<egui::Event> {
        Vec::new()
    }
}

/// Wandelt Texteingaben ohne Vorschau (Preedit) in normale Texteingaben um.
///
/// Die Bildschirmtastatur schickt jedes Zeichen als fertigen IME-Text, ohne vorher eine Vorschau
/// zu melden. egui 0.27 fügt solche Texte nur ein, solange die Schreibmarke dort steht, wo die
/// letzte Vorschau begann – also nur das erste Zeichen. `composing` merkt sich über Frames hinweg,
/// ob gerade eine Vorschau läuft; deren Abschluss bleibt bei egui.
pub fn plain_ime_commits_to_text(events: &mut [egui::Event], composing: &mut bool) {
    for event in events {
        match event {
            egui::Event::CompositionStart => *composing = true,
            egui::Event::CompositionEnd(text) => {
                if !*composing && !text.is_empty() && !text.contains(['\n', '\r']) {
                    *event = egui::Event::Text(std::mem::take(text));
                }
                *composing = false;
            }
            _ => {}
        }
    }
}

/// Tastendrücke für egui, die `back` Zeichen vor und `forward` Zeichen nach der Schreibmarke
/// löschen
pub fn deletion_key_events(back: u32, forward: u32) -> Vec<egui::Event> {
    let presses = std::iter::repeat_n(egui::Key::Backspace, back as usize)
        .chain(std::iter::repeat_n(egui::Key::Delete, forward as usize));
    presses
        .flat_map(|key| {
            [true, false].map(|pressed| egui::Event::Key {
                key,
                physical_key: None,
                pressed,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            })
        })
        .collect()
}

/// egui input for committed text: line breaks become the Enter key, as egui's text fields
/// take them only that way.
pub fn committed_text_events(text: &str) -> Vec<egui::Event> {
    let mut events = Vec::new();
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            for pressed in [true, false] {
                events.push(egui::Event::Key {
                    key: egui::Key::Enter,
                    physical_key: None,
                    pressed,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                });
            }
        }
        let line = line.trim_end_matches('\r');
        if !line.is_empty() {
            events.push(egui::Event::Text(line.to_string()));
        }
    }
    events
}

/// Entscheidet, ob die Bildschirmtastatur angefordert wird: sobald ein Textfeld den Fokus
/// bekommt und erneut bei jedem Antippen des fokussierten Felds (falls die Tastatur inzwischen
/// zugeklappt wurde).
pub fn should_request(was_active: bool, active: bool, pressed: bool) -> bool {
    active && (!was_active || pressed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_when_text_field_gets_focus() {
        assert!(should_request(false, true, false));
        assert!(should_request(false, true, true));
    }

    #[test]
    fn requests_again_when_focused_field_is_tapped() {
        assert!(should_request(true, true, true));
        assert!(!should_request(true, true, false));
    }

    #[test]
    fn commits_without_preedit_become_text() {
        let mut composing = false;
        let mut events = vec![
            egui::Event::CompositionEnd("h".into()),
            egui::Event::CompositionEnd("i".into()),
        ];
        plain_ime_commits_to_text(&mut events, &mut composing);
        assert_eq!(
            events,
            vec![egui::Event::Text("h".into()), egui::Event::Text("i".into())]
        );
        assert!(!composing);
    }

    #[test]
    fn commits_after_preedit_stay_with_egui() {
        let mut composing = false;
        let mut events = vec![
            egui::Event::CompositionStart,
            egui::Event::CompositionUpdate("ni".into()),
        ];
        plain_ime_commits_to_text(&mut events, &mut composing);
        assert!(composing);
        // Abschluss erst im nächsten Frame
        let mut events = vec![egui::Event::CompositionEnd("你".into())];
        plain_ime_commits_to_text(&mut events, &mut composing);
        assert_eq!(events, vec![egui::Event::CompositionEnd("你".into())]);
        assert!(!composing);
    }

    #[test]
    fn line_breaks_and_empty_commits_are_left_alone() {
        let mut composing = false;
        let mut events = vec![
            egui::Event::CompositionEnd("\n".into()),
            egui::Event::CompositionEnd(String::new()),
        ];
        let before = events.clone();
        plain_ime_commits_to_text(&mut events, &mut composing);
        assert_eq!(events, before);
    }

    #[test]
    fn deletions_become_key_presses() {
        let events = deletion_key_events(2, 1);
        let keys: Vec<_> = events
            .iter()
            .map(|e| match e {
                egui::Event::Key { key, pressed, .. } => (*key, *pressed),
                other => panic!("unerwartet: {other:?}"),
            })
            .collect();
        assert_eq!(
            keys,
            vec![
                (egui::Key::Backspace, true),
                (egui::Key::Backspace, false),
                (egui::Key::Backspace, true),
                (egui::Key::Backspace, false),
                (egui::Key::Delete, true),
                (egui::Key::Delete, false),
            ]
        );
        assert!(deletion_key_events(0, 0).is_empty());
    }

    #[test]
    fn committed_line_breaks_become_enter() {
        let events = committed_text_events("ab\nc\n");
        let kinds: Vec<String> = events
            .iter()
            .map(|e| match e {
                egui::Event::Text(t) => t.clone(),
                egui::Event::Key { pressed: true, .. } => "⏎".into(),
                _ => "-".into(),
            })
            .collect();
        assert_eq!(kinds, ["ab", "⏎", "-", "c", "⏎", "-"]);
    }

    #[test]
    fn no_request_without_focused_text_field() {
        assert!(!should_request(false, false, true));
        assert!(!should_request(true, false, true));
    }
}
