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
- `Cargo.toml`: `[lints.rust] warnings = "allow"`, because a path dependency is not built with
  capped lints like a crates.io dependency.

Remove the copy and the `[patch.crates-io]` entry when ink-notes moves to a winit version that
handles touch on its decorations.
