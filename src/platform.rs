//! Erkennung der Laufzeitumgebung und Wahl des Anzeigesystems (X11 oder Wayland).
//!
//! Unter WSL (WSLg) trennt der Wayland-Compositor die Verbindung, sobald die App ein Bild
//! über Vulkan übergibt ("Connection reset by peer"). Über X11 (Xwayland) läuft sie stabil,
//! daher wird unter WSL standardmäßig X11 verwendet. `INK_NOTES_DISPLAY=x11|wayland`
//! übersteuert die Wahl.

/// Anzeigesystem, mit dem die Ereignisschleife erzeugt wird.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayBackend {
    /// Automatische Wahl von winit (Wayland, falls verfügbar, sonst X11)
    Automatic,
    X11,
    Wayland,
}

/// Wählt das Anzeigesystem.
///
/// - `override_value`: Inhalt von `INK_NOTES_DISPLAY` (`x11` oder `wayland`, sonst ignoriert)
/// - `is_wsl`: läuft unter WSL
/// - `has_x11_display`: `DISPLAY` ist gesetzt (X11/Xwayland erreichbar)
pub fn choose_display_backend(
    override_value: Option<&str>,
    is_wsl: bool,
    has_x11_display: bool,
) -> DisplayBackend {
    match override_value
        .map(|v| v.trim().to_ascii_lowercase())
        .as_deref()
    {
        Some("x11") => return DisplayBackend::X11,
        Some("wayland") => return DisplayBackend::Wayland,
        _ => {}
    }
    if is_wsl && has_x11_display {
        DisplayBackend::X11
    } else {
        DisplayBackend::Automatic
    }
}

/// Läuft das Programm unter WSL (Windows-Subsystem für Linux)?
pub fn is_wsl() -> bool {
    if std::env::var_os("WSL_DISTRO_NAME").is_some() || std::env::var_os("WSL_INTEROP").is_some() {
        return true;
    }
    std::fs::read_to_string("/proc/sys/kernel/osrelease")
        .map(|release| release.to_ascii_lowercase().contains("microsoft"))
        .unwrap_or(false)
}

/// Wahl anhand der aktuellen Umgebung.
pub fn display_backend_from_env() -> DisplayBackend {
    choose_display_backend(
        std::env::var("INK_NOTES_DISPLAY").ok().as_deref(),
        is_wsl(),
        std::env::var_os("DISPLAY").is_some_and(|d| !d.is_empty()),
    )
}

/// Event loop for the desktop; on Linux with the display backend chosen above.
#[cfg(not(target_os = "android"))]
pub fn desktop_event_loop() -> Result<winit::event_loop::EventLoop<()>, winit::error::EventLoopError>
{
    #[allow(unused_mut)]
    let mut builder = winit::event_loop::EventLoopBuilder::new();
    #[cfg(target_os = "linux")]
    {
        use winit::platform::{wayland::EventLoopBuilderExtWayland, x11::EventLoopBuilderExtX11};
        match display_backend_from_env() {
            DisplayBackend::X11 => {
                eprintln!("[Anzeige] Verwende X11 (unter WSL Standard; übersteuerbar mit INK_NOTES_DISPLAY=wayland)");
                builder.with_x11();
            }
            DisplayBackend::Wayland => {
                eprintln!("[Anzeige] Verwende Wayland (INK_NOTES_DISPLAY=wayland)");
                builder.with_wayland();
            }
            DisplayBackend::Automatic => {}
        }
    }
    builder.build()
}

/// Gives the window the app id (Wayland: app_id, X11: WM_CLASS), so that dock and task bar
/// show the app icon for it.
pub fn with_app_id(
    builder: winit::window::WindowBuilder,
    #[allow(unused_variables)] app_id: &str,
) -> winit::window::WindowBuilder {
    #[cfg(target_os = "linux")]
    {
        let builder = winit::platform::wayland::WindowBuilderExtWayland::with_name(
            builder,
            app_id,
            "ink-notes",
        );
        winit::platform::x11::WindowBuilderExtX11::with_name(builder, app_id, "ink-notes")
    }
    #[cfg(not(target_os = "linux"))]
    builder
}

/// Ignores touches on the client-side title bar until `until` (palm rejection while the pen
/// is near; only Wayland draws its own title bar).
pub fn ignore_decoration_touches_until(
    #[allow(unused_variables)] until: Option<std::time::Instant>,
) {
    #[cfg(target_os = "linux")]
    winit::platform::wayland::ignore_decoration_touches_until(until);
}

