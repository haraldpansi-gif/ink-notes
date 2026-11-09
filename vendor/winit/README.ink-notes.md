# winit 0.29.15 for ink-notes

Copy of [winit 0.29.15](https://crates.io/crates/winit/0.29.15) (Apache-2.0, see
`LICENSE`), used through `[patch.crates-io]` in the top-level `Cargo.toml`. Examples, tests and
docs of the original crate were removed.

## Changes

- `src/platform_impl/linux/wayland/seat/touch/mod.rs`: on Wayland, winit draws the title bar
  itself (client-side decorations) but only handles the mouse there. Touches on the title bar
  are now passed to the frame like a left click: close, minimize and maximize work with a
  finger, and dragging the title bar moves the window (Issue #36). All changes are marked with
  `ink-notes patch`.
- `src/platform/wayland.rs`: `ignore_decoration_touches_until` lets ink-notes ignore touches on
  the title bar while the pen is near the screen, so a resting palm does not move the window
  (Issue #39).
- `src/platform/wayland.rs` and the touch handler: `take_touch_time` returns the compositor
  timestamp of each touch event (Started, Moved, Ended), so ink-notes measures the swipe speed
  for gliding with the real event times and not with the time it got around to handling them
  (Issue #28).
- `src/platform_impl/linux/wayland/event_loop/mod.rs`: every read from the Wayland socket wakes
  the app (`AboutToWait`), also when the events belong to another queue on the same connection.
  octotablet's pen events live on such a queue and only `dispatch_pending` it, so before, pen
  strokes waited until something else woke the loop (Issue #53).
- `src/platform_impl/linux/wayland/seat/touch/mod.rs`: a touch lift (`wl_touch.up`) is handed
  on right away instead of waiting for the next `wl_touch.frame`. On a GNOME laptop the lift
  of a swipe only arrived with the next touch, which fits a frame sent before the lift: the page did not glide after a
  swipe (Issue #28), and a ruler moved with the finger followed the hovering pen (Issue #55).
- `Cargo.toml`: `[lints.rust] warnings = "allow"`, because a path dependency is not built with
  capped lints like a crates.io dependency.

Remove the copy and the `[patch.crates-io]` entry when ink-notes moves to a winit version that
handles touch on its decorations.
