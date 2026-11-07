//! evdev backend: read the kernel input ABI from `/dev/input/event*`
//! directly.
//!
//! This works under X11 and Wayland alike, without any display server
//! protocol.  The logged-in user can normally read keyboard devices
//! (udev `uaccess`); the `input` group is the fallback.

#[cfg(not(target_pointer_width = "64"))]
compile_error!("the evdev backend assumes a 64-bit Linux ABI");

use crate::keys::evdev_keycode;

use std::ffi::{c_int, c_ulong};
use std::fs::{self, File};
use std::io::{self, Read};
use std::os::fd::{AsRawFd, RawFd};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::thread;

const EV_SYN: u16 = 0x00;
const EV_KEY: u16 = 0x01;
const SYN_DROPPED: u16 = 0x03;

/// ioctl requests (Linux `input.h`).
const EVIOCGKEY: c_ulong = 0x8060_4504; // _IOC(_IOC_READ, 'E', 0x04, 96)
const EVIOCGNAME: c_ulong = 0x8100_4506; // _IOC(_IOC_READ, 'E', 0x06, 256)
const EVIOCGBIT_EV: c_ulong = 0x801f_4520; // _IOC(_IOC_READ, 'E', 0x20, 31)
const EVIOCGBIT_KEY: c_ulong = 0x8060_4521; // _IOC(_IOC_READ, 'E', 0x21, 96)

unsafe extern "C" {
    fn ioctl(fd: c_int, request: c_ulong, ...) -> c_int;
    fn close(fd: c_int) -> c_int;
}

/// `struct input_event` on a 64-bit kernel (24 bytes).
#[repr(C)]
struct InputEvent {
    time: [u64; 2],
    event_type: u16,
    code: u16,
    value: i32,
}

fn input_event_from(bytes: &[u8]) -> InputEvent {
    InputEvent {
        time: [
            u64::from_ne_bytes(bytes[0..8].try_into().expect("enough bytes")),
            u64::from_ne_bytes(bytes[8..16].try_into().expect("enough bytes")),
        ],
        event_type: u16::from_ne_bytes(bytes[16..18].try_into().expect("enough bytes")),
        code: u16::from_ne_bytes(bytes[18..20].try_into().expect("enough bytes")),
        value: i32::from_ne_bytes(bytes[20..24].try_into().expect("enough bytes")),
    }
}

/// One interesting event from the kernel.
#[derive(Debug)]
enum Event {
    Key(u16, bool),
    SyncDropped,
}

/// A keyboard device read from `/dev/input`.
pub struct Evdev {
    fd: RawFd,
    receiver: Receiver<Event>,
    codes: Vec<u16>,
    state: Vec<bool>,
}

/// One event device under /dev/input, as reported by `list_devices`.
pub struct Device {
    pub path: PathBuf,
    pub name: String,
    pub keyboard: bool,
}

impl Evdev {
    /// Find the first keyboard under /dev/input and start reading it.
    pub fn autodetect(names: &[char]) -> Result<Self, String> {
        for device in list_devices()? {
            if device.keyboard {
                return open_device(&device.path, names);
            }
        }
        Err("no readable keyboard device found under /dev/input; \
grant your user read access (udev uaccess or the input group)"
            .into())
    }

    /// Drain queued events and return the current pressed state.
    pub fn poll(&mut self) -> Result<Vec<bool>, String> {
        loop {
            match self.receiver.try_recv() {
                Ok(Event::Key(code, pressed)) => {
                    if let Some(index) = self.codes.iter().position(|&watched| watched == code) {
                        self.state[index] = pressed;
                    }
                }
                Ok(Event::SyncDropped) => self.resync()?,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return Err("keyboard device disappeared".into()),
            }
        }
        Ok(self.state.clone())
    }

    /// Re-read the whole key state after the kernel dropped events.
    fn resync(&mut self) -> Result<(), String> {
        let mut mask = [0_u8; 96];
        if unsafe { ioctl(self.fd, EVIOCGKEY, mask.as_mut_ptr()) } < 0 {
            return Err(format!("EVIOCGKEY failed: {}", io::Error::last_os_error()));
        }
        for (state, &code) in self.state.iter_mut().zip(&self.codes) {
            *state = mask[code as usize / 8] & (1_u8 << (code % 8)) != 0;
        }
        Ok(())
    }
}

