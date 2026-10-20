//! macOS synthetic mouse/keyboard input via `enigo`.
//!
//! Every public function creates its own `Enigo` instance and is blocking, so
//! callers must run it inside `tokio::task::spawn_blocking`.

use std::time::Duration;

use enigo::{Axis, Button, Coordinate, Direction, Enigo, Key, Keyboard, Mouse, Settings};

use super::ComputerError;

pub fn move_mouse(x: i32, y: i32) -> Result<(), ComputerError> {
    let mut enigo = new_enigo()?;
    enigo.move_mouse(x, y, Coordinate::Abs).map_err(input_err)
}

pub fn click(x: i32, y: i32, button: Button, count: u32) -> Result<(), ComputerError> {
    let mut enigo = new_enigo()?;
    enigo.move_mouse(x, y, Coordinate::Abs).map_err(input_err)?;
    for i in 0..count.max(1) {
        if i > 0 {
            std::thread::sleep(Duration::from_millis(40));
        }
        enigo.button(button, Direction::Click).map_err(input_err)?;
    }
    Ok(())
}

pub fn drag(
    from_x: i32,
    from_y: i32,
    to_x: i32,
    to_y: i32,
    button: Button,
) -> Result<(), ComputerError> {
    let mut enigo = new_enigo()?;
    enigo
        .move_mouse(from_x, from_y, Coordinate::Abs)
        .map_err(input_err)?;
    enigo.button(button, Direction::Press).map_err(input_err)?;
    std::thread::sleep(Duration::from_millis(40));
    enigo
        .move_mouse(to_x, to_y, Coordinate::Abs)
        .map_err(input_err)?;
    std::thread::sleep(Duration::from_millis(60));
    enigo
        .button(button, Direction::Release)
        .map_err(input_err)?;
    Ok(())
}

pub fn scroll(
    direction: &str,
    amount: i32,
    position: Option<(i32, i32)>,
) -> Result<(), ComputerError> {
    let mut enigo = new_enigo()?;
    if let Some((x, y)) = position {
        enigo.move_mouse(x, y, Coordinate::Abs).map_err(input_err)?;
    }

    let amount = amount.max(1);
    let (length, axis) = match direction {
        "up" => (amount, Axis::Vertical),
        "down" => (-amount, Axis::Vertical),
        "left" => (-amount, Axis::Horizontal),
        _ => (amount, Axis::Horizontal), // "right"
    };
    enigo.scroll(length, axis).map_err(input_err)
}

pub fn type_text(text: &str, press_enter: bool) -> Result<(), ComputerError> {
    let mut enigo = new_enigo()?;
    enigo.text(text).map_err(input_err)?;
    if press_enter {
        enigo
            .key(Key::Return, Direction::Click)
            .map_err(input_err)?;
    }
    Ok(())
}

pub fn press_keys(combo: &str) -> Result<(), ComputerError> {
    let (modifiers, key) = parse_key_combo(combo)?;
    let mut enigo = new_enigo()?;
    for modifier in &modifiers {
        enigo.key(*modifier, Direction::Press).map_err(input_err)?;
    }
    enigo.key(key, Direction::Click).map_err(input_err)?;
    for modifier in modifiers.iter().rev() {
        enigo
            .key(*modifier, Direction::Release)
            .map_err(input_err)?;
    }
    Ok(())
}

#[derive(Debug, Default)]
pub struct CursorStatus {
    pub cursor: Option<(i32, i32)>,
    pub main_display: Option<(i32, i32)>,
}

/// Read cursor/display info. Deliberately best-effort: failures are returned
/// as `None` so `action=status` still succeeds on a machine without
/// Accessibility permission.
pub fn status() -> CursorStatus {
    match Enigo::new(&Settings::default()) {
        Ok(enigo) => CursorStatus {
            cursor: enigo.location().ok(),
            main_display: enigo.main_display().ok(),
        },
        Err(_) => CursorStatus::default(),
    }
}

/// Parse a combo such as `cmd+shift+4` into modifiers plus a key.
///
/// Supports `cmd`/`command`/`meta`/`super`, `ctrl`/`control`, `alt`/`option`,
/// `shift`, named keys (Return, Escape, F1-F12, arrows, ...), and single
/// characters. A literal plus key can be written as `cmd++` or `plus`.
pub fn parse_key_combo(combo: &str) -> Result<(Vec<Key>, Key), ComputerError> {
    let mut modifiers = Vec::new();
    let mut key: Option<Key> = None;

    for token in split_combo(combo) {
        if let Some(modifier) = modifier_key(&token) {
            modifiers.push(modifier);
            continue;
        }

        let parsed = parse_key(&token)?;
        if key.is_some() {
            return Err(ComputerError::InvalidArgs(format!(
                "key combo '{combo}' has more than one non-modifier key"
            )));
        }
        key = Some(parsed);
    }

    match key {
        Some(key) => Ok((modifiers, key)),
        None => Err(ComputerError::InvalidArgs(format!(
            "key combo '{combo}' does not contain a key"
        ))),
    }
}

