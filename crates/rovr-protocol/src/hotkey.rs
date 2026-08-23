use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyChord {
    pub modifiers: KeyModifiers,
    pub key: KeyCode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct KeyModifiers {
    pub command: bool,
    pub alt: bool,
    pub shift: bool,
    pub control: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyCode {
    A,
    B,
    C,
    D,
    E,
    F,
    G,
    H,
    I,
    J,
    K,
    L,
    M,
    N,
    O,
    P,
    Q,
    R,
    S,
    T,
    U,
    V,
    W,
    X,
    Y,
    Z,
    Digit0,
    Digit1,
    Digit2,
    Digit3,
    Digit4,
    Digit5,
    Digit6,
    Digit7,
    Digit8,
    Digit9,
    Enter,
    Tab,
    Space,
    Escape,
    Left,
    Right,
    Up,
    Down,
    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    F9,
    F10,
    F11,
    F12,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HotkeyParseError {
    MissingSeparator,
    EmptyKey,
    UnknownModifier(String),
    UnknownKey(String),
}

impl fmt::Display for HotkeyParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingSeparator => write!(f, "missing '-' separator"),
            Self::EmptyKey => write!(f, "key is empty"),
            Self::UnknownModifier(modifier) => write!(f, "unknown modifier {modifier:?}"),
            Self::UnknownKey(key) => write!(f, "unknown key {key:?}"),
        }
    }
}

pub fn parse_hotkey(s: &str) -> Result<KeyChord, HotkeyParseError> {
    let s = s.trim();
    let (mods_part, key_part) = if let Some(idx) = s.find(" - ") {
        let (a, b) = s.split_at(idx);
        (a, b[3..].trim())
    } else if let Some(idx) = s.find('-') {
        let (a, b) = s.split_at(idx);
        (a, b[1..].trim())
    } else {
        return Err(HotkeyParseError::MissingSeparator);
    };

    let mut modifiers = KeyModifiers::default();
    for modifier in mods_part.split('+') {
        match modifier.trim().to_lowercase().as_str() {
            "cmd" | "command" | "super" | "meta" => modifiers.command = true,
            "alt" | "option" | "opt" => modifiers.alt = true,
            "shift" => modifiers.shift = true,
            "ctrl" | "control" => modifiers.control = true,
            "" => {}
            unknown => return Err(HotkeyParseError::UnknownModifier(unknown.into())),
        }
    }

    if key_part.is_empty() {
        return Err(HotkeyParseError::EmptyKey);
    }
    let key = match key_part.to_lowercase().as_str() {
        "a" => KeyCode::A,
        "b" => KeyCode::B,
        "c" => KeyCode::C,
        "d" => KeyCode::D,
        "e" => KeyCode::E,
        "f" => KeyCode::F,
        "g" => KeyCode::G,
        "h" => KeyCode::H,
        "i" => KeyCode::I,
        "j" => KeyCode::J,
        "k" => KeyCode::K,
        "l" => KeyCode::L,
        "m" => KeyCode::M,
        "n" => KeyCode::N,
        "o" => KeyCode::O,
        "p" => KeyCode::P,
        "q" => KeyCode::Q,
        "r" => KeyCode::R,
        "s" => KeyCode::S,
        "t" => KeyCode::T,
        "u" => KeyCode::U,
        "v" => KeyCode::V,
        "w" => KeyCode::W,
        "x" => KeyCode::X,
        "y" => KeyCode::Y,
        "z" => KeyCode::Z,
        "0" => KeyCode::Digit0,
        "1" => KeyCode::Digit1,
        "2" => KeyCode::Digit2,
        "3" => KeyCode::Digit3,
        "4" => KeyCode::Digit4,
        "5" => KeyCode::Digit5,
        "6" => KeyCode::Digit6,
        "7" => KeyCode::Digit7,
        "8" => KeyCode::Digit8,
        "9" => KeyCode::Digit9,
        "return" | "enter" => KeyCode::Enter,
        "tab" => KeyCode::Tab,
        "space" => KeyCode::Space,
        "escape" | "esc" => KeyCode::Escape,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "f1" => KeyCode::F1,
        "f2" => KeyCode::F2,
        "f3" => KeyCode::F3,
        "f4" => KeyCode::F4,
        "f5" => KeyCode::F5,
        "f6" => KeyCode::F6,
        "f7" => KeyCode::F7,
        "f8" => KeyCode::F8,
        "f9" => KeyCode::F9,
        "f10" => KeyCode::F10,
        "f11" => KeyCode::F11,
        "f12" => KeyCode::F12,
        _ => return Err(HotkeyParseError::UnknownKey(key_part.into())),
    };
    Ok(KeyChord { modifiers, key })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_current_skhd_forms_and_aliases() {
        assert_eq!(parse_hotkey("alt - tab").unwrap().key, KeyCode::Tab);
        assert_eq!(
            parse_hotkey("alt + shift - tab").unwrap().modifiers,
            KeyModifiers {
                alt: true,
                shift: true,
                ..Default::default()
            }
        );
        assert_eq!(parse_hotkey("command-h").unwrap().key, KeyCode::H);
        assert_eq!(parse_hotkey("ctrl - f12").unwrap().key, KeyCode::F12);
        assert_eq!(parse_hotkey("option - esc").unwrap().key, KeyCode::Escape);
    }
}
