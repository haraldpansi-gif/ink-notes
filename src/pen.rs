//! Pen input independent of the platform (Issue #44).
//!
//! On the desktop the pen comes from octotablet (Wayland/X11 tablet protocols, Windows Ink),
//! on Android as touch samples with the tool type stylus or eraser, which the patched winit
//! hands over separately (`winit::platform::android::take_pen_samples`). Both are turned into
//! the same [`PenInput`] events, which the app handles in one place.

use std::time::{Duration, Instant};

/// What happened to the pen.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PenEvent {
    /// Pen comes into range (hovers above the screen)
    In,
    /// Pen left the range
    Out,
    /// Tip touches the screen (at the position of the last `Pose`)
    Down,
    /// Tip lifted
    Up,
    /// Position in logical pixels; pressure 0..=1 if known
    Pose {
        position: [f32; 2],
        pressure: Option<f32>,
    },
    /// Pen button pressed or released; `key` identifies the button across sessions
    Button { key: u64, pressed: bool },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PenInput {
    /// Identifies the pen (several pens can be in use on the desktop)
    pub tool: u64,
    /// Eraser end of the pen
    pub is_eraser: bool,
    pub event: PenEvent,
}

fn hash_of(value: impl std::hash::Hash) -> u64 {
    use std::hash::Hasher;
    // DefaultHasher::new() uses fixed keys: same input, same value in every session
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

/// Turns octotablet's events into [`PenInput`]s, with each `Down` after the `Pose` of the
/// same frame (see [`crate::platform::down_after_pose`]).
#[cfg(not(target_os = "android"))]
pub fn from_octotablet<'a>(
    events: impl IntoIterator<Item = octotablet::events::Event<'a>>,
) -> Vec<PenInput> {
    use octotablet::events::{Event, ToolEvent};

    let inputs = events.into_iter().filter_map(|event| {
        let Event::Tool { tool, event } = event else {
            return None;
        };
        let event = match event {
            ToolEvent::In { .. } => PenEvent::In,
            ToolEvent::Out | ToolEvent::Removed => PenEvent::Out,
            ToolEvent::Down => PenEvent::Down,
            ToolEvent::Up => PenEvent::Up,
            ToolEvent::Pose(pose) => PenEvent::Pose {
                position: pose.position,
                pressure: pose.pressure.get(),
            },
            ToolEvent::Button { button_id, pressed } => PenEvent::Button {
                key: hash_of(button_id),
                pressed,
            },
            _ => return None,
        };
        Some(PenInput {
            tool: hash_of(tool.id()),
            is_eraser: tool.tool_type == Some(octotablet::tool::Type::Eraser),
            event,
        })
    });
    crate::platform::down_after_pose(
        inputs,
        |i| i.event == PenEvent::Down,
        |i| matches!(i.event, PenEvent::Pose { .. }),
    )
}

/// What an Android pen sample reports (mirrors `winit::platform::android::PenAction`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleAction {
    HoverEnter,
    HoverMove,
    HoverExit,
    Down,
    Move,
    Up,
    Cancel,
}

/// One Android pen sample (mirrors `winit::platform::android::PenSample`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sample {
    pub action: SampleAction,
    /// Physical pixels
    pub x: f32,
    pub y: f32,
    pub pressure: f32,
    pub eraser: bool,
    /// `MotionEvent.getButtonState()`
    pub buttons: u32,
}

#[cfg(target_os = "android")]
impl From<winit::platform::android::PenSample> for Sample {
    fn from(s: winit::platform::android::PenSample) -> Self {
        use winit::platform::android::PenAction as A;
        Self {
            action: match s.action {
                A::HoverEnter => SampleAction::HoverEnter,
                A::HoverMove => SampleAction::HoverMove,
                A::HoverExit => SampleAction::HoverExit,
                A::Down => SampleAction::Down,
                A::Move => SampleAction::Move,
                A::Up => SampleAction::Up,
                A::Cancel => SampleAction::Cancel,
            },
            x: s.x,
            y: s.y,
            pressure: s.pressure,
            eraser: s.eraser,
            buttons: s.buttons,
        }
    }
}

