//! The macOS menu bar. Its items run the same [`AppCommand`]s as the keys,
//! with the keys from [`crate::shortcuts`] as their accelerators. A key a
//! menu item has is caught by the menu, so egui doesn't see it too.

use crossbeam_channel::Receiver;
use eframe::egui::{self, Key, KeyboardShortcut};
use muda::accelerator::{Accelerator, Code, Modifiers};
use muda::{
    AboutMetadata, IsMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem, Submenu,
};

use crate::shortcuts::{AppCommand, BINDINGS};
use crate::tabs::Panel;

pub struct MacMenu {
    /// Kept alive: dropping it would empty the menu bar.
    _menu: Menu,
    items: Vec<(MenuItem, AppCommand)>,
    events: Receiver<MenuId>,
    /// What the items were last enabled for.
    state: Option<MenuState>,
}

/// What decides which items are enabled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MenuState {
    pub has_tabs: bool,
    pub can_reopen: bool,
    pub on_shell: bool,
    pub on_aggregate: bool,
}

impl MacMenu {
    /// Builds the menu bar and puts it on the app. Call on the main thread
    /// once the app has launched, e.g. while creating the eframe app.
    pub fn install(ctx: &egui::Context) -> Self {
        let (sender, events) = crossbeam_channel::unbounded();
        let ctx = ctx.clone();
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            let _ = sender.send(event.id);
            ctx.request_repaint();
        }));

        let mut items = Vec::new();
        let mut item = |label: &str, command: AppCommand| {
            let accelerator = BINDINGS
                .iter()
                .find(|binding| binding.command == command)
                .and_then(|binding| binding.keys.first())
                .and_then(accelerator);
            let item = MenuItem::new(label, true, accelerator);
            items.push((item.clone(), command));
            item
        };
        let separator = PredefinedMenuItem::separator;

        let about = PredefinedMenuItem::about(
            None,
            Some(AboutMetadata {
                name: Some("nordpil".to_owned()),
                version: Some(env!("CARGO_PKG_VERSION").to_owned()),
                ..AboutMetadata::default()
            }),
        );
        let settings = item("Settings…", AppCommand::Settings);
        let app_menu = submenu(
            "nordpil",
            &[
                &about,
                &separator(),
                &settings,
                &separator(),
                &PredefinedMenuItem::services(None),
                &separator(),
                &PredefinedMenuItem::hide(None),
                &PredefinedMenuItem::hide_others(None),
                &PredefinedMenuItem::show_all(None),
                &separator(),
                &PredefinedMenuItem::quit(None),
            ],
        );

        let close = item("Close Tab", AppCommand::CloseTab);
        let reopen = item("Reopen Closed Tab", AppCommand::ReopenTab);
        let open_script = item("Open Script…", AppCommand::OpenScript);
        let save_script = item("Save Script…", AppCommand::SaveScript);
        let file = submenu(
            "File",
            &[&close, &reopen, &separator(), &open_script, &save_script],
        );

        let goto = item("Go to Collection…", AppCommand::GoTo);
        let focus = item("Focus Filter or Script", AppCommand::FocusEditor);
        let ask_ai = item("Ask AI…", AppCommand::AskAi);
        let documents = item("Documents", AppCommand::Panel(Panel::Documents));
        let indexes = item("Indexes", AppCommand::Panel(Panel::Indexes));
        let shell = item("Shell", AppCommand::Panel(Panel::Shell));
        let aggregate = item("Aggregation", AppCommand::Panel(Panel::Aggregate));
        let refresh = item("Refresh", AppCommand::Refresh);
        let stop = item("Stop", AppCommand::StopScript);
        let view = submenu(
            "View",
            &[
                &goto,
                &focus,
                &ask_ai,
                &separator(),
                &documents,
                &indexes,
                &shell,
                &aggregate,
                &separator(),
                &refresh,
                &stop,
            ],
        );

        let next = item("Show Next Tab", AppCommand::NextTab);
        let previous = item("Show Previous Tab", AppCommand::PreviousTab);
        let window = submenu(
            "Window",
            &[
                &PredefinedMenuItem::minimize(None),
                &PredefinedMenuItem::zoom(None),
                &separator(),
                &next,
                &previous,
                &separator(),
                &PredefinedMenuItem::bring_all_to_front(None),
            ],
        );

        let sheet = item("Keyboard Shortcuts", AppCommand::Shortcuts);
        let help = submenu("Help", &[&sheet]);

        let menu = Menu::new();
        for submenu in [&app_menu, &file, &view, &window, &help] {
            if let Err(error) = menu.append(submenu) {
                tracing::warn!(%error, "could not build the menu bar");
            }
        }
        menu.init_for_nsapp();
        window.set_as_windows_menu_for_nsapp();
        help.set_as_help_menu_for_nsapp();

        Self {
            _menu: menu,
            items,
            events,
            state: None,
        }
    }

    /// The commands of the items picked since the last call.
    pub fn commands(&self) -> Vec<AppCommand> {
        self.events
            .try_iter()
            .filter_map(|id| {
                self.items
                    .iter()
                    .find(|(item, _)| *item.id() == id)
                    .map(|(_, command)| *command)
            })
            .collect()
    }

    /// Greys out the items that would do nothing now.
    pub fn update(&mut self, state: MenuState) {
        if self.state == Some(state) {
            return;
        }
        self.state = Some(state);
        for (item, command) in &self.items {
            let enabled = match command {
                AppCommand::CloseTab
                | AppCommand::FocusEditor
                | AppCommand::NextTab
                | AppCommand::PreviousTab
                | AppCommand::SelectTab(_)
                | AppCommand::LastTab
                | AppCommand::Panel(_)
                | AppCommand::Refresh
                | AppCommand::AskAi => state.has_tabs,
                AppCommand::ReopenTab => state.can_reopen,
                AppCommand::SaveScript | AppCommand::OpenScript => state.on_shell,
                AppCommand::StopScript => state.on_shell || state.on_aggregate,
                AppCommand::GoTo | AppCommand::Settings | AppCommand::Shortcuts => true,
            };
            item.set_enabled(enabled);
        }
    }
}

