//! F1: every keyboard shortcut, grouped, with its keys as this platform
//! writes them.

use eframe::egui::{self, RichText};

use crate::shortcuts::{self, BINDINGS, Group, SHEET_LINES};
use crate::theme;
use crate::widgets::kit::{self, Tone};

/// Shows the sheet; returns false once it should close.
pub fn show(ctx: &egui::Context) -> bool {
    let mut open = true;
    let modal = kit::modal(ctx, egui::Id::unique("shortcuts_dialog")).show(ctx, |ui| {
        kit::dialog_width(ui, 820.0);
        kit::dialog_title(ui, "Keyboard shortcuts", None);
        let height = ctx.content_rect().height() - 220.0;
        egui::ScrollArea::vertical()
            .max_height(height.max(160.0))
            .show(ui, |ui| {
                if ui.available_width() >= 700.0 {
                    ui.columns(2, |columns| {
                        groups(&mut columns[0], &[Group::General, Group::Tabs]);
                        groups(
                            &mut columns[1],
                            &[Group::Documents, Group::Shell, Group::Aggregation],
                        );
                    });
                } else {
                    groups(ui, &Group::ALL);
                }
            });
        kit::dialog_footer(ui, |ui| {
            if kit::button(ui, Tone::Primary, None, "Done", kit::DIALOG_HEIGHT).clicked() {
                open = false;
            }
        });
    });
    open && !modal.should_close()
}

fn groups(ui: &mut egui::Ui, groups: &[Group]) {
    for group in groups {
        ui.add_space(8.0);
        kit::section_label(ui, group.title());
        ui.add_space(2.0);
        egui::Grid::new(("shortcuts", group.title()))
            .num_columns(2)
            .spacing([16.0, 6.0])
            .min_col_width(250.0)
            .show(ui, |ui| {
                for (label, keys, key_text) in lines(*group) {
                    ui.label(label);
                    ui.horizontal(|ui| keys_cell(ui, keys, key_text));
                    ui.end_row();
                }
            });
    }
}

type Line = (
    &'static str,
    &'static [egui::KeyboardShortcut],
    Option<&'static str>,
);

/// The group's sheet lines, then its labelled bindings.
fn lines(group: Group) -> Vec<Line> {
    let extra = SHEET_LINES
        .iter()
        .filter(|line| line.group == group)
        .map(|line| (line.label, line.keys, line.key_text));
    let bound = BINDINGS
        .iter()
        .filter(|binding| binding.group == group && !binding.label.is_empty())
        .map(|binding| (binding.label, binding.keys, None));
    extra.chain(bound).collect()
}

fn keys_cell(ui: &mut egui::Ui, keys: &[egui::KeyboardShortcut], key_text: Option<&str>) {
    let muted = theme::palette(ui).muted;
    for (index, shortcut) in keys.iter().enumerate() {
        if index > 0 {
            ui.label(RichText::new("or").size(12.5).color(muted));
        }
        let mut caps = shortcuts::key_caps(ui.ctx(), shortcut);
        if let (Some(text), Some(last)) = (key_text, caps.last_mut()) {
            *last = text;
        }
        kit::keys(ui, &caps);
    }
}
