//! App-wide keyboard shortcuts: one table that the key handling, the
//! shortcuts sheet and the macOS menu bar all read. Keys that only work in
//! one editor, such as Ctrl+Enter, stay with that editor.

use eframe::egui::{Event, InputState, Key, KeyboardShortcut, Modifiers};

/// What an app-wide shortcut does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppCommand {
    GoTo,
    /// The filter, or the script on the Shell panel.
    FocusEditor,
}

pub struct Binding {
    pub command: AppCommand,
    /// The first one is shown in hints and the menu bar.
    pub keys: &'static [KeyboardShortcut],
}

const fn command(key: Key) -> KeyboardShortcut {
    KeyboardShortcut::new(Modifiers::COMMAND, key)
}

pub const GO_TO: KeyboardShortcut = command(Key::K);
pub const FOCUS_EDITOR: KeyboardShortcut = command(Key::L);

/// More specific keys come before the ones they contain: Ctrl+Shift+Tab
/// before Ctrl+Tab, since a match ignores extra Shift.
pub const BINDINGS: &[Binding] = &[
    Binding {
        command: AppCommand::GoTo,
        keys: &[GO_TO],
    },
    Binding {
        command: AppCommand::FocusEditor,
        keys: &[FOCUS_EDITOR],
    },
];

/// Takes this frame's presses of app-wide shortcuts out of `input`, so no
/// widget sees them too.
pub fn pressed(input: &mut InputState) -> Vec<AppCommand> {
    let mut commands = Vec::new();
    for binding in BINDINGS {
        for shortcut in binding.keys {
            if consume(input, shortcut) {
                commands.push(binding.command);
            }
        }
    }
    commands
}

/// Like [`InputState::consume_shortcut`], but Alt must match too: on Windows
/// AltGr arrives as Ctrl+Alt, and AltGr+7 typing `{` mustn't count as Ctrl+7.
/// Extra Shift is still allowed, since some layouts need it for `/`.
fn consume(input: &mut InputState, shortcut: &KeyboardShortcut) -> bool {
    let mut found = false;
    input.events.retain(|event| {
        let matches = matches!(
            event,
            Event::Key { key, modifiers, pressed: true, .. }
                if *key == shortcut.logical_key
                    && modifiers.alt == shortcut.modifiers.alt
                    && modifiers.matches_logically(shortcut.modifiers)
        );
        found |= matches;
        !matches
    });
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(modifiers: Modifiers, key: Key) -> Vec<AppCommand> {
        let mut input = InputState::default();
        input.events.push(Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        });
        pressed(&mut input)
    }

    #[test]
    fn altgr_combinations_are_not_shortcuts() {
        assert_eq!(press(Modifiers::COMMAND, Key::K), vec![AppCommand::GoTo]);
        assert_eq!(
            press(Modifiers::COMMAND | Modifiers::ALT, Key::K),
            Vec::new()
        );
    }
}
