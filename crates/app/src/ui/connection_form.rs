//! The new/edit connection dialog. The connection string and the fields
//! below it edit the same connection and stay in sync.

use eframe::egui::{
    self, CornerRadius, Frame, Margin, RichText, Role, Sense, Stroke, TextEdit, TextStyle,
    WidgetInfo,
};
use nordpil_backend::store::{ConnectionColor, SavedConnection};
use nordpil_backend::uri::ConnectionUri;
use nordpil_backend::{Password, PasswordChange};
use uuid::Uuid;

use super::sidebar::color32;
use crate::icons;
use crate::theme;
use crate::widgets::kit::{self, Tone};

pub enum FormResult {
    Open,
    Cancelled,
    Save {
        connection: SavedConnection,
        password: PasswordChange,
        /// The typed password, if any, so connecting right after saving
        /// works even without a keyring.
        typed_password: Option<Password>,
        connect: bool,
    },
}

pub struct ConnectionForm {
    id: Uuid,
    is_new: bool,
    name: String,
    uri_text: String,
    uri_error: Option<String>,
    /// The last valid connection string, edited by the fields.
    uri: ConnectionUri,
    fields: Fields,
    password: String,
    password_changed: bool,
    color: Option<ConnectionColor>,
    read_only: bool,
    pub saving: bool,
    pub error: Option<String>,
}

struct Fields {
    srv: bool,
    hosts: String,
    username: String,
    auth_source: String,
    tls: bool,
}

impl Fields {
    fn from_uri(uri: &ConnectionUri) -> Self {
        Self {
            srv: uri.srv,
            hosts: uri.hosts.join(", "),
            username: uri.username.clone().unwrap_or_default(),
            auth_source: uri.option("authSource").unwrap_or_default(),
            tls: uri.tls(),
        }
    }
}

const DEFAULT_URI: &str = "mongodb://localhost:27017";

impl ConnectionForm {
    pub fn new() -> Self {
        Self::from_parts(Uuid::new_v4(), true, "", DEFAULT_URI, None, false)
    }

    pub fn edit(saved: &SavedConnection) -> Self {
        Self::from_parts(
            saved.id,
            false,
            &saved.name,
            &saved.uri,
            saved.color,
            saved.read_only,
        )
    }

    fn from_parts(
        id: Uuid,
        is_new: bool,
        name: &str,
        uri_text: &str,
        color: Option<ConnectionColor>,
        read_only: bool,
    ) -> Self {
        let uri = ConnectionUri::parse(uri_text)
            .or_else(|_| ConnectionUri::parse(DEFAULT_URI))
            .expect("the default connection string is valid");
        let mut form = Self {
            id,
            is_new,
            name: name.to_owned(),
            uri_text: uri_text.to_owned(),
            uri_error: None,
            fields: Fields::from_uri(&uri),
            uri,
            password: String::new(),
            password_changed: false,
            color,
            read_only,
            saving: false,
            error: None,
        };
        form.uri_text_changed();
        form
    }

    /// Re-parses the connection string and refreshes the fields from it.
    fn uri_text_changed(&mut self) {
        match ConnectionUri::parse(&self.uri_text) {
            Ok(mut uri) => {
                if let Some(password) = uri.password.take() {
                    // Keep passwords out of the visible connection string.
                    self.password = password;
                    self.password_changed = true;
                    self.uri_text = uri.to_string();
                }
                self.fields = Fields::from_uri(&uri);
                self.uri = uri;
                self.uri_error = None;
            }
            Err(error) => self.uri_error = Some(error.to_string()),
        }
    }

    /// Writes the fields into the connection string. The fields themselves
    /// stay as typed, so editing never fights the cursor.
    fn fields_changed(&mut self) {
        let uri = &mut self.uri;
        uri.srv = self.fields.srv;
        uri.hosts = self
            .fields
            .hosts
            .split(',')
            .map(str::trim)
            .filter(|host| !host.is_empty())
            .map(str::to_owned)
            .collect();
        if uri.srv {
            // SRV takes one host name without a port.
            uri.hosts.truncate(1);
            if let Some(host) = uri.hosts.first_mut()
                && let Some((name, _port)) = host.split_once(':')
            {
                *host = name.to_owned();
            }
        }
        uri.username = non_empty(&self.fields.username);
        uri.set_option("authSource", non_empty(&self.fields.auth_source).as_deref());
        uri.set_tls(self.fields.tls);

        self.uri_text = uri.to_string();
        self.uri_error = ConnectionUri::parse(&self.uri_text)
            .err()
            .map(|error| error.to_string());
    }

    /// Switching scheme resets TLS to that scheme's default, since SRV
    /// clusters (e.g. Atlas) expect TLS.
    fn set_srv(&mut self, srv: bool) {
        self.fields.srv = srv;
        self.fields.tls = srv;
        self.fields_changed();
        self.fields.hosts = self.uri.hosts.join(", ");
    }

