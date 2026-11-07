//! X11 backend: poll the X server's keyboard map with XQueryKeymap.
//!
//! libX11 is loaded at runtime with dlopen(3), so building needs no
//! X11 development headers, and no special permissions are required.

use std::ffi::{CStr, c_char, c_int, c_ulong, c_void};

const RTLD_LAZY: c_int = 1;

#[cfg_attr(target_env = "musl", link(name = "c"))]
#[cfg_attr(not(target_env = "musl"), link(name = "dl"))]
unsafe extern "C" {
    fn dlopen(filename: *const c_char, flag: c_int) -> *mut c_void;
    fn dlclose(handle: *mut c_void) -> c_int;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}

type Display = *mut c_void;
type XOpenDisplay = unsafe extern "C" fn(*const c_char) -> Display;
type XCloseDisplay = unsafe extern "C" fn(Display) -> c_int;
type XQueryKeymap = unsafe extern "C" fn(Display, *mut [c_char; 32]) -> c_int;
type XKeysymToKeycode = unsafe extern "C" fn(Display, c_ulong) -> u8;

/// Load one function pointer from the libX11 handle.
///
/// # Safety
/// `handle` must be a valid dlopen handle and `name` must name a
/// symbol with the same type as `T`.
unsafe fn load_symbol<T: Copy>(handle: *mut c_void, name: &CStr) -> Result<T, String> {
    let pointer = unsafe { dlsym(handle, name.as_ptr()) };
    if pointer.is_null() {
        return Err(format!("libX11 does not export {}", name.to_string_lossy()));
    }
    // Data pointers cannot be cast to function pointers in Rust, so
    // copy the pointer value instead.
    Ok(unsafe { std::mem::transmute_copy(&pointer) })
}

/// libX11-backed keyboard reader.
pub struct X11 {
    handle: *mut c_void,
    display: Display,
    close_display: XCloseDisplay,
    query_keymap: XQueryKeymap,
    keycodes: Vec<u8>,
}

impl X11 {
    /// Open the display and resolve keycodes for `names`.
    pub fn new(names: &[char]) -> Result<Self, String> {
        let library = c"libX11.so.6";
        let handle = unsafe { dlopen(library.as_ptr(), RTLD_LAZY) };
        if handle.is_null() {
            return Err("cannot load libX11.so.6 (not an X11 session? use --backend evdev)".into());
        }
        let open_display: XOpenDisplay = unsafe { load_symbol(handle, c"XOpenDisplay")? };
        let display = unsafe { open_display(std::ptr::null()) };
        if display.is_null() {
            return Err(
                "cannot connect to the X server (DISPLAY unset or unreachable; \
on Wayland use --backend evdev)"
                    .into(),
            );
        }
        let close_display: XCloseDisplay = unsafe { load_symbol(handle, c"XCloseDisplay")? };
        let query_keymap: XQueryKeymap = unsafe { load_symbol(handle, c"XQueryKeymap")? };
        let keysym_to_keycode: XKeysymToKeycode =
            unsafe { load_symbol(handle, c"XKeysymToKeycode")? };
        let keycodes = names
            .iter()
            .map(|&name| {
                // Every supported key name is a printable ASCII
                // character whose X11 keysym equals its ASCII code.
                let keysym = name as c_ulong;
                unsafe { keysym_to_keycode(display, keysym) }
            })
            .collect();
        Ok(Self {
            handle,
            display,
            close_display,
            query_keymap,
            keycodes,
        })
    }

    /// Query the current pressed state of every watched key.
    pub fn poll(&self) -> Result<Vec<bool>, String> {
        let mut mask = [0 as c_char; 32];
        if unsafe { (self.query_keymap)(self.display, &mut mask) } == 0 {
            return Err("XQueryKeymap failed".into());
        }
        Ok(self
            .keycodes
            .iter()
            .map(|&keycode| {
                if keycode == 0 {
                    return false;
                }
                let byte = mask[keycode as usize / 8] as u8;
                let bit = 1_u8 << (keycode % 8);
                (byte & bit) != 0
            })
            .collect())
    }
}

impl Drop for X11 {
    fn drop(&mut self) {
        unsafe { (self.close_display)(self.display) };
        unsafe { dlclose(self.handle) };
    }
}