/// Split `cmd+shift+4` while preserving a literal `+` written as `++`.
fn split_combo(combo: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    for ch in combo.trim().chars() {
        if ch == '+' {
            if current.is_empty() {
                tokens.push("+".into());
            } else {
                tokens.push(std::mem::take(&mut current));
            }
        } else {
            current.push(ch);
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
        .into_iter()
        .map(|t| t.trim().to_owned())
        .filter(|t| !t.is_empty())
        .collect()
}

fn modifier_key(token: &str) -> Option<Key> {
    match token.to_ascii_lowercase().as_str() {
        "cmd" | "command" | "meta" | "super" | "win" => Some(Key::Meta),
        "ctrl" | "control" => Some(Key::Control),
        "alt" | "option" | "opt" => Some(Key::Alt),
        "shift" => Some(Key::Shift),
        _ => None,
    }
}

fn parse_key(token: &str) -> Result<Key, ComputerError> {
    if token.chars().count() == 1 {
        return Ok(Key::Unicode(token.chars().next().unwrap_or(' ')));
    }

    let key = match token.to_ascii_lowercase().as_str() {
        "return" | "enter" => Key::Return,
        "escape" | "esc" => Key::Escape,
        "tab" => Key::Tab,
        "space" | "spacebar" => Key::Space,
        "backspace" => Key::Backspace,
        "delete" | "del" => Key::Delete,
        "home" => Key::Home,
        "end" => Key::End,
        "pageup" | "pgup" => Key::PageUp,
        "pagedown" | "pgdn" => Key::PageDown,
        "up" | "arrowup" | "uparrow" => Key::UpArrow,
        "down" | "arrowdown" | "downarrow" => Key::DownArrow,
        "left" | "arrowleft" | "leftarrow" => Key::LeftArrow,
        "right" | "arrowright" | "rightarrow" => Key::RightArrow,
        "capslock" => Key::CapsLock,
        "f1" => Key::F1,
        "f2" => Key::F2,
        "f3" => Key::F3,
        "f4" => Key::F4,
        "f5" => Key::F5,
        "f6" => Key::F6,
        "f7" => Key::F7,
        "f8" => Key::F8,
        "f9" => Key::F9,
        "f10" => Key::F10,
        "f11" => Key::F11,
        "f12" => Key::F12,
        "plus" => Key::Unicode('+'),
        "minus" => Key::Unicode('-'),
        "comma" => Key::Unicode(','),
        "period" | "dot" => Key::Unicode('.'),
        "slash" => Key::Unicode('/'),
        "semicolon" => Key::Unicode(';'),
        "quote" => Key::Unicode('\''),
        "backtick" | "grave" => Key::Unicode('`'),
        "backslash" => Key::Unicode('\\'),
        other => {
            return Err(ComputerError::InvalidArgs(format!(
                "unknown key '{other}' in combo"
            )));
        }
    };
    Ok(key)
}

fn new_enigo() -> Result<Enigo, ComputerError> {
    Enigo::new(&Settings::default()).map_err(|e| ComputerError::Input(e.to_string()))
}

fn input_err(e: impl std::fmt::Display) -> ComputerError {
    ComputerError::Input(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_modifier_combo() {
        let (modifiers, key) = parse_key_combo("cmd+shift+4").expect("valid combo");
        assert!(matches!(modifiers.as_slice(), [Key::Meta, Key::Shift]));
        assert!(matches!(key, Key::Unicode('4')));
    }

    #[test]
    fn parses_named_keys_case_insensitively() {
        let (_, key) = parse_key_combo("Return").expect("valid");
        assert!(matches!(key, Key::Return));
        let (_, key) = parse_key_combo("f5").expect("valid");
        assert!(matches!(key, Key::F5));
        let (modifiers, key) = parse_key_combo("ctrl+alt+delete").expect("valid");
        assert!(matches!(modifiers.as_slice(), [Key::Control, Key::Alt]));
        assert!(matches!(key, Key::Delete));
    }

    #[test]
    fn parses_literal_plus() {
        let (modifiers, key) = parse_key_combo("cmd++").expect("valid");
        assert!(matches!(modifiers.as_slice(), [Key::Meta]));
        assert!(matches!(key, Key::Unicode('+')));
    }

    #[test]
    fn rejects_missing_or_multiple_keys() {
        assert!(parse_key_combo("cmd+shift").is_err());
        assert!(parse_key_combo("").is_err());
        assert!(parse_key_combo("cmd+a+b").is_err());
    }

    #[test]
    #[ignore = "requires macOS Accessibility permission"]
    fn can_move_cursor_to_its_current_position() {
        let status = status();
        let (x, y) = status.cursor.expect("cursor location should be readable");
        move_mouse(x, y).expect("moving to the current position should succeed");
    }
}
