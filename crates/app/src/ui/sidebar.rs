//! Saved connections and, for open ones, their databases and collections.

use eframe::egui::collapsing_header::CollapsingState;
use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, CursorIcon, DragAndDrop, FontId, Frame, Layout,
    Margin, PointerButton, Pos2, Rangef, Rect, Response, Role, Sense, Stroke, WidgetInfo, pos2,
    vec2,
};
use nordpil_backend::CollectionKind;
use nordpil_backend::store::ConnectionColor;
use nordpil_backend::uri::ConnectionUri;

use super::Action;
use crate::connections::{Collections, ConnectionEntry, ConnectionState, DatabaseNode, Selection};
use crate::icons;
use crate::theme::{self, Palette};
use crate::widgets::kit;

const INDENT: f32 = 16.0;

/// The drag-and-drop payload while a connection is dragged to a new place.
struct DraggedConnection(uuid::Uuid);

/// `links` offers editing a database's linked fields, for lookups.
pub fn show(
    ui: &mut egui::Ui,
    entries: &[ConnectionEntry],
    selection: Option<&Selection>,
    links: bool,
    actions: &mut Vec<Action>,
) {
    let palette = theme::palette(ui);
    let header = Frame::new()
        .inner_margin(Margin {
            left: 18,
            right: 8,
            top: 0,
            bottom: 0,
        })
        .show(ui, |ui| {
            ui.set_height(crate::ui::chrome::TAB_BAR_HEIGHT);
            ui.horizontal_centered(|ui| {
                kit::section_label(ui, "Connections");
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if kit::icon_button(ui, icons::PLUS, "Add connection", 30.0).clicked() {
                        actions.push(Action::NewConnection);
                    }
                });
            });
        })
        .response;
    ui.painter().hline(
        header.rect.x_range(),
        header.rect.bottom() - 0.5,
        Stroke::new(1.0, palette.line),
    );

    if entries.is_empty() {
        Frame::new().inner_margin(Margin::same(18)).show(ui, |ui| {
            ui.weak("No saved connections yet.");
        });
        return;
    }
    egui::ScrollArea::vertical()
        .auto_shrink(false)
        .show(ui, |ui| {
            Frame::new().inner_margin(Margin::same(10)).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                let dragged = DragAndDrop::payload::<DraggedConnection>(ui.ctx()).map(|d| d.0);
                let mut blocks = Vec::with_capacity(entries.len());
                for (index, entry) in entries.iter().enumerate() {
                    let place = Place {
                        index,
                        last: index + 1 == entries.len(),
                        dragged: dragged == Some(entry.saved.id),
                    };
                    let top = ui.cursor().top();
                    connection(ui, entry, place, selection, links, actions);
                    blocks.push(Rangef::new(top, ui.cursor().top()));
                    ui.add_space(4.0);
                }
                if let Some(id) = dragged {
                    drag_connection(ui, entries, &blocks, id, actions);
                }
            });
        });
}

/// Where a connection is in the list.
#[derive(Clone, Copy)]
struct Place {
    index: usize,
    last: bool,
    /// It is being dragged to a new place.
    dragged: bool,
}

/// Follows a dragged connection: its ghost, the gap it would land in, and the
/// move when the pointer lets go. `blocks` are the heights the connections
/// take up, their databases included.
fn drag_connection(
    ui: &egui::Ui,
    entries: &[ConnectionEntry],
    blocks: &[Rangef],
    id: uuid::Uuid,
    actions: &mut Vec<Action>,
) {
    let Some(from) = entries.iter().position(|entry| entry.saved.id == id) else {
        return;
    };
    let Some(pointer) = ui.ctx().pointer_interact_pos() else {
        return;
    };
    ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
    ghost(ui, &entries[from], pointer);
    if !ui.clip_rect().contains(pointer) {
        return;
    }
    let gap = blocks
        .iter()
        .position(|block| pointer.y < block.center())
        .unwrap_or(blocks.len());
    // The gaps on either side of the dragged connection leave it in place.
    if gap == from || gap == from + 1 {
        return;
    }
    let above = gap.checked_sub(1).map(|index| blocks[index].max - 2.0);
    let below = blocks.get(gap).map(|block| block.min);
    let y = match (above, below) {
        (Some(above), Some(below)) => (above + below) / 2.0,
        (None, Some(below)) => below - 3.0,
        (Some(above), None) => above + 3.0,
        (None, None) => return,
    };
    ui.painter().hline(
        ui.max_rect().x_range(),
        y,
        Stroke::new(2.0, theme::palette(ui).accent),
    );
    if ui.input(|input| input.pointer.any_released()) {
        actions.push(Action::MoveConnection {
            id,
            index: if gap > from { gap - 1 } else { gap },
        });
    }
}

