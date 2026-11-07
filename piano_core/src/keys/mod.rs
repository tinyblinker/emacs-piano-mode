//! Cross-platform keyboard readers with zero external crates.
//!
//! The platform backends poll the operating system's keyboard state,
//! so both key presses and key releases are observed (a release is
//! detected with poll-interval latency):
//!
//! - Linux, X11 sessions: `XQueryKeymap` (libX11 loaded via dlopen).
//! - Linux, Wayland sessions: the kernel evdev ABI (`/dev/input`).
//! - Windows: `GetAsyncKeyState` from user32.dll.
//! - macOS: `CGEventSourceKeyState` from the CoreGraphics framework.
//!
//! None of these backends require root or special group membership
//! beyond what a desktop login already grants.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

#[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
compile_error!("piano-keys supports Linux, Windows and macOS only");

/// Key names accepted by `--keys`.
pub const SUPPORTED_KEYS: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789;,./[]'\\=-";

/// Which platform backend to use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    /// Pick the backend automatically for the current session.
    Auto,
    /// Linux: X11 `XQueryKeymap`.
    X11,
    /// Linux: raw evdev ABI from `/dev/input`.
    Evdev,
}

impl Backend {
    /// Parse a `--backend` argument.
    pub fn parse(name: &str) -> Result<Self, String> {
        match name {
            "auto" => Ok(Self::Auto),
            "x11" => Ok(Self::X11),
            "evdev" => Ok(Self::Evdev),
            other => Err(format!(
                "unknown backend {other:?} (expected x11, evdev or auto)"
            )),
        }
    }
}

/// Parse a single key name, e.g. `"a"`, `"A"` or `";"`.
pub fn parse_key(name: &str) -> Option<char> {
    let mut chars = name.chars();
    let key = chars.next()?;
    if chars.next().is_some() {
        return None; // more than one character
    }
    let key = key.to_ascii_uppercase();
    if SUPPORTED_KEYS.contains(key) {
        Some(key)
    } else {
        None
    }
}

/// Parse a comma-separated `--keys` list, sorted and deduplicated.
pub fn parse_keys(list: &str) -> Result<Vec<char>, String> {
    let mut keys = Vec::new();
    for item in list.split(',') {
        let item = item.trim();
        if item.is_empty() {
            continue;
        }
        match parse_key(item) {
            Some(key) => keys.push(key),
            None => {
                return Err(format!(
                    "unsupported key name {item:?}; supported names: {SUPPORTED_KEYS}"
                ));
            }
        }
    }
    keys.sort_unstable();
    keys.dedup();
    if keys.is_empty() {
        Err("empty key list".into())
    } else {
        Ok(keys)
    }
}

/// The default watched keys: every letter and digit.
pub fn default_keys() -> Vec<char> {
    ('A'..='Z').chain('0'..='9').collect()
}

/// Linux evdev key code (`KEY_*` from input-event-codes.h) for a key
/// name, or `None` when the name has no keyboard mapping.
#[cfg(any(test, target_os = "linux"))]
pub fn linux_evdev_keycode(key: char) -> Option<u16> {
    let code = match key {
        'A' => 30,
        'B' => 48,
        'C' => 46,
        'D' => 32,
        'E' => 18,
        'F' => 33,
        'G' => 34,
        'H' => 35,
        'I' => 23,
        'J' => 36,
        'K' => 37,
        'L' => 38,
        'M' => 50,
        'N' => 49,
        'O' => 24,
        'P' => 25,
        'Q' => 16,
        'R' => 19,
        'S' => 31,
        'T' => 20,
        'U' => 22,
        'V' => 47,
        'W' => 17,
        'X' => 45,
        'Y' => 21,
        'Z' => 44,
        '0' => 11,
        '1' => 2,
        '2' => 3,
        '3' => 4,
        '4' => 5,
        '5' => 6,
        '6' => 7,
        '7' => 8,
        '8' => 9,
        '9' => 10,
        ';' => 39,
        ',' => 51,
        '.' => 52,
        '/' => 53,
        '[' => 26,
        ']' => 27,
        '\'' => 40,
        '\\' => 43,
        '=' => 13,
        '-' => 12,
        _ => return None,
    };
    Some(code)
}

/// Windows virtual key code (`VK_*`) for a key name.
///
/// Letters and digits are layout independent; the OEM punctuation
/// codes assume a US keyboard layout.
#[cfg(any(test, target_os = "windows"))]
pub fn windows_virtual_key(key: char) -> i32 {
    match key {
        'A'..='Z' | '0'..='9' => key as i32,
        ';' => 0xBA,  // VK_OEM_1
        ',' => 0xBC,  // VK_OEM_COMMA
        '.' => 0xBE,  // VK_OEM_PERIOD
        '/' => 0xBF,  // VK_OEM_2
        '[' => 0xDB,  // VK_OEM_4
        ']' => 0xDD,  // VK_OEM_6
        '\'' => 0xDE, // VK_OEM_7
        '\\' => 0xDC, // VK_OEM_5
        '=' => 0xBB,  // VK_OEM_PLUS
        '-' => 0xBD,  // VK_OEM_MINUS
        _ => 0,
    }
}

