//! Touch handling.

use sctk::reexports::client::protocol::wl_seat::WlSeat;
use sctk::reexports::client::protocol::wl_surface::WlSurface;
use sctk::reexports::client::protocol::wl_touch::WlTouch;
use sctk::reexports::client::{Connection, Proxy, QueueHandle};
use sctk::reexports::csd_frame::FrameClick;

use std::time::Duration;

use sctk::compositor::SurfaceData;
use sctk::seat::touch::{TouchData, TouchHandler};

use crate::dpi::LogicalPosition;
use crate::event::{Touch, TouchPhase, WindowEvent};

use crate::platform_impl::wayland::state::WinitState;
use crate::platform_impl::wayland::{self, DeviceId, WindowId};

impl TouchHandler for WinitState {
    fn down(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        touch: &WlTouch,
        serial: u32,
        time: u32,
        surface: WlSurface,
        id: i32,
        position: (f64, f64),
    ) {
        // ink-notes patch: a touch on the client-side decorations acts like a left click, so
        // the title bar buttons work with a finger and the window can be moved.
        let parent_surface = surface
            .data::<SurfaceData>()
            .and_then(|data| data.parent_surface().cloned());
        if let Some(parent_surface) = parent_surface.filter(|parent| *parent != surface) {
            // A palm resting on the title bar while writing must not move or close the window
            if crate::platform::wayland::decoration_touches_ignored() {
                return;
            }
            let window_id = wayland::make_wid(&parent_surface);
            let mut window = match self.windows.get_mut().get_mut(&window_id) {
                Some(window) => window.lock().unwrap(),
                None => return,
            };
            let seat = touch.seat();
            window.frame_point_moved(seat, &surface, Duration::ZERO, position.0, position.1);
            window.frame_click(
                FrameClick::Normal,
                true,
                seat,
                serial,
                Duration::from_millis(time as u64),
                window_id,
                &mut self.window_compositor_updates,
            );
            drop(window);
            let seat_state = self.seats.get_mut(&touch.seat().id()).unwrap();
            seat_state.touch_map.insert(
                id,
                TouchPoint {
                    surface,
                    location: LogicalPosition::<f64>::from(position),
                    frame_of: Some(window_id),
                },
            );
            return;
        }

        let window_id = wayland::make_wid(&surface);
        let scale_factor = match self.windows.get_mut().get(&window_id) {
            Some(window) => window.lock().unwrap().scale_factor(),
            None => return,
        };

        let location = LogicalPosition::<f64>::from(position);

        let seat_state = self.seats.get_mut(&touch.seat().id()).unwrap();

        // Update the state of the point.
        seat_state
            .touch_map
            .insert(id, TouchPoint { surface, location, frame_of: None });

        // ink-notes patch: keep the event time for `take_touch_time`
        crate::platform::wayland::push_touch_time(id as u64, TouchPhase::Started, time);

        self.events_sink.push_window_event(
            WindowEvent::Touch(Touch {
                device_id: crate::event::DeviceId(crate::platform_impl::DeviceId::Wayland(
                    DeviceId,
                )),
                phase: TouchPhase::Started,
                location: location.to_physical(scale_factor),
                force: None,
                id: id as u64,
            }),
            window_id,
        );
    }

    fn up(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        touch: &WlTouch,
        serial: u32,
        time: u32,
        id: i32,
    ) {
        let seat_state = self.seats.get_mut(&touch.seat().id()).unwrap();

        // Remove the touch point.
        let touch_point = match seat_state.touch_map.remove(&id) {
            Some(touch_point) => touch_point,
            None => return,
        };

        // ink-notes patch: releasing a touch on the decorations completes the click.
        if let Some(window_id) = touch_point.frame_of {
            if let Some(window) = self.windows.get_mut().get_mut(&window_id) {
                let mut window = window.lock().unwrap();
                window.frame_click(
                    FrameClick::Normal,
                    false,
                    touch.seat(),
                    serial,
                    Duration::from_millis(time as u64),
                    window_id,
                    &mut self.window_compositor_updates,
                );
                window.frame_point_left();
            }
            return;
        }

        let window_id = wayland::make_wid(&touch_point.surface);
        let scale_factor = match self.windows.get_mut().get(&window_id) {
            Some(window) => window.lock().unwrap().scale_factor(),
            None => return,
        };

        // ink-notes patch: keep the event time for `take_touch_time`
        crate::platform::wayland::push_touch_time(id as u64, TouchPhase::Ended, time);
        self.events_sink.push_window_event(
            WindowEvent::Touch(Touch {
                device_id: crate::event::DeviceId(crate::platform_impl::DeviceId::Wayland(
                    DeviceId,
                )),
                phase: TouchPhase::Ended,
                location: touch_point.location.to_physical(scale_factor),
                force: None,
                id: id as u64,
            }),
            window_id,
        );
    }

