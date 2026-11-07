//! piano-keys — cross-platform keyboard press/release reporter.
//!
//! Watches a set of keys and prints one line per state change:
//!
//! ```text
//! down A
//! up A
//! ```
//!
//! Key state is polled, so key releases are detected with
//! poll-interval latency.  No external crates are used, and no root
//! or `input` group membership is required:
//!
//! - Linux, X11 sessions: `XQueryKeymap` (libX11 loaded via dlopen).
//! - Linux, Wayland sessions: the kernel evdev ABI via `/dev/input`.
//! - Windows: `GetAsyncKeyState` from user32.dll.
//! - macOS: `CGEventSourceKeyState` from the CoreGraphics framework.

mod keys;

use std::io::{self, Write};
use std::time::Duration;

const DEFAULT_POLL_MS: u64 = 4;

const USAGE: &str = "\
piano-keys — report keyboard press/release events on stdout

USAGE:
  piano-keys [--keys K1,K2,...] [--poll-ms N] [--backend NAME] [--help]

  --keys      comma-separated key names to watch
              (default: every letter and digit)
  --poll-ms   polling interval in milliseconds, 1-100 (default: 4)
  --backend   keyboard backend (Linux: x11, evdev or auto; default: auto)

OUTPUT, one line per state change:
  down A
  up A
";

struct Args {
    keys: Vec<char>,
    poll_ms: u64,
    backend: keys::Backend,
}

fn parse_args() -> Result<Args, String> {
    let mut keys = None;
    let mut poll_ms = DEFAULT_POLL_MS;
    let mut backend = keys::Backend::Auto;
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
                backend = keys::Backend::parse(&value)?;
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
    let mut keyboard = match keys::Keyboard::new(&args.keys, args.backend) {
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
