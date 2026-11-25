//! All that shows while nordpil is locked: why it locked, whose sign-in
//! unlocks it, and how.

use eframe::egui::{Align2, CornerRadius, FontId, Id, Key, RichText, Sense, TextEdit, Ui, vec2};

use crate::lock::{Locked, Method, Reason};
use crate::ui::dialogs;
use crate::widgets::kit::{self, Tone};
use crate::{icons, theme};

/// Returns true when the user asks to unlock.
pub fn show(ui: &mut Ui, locked: &mut Locked, attempting: bool) -> bool {
    let palette = theme::palette(ui);
    let mut unlock = false;
    ui.vertical_centered(|ui| {
        let top = (ui.available_height() - 340.0).max(0.0) / 2.0;
        ui.add_space(top);
        let (rect, _) = ui.allocate_exact_size(vec2(64.0, 64.0), Sense::hover());
        ui.painter()
            .rect_filled(rect, CornerRadius::same(18), palette.accent_soft);
        ui.painter().text(
            rect.center(),
            Align2::CENTER_CENTER,
            icons::LOCK,
            FontId::proportional(32.0),
            palette.accent,
        );
        ui.add_space(14.0);
        ui.label(
            RichText::new("nordpil is locked")
                .family(theme::semibold())
                .size(24.0)
                .color(palette.text),
        );
        ui.add_space(4.0);
        ui.label(
            RichText::new(reason_text(locked))
                .size(14.5)
                .color(palette.muted),
        );
        if let Some(user) = &locked.user {
            ui.add_space(18.0);
            ui.label(
                RichText::new(user)
                    .family(theme::semibold())
                    .size(15.0)
                    .color(palette.text),
            );
        }
        ui.add_space(10.0);
        if locked.method == Method::Password {
            let label = ui.label(RichText::new("Password").color(palette.muted));
            let mut output = ui
                .add_enabled_ui(!attempting, |ui| {
                    TextEdit::singleline(&mut locked.password)
                        .id(Id::unique("lock_password"))
                        .password(true)
                        .desired_width(280.0)
                        .show(ui)
                })
                .inner;
            // Undo would bring back the password of an earlier attempt or lock.
            output.state.clear_undoer();
            output.state.store(ui.ctx(), output.response.id);
            let field = output.response.response.labelled_by(label.id);
            if !attempting && ui.memory(|memory| memory.focused().is_none()) {
                field.request_focus();
            }
            unlock |= field.lost_focus() && ui.input(|input| input.key_pressed(Key::Enter));
            ui.label(
                RichText::new("Your Linux login password, checked by the system.")
                    .size(12.5)
                    .color(palette.muted),
            );
            ui.add_space(8.0);
        }
        let button = ui
            .add_enabled_ui(!attempting, |ui| {
                kit::button(
                    ui,
                    Tone::Primary,
                    Some(icons::KEY),
                    "Unlock",
                    kit::DIALOG_HEIGHT,
                )
            })
            .inner;
        unlock |= button.clicked();
        if attempting {
            ui.add_space(8.0);
            ui.spinner();
        }
        if let Some(error) = &locked.error {
            ui.add_space(8.0);
            dialogs::error_label(ui, error);
        }
        if let Some(hint) = &locked.hint {
            ui.label(RichText::new(hint).color(palette.muted));
        }
    });
    unlock && !attempting
}

/// E.g. "Locked after 1 h idle at 14:05".
fn reason_text(locked: &Locked) -> String {
    match locked.reason {
        Reason::Idle(timeout) => {
            let minutes = timeout.duration().unwrap_or_default().as_secs() / 60;
            let idle = if minutes < 60 {
                format!("{minutes} min")
            } else {
                format!("{} h", minutes / 60)
            };
            let at = chrono::DateTime::<chrono::Local>::from(locked.at).format("%H:%M");
            format!("Locked after {idle} idle at {at}")
        }
        Reason::Manual => "Locked manually".to_owned(),
        Reason::Launch => "Locked at launch".to_owned(),
    }
}
