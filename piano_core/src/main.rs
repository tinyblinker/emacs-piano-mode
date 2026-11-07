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
//! poll-interval latency.  The reader talks straight to the kernel
//! evdev ABI (`/dev/input/event*`), which is independent of X11 and
//! Wayland; it only needs read access to the keyboard device (udev
//! `uaccess` or the `input` group).

#[cfg(not(target_os = "linux"))]
compile_error!("piano-keys supports Linux only");

mod evdev;
mod keys;

use std::io::{self, Write};
use std::path::Path;
use std::time::Duration;

const DEFAULT_POLL_MS: u64 = 4;

const USAGE: &str = "\
piano-keys — report keyboard press/release events on stdout (Linux)

USAGE:
  piano-keys [--keys K1,K2,...] [--poll-ms N] [--device PATH]
             [--list-devices] [--help]

  --keys          comma-separated key names to watch
                  (default: every letter and digit)
  --poll-ms       polling interval in milliseconds, 1-100 (default: 4)
  --device        keyboard device to use instead of auto-detection
  --list-devices  list the keyboard devices and exit

Requires read access to /dev/input (udev uaccess or the input group).

OUTPUT, one line per state change:
  down A
  up A
";

struct Args {
    keys: Vec<char>,
    poll_ms: u64,
    device: Option<String>,
    list_devices: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut keys = None;
    let mut poll_ms = DEFAULT_POLL_MS;
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
    let keyboard = match args.device.as_deref() {
        Some(path) => evdev::open_device(Path::new(path), &args.keys),
        None => evdev::Evdev::autodetect(&args.keys),
    };
    let mut keyboard = match keyboard {
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
