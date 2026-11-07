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
use std::time::Duration;

const DEFAULT_POLL_MS: u64 = 4;

const USAGE: &str = "\
piano-keys — report keyboard press/release events on stdout (Linux)

USAGE:
  piano-keys [--keys K1,K2,...] [--poll-ms N] [--backend NAME] [--help]

  --keys      comma-separated key names to watch
              (default: every letter and digit)
  --poll-ms   polling interval in milliseconds, 1-100 (default: 4)
  --backend   keyboard backend: x11, evdev or auto (default: auto)

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
    fn open(names: &[char], choice: Backend) -> Result<Self, String> {
        match choice {
            Backend::X11 => Ok(Self::X11(x11::X11::open(names)?)),
            Backend::Evdev => Ok(Self::Evdev(evdev::Evdev::open(names)?)),
            Backend::Auto => {
                if wayland_session() {
                    // Wayland: prefer evdev, see the module docs above.
                    Ok(Self::Evdev(evdev::Evdev::open(names)?))
                } else {
                    match x11::X11::open(names) {
                        Ok(x11) => Ok(Self::X11(x11)),
                        Err(x11_error) => match evdev::Evdev::open(names) {
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

/// Whether this looks like a native Wayland session.
fn wayland_session() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some()
        || std::env::var("XDG_SESSION_TYPE").is_ok_and(|value| value == "wayland")
}

struct Args {
    keys: Vec<char>,
    poll_ms: u64,
    backend: Backend,
}

fn parse_args() -> Result<Args, String> {
    let mut keys = None;
    let mut poll_ms = DEFAULT_POLL_MS;
    let mut backend = Backend::Auto;
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
    let mut keyboard = match Keyboard::open(&args.keys, args.backend) {
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