    pub fn show(&mut self, ctx: &egui::Context, keyring_available: bool) -> FormResult {
        let mut result = FormResult::Open;
        let modal = kit::modal(ctx, egui::Id::unique("connection_form")).show(ctx, |ui| {
            kit::dialog_width(ui, 600.0);
            kit::dialog_title(
                ui,
                if self.is_new {
                    "New connection"
                } else {
                    "Edit connection"
                },
                None,
            );
            ui.add_enabled_ui(!self.saving, |ui| {
                self.main_fields(ui);
                ui.add_space(12.0);
                self.details_box(ui, keyring_available);
                ui.add_space(14.0);
                self.appearance(ui);
            });

            if let Some(error) = &self.error {
                super::dialogs::error_label(ui, error);
            }
            kit::dialog_footer(ui, |ui| {
                let valid = self.uri_error.is_none() && !self.saving;
                let height = kit::DIALOG_HEIGHT;
                let connect = ui
                    .add_enabled_ui(valid, |ui| {
                        kit::button(
                            ui,
                            Tone::Primary,
                            Some(icons::PLUG),
                            "Save and connect",
                            height,
                        )
                    })
                    .inner;
                if connect.clicked() {
                    result = self.save(true);
                }
                let save = ui
                    .add_enabled_ui(valid, |ui| {
                        kit::button(ui, Tone::Secondary, None, "Save", height)
                    })
                    .inner;
                if save.clicked() {
                    result = self.save(false);
                }
                let cancel = ui
                    .add_enabled_ui(!self.saving, |ui| {
                        kit::button(ui, Tone::Ghost, None, "Cancel", height)
                    })
                    .inner;
                if cancel.clicked() {
                    result = FormResult::Cancelled;
                }
                if self.saving {
                    ui.spinner();
                }
            });
        });
        if modal.should_close() && !self.saving {
            result = FormResult::Cancelled;
        }
        result
    }

    /// The connection details, collapsible, in a bordered box.
    fn details_box(&mut self, ui: &mut egui::Ui, keyring_available: bool) {
        let palette = theme::palette(ui);
        let id = ui.make_persistent_id("connection_details");
        let mut state =
            egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, true);
        Frame::new()
            .fill(palette.main)
            .stroke(Stroke::new(1.0, palette.line))
            .corner_radius(CornerRadius::same(12))
            .inner_margin(Margin::symmetric(14, 10))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                let open = state.is_open();
                let header = ui.add(
                    egui::Button::new((
                        RichText::new(if open {
                            icons::CHEVRON_DOWN
                        } else {
                            icons::CHEVRON_RIGHT
                        })
                        .size(14.0)
                        .color(palette.subtle),
                        RichText::new("Connection details").family(theme::medium()),
                        egui::Atom::grow(),
                        RichText::new("Kept in sync with the connection string")
                            .size(12.0)
                            .color(palette.subtle),
                    ))
                    .frame(false)
                    .min_size(egui::vec2(ui.available_width(), 26.0)),
                );
                header
                    .widget_info(|| WidgetInfo::labeled(Role::Button, true, "Connection details"));
                if header.clicked() {
                    state.set_open(!open);
                    state.store(ui.ctx());
                }
                if open {
                    ui.add_space(6.0);
                    self.detail_fields(ui, keyring_available);
                }
            });
    }

    fn main_fields(&mut self, ui: &mut egui::Ui) {
        egui::Grid::new("connection_form_main")
            .num_columns(2)
            .spacing([16.0, 10.0])
            .min_col_width(140.0)
            .show(ui, |ui| {
                let label = ui.label("Name");
                ui.add(
                    TextEdit::singleline(&mut self.name)
                        .hint_text(self.uri.hosts.join(","))
                        .desired_width(f32::INFINITY),
                )
                .labelled_by(label.id);
                ui.end_row();

                let label = ui.label("Connection string");
                let response = ui
                    .add(
                        TextEdit::singleline(&mut self.uri_text)
                            .font(TextStyle::Monospace)
                            .desired_width(f32::INFINITY),
                    )
                    .labelled_by(label.id);
                if response.changed() {
                    self.uri_text_changed();
                }
                ui.end_row();
            });
        if let Some(error) = &self.uri_error {
            ui.colored_label(ui.visuals().error_fg_color, error);
        }
    }

    fn detail_fields(&mut self, ui: &mut egui::Ui, keyring_available: bool) {
        let mut changed = false;
        egui::Grid::new("connection_form_details")
            .num_columns(2)
            .spacing([16.0, 10.0])
            .min_col_width(126.0)
            .show(ui, |ui| {
                ui.label("");
                let mut srv = self.fields.srv;
                if ui
                    .checkbox(&mut srv, "DNS seed list (mongodb+srv)")
                    .changed()
                {
                    self.set_srv(srv);
                }
                ui.end_row();

                changed |= text_row(
                    ui,
                    "Hosts",
                    &mut self.fields.hosts,
                    "host:27017, host2:27017",
                );
                changed |= text_row(ui, "Username", &mut self.fields.username, "");

                let label = ui.label("Password");
                let hint = if self.is_new || !keyring_available {
                    ""
                } else {
                    "unchanged"
                };
                let response = ui
                    .add(
                        TextEdit::singleline(&mut self.password)
                            .password(true)
                            .hint_text(hint)
                            .desired_width(f32::INFINITY),
                    )
                    .labelled_by(label.id);
                self.password_changed |= response.changed();
                ui.end_row();

                changed |= text_row(ui, "Auth database", &mut self.fields.auth_source, "admin");

                ui.label("");
                changed |= ui.checkbox(&mut self.fields.tls, "TLS").changed();
                ui.end_row();
            });
        if changed {
            self.fields_changed();
        }
        if !keyring_available {
            ui.weak(
                "No system keyring found, so the password is not saved. \
                 You'll be asked for it when connecting.",
            );
        }
    }

    fn appearance(&mut self, ui: &mut egui::Ui) {
        let palette = theme::palette(ui);
        ui.horizontal(|ui| {
            ui.allocate_ui_with_layout(
                egui::vec2(156.0, 28.0),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    ui.set_width(156.0);
                    ui.label("Color");
                },
            );
            let none = ui.add(
                egui::Button::selectable(self.color.is_none(), RichText::new("None").size(12.5))
                    .corner_radius(CornerRadius::same(255))
                    .min_size(egui::vec2(0.0, 28.0)),
            );
            if none.clicked() {
                self.color = None;
            }
            for color in ConnectionColor::ALL {
                let selected = self.color == Some(color);
                let (rect, response) =
                    ui.allocate_exact_size(egui::vec2(28.0, 28.0), Sense::click());
                let center = rect.center();
                ui.painter().circle_filled(center, 10.0, color32(color));
                if selected {
                    ui.painter()
                        .circle_stroke(center, 13.0, Stroke::new(2.0, color32(color)));
                } else if response.hovered() {
                    ui.painter()
                        .circle_stroke(center, 13.0, Stroke::new(1.0, palette.line_strong));
                }
                response.widget_info(|| {
                    WidgetInfo::selected(Role::RadioButton, true, selected, format!("{color:?}"))
                });
                if response.clicked() {
                    self.color = Some(color);
                }
            }
        });
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.add_space(164.0);
            ui.checkbox(
                &mut self.read_only,
                "Read-only: block inserts, updates, deletes and drops",
            );
        });
    }

    fn save(&mut self, connect: bool) -> FormResult {
        let typed_password = (!self.password.is_empty()).then(|| Password::new(&self.password));
        let password = match (&typed_password, self.password_changed) {
            (_, false) => PasswordChange::Keep,
            (Some(password), true) => PasswordChange::Set(password.clone()),
            (None, true) => PasswordChange::Clear,
        };
        self.saving = true;
        self.error = None;
        FormResult::Save {
            connection: SavedConnection {
                id: self.id,
                name: self.name.trim().to_owned(),
                uri: self.uri_text.trim().to_owned(),
                color: self.color,
                read_only: self.read_only,
            },
            password,
            typed_password,
            connect,
        }
    }
}

