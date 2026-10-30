//! The design's building blocks: buttons, segmented controls, pills, key
//! caps and cards, styled from the theme's tokens.

use eframe::egui::{
    self, Button, Color32, CornerRadius, Frame, Margin, Response, RichText, Role, Sense, Stroke,
    Ui, WidgetInfo, vec2,
};

use crate::theme::{self, Palette};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    /// The accent-filled main action, e.g. Find or Save.
    Primary,
    /// A raised button with a hairline border.
    Secondary,
    /// No frame until hovered.
    Ghost,
    /// A destructive action.
    Danger,
}

/// Button heights from the design.
pub const TOOLBAR_HEIGHT: f32 = 34.0;
pub const QUERY_HEIGHT: f32 = 40.0;
pub const DIALOG_HEIGHT: f32 = 38.0;

/// A button with an optional icon before its label. The icon is left out
/// of the accessible name.
pub fn button(ui: &mut Ui, tone: Tone, icon: Option<&str>, label: &str, height: f32) -> Response {
    let palette = theme::palette(ui);
    let (fill, text, stroke) = match tone {
        Tone::Primary => (palette.accent, palette.accent_ink, Stroke::NONE),
        Tone::Secondary => (palette.raised, palette.text, Stroke::new(1.0, palette.line)),
        Tone::Ghost => (Color32::TRANSPARENT, palette.text, Stroke::NONE),
        Tone::Danger => (palette.danger_soft, palette.danger, Stroke::NONE),
    };
    let weight = if tone == Tone::Primary {
        theme::semibold()
    } else {
        egui::FontFamily::Proportional
    };
    let label_text = RichText::new(label).color(text).family(weight);
    let mut button = match icon {
        Some(icon) => {
            let icon_color = if tone == Tone::Ghost {
                palette.muted
            } else {
                text
            };
            Button::new((RichText::new(icon).size(16.0).color(icon_color), label_text))
        }
        None => Button::new(label_text),
    }
    .min_size(vec2(0.0, height))
    .corner_radius(CornerRadius::same(if height >= QUERY_HEIGHT {
        10
    } else {
        9
    }));
    button = match tone {
        Tone::Ghost => button.frame_when_inactive(false),
        _ => button.fill(fill).stroke(stroke),
    };
    let response = ui.add(button);
    if matches!(tone, Tone::Primary | Tone::Danger) && response.hovered() && ui.is_enabled() {
        hover_tint(ui, &response, palette);
    }
    let enabled = ui.is_enabled();
    response.widget_info(|| WidgetInfo::labeled(Role::Button, enabled, label));
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A square button showing only an icon; `label` names it for screen
/// readers and shows as a tooltip.
pub fn icon_button(ui: &mut Ui, icon: &str, label: &str, size: f32) -> Response {
    let palette = theme::palette(ui);
    let response = ui
        .add(
            Button::new(RichText::new(icon).size(size * 0.5).color(palette.muted))
                .frame_when_inactive(false)
                .min_size(vec2(size, size))
                .corner_radius(CornerRadius::same(8)),
        )
        .on_hover_text(label);
    let enabled = ui.is_enabled();
    response.widget_info(|| WidgetInfo::labeled(Role::Button, enabled, label));
    response
}

fn hover_tint(ui: &Ui, response: &Response, palette: &Palette) {
    let tint = if ui.visuals().dark_mode {
        Color32::from_white_alpha(22)
    } else {
        Color32::from_black_alpha(18)
    };
    let _ = palette;
    ui.painter()
        .rect_filled(response.rect, CornerRadius::same(9), tint);
}

/// A row of mutually exclusive options in a sunken track, e.g. List, JSON,
/// Table. Returns true when the value changed.
pub fn segmented<T: PartialEq + Copy>(
    ui: &mut Ui,
    value: &mut T,
    items: &[(T, Option<&str>, &str)],
    height: f32,
) -> bool {
    let palette = theme::palette(ui);
    let mut changed = false;
    Frame::new()
        .fill(palette.track)
        .stroke(Stroke::new(1.0, palette.line))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::same(3))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            // In a right-to-left row, add the items backwards so they read in order.
            let ordered: Vec<_> = if ui.layout().prefer_right_to_left() {
                items.iter().rev().collect()
            } else {
                items.iter().collect()
            };
            ui.horizontal(|ui| {
                for &(item, icon, label) in ordered {
                    let selected = *value == item;
                    let color = if selected {
                        palette.text
                    } else {
                        palette.muted
                    };
                    let text = RichText::new(label).color(color);
                    let mut button = match icon {
                        Some(icon) => {
                            Button::new((RichText::new(icon).size(15.0).color(color), text))
                        }
                        None => Button::new(text),
                    }
                    .min_size(vec2(0.0, height - 8.0))
                    .corner_radius(CornerRadius::same(7));
                    button = if selected {
                        button.fill(palette.active).stroke(Stroke::NONE)
                    } else {
                        button.frame_when_inactive(false)
                    };
                    let response = ui.add(button);
                    let enabled = ui.is_enabled();
                    response
                        .widget_info(|| WidgetInfo::selected(Role::Tab, enabled, selected, label));
                    if response.clicked() && !selected {
                        *value = item;
                        changed = true;
                    }
                }
            });
        });
    changed
}

