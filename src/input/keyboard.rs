use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};

use crate::error::Result;
use crate::player::commands::Command;

/// Poll the terminal for a player command.
///
/// SHIFT+LEFT / SHIFT+RIGHT are bound when the terminal reports them.
/// `[` and `]` are the reliable 10-frame alternatives.
pub fn poll_command(timeout: Duration) -> Result<Option<Command>> {
    if !event::poll(timeout)? {
        return Ok(None);
    }
    match event::read()? {
        Event::Key(key) if key.kind == KeyEventKind::Press || key.kind == KeyEventKind::Repeat => {
            Ok(key_to_command(key.code, key.modifiers))
        }
        _ => Ok(None),
    }
}

/// Map a GDK keyval name (`gdk_keyval_name`) onto the same commands as the terminal.
pub fn command_from_key_name(name: &str, shift: bool) -> Option<Command> {
    let modifiers = if shift {
        KeyModifiers::SHIFT
    } else {
        KeyModifiers::NONE
    };
    let code = match name {
        "space" | "KP_Space" => KeyCode::Char(' '),
        "Left" | "KP_Left" => KeyCode::Left,
        "Right" | "KP_Right" => KeyCode::Right,
        "Up" | "KP_Up" => KeyCode::Up,
        "Down" | "KP_Down" => KeyCode::Down,
        "bracketleft" => KeyCode::Char('['),
        "bracketright" => KeyCode::Char(']'),
        "plus" | "KP_Add" => KeyCode::Char('+'),
        "equal" => KeyCode::Char('='),
        "minus" | "KP_Subtract" => KeyCode::Char('-'),
        "underscore" => KeyCode::Char('_'),
        "0" | "KP_0" => KeyCode::Char('0'),
        "q" | "Q" => KeyCode::Char('q'),
        "Escape" => KeyCode::Esc,
        "i" | "I" => KeyCode::Char('i'),
        "h" | "H" => KeyCode::Char('h'),
        "l" | "L" => KeyCode::Char('l'),
        _ => return None,
    };
    key_to_command(code, modifiers)
}

pub fn key_to_command(code: KeyCode, modifiers: KeyModifiers) -> Option<Command> {
    let shift = modifiers.contains(KeyModifiers::SHIFT);

    match code {
        KeyCode::Char(' ') => Some(Command::PlayPause),
        KeyCode::Right if shift => Some(Command::StepForward(10)),
        KeyCode::Left if shift => Some(Command::StepBackward(10)),
        KeyCode::Right => Some(Command::StepForward(1)),
        KeyCode::Left => Some(Command::StepBackward(1)),
        KeyCode::Up => Some(Command::VolumeUp),
        KeyCode::Down => Some(Command::VolumeDown),
        KeyCode::Char(']') => Some(Command::StepForward(10)),
        KeyCode::Char('[') => Some(Command::StepBackward(10)),
        KeyCode::Char('+') | KeyCode::Char('=') => Some(Command::SpeedUp),
        KeyCode::Char('-') | KeyCode::Char('_') => Some(Command::SpeedDown),
        KeyCode::Char('0') => Some(Command::SpeedReset),
        KeyCode::Char('q') | KeyCode::Char('Q') => Some(Command::Quit),
        KeyCode::Esc => Some(Command::Quit),
        KeyCode::Char('i') | KeyCode::Char('I') => Some(Command::ShowInfo),
        KeyCode::Char('h') | KeyCode::Char('H') => Some(Command::SaveHm),
        KeyCode::Char('l') | KeyCode::Char('L') => Some(Command::ToggleLoop),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn space_toggles() {
        assert_eq!(
            key_to_command(KeyCode::Char(' '), KeyModifiers::NONE),
            Some(Command::PlayPause)
        );
    }

    #[test]
    fn brackets_step_ten() {
        assert_eq!(
            key_to_command(KeyCode::Char(']'), KeyModifiers::NONE),
            Some(Command::StepForward(10))
        );
        assert_eq!(
            key_to_command(KeyCode::Char('['), KeyModifiers::NONE),
            Some(Command::StepBackward(10))
        );
    }

    #[test]
    fn shift_arrows_step_ten() {
        assert_eq!(
            key_to_command(KeyCode::Right, KeyModifiers::SHIFT),
            Some(Command::StepForward(10))
        );
        assert_eq!(
            key_to_command(KeyCode::Left, KeyModifiers::SHIFT),
            Some(Command::StepBackward(10))
        );
    }

    #[test]
    fn gdk_names_match_terminal_keys() {
        assert_eq!(
            command_from_key_name("space", false),
            Some(Command::PlayPause)
        );
        assert_eq!(
            command_from_key_name("Right", false),
            Some(Command::StepForward(1))
        );
        assert_eq!(
            command_from_key_name("Left", true),
            Some(Command::StepBackward(10))
        );
        assert_eq!(
            command_from_key_name("bracketright", false),
            Some(Command::StepForward(10))
        );
        assert_eq!(command_from_key_name("Escape", false), Some(Command::Quit));
        assert_eq!(command_from_key_name("q", false), Some(Command::Quit));
    }
}