fn text_row(ui: &mut egui::Ui, label: &str, text: &mut String, hint: &str) -> bool {
    let label = ui.label(label);
    let changed = ui
        .add(
            TextEdit::singleline(text)
                .hint_text(hint.to_owned())
                .desired_width(f32::INFINITY),
        )
        .labelled_by(label.id)
        .changed();
    ui.end_row();
    changed
}

fn non_empty(text: &str) -> Option<String> {
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn form_with_uri(uri: &str) -> ConnectionForm {
        let mut form = ConnectionForm::new();
        form.uri_text = uri.to_owned();
        form.uri_text_changed();
        form
    }

    #[test]
    fn password_in_connection_string_moves_to_password_field() {
        let form = form_with_uri("mongodb://bob:s3cret@db.local:27017/");

        assert_eq!(form.uri_text, "mongodb://bob@db.local:27017");
        assert_eq!(form.password, "s3cret");
        assert_eq!(form.fields.username, "bob");
        assert_eq!(form.fields.hosts, "db.local:27017");
    }

    #[test]
    fn fields_rewrite_connection_string() {
        let mut form = form_with_uri("mongodb://localhost:27017/?appName=x");
        form.fields.hosts = "a:1, b:2".to_owned();
        form.fields.username = "bob".to_owned();
        form.fields.auth_source = "admin".to_owned();
        form.fields.tls = true;

        form.fields_changed();

        assert_eq!(
            form.uri_text,
            "mongodb://bob@a:1,b:2/?appName=x&authSource=admin&tls=true"
        );
        assert_eq!(form.uri_error, None);
    }

    #[test]
    fn srv_keeps_one_host_without_port() {
        let mut form = form_with_uri("mongodb://a.example.net:27017,b.example.net:27017");

        form.set_srv(true);

        assert_eq!(form.uri_text, "mongodb+srv://a.example.net");
        assert_eq!(form.fields.hosts, "a.example.net");
        assert!(form.fields.tls);
    }

    #[test]
    fn invalid_connection_string_keeps_last_fields() {
        let mut form = form_with_uri("mongodb://bob@localhost");
        form.uri_text = "mongodb://".to_owned();

        form.uri_text_changed();

        assert!(form.uri_error.is_some());
        assert_eq!(form.fields.username, "bob");
    }
}
