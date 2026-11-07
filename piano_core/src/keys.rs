//! Key name parsing and the Linux key code table.

/// Key names accepted by `--keys`.
pub const SUPPORTED_KEYS: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789;,./[]'\\=-";

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
pub fn evdev_keycode(key: char) -> Option<u16> {
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
    fn evdev_codes_match_the_kernel_abi() {
        assert_eq!(evdev_keycode('A'), Some(30));
        assert_eq!(evdev_keycode('Z'), Some(44));
        assert_eq!(evdev_keycode('1'), Some(2));
        assert_eq!(evdev_keycode('0'), Some(11));
        assert_eq!(evdev_keycode(';'), Some(39));
        assert_eq!(evdev_keycode('-'), Some(12));
    }
}