/// The two pen buttons of Android's `MotionEvent` (`BUTTON_STYLUS_PRIMARY`/`SECONDARY`).
const ANDROID_BUTTONS: [(u32, &str); 2] = [
    (0x20, "android-stylus-primary"),
    (0x40, "android-stylus-secondary"),
];

/// How long the pen may be out of sight before it counts as gone. Android reports
/// `HoverExit` right before the tip touches down, and pens without hover report nothing
/// after lifting.
const ANDROID_OUT_DELAY: Duration = Duration::from_millis(150);

/// Turns Android pen samples into [`PenInput`]s.
#[derive(Debug, Default)]
pub struct AndroidPen {
    in_range: bool,
    down: bool,
    eraser: bool,
    buttons: u32,
    /// Since when the pen is neither hovering nor touching
    gone_since: Option<Instant>,
}

impl AndroidPen {
    /// The single pen Android reports
    const TOOL: u64 = 1;

    fn push(&self, out: &mut Vec<PenInput>, event: PenEvent) {
        out.push(PenInput {
            tool: Self::TOOL,
            is_eraser: self.eraser,
            event,
        });
    }

    /// Events for `samples` (in this order); `scale` converts physical to logical pixels.
    pub fn convert(&mut self, samples: &[Sample], scale: f32, now: Instant) -> Vec<PenInput> {
        let mut out = Vec::new();
        for sample in samples {
            self.eraser = sample.eraser;
            for (mask, name) in ANDROID_BUTTONS {
                let pressed = sample.buttons & mask != 0;
                if pressed != (self.buttons & mask != 0) {
                    self.push(
                        &mut out,
                        PenEvent::Button {
                            key: hash_of(name),
                            pressed,
                        },
                    );
                }
            }
            self.buttons = sample.buttons;

            let position = [sample.x / scale, sample.y / scale];
            let touching = Some(sample.pressure.clamp(0.0, 1.0));
            match sample.action {
                SampleAction::HoverEnter | SampleAction::HoverMove => {
                    self.gone_since = None;
                    self.enter(&mut out);
                    let pose = PenEvent::Pose {
                        position,
                        pressure: None,
                    };
                    self.push(&mut out, pose);
                }
                SampleAction::HoverExit => {
                    if !self.down {
                        self.gone_since.get_or_insert(now);
                    }
                }
                SampleAction::Down => {
                    self.gone_since = None;
                    self.enter(&mut out);
                    let pose = PenEvent::Pose {
                        position,
                        pressure: touching,
                    };
                    self.push(&mut out, pose);
                    if !self.down {
                        self.down = true;
                        self.push(&mut out, PenEvent::Down);
                    }
                }
                SampleAction::Move => {
                    self.gone_since = None;
                    self.enter(&mut out);
                    let pressure = if self.down { touching } else { None };
                    self.push(&mut out, PenEvent::Pose { position, pressure });
                }
                SampleAction::Up | SampleAction::Cancel => {
                    if self.down {
                        if sample.action == SampleAction::Up {
                            let pose = PenEvent::Pose {
                                position,
                                pressure: touching,
                            };
                            self.push(&mut out, pose);
                        }
                        self.down = false;
                        self.push(&mut out, PenEvent::Up);
                    }
                    self.gone_since = Some(now);
                }
            }
        }
        out.extend(self.poll(now));
        out
    }

    fn enter(&mut self, out: &mut Vec<PenInput>) {
        if !self.in_range {
            self.in_range = true;
            self.push(out, PenEvent::In);
        }
    }