impl Drop for Evdev {
    fn drop(&mut self) {
        // Closing the descriptor makes the blocked read in the reader
        // thread return, so the thread exits.
        unsafe { close(self.fd) };
    }
}

/// Enumerate the event devices under /dev/input with their names and
/// whether they look like a keyboard, ordered by event number.
pub fn list_devices() -> Result<Vec<Device>, String> {
    let mut devices = Vec::new();
    for entry in fs::read_dir("/dev/input")
        .map_err(|error| format!("cannot list /dev/input: {error}"))?
        .filter_map(|entry| entry.ok())
    {
        let path = entry.path();
        if !path
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with("event"))
        {
            continue;
        }
        let (name, keyboard) = match File::open(&path) {
            Ok(file) => (device_name(file.as_raw_fd()), is_keyboard(&file)),
            Err(_) => (String::new(), false), // not readable: list it, but not as a keyboard
        };
        devices.push(Device {
            path,
            name,
            keyboard,
        });
    }
    devices.sort_by_key(|device| event_number(&device.path));
    Ok(devices)
}

/// The event number of an `/dev/input/eventN` path, for stable sorting.
fn event_number(path: &Path) -> u64 {
    path.file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_prefix("event"))
        .and_then(|suffix| suffix.parse().ok())
        .unwrap_or(0)
}

/// Read the device name with the EVIOCGNAME ioctl.
fn device_name(fd: RawFd) -> String {
    let mut name = [0_u8; 256];
    if unsafe { ioctl(fd, EVIOCGNAME, name.as_mut_ptr()) } < 0 {
        return String::new();
    }
    c_string(&name)
}

