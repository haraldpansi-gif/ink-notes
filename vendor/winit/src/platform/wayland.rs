//! # Wayland
//!
//! **Note:** Windows don't appear on Wayland until you draw/present to them.
//!
//! By default, Winit loads system libraries using `dlopen`. This can be
//! disabled by disabling the `"wayland-dlopen"` cargo feature.
//!
//! ## Client-side decorations
//!
//! Winit provides client-side decorations by default, but the behaviour can
//! be controlled with the following feature flags:
//!
//! * `wayland-csd-adwaita` (default).
//! * `wayland-csd-adwaita-crossfont`.
//! * `wayland-csd-adwaita-notitle`.

use std::ffi::c_void;
use std::ptr::NonNull;

use crate::event_loop::{ActiveEventLoop, EventLoop, EventLoopBuilder};
use crate::monitor::MonitorHandle;
use crate::window::{Window, WindowAttributes};

pub use crate::window::Theme;

/// ink-notes patch: touches on the client-side decorations that start before this instant
/// are ignored (palm rejection while a pen is near the screen); `None` accepts them.
static IGNORE_DECORATION_TOUCHES_UNTIL: std::sync::Mutex<Option<std::time::Instant>> =
    std::sync::Mutex::new(None);

/// ink-notes patch: ignore touches on the client-side decorations until `until`.
pub fn ignore_decoration_touches_until(until: Option<std::time::Instant>) {
    *IGNORE_DECORATION_TOUCHES_UNTIL
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = until;
}

/// ink-notes patch: whether a touch on the decorations is ignored right now.
pub(crate) fn decoration_touches_ignored() -> bool {
    IGNORE_DECORATION_TOUCHES_UNTIL
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .is_some_and(|until| std::time::Instant::now() < until)
}

/// ink-notes patch: compositor timestamps (ms) of the touch events not yet taken, in event order.
static TOUCH_TIMES: std::sync::Mutex<
    std::collections::VecDeque<(u64, crate::event::TouchPhase, u32)>,
> = std::sync::Mutex::new(std::collections::VecDeque::new());

/// ink-notes patch: remembers the compositor time of a touch event that is about to be sent.
pub(crate) fn push_touch_time(id: u64, phase: crate::event::TouchPhase, time: u32) {
    let mut times = TOUCH_TIMES.lock().unwrap_or_else(|e| e.into_inner());
    // Nobody takes them (e.g. an app that never asks): keep only the latest
    if times.len() >= 256 {
        times.pop_front();
    }
    times.push_back((id, phase, time));
}

/// ink-notes patch: compositor time in milliseconds (arbitrary origin, wraps around) of the
/// touch event with this id and phase (`Started`, `Moved` or `Ended`; `Cancelled` has none),
/// for the event currently being handled. Call it once for every such `WindowEvent::Touch`.
/// `None` on X11 or if the time is unknown.
pub fn take_touch_time(id: u64, phase: crate::event::TouchPhase) -> Option<u32> {
    let mut times = TOUCH_TIMES.lock().unwrap_or_else(|e| e.into_inner());
    // Older entries that were never taken are skipped
    while let Some((i, p, time)) = times.pop_front() {
        if i == id && p == phase {
            return Some(time);
        }
    }
    None
}

/// Additional methods on [`ActiveEventLoop`] that are specific to Wayland.
pub trait ActiveEventLoopExtWayland {
    /// True if the [`ActiveEventLoop`] uses Wayland.
    fn is_wayland(&self) -> bool;
}

impl ActiveEventLoopExtWayland for ActiveEventLoop {
    #[inline]
    fn is_wayland(&self) -> bool {
        self.p.is_wayland()
    }
}

/// Additional methods on [`EventLoop`] that are specific to Wayland.
pub trait EventLoopExtWayland {
    /// True if the [`EventLoop`] uses Wayland.
    fn is_wayland(&self) -> bool;
}

impl<T: 'static> EventLoopExtWayland for EventLoop<T> {
    #[inline]
    fn is_wayland(&self) -> bool {
        self.event_loop.is_wayland()
    }
}

/// Additional methods on [`EventLoopBuilder`] that are specific to Wayland.
pub trait EventLoopBuilderExtWayland {
    /// Force using Wayland.
    fn with_wayland(&mut self) -> &mut Self;

    /// Whether to allow the event loop to be created off of the main thread.
    ///
    /// By default, the window is only allowed to be created on the main
    /// thread, to make platform compatibility easier.
    fn with_any_thread(&mut self, any_thread: bool) -> &mut Self;
}

impl<T> EventLoopBuilderExtWayland for EventLoopBuilder<T> {
    #[inline]
    fn with_wayland(&mut self) -> &mut Self {
        self.platform_specific.forced_backend = Some(crate::platform_impl::Backend::Wayland);
        self
    }

    #[inline]
    fn with_any_thread(&mut self, any_thread: bool) -> &mut Self {
        self.platform_specific.any_thread = any_thread;
        self
    }
}

/// Additional methods on [`Window`] that are specific to Wayland.
///
/// [`Window`]: crate::window::Window
pub trait WindowExtWayland {
    /// Returns `xdg_toplevel` of the window or [`None`] if the window is X11 window.
    fn xdg_toplevel(&self) -> Option<NonNull<c_void>>;
}

impl WindowExtWayland for Window {
    #[inline]
    fn xdg_toplevel(&self) -> Option<NonNull<c_void>> {
        #[allow(clippy::single_match)]
        match &self.window {
            #[cfg(x11_platform)]
            crate::platform_impl::Window::X(_) => None,
            #[cfg(wayland_platform)]
            crate::platform_impl::Window::Wayland(window) => window.xdg_toplevel(),
        }
    }
}

/// Additional methods on [`WindowAttributes`] that are specific to Wayland.
pub trait WindowAttributesExtWayland {
    /// Build window with the given name.
    ///
    /// The `general` name sets an application ID, which should match the `.desktop`
    /// file distributed with your program. The `instance` is a `no-op`.
    ///
    /// For details about application ID conventions, see the
    /// [Desktop Entry Spec](https://specifications.freedesktop.org/desktop-entry-spec/desktop-entry-spec-latest.html#desktop-file-id)
    fn with_name(self, general: impl Into<String>, instance: impl Into<String>) -> Self;
}

impl WindowAttributesExtWayland for WindowAttributes {
    #[inline]
    fn with_name(mut self, general: impl Into<String>, instance: impl Into<String>) -> Self {
        self.platform_specific.name =
            Some(crate::platform_impl::ApplicationName::new(general.into(), instance.into()));
        self
    }
}

/// Additional methods on `MonitorHandle` that are specific to Wayland.
pub trait MonitorHandleExtWayland {
    /// Returns the inner identifier of the monitor.
    fn native_id(&self) -> u32;
}

impl MonitorHandleExtWayland for MonitorHandle {
    #[inline]
    fn native_id(&self) -> u32 {
        self.inner.native_identifier()
    }
}
