//! The window's frame around the content: the top bar with the Ctrl+K
//! field, the tab bar, a collection's header, and the welcome screen.

use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, FontId, Layout, Rect, Response, RichText, Role,
    Sense, Stroke, StrokeKind, Ui, WidgetInfo, pos2, vec2,
};

use crate::icons;
use crate::tabs::Panel;
use crate::theme;
use crate::widgets::kit::{self, Tone};

pub const TOP_BAR_HEIGHT: f32 = 48.0;
pub const TAB_BAR_HEIGHT: f32 = 44.0;

#[derive(Default)]
pub struct TopBarOutput {
    pub goto: bool,
    pub settings: bool,
}

/// Logo on the left, the "Go to collection" field in the middle, settings
/// on the right.
pub fn top_bar(ui: &mut Ui) -> TopBarOutput {
    let palette = theme::palette(ui);
    let mut output = TopBarOutput::default();
    let rect = ui.max_rect();

    ui.horizontal_centered(|ui| {
        ui.add_space(4.0);
        logo(ui, 26.0);
        ui.label(
            RichText::new("nordpil")
                .family(theme::semibold())
                .size(15.0)
                .color(palette.text),
        );
    });

    let width = (rect.width() - 2.0 * 200.0).clamp(240.0, 560.0);
    let field = Rect::from_center_size(rect.center(), vec2(width, 32.0));
    let response = ui.interact(field, ui.scope_id().with("goto_field"), Sense::click());
    let hovered = response.hovered();
    let painter = ui.painter();
    painter.rect(
        field,
        CornerRadius::same(9),
        if hovered {
            palette.hover
        } else {
            palette.raised
        },
        Stroke::new(
            1.0,
            if hovered {
                palette.line_strong
            } else {
                palette.line
            },
        ),
        StrokeKind::Inside,
    );
    painter.text(
        pos2(field.left() + 14.0, field.center().y),
        Align2::LEFT_CENTER,
        icons::SEARCH,
        FontId::proportional(15.0),
        palette.subtle,
    );
    painter.text(
        pos2(field.left() + 36.0, field.center().y),
        Align2::LEFT_CENTER,
        "Go to collection…",
        FontId::proportional(13.5),
        palette.subtle,
    );
    let mut keys_ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(field.shrink2(vec2(7.0, 6.0)))
            .layout(Layout::right_to_left(Align::Center)),
    );
    kit::shortcut(&mut keys_ui, &crate::shortcuts::GO_TO);
    response.widget_info(|| WidgetInfo::labeled(Role::Button, true, "Go to collection"));
    output.goto = response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .clicked();

    let gear = Rect::from_min_max(pos2(rect.right() - 40.0, rect.top()), rect.right_bottom());
    let mut gear_ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(gear)
            .layout(Layout::right_to_left(Align::Center)),
    );
    output.settings = kit::icon_button(&mut gear_ui, icons::SETTINGS, "Settings", 32.0).clicked();
    output
}

/// The north arrow in an accent tile.
pub fn logo(ui: &mut Ui, size: f32) -> Response {
    let palette = theme::palette(ui);
    let (rect, response) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
    ui.painter().rect_filled(
        rect,
        CornerRadius::same((size * 0.27) as u8),
        palette.accent,
    );
    ui.painter().text(
        rect.center() + vec2(0.0, size * 0.03),
        Align2::CENTER_CENTER,
        icons::NAVIGATION,
        FontId::proportional(size * 0.58),
        palette.accent_ink,
    );
    response
}

pub enum TabEvent {
    Select(usize),
    Close(usize),
}

/// The open collections as tabs; the active one joins the content below.
/// What a tab in the tab bar shows.
pub struct TabTitle {
    pub database: String,
    pub collection: String,
    /// The connection's color, if it has one.
    pub color: Option<Color32>,
}

/// A tab's icon takes its connection's color, dimmed on inactive tabs.
/// Without a color it is accent when active, subtle otherwise.
pub fn tab_icon_color(palette: &theme::Palette, color: Option<Color32>, selected: bool) -> Color32 {
    match (color, selected) {
        (Some(color), true) => color,
        (Some(color), false) => color.gamma_multiply(0.6),
        (None, true) => palette.accent,
        (None, false) => palette.subtle,
    }
}