/// The dragged connection's dot and name, next to the pointer.
fn ghost(ui: &egui::Ui, entry: &ConnectionEntry, pointer: Pos2) {
    let palette = theme::palette(ui);
    let color = entry
        .saved
        .color
        .map_or(palette.subtle, |color| color32(&palette, color));
    let connected = matches!(entry.state, ConnectionState::Connected(_));
    egui::Area::new(egui::Id::unique("connection_ghost"))
        .order(egui::Order::Tooltip)
        .interactable(false)
        .fixed_pos(pointer + vec2(12.0, -16.0))
        .show(ui.ctx(), |ui| {
            Frame::new()
                .fill(palette.raised)
                .stroke(Stroke::new(1.0, palette.line))
                .corner_radius(CornerRadius::same(8))
                .shadow(ui.visuals().popup_shadow)
                .inner_margin(Margin::symmetric(10, 6))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let (dot, _) = ui.allocate_exact_size(vec2(10.0, 20.0), Sense::hover());
                        if connected {
                            ui.painter().circle_filled(dot.center(), 4.5, color);
                        } else {
                            ui.painter()
                                .circle_stroke(dot.center(), 3.8, Stroke::new(2.0, color));
                        }
                        ui.label(
                            egui::RichText::new(&entry.saved.name)
                                .font(FontId::new(14.0, theme::medium()))
                                .color(palette.text),
                        );
                    });
                });
        });
}

/// What a tree row shows after its label.
enum Trailing {
    None,
    Text(String),
    Pill(&'static str),
    ReadOnly,
}

struct Row<'a> {
    depth: usize,
    /// Some(open) for rows that expand.
    chevron: Option<bool>,
    icon: Option<&'static str>,
    /// A connection's color dot, filled when connected.
    dot: Option<(Color32, bool)>,
    label: &'a str,
    strong: bool,
    selected: bool,
    /// Faded, while it is dragged to a new place.
    dimmed: bool,
    trailing: Trailing,
}

fn tree_row(ui: &mut egui::Ui, row: Row<'_>) -> Response {
    let palette = theme::palette(ui);
    let height = if row.depth == 0 { 32.0 } else { 30.0 };
    // Connections can be dragged to a new place.
    let sense = if row.depth == 0 {
        Sense::click_and_drag()
    } else {
        Sense::click()
    };
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), height), sense);
    let opacity = ui.opacity();
    if row.dimmed {
        ui.set_opacity(0.4);
    }
    let painter = ui.painter();
    if row.selected {
        painter.rect_filled(rect, CornerRadius::same(8), palette.accent_soft);
    } else if response.hovered() {
        painter.rect_filled(rect, CornerRadius::same(8), palette.hover);
    }
    // Indent guides, one per level above this row.
    for level in 0..row.depth {
        let x = rect.left() + 15.0 + level as f32 * INDENT;
        painter.vline(
            x,
            rect.expand2(vec2(0.0, 1.0)).y_range(),
            Stroke::new(1.0, palette.line),
        );
    }
    let y = rect.center().y;
    let mut x = rect.left() + 8.0 + row.depth as f32 * INDENT;
    if let Some(open) = row.chevron {
        painter.text(
            pos2(x + 7.0, y),
            Align2::CENTER_CENTER,
            if open {
                icons::CHEVRON_DOWN
            } else {
                icons::CHEVRON_RIGHT
            },
            FontId::proportional(13.0),
            palette.subtle,
        );
        x += 20.0;
    }
    if let Some((color, connected)) = row.dot {
        let center = pos2(x + 4.5, y);
        if connected {
            painter.circle_filled(center, 7.0, color.gamma_multiply(0.2));
            painter.circle_filled(center, 4.5, color);
        } else {
            painter.circle_stroke(center, 3.8, Stroke::new(2.0, color));
        }
        x += 17.0;
    }
    if let Some(icon) = row.icon {
        let color = if row.selected {
            palette.accent
        } else {
            palette.muted
        };
        painter.text(
            pos2(x, y),
            Align2::LEFT_CENTER,
            icon,
            FontId::proportional(16.0),
            color,
        );
        x += 24.0;
    }

    let text_color = if row.selected {
        palette.accent_text
    } else if row.strong || row.depth == 1 {
        palette.text
    } else {
        palette.muted
    };
    let font = if row.strong || row.selected {
        FontId::new(14.0, theme::medium())
    } else {
        FontId::proportional(14.0)
    };
    // The label keeps its width where it can; a long host gets cut instead,
    // down to 40% of the room.
    let room = rect.right() - 8.0 - x;
    let label_width = kit::text_width(ui, row.label, font.clone());
    let trailing_room = (room - label_width - 12.0).max(room * 0.4);
    let trailing_width = paint_trailing(ui, &palette, &row.trailing, rect, trailing_room);
    let max_width = (rect.right() - trailing_width - x - 8.0).max(10.0);
    let mut job = egui::text::LayoutJob::simple_singleline(row.label.to_owned(), font, text_color);
    job.wrap.max_width = max_width;
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    let galley = ui.fonts_mut(|fonts| fonts.layout_job(job));
    ui.painter()
        .galley(pos2(x, y - galley.size().y / 2.0), galley, text_color);
    ui.set_opacity(opacity);
    let label = row.label.to_owned();
    let selected = row.selected;
    response.widget_info(|| WidgetInfo::selected(Role::TreeItem, true, selected, &label));
    response
}

