//! Windows backend: poll `GetAsyncKeyState` from user32.dll.
//!
//! Works without any special privileges and even when the piano
//! process is not focused.

use std::ffi::c_int;

#[link(name = "user32")]
unsafe extern "system" {
    fn GetAsyncKeyState(v_key: c_int) -> i16;
}

pub fn is_pressed(key: char) -> bool {
    // The most significant bit is set while the key is down.
    unsafe { GetAsyncKeyState(super::windows_virtual_key(key)) & 0x8000 != 0 }
}