pub fn tab_bar(ui: &mut Ui, titles: &[TabTitle], active: Option<usize>) -> Option<TabEvent> {
    let palette = theme::palette(ui);
    let mut event = None;
    egui::ScrollArea::horizontal()
        .id_salt("tab_bar")
        .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                ui.add_space(12.0);
                for (index, title) in titles.iter().enumerate() {
                    let TabTitle {
                        database,
                        collection,
                        color,
                    } = title;
                    let selected = active == Some(index);
                    let font = FontId::proportional(14.0);
                    let label = format!("{database}.{collection}");
                    let text_width = ui.fonts_mut(|fonts| {
                        fonts
                            .layout_no_wrap(label.clone(), font.clone(), palette.text)
                            .size()
                            .x
                    });
                    let width = 12.0 + 16.0 + 8.0 + text_width + 8.0 + 22.0 + 6.0;
                    let (rect, response) =
                        ui.allocate_exact_size(vec2(width, TAB_BAR_HEIGHT), Sense::click());
                    let tab = Rect::from_min_max(
                        pos2(rect.left(), rect.bottom() - 36.0),
                        pos2(rect.right(), rect.bottom() + 1.0),
                    );
                    let painter = ui.painter();
                    if selected {
                        painter.rect(
                            tab,
                            CornerRadius {
                                nw: 9,
                                ne: 9,
                                sw: 0,
                                se: 0,
                            },
                            palette.main,
                            Stroke::new(1.0, palette.line),
                            StrokeKind::Inside,
                        );
                        // Open the bottom edge into the content.
                        painter.hline(
                            tab.x_range().shrink(1.0),
                            tab.bottom() - 1.0,
                            Stroke::new(2.0, palette.main),
                        );
                    } else if response.hovered() {
                        painter.rect_filled(
                            tab.shrink2(vec2(0.0, 3.0)).translate(vec2(0.0, -2.0)),
                            CornerRadius::same(8),
                            palette.hover,
                        );
                    }
                    let y = tab.center().y - 1.0;
                    painter.text(
                        pos2(tab.left() + 12.0, y),
                        Align2::LEFT_CENTER,
                        icons::LAYERS,
                        FontId::proportional(15.0),
                        tab_icon_color(palette, *color, selected),
                    );
                    let mut job = egui::text::LayoutJob::default();
                    job.append(
                        &format!("{database}."),
                        0.0,
                        egui::TextFormat::simple(font.clone(), palette.subtle),
                    );
                    job.append(
                        collection,
                        0.0,
                        egui::TextFormat::simple(
                            font.clone(),
                            if selected {
                                palette.text
                            } else {
                                palette.muted
                            },
                        ),
                    );
                    let galley = ui.fonts_mut(|fonts| fonts.layout_job(job));
                    painter.galley(
                        pos2(tab.left() + 36.0, y - galley.size().y / 2.0),
                        galley,
                        palette.text,
                    );
                    response
                        .widget_info(|| WidgetInfo::selected(Role::Tab, true, selected, &label));
                    if response.clicked() {
                        event = Some(TabEvent::Select(index));
                    }
                    if response.middle_clicked() {
                        event = Some(TabEvent::Close(index));
                    }
                    let close_rect =
                        Rect::from_center_size(pos2(tab.right() - 17.0, y), vec2(22.0, 22.0));
                    let close = ui
                        .interact(
                            close_rect,
                            ui.scope_id().with(("close_tab", index)),
                            Sense::click(),
                        )
                        .on_hover_text("Close tab")
                        .on_hover_cursor(egui::CursorIcon::PointingHand);
                    kit::paint_icon_button(ui, &close, icons::X, 22.0, None);
                    close.widget_info(|| WidgetInfo::labeled(Role::Button, true, "Close tab"));
                    if close.clicked() {
                        event = Some(TabEvent::Close(index));
                    }
                }
            });
        });
    event
}