/// Paints the trailing text, a host cut to `room`, and returns its width.
fn paint_trailing(
    ui: &egui::Ui,
    palette: &Palette,
    trailing: &Trailing,
    rect: Rect,
    room: f32,
) -> f32 {
    let painter = ui.painter();
    let right = rect.right() - 8.0;
    let y = rect.center().y;
    match trailing {
        Trailing::None => 0.0,
        Trailing::Text(text) => {
            let mut job = egui::text::LayoutJob::simple_singleline(
                text.clone(),
                FontId::monospace(11.5),
                palette.subtle,
            );
            job.wrap.max_width = room.max(10.0);
            job.wrap.max_rows = 1;
            job.wrap.break_anywhere = true;
            let galley = ui.fonts_mut(|fonts| fonts.layout_job(job));
            let width = galley.size().x;
            painter.galley(
                pos2(right - width, y - galley.size().y / 2.0),
                galley,
                palette.subtle,
            );
            width + 8.0
        }
        Trailing::ReadOnly => {
            let galley = painter.layout_no_wrap(
                format!("{} read-only", icons::LOCK),
                FontId::proportional(11.5),
                palette.subtle,
            );
            let width = galley.size().x;
            painter.galley(
                pos2(right - width, y - galley.size().y / 2.0),
                galley,
                palette.subtle,
            );
            width + 8.0
        }
        Trailing::Pill(text) => {
            let galley = painter.layout_no_wrap(
                (*text).to_owned(),
                FontId::proportional(11.0),
                palette.subtle,
            );
            let pill = Rect::from_min_max(
                pos2(right - galley.size().x - 14.0, y - 9.0),
                pos2(right, y + 9.0),
            );
            painter.rect(
                pill,
                CornerRadius::same(255),
                palette.raised,
                Stroke::new(1.0, palette.line),
                egui::StrokeKind::Inside,
            );
            painter.galley(
                pos2(pill.left() + 7.0, y - galley.size().y / 2.0),
                galley,
                palette.subtle,
            );
            pill.width() + 8.0
        }
    }
}

