//! App-wide keyboard shortcuts: one table that the key handling, the
//! shortcuts sheet and the macOS menu bar all read. Keys that only work in
//! one editor, such as Ctrl+Enter, stay with that editor.

use eframe::egui::{self, Event, FontId, InputState, Key, KeyboardShortcut, Modifiers};

use crate::tabs::Panel;

/// What an app-wide shortcut does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppCommand {
    GoTo,
    /// The filter, or the script on the Shell panel.
    FocusEditor,
    CloseTab,
    ReopenTab,
    NextTab,
    PreviousTab,
    /// Counted from 0.
    SelectTab(usize),
    LastTab,
    Panel(Panel),
    /// Runs the Documents query again, or reloads the indexes.
    Refresh,
    Settings,
    SaveScript,
    OpenScript,
    StopScript,
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
pub const CLOSE_TAB: KeyboardShortcut = command(Key::W);
pub const REOPEN_TAB: KeyboardShortcut =
    KeyboardShortcut::new(Modifiers::COMMAND.plus(Modifiers::SHIFT), Key::T);
/// Control on macOS too, where Cmd+Tab switches apps.
pub const NEXT_TAB: KeyboardShortcut = KeyboardShortcut::new(Modifiers::CTRL, Key::Tab);
pub const PREVIOUS_TAB: KeyboardShortcut =
    KeyboardShortcut::new(Modifiers::CTRL.plus(Modifiers::SHIFT), Key::Tab);
pub const LAST_TAB: KeyboardShortcut = command(Key::Num9);
pub const DOCUMENTS: KeyboardShortcut = KeyboardShortcut::new(Modifiers::ALT, Key::Num1);
pub const INDEXES: KeyboardShortcut = KeyboardShortcut::new(Modifiers::ALT, Key::Num2);
pub const SHELL: KeyboardShortcut = KeyboardShortcut::new(Modifiers::ALT, Key::Num3);
pub const REFRESH: KeyboardShortcut = command(Key::R);
pub const REFRESH_F5: KeyboardShortcut = KeyboardShortcut::new(Modifiers::NONE, Key::F5);
pub const SETTINGS: KeyboardShortcut = command(Key::Comma);
/// Saves a document dialog; handled there.
pub const SAVE_DOCUMENT: KeyboardShortcut = command(Key::Enter);
/// In the shell editor; handled there.
pub const RUN_STATEMENT: KeyboardShortcut = command(Key::Enter);
pub const RUN_SCRIPT: KeyboardShortcut = KeyboardShortcut::new(Modifiers::SHIFT, Key::Enter);
/// Also stops a script, when no popup or dialog wants it; handled in the app.
pub const ESCAPE: KeyboardShortcut = KeyboardShortcut::new(Modifiers::NONE, Key::Escape);
pub const SAVE_SCRIPT: KeyboardShortcut = command(Key::S);
pub const OPEN_SCRIPT: KeyboardShortcut = command(Key::O);
pub const STOP_SCRIPT: KeyboardShortcut = command(Key::Period);

