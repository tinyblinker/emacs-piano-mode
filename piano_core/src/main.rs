//! piano-keys — Linux keyboard press/release reporter.
//!
//! Watches a set of keys and prints one line per state change:
//!
//! ```text
//! down A
//! up A
//! ```
//!
//! Key state is polled, so key releases are detected with
//! poll-interval latency.  Two zero-dependency backends are used:
//!
//! - X11: `XQueryKeymap` (libX11 loaded with dlopen) — no privileges.
//! - evdev: the kernel input ABI from `/dev/input` — used under
//!   Wayland, where XWayland only knows key state while an X11
//!   client is focused.  Needs seat access (udev `uaccess`), which
//!   normally requires no `input` group membership.

#[cfg(not(target_os = "linux"))]
compile_error!("piano-keys supports Linux only");

mod evdev;
mod keys;
mod x11;

use std::io::{self, Write};
use std::path::Path;
use std::time::Duration;

const DEFAULT_POLL_MS: u64 = 4;

const USAGE: &str = "\
piano-keys — report keyboard press/release events on stdout (Linux)

USAGE:
  piano-keys [--keys K1,K2,...] [--poll-ms N] [--backend NAME]
             [--device PATH] [--list-devices] [--help]

  --keys          comma-separated key names to watch
                  (default: every letter and digit)
  --poll-ms       polling interval in milliseconds, 1-100 (default: 4)
  --backend       keyboard backend: x11, evdev or auto (default: auto)
  --device        evdev device to use instead of auto-detection
  --list-devices  list the event devices and exit

OUTPUT, one line per state change:
  down A
  up A
";

/// The selected keyboard backend.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Backend {
    Auto,
    X11,
    Evdev,
}

impl Backend {
    fn parse(name: &str) -> Result<Self, String> {
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

/// An open keyboard reader.
enum Keyboard {
    X11(x11::X11),
    Evdev(evdev::Evdev),
}

impl Keyboard {
    /// Open the backend selected by `choice` for the given key `names`.
    /// `device` pins the evdev backend to one device file.
    fn open(names: &[char], choice: Backend, device: Option<&Path>) -> Result<Self, String> {
        match choice {
            Backend::X11 => Ok(Self::X11(x11::X11::open(names)?)),
            Backend::Evdev => Ok(Self::Evdev(open_evdev(names, device)?)),
            Backend::Auto => {
                if wayland_session() {
                    // Wayland: prefer evdev, see the module docs above.
                    Ok(Self::Evdev(open_evdev(names, device)?))
                } else {
                    match x11::X11::open(names) {
                        Ok(x11) => Ok(Self::X11(x11)),
                        Err(x11_error) => match open_evdev(names, device) {
                            Ok(evdev) => Ok(Self::Evdev(evdev)),
                            Err(evdev_error) => Err(format!(
                                "x11 backend failed: {x11_error}; \
evdev backend failed: {evdev_error}"
                            )),
                        },
                    }
                }
            }
        }
    }

    /// Poll the keyboard; returns one pressed flag per watched key.
    fn poll(&mut self) -> Result<Vec<bool>, String> {
        match self {
            Self::X11(backend) => backend.poll(),
            Self::Evdev(backend) => backend.poll(),
        }
    }
}

/// Open the evdev backend, pinned to `device` when given.
fn open_evdev(names: &[char], device: Option<&Path>) -> Result<evdev::Evdev, String> {
    match device {
        Some(path) => evdev::open_device(path, names),
        None => evdev::Evdev::autodetect(names),
    }
}

/// Whether this looks like a native Wayland session.
fn wayland_session() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some()
        || std::env::var("XDG_SESSION_TYPE").is_ok_and(|value| value == "wayland")
}

struct Args {
    keys: Vec<char>,
    poll_ms: u64,
    backend: Backend,
    device: Option<String>,
    list_devices: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut keys = None;
    let mut poll_ms = DEFAULT_POLL_MS;
    let mut backend = Backend::Auto;
    let mut device = None;
    let mut list_devices = false;
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--help" | "-h" => {
                print!("{USAGE}");
                std::process::exit(0);
            }
            "--keys" => {
                keys = Some(arguments.next().ok_or("--keys requires a value")?);
            }
            "--poll-ms" => {
                let value = arguments.next().ok_or("--poll-ms requires a value")?;
                poll_ms = value
                    .parse()
                    .map_err(|_| format!("invalid --poll-ms {value:?}"))?;
                if !(1..=100).contains(&poll_ms) {
                    return Err("--poll-ms must be between 1 and 100".into());
                }
            }
            "--backend" => {
                let value = arguments.next().ok_or("--backend requires a value")?;
                backend = Backend::parse(&value)?;
            }
            "--device" => {
                device = Some(arguments.next().ok_or("--device requires a value")?);
            }
            "--list-devices" => list_devices = true,
            other => return Err(format!("unexpected argument {other:?}")),
        }
    }
    let keys = match keys {
        Some(list) => keys::parse_keys(&list)?,
        None => keys::default_keys(),
    };
    Ok(Args {
        keys,
        poll_ms,
        backend,
        device,
        list_devices,
    })
}

/// Write one line to stdout and flush it.  Fails once the consumer
/// (Emacs) is gone.
fn emit(line: &str) -> io::Result<()> {
    let mut stdout = io::stdout().lock();
    stdout.write_all(line.as_bytes())?;
    stdout.write_all(b"\n")?;
    stdout.flush()
}

/// Split the previous and current key states into changed indices.
fn diff(previous: &[bool], current: &[bool]) -> (Vec<usize>, Vec<usize>) {
    let mut down = Vec::new();
    let mut up = Vec::new();
    for index in 0..previous.len() {
        if current[index] && !previous[index] {
            down.push(index);
        } else if !current[index] && previous[index] {
            up.push(index);
        }
    }
    (down, up)
}

fn main() {
    let args = match parse_args() {
        Ok(args) => args,
        Err(message) => {
            eprintln!("piano-keys: {message}");
            eprint!("{USAGE}");
            std::process::exit(2);
        }
    };
    if args.list_devices {
        match evdev::list_devices() {
            Ok(devices) => {
                for device in devices {
                    println!(
                        "{}\t{}\t{}",
                        device.path.display(),
                        device.name,
                        if device.keyboard { "yes" } else { "no" }
                    );
                }
                return;
            }
            Err(message) => {
                eprintln!("piano-keys: {message}");
                std::process::exit(1);
            }
        }
    }
    let mut keyboard = match Keyboard::open(
        &args.keys,
        args.backend,
        args.device.as_deref().map(Path::new),
    ) {
        Ok(keyboard) => keyboard,
        Err(message) => {
            eprintln!("piano-keys: {message}");
            std::process::exit(1);
        }
    };
    let mut previous = vec![false; args.keys.len()];
    loop {
        std::thread::sleep(Duration::from_millis(args.poll_ms));
        let current = match keyboard.poll() {
            Ok(current) => current,
            Err(message) => {
                eprintln!("piano-keys: {message}");
                std::process::exit(1);
            }
        };
        let (down, up) = diff(&previous, &current);
        for index in down {
            if emit(&format!("down {}", args.keys[index])).is_err() {
                return; // consumer is gone
            }
        }
        for index in up {
            if emit(&format!("up {}", args.keys[index])).is_err() {
                return;
            }
        }
        previous = current;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_reports_presses_and_releases() {
        let previous = [false, true, false, true];
        let current = [true, true, true, false];
        assert_eq!(diff(&previous, &current), (vec![0, 2], vec![3]));
        assert_eq!(diff(&current, &current), (vec![], vec![]));
    }
}