fn connection(
    ui: &mut egui::Ui,
    entry: &ConnectionEntry,
    place: Place,
    selection: Option<&Selection>,
    links: bool,
    actions: &mut Vec<Action>,
) {
    let id = entry.saved.id;
    let palette = theme::palette(ui);
    let color = entry
        .saved
        .color
        .map_or(palette.subtle, |color| color32(&palette, color));
    let trailing = if entry.saved.read_only {
        Trailing::ReadOnly
    } else {
        host(&entry.saved.uri).map_or(Trailing::None, Trailing::Text)
    };
    let state_id = ui.make_persistent_id(("connection", id));
    let mut state = CollapsingState::load_with_default_open(ui.ctx(), state_id, true);
    let ConnectionState::Connected(databases) = &entry.state else {
        let connecting = matches!(entry.state, ConnectionState::Connecting(_));
        let response = tree_row(
            ui,
            Row {
                depth: 0,
                chevron: Some(false),
                icon: None,
                dot: Some((color, false)),
                label: &entry.saved.name,
                strong: true,
                selected: false,
                dimmed: place.dragged,
                trailing: if connecting || matches!(entry.state, ConnectionState::Failed(_)) {
                    Trailing::None
                } else {
                    trailing
                },
            },
        );
        let mut right = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(response.rect.shrink2(vec2(6.0, 0.0)))
                .layout(Layout::right_to_left(Align::Center)),
        );
        match &entry.state {
            ConnectionState::Connecting(_) => {
                if kit::icon_button(&mut right, icons::X, "Cancel connecting", 24.0).clicked() {
                    actions.push(Action::CancelConnect(id));
                }
                right.spinner();
            }
            ConnectionState::Failed(error) => {
                right
                    .label(
                        egui::RichText::new(format!("{} failed", icons::CIRCLE_ALERT))
                            .size(12.0)
                            .color(theme::palette(ui).danger),
                    )
                    .on_hover_text(&error.message);
            }
            _ => {}
        }
        start_drag(&response, id);
        if response.clicked() && !connecting {
            actions.push(Action::Connect(id));
        }
        response
            .on_hover_text("Click to connect")
            .context_menu(|ui| {
                if !connecting && ui.button("Connect").clicked() {
                    actions.push(Action::Connect(id));
                }
                connection_menu_common(ui, id, place, actions);
            });
        return;
    };

    let open = state.is_open();
    let response = tree_row(
        ui,
        Row {
            depth: 0,
            chevron: Some(open),
            icon: None,
            dot: Some((color, true)),
            label: &entry.saved.name,
            strong: true,
            selected: false,
            dimmed: place.dragged,
            trailing,
        },
    );
    start_drag(&response, id);
    if response.clicked() {
        state.set_open(!open);
        state.store(ui.ctx());
    }
    response.context_menu(|ui| {
        if ui.button("Disconnect").clicked() {
            actions.push(Action::Disconnect(id));
        }
        if ui.button("Refresh").clicked() {
            actions.push(Action::RefreshDatabases(id));
        }
        if !entry.saved.read_only && ui.button("Create database…").clicked() {
            actions.push(Action::CreateCollection {
                connection: id,
                database: None,
            });
        }
        connection_menu_common(ui, id, place, actions);
    });
    if open {
        for database in databases {
            database_node(ui, entry, database, selection, links, actions);
        }
    }
}

fn start_drag(row: &Response, id: uuid::Uuid) {
    if row.drag_started_by(PointerButton::Primary) {
        row.dnd_set_drag_payload(DraggedConnection(id));
    }
}

/// The first host of a connection string, without its port.
fn host(uri: &str) -> Option<String> {
    let parsed = ConnectionUri::parse(uri).ok()?;
    let host = parsed.hosts.first()?;
    let host = host
        .rsplit_once(':')
        .map_or(host.as_str(), |(name, _)| name);
    Some(host.to_owned())
}

fn connection_menu_common(
    ui: &mut egui::Ui,
    id: uuid::Uuid,
    place: Place,
    actions: &mut Vec<Action>,
) {
    let Place { index, last, .. } = place;
    ui.separator();
    if ui
        .add_enabled(index > 0, egui::Button::new("Move up"))
        .clicked()
    {
        actions.push(Action::MoveConnection {
            id,
            index: index.saturating_sub(1),
        });
    }
    if ui
        .add_enabled(!last, egui::Button::new("Move down"))
        .clicked()
    {
        actions.push(Action::MoveConnection {
            id,
            index: index + 1,
        });
    }
    ui.separator();
    if ui.button("Edit…").clicked() {
        actions.push(Action::EditConnection(id));
    }
    if ui.button("Delete…").clicked() {
        actions.push(Action::DeleteConnection(id));
    }
}

