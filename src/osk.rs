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

use std::ffi::c_void;

use wayland_client::{
    delegate_noop,
    protocol::{wl_registry, wl_seat},
    Connection, Dispatch, EventQueue, QueueHandle,
};
use wayland_protocols::wp::text_input::zv3::client::{
    zwp_text_input_manager_v3::ZwpTextInputManagerV3,
    zwp_text_input_v3::{self, ZwpTextInputV3},
};

/// Platzhalter-Umgebungstext: ein Zeichen vor der Schreibmarke, damit die Rücktaste der
/// Bildschirmtastatur immer etwas zu löschen hat (ein Byte = ein Zeichen)
const SURROUNDING_PLACEHOLDER: &str = " ";

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

/// Entscheidet, ob die Bildschirmtastatur angefordert wird: sobald ein Textfeld den Fokus
/// bekommt und erneut bei jedem Antippen des fokussierten Felds (falls die Tastatur inzwischen
/// zugeklappt wurde).
pub fn should_request(was_active: bool, active: bool, pressed: bool) -> bool {
    active && (!was_active || pressed)
}

#[derive(Default)]
struct State {
    seat: Option<wl_seat::WlSeat>,
    manager: Option<ZwpTextInputManagerV3>,
    text_input: Option<ZwpTextInputV3>,
    /// Von der Bildschirmtastatur gewünschtes Löschen (Zeichen vor/nach der Schreibmarke)
    delete_back: u32,
    delete_forward: u32,
}

/// Eigenes text-input-Objekt, mit dem ink-notes die Bildschirmtastatur anfordert
pub struct KeyboardRequester {
    conn: Connection,
    queue: EventQueue<State>,
    state: State,
}

impl KeyboardRequester {
    /// Hängt sich an die Wayland-Verbindung des Fensters.
    ///
    /// # Safety
    /// `wl_display` muss auf die `wl_display` des Fensters zeigen und gültig bleiben, solange
    /// der `KeyboardRequester` lebt.
    pub unsafe fn new(wl_display: *mut c_void) -> Self {
        // Safety: an den Aufrufer weitergereicht
        let backend =
            unsafe { wayland_backend::client::Backend::from_foreign_display(wl_display.cast()) };
        let conn = Connection::from_backend(backend);
        let queue = conn.new_event_queue();
        conn.display().get_registry(&queue.handle(), ());
        let _ = conn.flush();
        Self {
            conn,
            queue,
            state: State::default(),
        }
    }

    /// Verarbeitet eingetroffene Ereignisse; einmal pro Frame aufrufen.
    pub fn pump(&mut self) {
        let _ = self.queue.dispatch_pending(&mut self.state);
        if self.state.text_input.is_none() {
            if let (Some(manager), Some(seat)) = (&self.state.manager, &self.state.seat) {
                self.state.text_input =
                    Some(manager.get_text_input(seat, &self.queue.handle(), ()));
            }
        }
    }

    /// Bittet den Compositor, die Bildschirmtastatur einzublenden. Muss nach dem `enable` von
    /// winit kommen (also nach `handle_platform_output`).
    pub fn request(&mut self) {
        self.pump();
        if let Some(text_input) = &self.state.text_input {
            text_input.enable();
            self.send_surrounding_placeholder();
        }
    }

    /// Tastendrücke für das seit dem letzten Aufruf gewünschte Löschen (vor `begin_frame`
    /// aufrufen und an die egui-Eingabe anhängen)
    pub fn take_deletions(&mut self) -> Vec<egui::Event> {
        self.pump();
        // Den Platzhalter hier nicht neu melden: Die Tastatur löscht bei jeder neuen Meldung
        // erneut, solange die Rücktaste gedrückt ist – ein kurzes Antippen löschte sonst doppelt.
        let back = std::mem::take(&mut self.state.delete_back);
        let forward = std::mem::take(&mut self.state.delete_forward);
        deletion_key_events(back, forward)
    }

    fn send_surrounding_placeholder(&self) {
        if let Some(text_input) = &self.state.text_input {
            let len = SURROUNDING_PLACEHOLDER.len() as i32;
            text_input.set_surrounding_text(SURROUNDING_PLACEHOLDER.to_owned(), len, len);
            text_input.commit();
            let _ = self.conn.flush();
        }
    }
}

impl Dispatch<wl_registry::WlRegistry, ()> for State {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name, interface, ..
        } = event
        {
            match interface.as_str() {
                "wl_seat" if state.seat.is_none() => {
                    state.seat = Some(registry.bind(name, 1, qh, ()));
                }
                "zwp_text_input_manager_v3" if state.manager.is_none() => {
                    state.manager = Some(registry.bind(name, 1, qh, ()));
                }
                _ => {}
            }
        }
    }
}

delegate_noop!(State: ignore wl_seat::WlSeat);
delegate_noop!(State: ZwpTextInputManagerV3);
impl Dispatch<ZwpTextInputV3, ()> for State {
    fn event(
        state: &mut Self,
        _: &ZwpTextInputV3,
        event: zwp_text_input_v3::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // Getippter Text kommt über winit; hier nur das Löschen
        if let zwp_text_input_v3::Event::DeleteSurroundingText {
            before_length,
            after_length,
        } = event
        {
            state.delete_back += before_length;
            state.delete_forward += after_length;
        }
    }
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
    fn no_request_without_focused_text_field() {
        assert!(!should_request(false, false, true));
        assert!(!should_request(true, false, true));
    }
}
