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

#[cfg(test)]
mod tests {
    use super::*;

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