/// Time of a touch event as reported by the compositor (milliseconds, Wayland only).
pub fn take_touch_time(
    #[allow(unused_variables)] id: u64,
    #[allow(unused_variables)] phase: winit::event::TouchPhase,
) -> Option<u32> {
    #[cfg(target_os = "linux")]
    return winit::platform::wayland::take_touch_time(id, phase);
    #[cfg(not(target_os = "linux"))]
    None
}

/// Bringt die Ereignisse eines Tablet-Frames in eine brauchbare Reihenfolge: octotablet meldet
/// das Aufsetzen des Stifts (`Down`) vor der Position desselben Frames (`Pose`). Bis dahin
/// gälte noch die alte Position – kam der Stift schnell heran (Annähern und Aufsetzen im selben
/// Frame), läge sie ganz woanders: Ein Tippen auf einen Knopf ginge daneben, ein Strich begänne
/// mit einer Linie von der alten Stelle. Hier folgt jedes `Down` auf die nächste `Pose`, spätestens
/// vor dem nächsten anderen Ereignis.
pub fn down_after_pose<T>(
    events: impl IntoIterator<Item = T>,
    is_down: impl Fn(&T) -> bool,
    is_pose: impl Fn(&T) -> bool,
) -> Vec<T> {
    let mut ordered = Vec::new();
    let mut deferred = None;
    for event in events {
        if is_down(&event) {
            if let Some(down) = deferred.replace(event) {
                ordered.push(down);
            }
            continue;
        }
        let pose = is_pose(&event);
        if !pose {
            ordered.extend(deferred.take());
        }
        ordered.push(event);
        if pose {
            ordered.extend(deferred.take());
        }
    }
    ordered.extend(deferred);
    ordered
}

/// System clipboard: arboard on the desktop; on Android not available yet (Issue #42).
pub mod clipboard {
    #[cfg(not(target_os = "android"))]
    pub use arboard::{Clipboard, ImageData};

    #[cfg(target_os = "android")]
    pub use android::{Clipboard, ImageData};

    #[cfg(target_os = "android")]
    mod android {
        use std::borrow::Cow;

        pub struct ImageData<'a> {
            pub width: usize,
            pub height: usize,
            pub bytes: Cow<'a, [u8]>,
        }

        impl ImageData<'_> {
            pub fn to_owned_img(&self) -> ImageData<'static> {
                ImageData {
                    width: self.width,
                    height: self.height,
                    bytes: Cow::Owned(self.bytes.to_vec()),
                }
            }
        }

        pub struct Clipboard;

        impl Clipboard {
            pub fn new() -> Result<Self, &'static str> {
                Err("not available on Android")
            }

            pub fn get_image(&mut self) -> Result<ImageData<'static>, &'static str> {
                Err("not available on Android")
            }

            pub fn get_text(&mut self) -> Result<String, &'static str> {
                Err("not available on Android")
            }

            pub fn set_image(&mut self, _image: ImageData) -> Result<(), &'static str> {
                Err("not available on Android")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_down_follows_pose_of_its_frame() {
        let order = |events: &str| {
            down_after_pose(events.chars(), |c| *c == 'D', |c| *c == 'P')
                .into_iter()
                .collect::<String>()
        };
        // In, Down, Pose, Button, Frame  ->  Down erst mit der neuen Position
        assert_eq!(order("IDPBF"), "IPDBF");
        // Down ohne Pose im Frame: vor dem Frame-Ende
        assert_eq!(order("DF"), "DF");
        assert_eq!(order("DBF"), "DBF");
        // Am Ende ohne weiteres Ereignis
        assert_eq!(order("PD"), "PD");
        // Andere Ereignisse bleiben unverändert
        assert_eq!(order("PFPFUF"), "PFPFUF");
        assert_eq!(order("IDPFPFUF"), "IPDFPFUF");
    }

    #[test]
    fn test_choose_display_backend() {
        // WSL mit X11-Display: X11 (Wayland unter WSLg bricht ab)
        assert_eq!(
            choose_display_backend(None, true, true),
            DisplayBackend::X11
        );
        // WSL ohne X11-Display: winit entscheiden lassen
        assert_eq!(
            choose_display_backend(None, true, false),
            DisplayBackend::Automatic
        );
        // Natives Linux: winit entscheiden lassen
        assert_eq!(
            choose_display_backend(None, false, true),
            DisplayBackend::Automatic
        );
        // Übersteuerung
        assert_eq!(
            choose_display_backend(Some("wayland"), true, true),
            DisplayBackend::Wayland
        );
        assert_eq!(
            choose_display_backend(Some(" X11 "), false, true),
            DisplayBackend::X11
        );
        assert_eq!(
            choose_display_backend(Some("quatsch"), true, true),
            DisplayBackend::X11
        );
    }
}