fn database_node(
    ui: &mut egui::Ui,
    entry: &ConnectionEntry,
    database: &DatabaseNode,
    selection: Option<&Selection>,
    links: bool,
    actions: &mut Vec<Action>,
) {
    let connection = entry.saved.id;
    let name = &database.info.name;
    let state_id = ui.make_persistent_id(("database", connection, name));
    let mut state = CollapsingState::load_with_default_open(ui.ctx(), state_id, false);
    let open = state.is_open();
    if open && matches!(database.collections, Collections::NotLoaded) {
        actions.push(Action::LoadCollections {
            connection,
            database: name.clone(),
        });
    }

    let response = tree_row(
        ui,
        Row {
            depth: 1,
            chevron: Some(open),
            icon: Some(icons::DATABASE),
            dot: None,
            label: name,
            strong: false,
            selected: false,
            dimmed: false,
            trailing: Trailing::None,
        },
    );
    if response.clicked() {
        state.set_open(!open);
        state.store(ui.ctx());
    }
    response.context_menu(|ui| {
        if ui.button("Refresh").clicked() {
            actions.push(Action::LoadCollections {
                connection,
                database: name.clone(),
            });
        }
        if links && ui.button("Reference mappings…").clicked() {
            actions.push(Action::EditMappings {
                connection,
                database: name.clone(),
            });
        }
        if !entry.saved.read_only {
            if ui.button("Create collection…").clicked() {
                actions.push(Action::CreateCollection {
                    connection,
                    database: Some(name.clone()),
                });
            }
            if ui.button("Drop database…").clicked() {
                actions.push(Action::Drop {
                    connection,
                    database: name.clone(),
                    collection: None,
                });
            }
        }
    });
    if !open {
        return;
    }
    let note = |ui: &mut egui::Ui, text: &str, color: Color32| {
        ui.horizontal(|ui| {
            ui.add_space(8.0 + 2.0 * INDENT + 4.0);
            ui.label(egui::RichText::new(text).size(12.5).color(color))
        })
        .inner
    };
    let palette = theme::palette(ui);
    match &database.collections {
        Collections::NotLoaded | Collections::Loading => {
            ui.horizontal(|ui| {
                ui.add_space(8.0 + 2.0 * INDENT + 4.0);
                ui.spinner();
            });
        }
        Collections::Failed(error) => {
            note(ui, "Could not load collections", palette.danger).on_hover_text(&error.message);
        }
        Collections::Loaded(collections) if collections.is_empty() => {
            note(ui, "No collections", palette.subtle);
        }
        Collections::Loaded(collections) => {
            for collection in collections {
                let target = Selection {
                    connection,
                    database: name.clone(),
                    collection: collection.name.clone(),
                };
                let selected = selection == Some(&target);
                let (icon, trailing) = match collection.kind {
                    CollectionKind::View => (icons::EYE, Trailing::Pill("view")),
                    CollectionKind::Timeseries => (icons::CLOCK, Trailing::Pill("time series")),
                    CollectionKind::Collection if collection.capped => {
                        (icons::LAYERS, Trailing::Pill("capped"))
                    }
                    CollectionKind::Collection => (icons::LAYERS, Trailing::None),
                };
                let response = tree_row(
                    ui,
                    Row {
                        depth: 2,
                        chevron: None,
                        icon: Some(icon),
                        dot: None,
                        label: &collection.name,
                        strong: false,
                        selected,
                        dimmed: false,
                        trailing,
                    },
                );
                if response.clicked() {
                    actions.push(Action::Select(target));
                }
                if !entry.saved.read_only {
                    response.context_menu(|ui| {
                        if ui.button("Drop collection…").clicked() {
                            actions.push(Action::Drop {
                                connection,
                                database: name.clone(),
                                collection: Some(collection.name.clone()),
                            });
                        }
                    });
                }
            }
        }
    }
}

pub fn color32(palette: &Palette, color: ConnectionColor) -> Color32 {
    match color {
        ConnectionColor::Red => palette.conn_red,
        ConnectionColor::Orange => palette.conn_orange,
        ConnectionColor::Yellow => palette.conn_yellow,
        ConnectionColor::Green => palette.conn_green,
        ConnectionColor::Teal => palette.conn_teal,
        ConnectionColor::Blue => palette.conn_blue,
        ConnectionColor::Purple => palette.conn_purple,
        ConnectionColor::Pink => palette.conn_pink,
    }
}