/// The bytes up to the first NUL, as a lossy string.
fn c_string(bytes: &[u8]) -> String {
    let end = bytes
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

/// Open one event device and start reading it.
pub fn open_device(path: &Path, names: &[char]) -> Result<Evdev, String> {
    let file =
        File::open(path).map_err(|error| format!("cannot open {}: {error}", path.display()))?;
    let fd = file.as_raw_fd();
    let codes = names
        .iter()
        .map(|&name| evdev_keycode(name).expect("validated key name without an evdev keycode"))
        .collect();
    Ok(Evdev {
        fd,
        receiver: spawn_reader(file),
        codes,
        state: vec![false; names.len()],
    })
}

/// Spawn a thread that reads events from `file` forever.
fn spawn_reader(file: File) -> Receiver<Event> {
    let (sender, receiver) = channel();
    thread::Builder::new()
        .name("piano-keys-evdev".into())
        .spawn(move || read_events(file, &sender))
        .expect("failed to spawn evdev reader thread");
    receiver
}

/// Block reading `file` and push parsed events into `sender`.
fn read_events(mut file: File, sender: &Sender<Event>) {
    let event_size = std::mem::size_of::<InputEvent>();
    let mut pending: Vec<u8> = Vec::with_capacity(4096 + event_size);
    let mut buffer = [0_u8; 4096];
    loop {
        let read = match file.read(&mut buffer) {
            Ok(0) | Err(_) => return,
            Ok(read) => read,
        };
        pending.extend_from_slice(&buffer[..read]);
        for event in drain_events(&mut pending, event_size) {
            if sender.send(event).is_err() {
                return; // consumer is gone
            }
        }
    }
}

/// Parse complete events out of `pending`, leaving partial events.
fn drain_events(pending: &mut Vec<u8>, event_size: usize) -> Vec<Event> {
    let mut events = Vec::new();
    let mut offset = 0;
    while pending.len() - offset >= event_size {
        let event = input_event_from(&pending[offset..]);
        match (event.event_type, event.code) {
            (EV_SYN, SYN_DROPPED) => events.push(Event::SyncDropped),
            (EV_KEY, code) => events.push(Event::Key(code, event.value != 0)),
            _ => {}
        }
        offset += event_size;
    }
    pending.drain(..offset);
    events
}

/// Whether an opened event device looks like a keyboard.
fn is_keyboard(file: &File) -> bool {
    let fd = file.as_raw_fd();
    let mut ev_mask = [0_u8; 32];
    if unsafe { ioctl(fd, EVIOCGBIT_EV, ev_mask.as_mut_ptr()) } < 0 {
        return false;
    }
    if ev_mask[0] & (1_u8 << EV_KEY) == 0 {
        return false;
    }
    let mut key_mask = [0_u8; 96];
    if unsafe { ioctl(fd, EVIOCGBIT_KEY, key_mask.as_mut_ptr()) } < 0 {
        return false;
    }
    // KEY_A..KEY_M span codes 16..=50; a real keyboard has at least one.
    (16..=50).any(|code| key_mask[code as usize / 8] & (1_u8 << (code % 8)) != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event_bytes(event_type: u16, code: u16, value: i32) -> Vec<u8> {
        let event = InputEvent {
            time: [0, 0],
            event_type,
            code,
            value,
        };
        let pointer = &event as *const InputEvent as *const u8;
        // SAFETY: the struct is plain data and we copy exactly its size.
        unsafe { std::slice::from_raw_parts(pointer, std::mem::size_of::<InputEvent>()) }.to_vec()
    }

    #[test]
    fn parses_key_events_and_ignores_others() {
        let mut bytes = Vec::new();
        bytes.extend(event_bytes(EV_KEY, 30, 1)); // KEY_A down
        bytes.extend(event_bytes(0x04, 0, 0)); // EV_MSC, ignored
        bytes.extend(event_bytes(EV_KEY, 30, 0)); // KEY_A up
        bytes.extend(event_bytes(EV_SYN, SYN_DROPPED, 0));
        let events = drain_events(&mut bytes, std::mem::size_of::<InputEvent>());
        match events.as_slice() {
            [
                Event::Key(30, true),
                Event::Key(30, false),
                Event::SyncDropped,
            ] => {}
            other => panic!("unexpected events: {other:?}"),
        }
        assert!(bytes.is_empty());
    }

    #[test]
    fn keeps_partial_events_pending() {
        let mut bytes = Vec::new();
        bytes.extend(event_bytes(EV_KEY, 30, 1));
        let size = std::mem::size_of::<InputEvent>();
        bytes.extend_from_slice(&[0xAB; 4]); // cut an event in half
        let events = drain_events(&mut bytes, size);
        assert_eq!(events.len(), 1);
        assert_eq!(bytes.len(), 4);
    }

    #[test]
    fn key_bit_arithmetic_matches_the_kernel() {
        let mut mask = [0_u8; 96];
        mask[30 / 8] |= 1 << (30 % 8); // KEY_A
        assert_ne!(mask[30 / 8] & (1 << (30 % 8)), 0);
        assert_eq!(mask[1] & (1 << 3), 0);
    }

    #[test]
    fn event_numbers_sort_numerically() {
        let number = |path: &str| event_number(Path::new(path));
        assert_eq!(number("/dev/input/event10"), 10);
        assert_eq!(number("/dev/input/event2"), 2);
        assert!(number("/dev/input/event2") < number("/dev/input/event10"));
        assert_eq!(number("/dev/input/mouse0"), 0);
    }

    #[test]
    fn c_strings_end_at_the_first_nul() {
        assert_eq!(c_string(b"Keychron K8 Pro\0ignored"), "Keychron K8 Pro");
        assert_eq!(c_string(b""), "");
        assert_eq!(c_string(b"no nul"), "no nul");
    }

    #[test]
    fn reads_events_from_a_file_descriptor() {
        use std::io::Write;
        let mut bytes = Vec::new();
        bytes.extend(event_bytes(EV_KEY, 30, 1));
        bytes.extend(event_bytes(EV_KEY, 30, 0));
        let mut path = std::env::temp_dir();
        path.push(format!("piano-keys-test-{}", std::process::id()));
        {
            let mut file = File::create(&path).unwrap();
            file.write_all(&bytes).unwrap();
        }
        let receiver = spawn_reader(File::open(&path).unwrap());
        match (receiver.recv(), receiver.recv()) {
            (Ok(Event::Key(30, true)), Ok(Event::Key(30, false))) => {}
            other => panic!("unexpected events: {other:?}"),
        }
        // EOF: the reader thread exits and the channel disconnects.
        assert!(receiver.recv().is_err());
        let _ = std::fs::remove_file(&path);
    }
}