    fn motion(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        touch: &WlTouch,
        time: u32,
        id: i32,
        position: (f64, f64),
    ) {
        let seat_state = self.seats.get_mut(&touch.seat().id()).unwrap();

        // Remove the touch point.
        let touch_point = match seat_state.touch_map.get_mut(&id) {
            Some(touch_point) => touch_point,
            None => return,
        };

        // ink-notes patch: moving a touch on the decorations starts a pending window move.
        if let Some(window_id) = touch_point.frame_of {
            let surface = touch_point.surface.clone();
            if let Some(window) = self.windows.get_mut().get_mut(&window_id) {
                window.lock().unwrap().frame_point_moved(
                    touch.seat(),
                    &surface,
                    Duration::ZERO,
                    position.0,
                    position.1,
                );
            }
            return;
        }

        let window_id = wayland::make_wid(&touch_point.surface);
        let scale_factor = match self.windows.get_mut().get(&window_id) {
            Some(window) => window.lock().unwrap().scale_factor(),
            None => return,
        };

        touch_point.location = LogicalPosition::<f64>::from(position);

        // ink-notes patch: keep the event time for `take_touch_time`
        crate::platform::wayland::push_touch_time(id as u64, TouchPhase::Moved, time);

        self.events_sink.push_window_event(
            WindowEvent::Touch(Touch {
                device_id: crate::event::DeviceId(crate::platform_impl::DeviceId::Wayland(
                    DeviceId,
                )),
                phase: TouchPhase::Moved,
                location: touch_point.location.to_physical(scale_factor),
                force: None,
                id: id as u64,
            }),
            window_id,
        );
    }

    fn cancel(&mut self, _: &Connection, _: &QueueHandle<Self>, touch: &WlTouch) {
        let seat_state = self.seats.get_mut(&touch.seat().id()).unwrap();

        for (id, touch_point) in seat_state.touch_map.drain() {
            if touch_point.frame_of.is_some() {
                continue;
            }
            let window_id = wayland::make_wid(&touch_point.surface);
            let scale_factor = match self.windows.get_mut().get(&window_id) {
                Some(window) => window.lock().unwrap().scale_factor(),
                None => return,
            };

            let location = touch_point.location.to_physical(scale_factor);

            self.events_sink.push_window_event(
                WindowEvent::Touch(Touch {
                    device_id: crate::event::DeviceId(crate::platform_impl::DeviceId::Wayland(
                        DeviceId,
                    )),
                    phase: TouchPhase::Cancelled,
                    location,
                    force: None,
                    id: id as u64,
                }),
                window_id,
            );
        }
    }

    fn shape(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlTouch,
        _: i32,
        _: f64,
        _: f64,
    ) {
        // Blank.
    }

    fn orientation(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlTouch, _: i32, _: f64) {
        // Blank.
    }
}

/// The state of the touch point.
#[derive(Debug)]
pub struct TouchPoint {
    /// The surface on which the point is present.
    pub surface: WlSurface,

    /// The location of the point on the surface.
    pub location: LogicalPosition<f64>,

    /// ink-notes patch: the window whose decorations this point touches.
    pub frame_of: Option<WindowId>,
}

pub trait TouchDataExt {
    fn seat(&self) -> &WlSeat;
}

impl TouchDataExt for WlTouch {
    fn seat(&self) -> &WlSeat {
        self.data::<TouchData>()
            .expect("failed to get touch data.")
            .seat()
    }
}

sctk::delegate_touch!(WinitState);