/// Ctrl+1 to Ctrl+8 go to that tab.
const fn tab(index: usize, keys: &'static [KeyboardShortcut]) -> Binding {
    Binding {
        command: AppCommand::SelectTab(index),
        keys,
    }
}

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
    Binding {
        command: AppCommand::CloseTab,
        keys: &[CLOSE_TAB],
    },
    Binding {
        command: AppCommand::ReopenTab,
        keys: &[REOPEN_TAB],
    },
    Binding {
        command: AppCommand::PreviousTab,
        keys: &[PREVIOUS_TAB],
    },
    Binding {
        command: AppCommand::NextTab,
        keys: &[NEXT_TAB],
    },
    tab(0, &[command(Key::Num1)]),
    tab(1, &[command(Key::Num2)]),
    tab(2, &[command(Key::Num3)]),
    tab(3, &[command(Key::Num4)]),
    tab(4, &[command(Key::Num5)]),
    tab(5, &[command(Key::Num6)]),
    tab(6, &[command(Key::Num7)]),
    tab(7, &[command(Key::Num8)]),
    Binding {
        command: AppCommand::LastTab,
        keys: &[LAST_TAB],
    },
    Binding {
        command: AppCommand::Panel(Panel::Documents),
        keys: &[DOCUMENTS],
    },
    Binding {
        command: AppCommand::Panel(Panel::Indexes),
        keys: &[INDEXES],
    },
    Binding {
        command: AppCommand::Panel(Panel::Shell),
        keys: &[SHELL],
    },
    Binding {
        command: AppCommand::Refresh,
        keys: &[REFRESH, REFRESH_F5],
    },
    Binding {
        command: AppCommand::Settings,
        keys: &[SETTINGS],
    },
    Binding {
        command: AppCommand::SaveScript,
        keys: &[SAVE_SCRIPT],
    },
    Binding {
        command: AppCommand::OpenScript,
        keys: &[OPEN_SCRIPT],
    },
    Binding {
        command: AppCommand::StopScript,
        keys: &[STOP_SCRIPT],
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

/// The key caps of `shortcut`, as macOS or everyone else writes them:
/// ⌃ ⌥ ⇧ ⌘ L on a Mac (names when the font lacks the symbols), Ctrl Alt
/// Shift L elsewhere.
pub fn key_caps(ctx: &egui::Context, shortcut: &KeyboardShortcut) -> Vec<&'static str> {
    let mac = ctx.os().is_mac();
    let symbols = mac && ctx.fonts_mut(|fonts| fonts.has_glyphs(&FontId::monospace(11.0), "⌃⌥⇧⌘"));
    caps(shortcut, mac, symbols)
}

/// `shortcut` as text for a tooltip, e.g. "Ctrl+L" or "⌘L".
pub fn text(ctx: &egui::Context, shortcut: &KeyboardShortcut) -> String {
    let caps = key_caps(ctx, shortcut);
    let symbols = caps.first().is_some_and(|cap| "⌃⌥⇧⌘".contains(cap));
    caps.join(if symbols { "" } else { "+" })
}

fn caps(shortcut: &KeyboardShortcut, mac: bool, symbols: bool) -> Vec<&'static str> {
    let Modifiers {
        alt, ctrl, shift, ..
    } = shortcut.modifiers;
    let command = shortcut.modifiers.command || shortcut.modifiers.mac_cmd;
    let mut caps = Vec::new();
    let mut add = |on: bool, cap: &'static str| {
        if on {
            caps.push(cap);
        }
    };
    match (mac, symbols) {
        // Apple's order: Control, Option, Shift, Command.
        (true, true) => {
            add(ctrl, "⌃");
            add(alt, "⌥");
            add(shift, "⇧");
            add(command, "⌘");
        }
        (true, false) => {
            add(ctrl, "Ctrl");
            add(alt, "Option");
            add(shift, "Shift");
            add(command, "Cmd");
        }
        (false, _) => {
            add(ctrl || command, "Ctrl");
            add(alt, "Alt");
            add(shift, "Shift");
        }
    }
    caps.push(match shortcut.logical_key {
        Key::Escape => "Esc",
        key => key.symbol_or_name(),
    });
    caps
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
    fn key_caps_follow_the_platform() {
        let shortcut = KeyboardShortcut::new(Modifiers::COMMAND | Modifiers::SHIFT, Key::T);
        assert_eq!(caps(&shortcut, false, false), ["Ctrl", "Shift", "T"]);
        assert_eq!(caps(&shortcut, true, true), ["⇧", "⌘", "T"]);
        assert_eq!(caps(&shortcut, true, false), ["Shift", "Cmd", "T"]);
        let tab = KeyboardShortcut::new(Modifiers::CTRL, Key::Tab);
        assert_eq!(caps(&tab, true, true), ["⌃", "Tab"]);
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