fn submenu(title: &str, items: &[&dyn IsMenuItem]) -> Submenu {
    Submenu::with_items(title, true, items).unwrap_or_else(|error| {
        tracing::warn!(%error, title, "could not build a menu");
        Submenu::new(title, true)
    })
}

/// `shortcut` as a menu accelerator; `None` for keys the menu can't show.
fn accelerator(shortcut: &KeyboardShortcut) -> Option<Accelerator> {
    let egui::Modifiers {
        alt, ctrl, shift, ..
    } = shortcut.modifiers;
    let command = shortcut.modifiers.command || shortcut.modifiers.mac_cmd;
    let mut modifiers = Modifiers::empty();
    modifiers.set(Modifiers::META, command);
    modifiers.set(Modifiers::CONTROL, ctrl);
    modifiers.set(Modifiers::ALT, alt);
    modifiers.set(Modifiers::SHIFT, shift);
    let code = match shortcut.logical_key {
        Key::K => Code::KeyK,
        Key::L => Code::KeyL,
        Key::O => Code::KeyO,
        Key::R => Code::KeyR,
        Key::S => Code::KeyS,
        Key::T => Code::KeyT,
        Key::W => Code::KeyW,
        Key::Comma => Code::Comma,
        Key::Period => Code::Period,
        Key::Slash => Code::Slash,
        Key::Tab => Code::Tab,
        Key::F1 => Code::F1,
        Key::F5 => Code::F5,
        Key::Num1 => Code::Digit1,
        Key::Num2 => Code::Digit2,
        Key::Num3 => Code::Digit3,
        Key::Num4 => Code::Digit4,
        Key::Num9 => Code::Digit9,
        _ => return None,
    };
    Some(Accelerator::new(modifiers, code))
}