/// A small rounded label, e.g. `view` or `unique`.
pub fn pill(ui: &mut Ui, text: &str, accent: bool) -> Response {
    let palette = theme::palette(ui);
    let (fill, color, stroke) = if accent {
        (palette.accent_soft, palette.accent_text, Stroke::NONE)
    } else {
        (
            palette.raised,
            palette.subtle,
            Stroke::new(1.0, palette.line),
        )
    };
    let galley =
        ui.painter()
            .layout_no_wrap(text.to_owned(), egui::FontId::proportional(12.0), color);
    let (rect, response) =
        ui.allocate_exact_size(vec2(galley.size().x + 16.0, 20.0), Sense::hover());
    ui.painter().rect(
        rect,
        CornerRadius::same(255),
        fill,
        stroke,
        egui::StrokeKind::Inside,
    );
    ui.painter()
        .galley(rect.center() - galley.size() / 2.0, galley, color);
    response.widget_info(|| WidgetInfo::labeled(Role::Label, true, text));
    response
}

/// Key caps such as Ctrl K, shown as a hint.
pub fn keys(ui: &mut Ui, keys: &[&str]) {
    let palette = theme::palette(ui);
    let ordered: Vec<_> = if ui.layout().prefer_right_to_left() {
        keys.iter().rev().collect()
    } else {
        keys.iter().collect()
    };
    let spacing = ui.spacing().item_spacing.x;
    ui.spacing_mut().item_spacing.x = 3.0;
    for key in ordered {
        let galley = ui.painter().layout_no_wrap(
            (*key).to_owned(),
            egui::FontId::monospace(11.0),
            palette.muted,
        );
        let (rect, _) =
            ui.allocate_exact_size(vec2(galley.size().x.max(8.0) + 10.0, 19.0), Sense::hover());
        ui.painter().rect(
            rect,
            CornerRadius::same(5),
            palette.surface,
            Stroke::new(1.0, palette.line),
            egui::StrokeKind::Inside,
        );
        ui.painter()
            .galley(rect.center() - galley.size() / 2.0, galley, palette.muted);
    }
    ui.spacing_mut().item_spacing.x = spacing;
}

/// An uppercase label over a group, e.g. CONNECTIONS.
pub fn section_label(ui: &mut Ui, text: &str) -> Response {
    let palette = theme::palette(ui);
    ui.label(
        RichText::new(text.to_uppercase())
            .text_style(theme::section_style())
            .extra_letter_spacing(0.7)
            .color(palette.subtle),
    )
}

/// A white (or raised, in dark) card with a hairline border.
pub fn card(ui: &Ui) -> Frame {
    let palette = theme::palette(ui);
    Frame::new()
        .fill(palette.surface)
        .stroke(Stroke::new(1.0, palette.line))
        .corner_radius(CornerRadius::same(12))
}

/// The raised band behind a table's header row inside a [`card`].
pub fn table_header_band(ui: &Ui, height: f32) {
    let palette = theme::palette(ui);
    let top = ui.cursor().top();
    let rect = egui::Rect::from_x_y_ranges(ui.max_rect().x_range(), top..=top + height);
    ui.painter().rect_filled(
        rect,
        CornerRadius {
            nw: 12,
            ne: 12,
            sw: 0,
            se: 0,
        },
        palette.raised,
    );
    ui.painter().hline(
        rect.x_range(),
        rect.bottom(),
        Stroke::new(1.0, palette.line),
    );
}

/// A thin vertical divider between toolbar groups.
pub fn divider(ui: &mut Ui, height: f32) {
    let palette = theme::palette(ui);
    let (rect, _) = ui.allocate_exact_size(vec2(9.0, height), Sense::hover());
    ui.painter().vline(
        rect.center().x,
        rect.y_range(),
        Stroke::new(1.0, palette.line),
    );
}

/// A dialog over a dimmed window, framed like the design's dialogs.
pub fn modal(ctx: &egui::Context, id: egui::Id) -> egui::Modal {
    let visuals = &ctx.global_style().visuals;
    let palette = theme::of(visuals);
    egui::Modal::new(id).backdrop_color(palette.scrim).frame(
        Frame::new()
            .fill(palette.surface)
            .stroke(Stroke::new(1.0, palette.line_strong))
            .corner_radius(CornerRadius::same(14))
            .shadow(visuals.window_shadow)
            .inner_margin(Margin::same(24)),
    )
}

/// A dialog's title, with an optional line under it such as the namespace.
pub fn dialog_title(ui: &mut Ui, title: &str, subtitle: Option<&str>) {
    let palette = theme::palette(ui);
    ui.label(
        RichText::new(title)
            .text_style(theme::title_style())
            .color(palette.text),
    );
    if let Some(subtitle) = subtitle {
        ui.label(
            RichText::new(subtitle)
                .monospace()
                .size(12.5)
                .color(palette.subtle),
        );
    }
    ui.add_space(10.0);
}

/// Lays out a dialog's buttons from the right: call with the main action
/// first. Separated from the content by a hairline.
pub fn dialog_footer<R>(ui: &mut Ui, add_buttons: impl FnOnce(&mut Ui) -> R) -> R {
    let palette = theme::palette(ui);
    ui.add_space(12.0);
    let rect = ui.available_rect_before_wrap();
    ui.painter()
        .hline(rect.x_range(), rect.top(), Stroke::new(1.0, palette.line));
    ui.add_space(14.0);
    ui.with_layout(
        egui::Layout::right_to_left(egui::Align::Center),
        add_buttons,
    )
    .inner
}
