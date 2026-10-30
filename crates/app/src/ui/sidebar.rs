//! Saved connections and, for open ones, their databases and collections.

use eframe::egui::collapsing_header::CollapsingState;
use eframe::egui::{self, Color32, Label, Sense};
use nordpil_backend::CollectionKind;
use nordpil_backend::store::ConnectionColor;

use super::Action;
use crate::connections::{Collections, ConnectionEntry, ConnectionState, DatabaseNode, Selection};

pub fn show(
    ui: &mut egui::Ui,
    entries: &[ConnectionEntry],
    selection: Option<&Selection>,
    actions: &mut Vec<Action>,
) {
    ui.horizontal(|ui| {
        ui.heading("Connections");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button("+").on_hover_text("New connection").clicked() {
                actions.push(Action::NewConnection);
            }
        });
    });
    ui.separator();

    if entries.is_empty() {
        ui.weak("No saved connections yet.");
        if ui.button("New connection").clicked() {
            actions.push(Action::NewConnection);
        }
        return;
    }
    egui::ScrollArea::vertical()
        .auto_shrink(false)
        .show(ui, |ui| {
            for entry in entries {
                connection(ui, entry, selection, actions);
            }
        });
}

fn connection(
    ui: &mut egui::Ui,
    entry: &ConnectionEntry,
    selection: Option<&Selection>,
    actions: &mut Vec<Action>,
) {
    let id = entry.saved.id;
    let ConnectionState::Connected(databases) = &entry.state else {
        ui.horizontal(|ui| {
            color_dot(ui, entry.saved.color);
            let connecting = matches!(entry.state, ConnectionState::Connecting(_));
            let response = clickable_label(ui, &entry.saved.name);
            match &entry.state {
                ConnectionState::Connecting(_) => {
                    ui.spinner();
                    if ui.small_button("Cancel").clicked() {
                        actions.push(Action::CancelConnect(id));
                    }
                }
                ConnectionState::Failed(error) => {
                    ui.colored_label(ui.visuals().error_fg_color, "failed")
                        .on_hover_text(&error.message);
                }
                _ => {}
            }
            if response.clicked() && !connecting {
                actions.push(Action::Connect(id));
            }
            response
                .on_hover_text("Click to connect")
                .context_menu(|ui| {
                    if !connecting && ui.button("Connect").clicked() {
                        actions.push(Action::Connect(id));
                    }
                    connection_menu_common(ui, id, actions);
                });
        });
        return;
    };

    let state_id = ui.make_persistent_id(("connection", id));
    let (_, header, _) = CollapsingState::load_with_default_open(ui.ctx(), state_id, true)
        .show_header(ui, |ui| {
            color_dot(ui, entry.saved.color);
            let response = clickable_label(ui, &entry.saved.name);
            if entry.saved.read_only {
                ui.weak("read-only");
            }
            response
        })
        .body(|ui| {
            for database in databases {
                database_node(ui, entry, database, selection, actions);
            }
        });
    toggle_on_click(ui, state_id, &header.inner);
    header.inner.context_menu(|ui| {
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
        connection_menu_common(ui, id, actions);
    });
}

fn connection_menu_common(ui: &mut egui::Ui, id: uuid::Uuid, actions: &mut Vec<Action>) {
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
    actions: &mut Vec<Action>,
) {
    let connection = entry.saved.id;
    let name = &database.info.name;
    let state_id = ui.make_persistent_id(("database", connection, name));
    let state = CollapsingState::load_with_default_open(ui.ctx(), state_id, false);
    if state.is_open() && matches!(database.collections, Collections::NotLoaded) {
        actions.push(Action::LoadCollections {
            connection,
            database: name.clone(),
        });
    }

    let (_, header, _) = state
        .show_header(ui, |ui| clickable_label(ui, name))
        .body(|ui| match &database.collections {
            Collections::NotLoaded | Collections::Loading => {
                ui.spinner();
            }
            Collections::Failed(error) => {
                ui.colored_label(ui.visuals().error_fg_color, "Could not load collections")
                    .on_hover_text(&error.message);
            }
            Collections::Loaded(collections) if collections.is_empty() => {
                ui.weak("No collections");
            }
            Collections::Loaded(collections) => {
                for collection in collections {
                    let target = Selection {
                        connection,
                        database: name.clone(),
                        collection: collection.name.clone(),
                    };
                    let selected = selection == Some(&target);
                    let response = ui
                        .horizontal(|ui| {
                            let response = ui.selectable_label(selected, &collection.name);
                            if let Some(tag) = collection_tag(collection.kind, collection.capped) {
                                ui.weak(tag);
                            }
                            response
                        })
                        .inner;
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
        });
    toggle_on_click(ui, state_id, &header.inner);
    header.inner.context_menu(|ui| {
        if ui.button("Refresh").clicked() {
            actions.push(Action::LoadCollections {
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
}

fn collection_tag(kind: CollectionKind, capped: bool) -> Option<&'static str> {
    match kind {
        CollectionKind::View => Some("view"),
        CollectionKind::Timeseries => Some("time series"),
        CollectionKind::Collection if capped => Some("capped"),
        CollectionKind::Collection => None,
    }
}

fn clickable_label(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.add(Label::new(text).sense(Sense::click()).truncate())
}

/// Lets a click on a tree label expand or collapse it, not only the arrow.
fn toggle_on_click(ui: &egui::Ui, state_id: egui::Id, label: &egui::Response) {
    if label.clicked()
        && let Some(mut state) = CollapsingState::load(ui.ctx(), state_id)
    {
        state.set_open(!state.is_open());
        state.store(ui.ctx());
    }
}

fn color_dot(ui: &mut egui::Ui, color: Option<ConnectionColor>) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), Sense::hover());
    let fill = color.map_or(ui.visuals().weak_text_color(), color32);
    ui.painter().circle_filled(rect.center(), 4.0, fill);
}

pub fn color32(color: ConnectionColor) -> Color32 {
    match color {
        ConnectionColor::Red => Color32::from_rgb(220, 50, 47),
        ConnectionColor::Orange => Color32::from_rgb(232, 125, 32),
        ConnectionColor::Yellow => Color32::from_rgb(220, 180, 30),
        ConnectionColor::Green => Color32::from_rgb(60, 170, 80),
        ConnectionColor::Teal => Color32::from_rgb(40, 170, 160),
        ConnectionColor::Blue => Color32::from_rgb(50, 120, 220),
        ConnectionColor::Purple => Color32::from_rgb(140, 90, 210),
        ConnectionColor::Pink => Color32::from_rgb(220, 80, 150),
    }
}