/// Breadcrumb, collection name and count, and the Documents/Indexes switch,
/// which moves under the name when the window is narrow.
pub fn collection_header(
    ui: &mut Ui,
    connection: &str,
    database: &str,
    collection: &str,
    count: Option<String>,
    panel: &mut Panel,
) {
    let palette = theme::palette(ui);
    let panels = [
        (Panel::Documents, Some(icons::FILE_TEXT), "Documents"),
        (Panel::Indexes, Some(icons::KEY), "Indexes"),
        (Panel::Shell, Some(icons::SQUARE_TERMINAL), "Shell"),
    ];
    let title_width = kit::text_width(ui, collection, FontId::new(21.0, theme::semibold()))
        + count.as_deref().map_or(0.0, |count| {
            kit::text_width(ui, count, FontId::proportional(13.0)) + 10.0
        });
    let switch_width = kit::segmented_width(ui, &panels, true);
    let stacked = ui.available_width() < title_width + switch_width + 32.0;

    let title = |ui: &mut Ui| {
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 5.0;
                let crumb = |text: &str| RichText::new(text).size(12.5).color(palette.subtle);
                ui.label(crumb(connection));
                ui.label(crumb(icons::CHEVRON_RIGHT).size(11.0));
                ui.label(crumb(database));
            });
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                ui.add(egui::Label::new(RichText::new(collection).heading()).truncate());
                if let Some(count) = &count {
                    ui.label(RichText::new(count).size(13.0).color(palette.subtle));
                }
            });
        });
    };
    if stacked {
        title(ui);
        ui.add_space(8.0);
        kit::segmented(ui, panel, &panels, 38.0);
    } else {
        ui.horizontal(|ui| {
            title(ui);
            ui.with_layout(Layout::right_to_left(Align::Max), |ui| {
                kit::segmented(ui, panel, &panels, 38.0);
            });
        });
    }
}

/// Shown when no collection is open. Returns true when "New connection"
/// is clicked.
pub fn welcome(ui: &mut Ui, has_connections: bool) -> bool {
    let palette = theme::palette(ui);
    let mut new_connection = false;
    ui.vertical_centered(|ui| {
        let top = (ui.available_height() - 260.0).max(0.0) / 2.0;
        ui.add_space(top);
        let (rect, _) = ui.allocate_exact_size(vec2(64.0, 64.0), Sense::hover());
        ui.painter()
            .rect_filled(rect, CornerRadius::same(18), palette.accent_soft);
        ui.painter().text(
            rect.center(),
            Align2::CENTER_CENTER,
            icons::NAVIGATION,
            FontId::proportional(32.0),
            palette.accent,
        );
        ui.add_space(14.0);
        ui.label(
            RichText::new(if has_connections {
                "Open a collection"
            } else {
                "Welcome to nordpil"
            })
            .family(theme::semibold())
            .size(24.0)
            .color(palette.text),
        );
        ui.add_space(4.0);
        ui.label(
            RichText::new(if has_connections {
                format!(
                    "Pick one in the sidebar, or press {} to go to any collection.",
                    crate::shortcuts::text(ui.ctx(), &crate::shortcuts::GO_TO)
                )
            } else {
                "Save a connection to browse databases, run queries and edit documents.".to_owned()
            })
            .size(14.5)
            .color(palette.muted),
        );
        if !has_connections {
            ui.add_space(14.0);
            new_connection =
                kit::button(ui, Tone::Primary, Some(icons::PLUS), "New connection", 40.0).clicked();
        }
    });
    new_connection
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tab_icons_take_the_connection_color_dimmed_when_inactive() {
        let palette = theme::of(&egui::Visuals::dark());
        let red = Color32::from_rgb(0xe5, 0x63, 0x6f);

        assert_eq!(tab_icon_color(palette, Some(red), true), red);
        assert_eq!(
            tab_icon_color(palette, Some(red), false),
            red.gamma_multiply(0.6)
        );
        assert_eq!(tab_icon_color(palette, None, true), palette.accent);
        assert_eq!(tab_icon_color(palette, None, false), palette.subtle);
    }
}