/// macOS virtual key code (`kVK_ANSI_*`) for a key name.
///
/// These are physical key codes, independent of the keyboard layout.
#[cfg(any(test, target_os = "macos"))]
pub fn macos_virtual_key(key: char) -> u16 {
    match key {
        'A' => 0x00,
        'S' => 0x01,
        'D' => 0x02,
        'F' => 0x03,
        'H' => 0x04,
        'G' => 0x05,
        'Z' => 0x06,
        'X' => 0x07,
        'C' => 0x08,
        'V' => 0x09,
        'B' => 0x0B,
        'Q' => 0x0C,
        'W' => 0x0D,
        'E' => 0x0E,
        'R' => 0x0F,
        'Y' => 0x10,
        'T' => 0x11,
        '1' => 0x12,
        '2' => 0x13,
        '3' => 0x14,
        '4' => 0x15,
        '6' => 0x16,
        '5' => 0x17,
        '=' => 0x18,
        '9' => 0x19,
        '7' => 0x1A,
        '-' => 0x1B,
        '8' => 0x1C,
        '0' => 0x1D,
        ']' => 0x1E,
        'O' => 0x1F,
        'U' => 0x20,
        '[' => 0x21,
        'I' => 0x22,
        'P' => 0x23,
        'L' => 0x25,
        'J' => 0x26,
        '\'' => 0x27,
        'K' => 0x28,
        ';' => 0x29,
        '\\' => 0x2A,
        ',' => 0x2B,
        '/' => 0x2C,
        'N' => 0x2D,
        'M' => 0x2E,
        '.' => 0x2F,
        _ => 0,
    }
}

/// Keyboard reader for the current platform.
pub struct Keyboard {
    #[cfg(not(target_os = "linux"))]
    names: Vec<char>,
    #[cfg(target_os = "linux")]
    backend: linux::Backend,
}

impl Keyboard {
    /// Create a reader watching `names`.
    #[cfg(target_os = "linux")]
    pub fn new(names: &[char], backend: Backend) -> Result<Self, String> {
        Ok(Self {
            backend: linux::Backend::new(names, backend)?,
        })
    }

    /// Create a reader watching `names`.
    #[cfg(not(target_os = "linux"))]
    pub fn new(names: &[char], backend: Backend) -> Result<Self, String> {
        if backend != Backend::Auto {
            return Err("--backend is only supported on Linux".into());
        }
        Ok(Self {
            names: names.to_vec(),
        })
    }

    /// Poll the keyboard; returns one pressed flag per watched key.
    #[cfg(target_os = "linux")]
    pub fn poll(&mut self) -> Result<Vec<bool>, String> {
        self.backend.poll()
    }

    /// Poll the keyboard; returns one pressed flag per watched key.
    #[cfg(target_os = "windows")]
    pub fn poll(&mut self) -> Result<Vec<bool>, String> {
        Ok(self
            .names
            .iter()
            .map(|&key| windows::is_pressed(key))
            .collect())
    }

    /// Poll the keyboard; returns one pressed flag per watched key.
    #[cfg(target_os = "macos")]
    pub fn poll(&mut self) -> Result<Vec<bool>, String> {
        Ok(self
            .names
            .iter()
            .map(|&key| macos::is_pressed(key))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_normalizes_key_names() {
        assert_eq!(parse_key("a"), Some('A'));
        assert_eq!(parse_key("9"), Some('9'));
        assert_eq!(parse_key(";"), Some(';'));
        assert_eq!(parse_key("ab"), None);
        assert_eq!(parse_key("?"), None);
        assert_eq!(parse_key("RET"), None);
    }

    #[test]
    fn parses_sorts_and_dedups_key_lists() {
        assert_eq!(parse_keys("W,A,w").unwrap(), vec!['A', 'W']);
        assert_eq!(parse_keys(" A , B , C ").unwrap(), vec!['A', 'B', 'C']);
        assert!(parse_keys("A,?").is_err());
        assert!(parse_keys("").is_err());
    }

    #[test]
    fn default_keys_cover_all_letters_and_digits() {
        assert_eq!(default_keys().len(), 36);
        assert_eq!(default_keys()[0], 'A');
        assert_eq!(default_keys()[35], '9');
    }

    #[test]
    fn linux_evdev_codes_match_the_kernel_abi() {
        assert_eq!(linux_evdev_keycode('A'), Some(30));
        assert_eq!(linux_evdev_keycode('Z'), Some(44));
        assert_eq!(linux_evdev_keycode('1'), Some(2));
        assert_eq!(linux_evdev_keycode('0'), Some(11));
        assert_eq!(linux_evdev_keycode(';'), Some(39));
        assert_eq!(linux_evdev_keycode('-'), Some(12));
    }

    #[test]
    fn windows_virtual_keys_match_user32() {
        assert_eq!(windows_virtual_key('A'), 0x41);
        assert_eq!(windows_virtual_key('9'), 0x39);
        assert_eq!(windows_virtual_key(';'), 0xBA);
    }

    #[test]
    fn macos_virtual_keys_match_coregraphics() {
        assert_eq!(macos_virtual_key('A'), 0x00);
        assert_eq!(macos_virtual_key('W'), 0x0D);
        assert_eq!(macos_virtual_key(';'), 0x29);
        assert_eq!(macos_virtual_key('1'), 0x12);
    }

    #[test]
    fn parses_backend_names() {
        assert_eq!(Backend::parse("auto").unwrap(), Backend::Auto);
        assert_eq!(Backend::parse("evdev").unwrap(), Backend::Evdev);
        assert!(Backend::parse("xinput").is_err());
    }
}
