//! Wayland part of [`super`]: an own text-input object on the window's connection.

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

use super::deletion_key_events;

/// Platzhalter-Umgebungstext: ein Zeichen vor der Schreibmarke, damit die Rücktaste der
/// Bildschirmtastatur immer etwas zu löschen hat (ein Byte = ein Zeichen)
const SURROUNDING_PLACEHOLDER: &str = " ";

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
    /// Requester for the window if it runs on Wayland.
    ///
    /// # Safety
    /// The window must outlive the requester.
    pub unsafe fn for_window(window: &winit::window::Window) -> Option<Self> {
        use winit::raw_window_handle::{HasDisplayHandle, RawDisplayHandle};
        match window.display_handle().map(|handle| handle.as_raw()) {
            // Safety: passed on to the caller
            Ok(RawDisplayHandle::Wayland(handle)) => {
                Some(unsafe { Self::new(handle.display.as_ptr()) })
            }
            _ => None,
        }
    }

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