    /// `Out` once the pen has been gone for a moment; call regularly while it is in range.
    pub fn poll(&mut self, now: Instant) -> Vec<PenInput> {
        let mut out = Vec::new();
        let gone = self
            .gone_since
            .is_some_and(|since| now.duration_since(since) >= ANDROID_OUT_DELAY);
        if gone && self.in_range && !self.down {
            self.in_range = false;
            self.gone_since = None;
            if self.buttons != 0 {
                for (mask, name) in ANDROID_BUTTONS {
                    if self.buttons & mask != 0 {
                        self.push(
                            &mut out,
                            PenEvent::Button {
                                key: hash_of(name),
                                pressed: false,
                            },
                        );
                    }
                }
                self.buttons = 0;
            }
            self.push(&mut out, PenEvent::Out);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(action: SampleAction, x: f32, pressure: f32, buttons: u32) -> Sample {
        Sample {
            action,
            x,
            y: 10.0,
            pressure,
            eraser: false,
            buttons,
        }
    }

    fn kinds(inputs: &[PenInput]) -> Vec<&'static str> {
        inputs
            .iter()
            .map(|i| match i.event {
                PenEvent::In => "in",
                PenEvent::Out => "out",
                PenEvent::Down => "down",
                PenEvent::Up => "up",
                PenEvent::Pose { pressure: None, .. } => "hover",
                PenEvent::Pose { .. } => "pose",
                PenEvent::Button { pressed: true, .. } => "button-down",
                PenEvent::Button { .. } => "button-up",
            })
            .collect()
    }

    #[test]
    fn test_android_stroke_with_hover() {
        use SampleAction::*;
        let mut pen = AndroidPen::default();
        let t = Instant::now();
        let ms = |n| t + Duration::from_millis(n);

        let out = pen.convert(&[sample(HoverEnter, 20.0, 0.0, 0)], 2.0, t);
        assert_eq!(kinds(&out), ["in", "hover"]);
        assert_eq!(
            out[1].event,
            PenEvent::Pose {
                position: [10.0, 5.0],
                pressure: None
            }
        );

        // Android ends the hover right before the tip touches: no Out in between
        let out = pen.convert(&[sample(HoverExit, 20.0, 0.0, 0)], 2.0, ms(5));
        assert!(out.is_empty());
        let out = pen.convert(
            &[sample(Down, 20.0, 0.4, 0), sample(Move, 30.0, 0.5, 0)],
            2.0,
            ms(10),
        );
        assert_eq!(kinds(&out), ["pose", "down", "pose"]);
        assert!(pen.poll(ms(500)).is_empty(), "still touching");

        let out = pen.convert(&[sample(Up, 30.0, 0.0, 0)], 2.0, ms(600));
        assert_eq!(kinds(&out), ["pose", "up"]);
        let out = pen.convert(&[sample(HoverEnter, 30.0, 0.0, 0)], 2.0, ms(610));
        assert_eq!(kinds(&out), ["hover"]);

        pen.convert(&[sample(HoverExit, 30.0, 0.0, 0)], 2.0, ms(700));
        assert!(pen.poll(ms(750)).is_empty());
        assert_eq!(kinds(&pen.poll(ms(900))), ["out"]);
    }

    #[test]
    fn test_android_pen_without_hover_leaves_after_lifting() {
        use SampleAction::*;
        let mut pen = AndroidPen::default();
        let t = Instant::now();
        let out = pen.convert(&[sample(Down, 0.0, 0.7, 0)], 1.0, t);
        assert_eq!(kinds(&out), ["in", "pose", "down"]);
        pen.convert(&[sample(Up, 0.0, 0.0, 0)], 1.0, t);
        assert_eq!(kinds(&pen.poll(t + Duration::from_millis(200))), ["out"]);
    }

    #[test]
    fn test_android_buttons_and_cancel() {
        use SampleAction::*;
        let mut pen = AndroidPen::default();
        let t = Instant::now();
        let out = pen.convert(
            &[
                sample(HoverMove, 0.0, 0.0, 0x20),
                sample(Down, 0.0, 0.5, 0x20),
                sample(Cancel, 0.0, 0.0, 0x20),
            ],
            1.0,
            t,
        );
        assert_eq!(
            kinds(&out),
            ["button-down", "in", "hover", "pose", "down", "up"]
        );
        // Pen gone with the button still held: released before Out
        let out = pen.poll(t + Duration::from_millis(200));
        assert_eq!(kinds(&out), ["button-up", "out"]);
    }
}
