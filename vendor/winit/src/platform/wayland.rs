use crate::{
    event_loop::{EventLoopBuilder, EventLoopWindowTarget},
    monitor::MonitorHandle,
    window::{Window, WindowBuilder},
};

use crate::platform_impl::{ApplicationName, Backend};

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

/// Additional methods on [`EventLoopWindowTarget`] that are specific to Wayland.
pub trait EventLoopWindowTargetExtWayland {
    /// True if the [`EventLoopWindowTarget`] uses Wayland.
    fn is_wayland(&self) -> bool;
}

impl<T> EventLoopWindowTargetExtWayland for EventLoopWindowTarget<T> {
    #[inline]
    fn is_wayland(&self) -> bool {
        self.p.is_wayland()
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
        self.platform_specific.forced_backend = Some(Backend::Wayland);
        self
    }

    #[inline]
    fn with_any_thread(&mut self, any_thread: bool) -> &mut Self {
        self.platform_specific.any_thread = any_thread;
        self
    }
}

/// Additional methods on [`Window`] that are specific to Wayland.
pub trait WindowExtWayland {}

impl WindowExtWayland for Window {}

/// Additional methods on [`WindowBuilder`] that are specific to Wayland.
pub trait WindowBuilderExtWayland {
    /// Build window with the given name.
    ///
    /// The `general` name sets an application ID, which should match the `.desktop`
    /// file destributed with your program. The `instance` is a `no-op`.
    ///
    /// For details about application ID conventions, see the
    /// [Desktop Entry Spec](https://specifications.freedesktop.org/desktop-entry-spec/desktop-entry-spec-latest.html#desktop-file-id)
    fn with_name(self, general: impl Into<String>, instance: impl Into<String>) -> Self;
}

impl WindowBuilderExtWayland for WindowBuilder {
    #[inline]
    fn with_name(mut self, general: impl Into<String>, instance: impl Into<String>) -> Self {
        self.platform_specific.name = Some(ApplicationName::new(general.into(), instance.into()));
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
