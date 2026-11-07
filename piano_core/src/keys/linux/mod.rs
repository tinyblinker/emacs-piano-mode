//! Linux backends: X11 key-map polling and the raw evdev ABI.

pub mod evdev;
pub mod x11;

/// The active Linux backend.
pub enum Backend {
    X11(x11::X11),
    Evdev(evdev::Evdev),
}

impl Backend {
    /// Create the backend selected by `choice`.
    pub fn new(names: &[char], choice: crate::keys::Backend) -> Result<Self, String> {
        match choice {
            crate::keys::Backend::X11 => Ok(Self::X11(x11::X11::new(names)?)),
            crate::keys::Backend::Evdev => Ok(Self::Evdev(evdev::Evdev::autodetect(names)?)),
            crate::keys::Backend::Auto => {
                if wayland_session() {
                    // Under a native Wayland session XWayland only
                    // knows key state while an X11 client is focused,
                    // so the evdev backend is the reliable choice.
                    Ok(Self::Evdev(evdev::Evdev::autodetect(names)?))
                } else {
                    match x11::X11::new(names) {
                        Ok(x11) => Ok(Self::X11(x11)),
                        Err(x11_error) => match evdev::Evdev::autodetect(names) {
                            Ok(evdev) => Ok(Self::Evdev(evdev)),
                            Err(evdev_error) => Err(format!(
                                "X11 backend failed: {x11_error}; evdev backend failed: {evdev_error}"
                            )),
                        },
                    }
                }
            }
        }
    }

    /// Poll the keyboard state.
    pub fn poll(&mut self) -> Result<Vec<bool>, String> {
        match self {
            Self::X11(backend) => backend.poll(),
            Self::Evdev(backend) => backend.poll(),
        }
    }
}

/// Whether this looks like a native Wayland session.
fn wayland_session() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some()
        || std::env::var("XDG_SESSION_TYPE").is_ok_and(|value| value == "wayland")
}
