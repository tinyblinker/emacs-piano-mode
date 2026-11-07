//! macOS backend: poll `CGEventSourceKeyState` from CoreGraphics.
//!
//! Uses the combined-session event source.  Depending on the macOS
//! version, System Settings may ask for Input Monitoring permission.

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGEventSourceKeyState(state_id: i32, key: u16) -> bool;
}

/// kCGEventSourceStateCombinedSessionState
const COMBINED_SESSION: i32 = 0;

pub fn is_pressed(key: char) -> bool {
    unsafe { CGEventSourceKeyState(COMBINED_SESSION, super::macos_virtual_key(key)) }
}
