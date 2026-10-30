//! Export and import dialogs, with a native file picker and live progress.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use crossbeam_channel::Receiver;
use eframe::egui::{self, Button, TextEdit};
use nordpil_backend::transfer::{ExportJob, Format, ImportJob, ImportSummary, JsonMode, Progress};
use nordpil_backend::{BackendError, Command, FindQuery, Namespace, RequestId};

pub enum Kind {
    Export {
        /// The tab's current query; `None` when no query has run yet.
        query: Option<Box<FindQuery>>,
        whole_collection: bool,
        json_mode: JsonMode,
        /// CSV columns and whether each is included.
        fields: Vec<(String, bool)>,
    },
    Import {
        csv_as_text: bool,
    },
}

pub struct TransferDialog {
    pub namespace: Namespace,
    pub kind: Kind,
    path: String,
    format: Format,
    picker: Option<Receiver<Option<PathBuf>>>,
    pub running: Option<(RequestId, Arc<Progress>)>,
    /// What happened, once finished.
    pub outcome: Option<Result<String, String>>,
}

pub enum Outcome {
    Open,
    Close,
    /// Send the command, then call [`TransferDialog::started`] with its id.
    Send(Box<Command>, Arc<Progress>),
    Cancel(RequestId),
}

impl TransferDialog {
    pub fn export(namespace: Namespace, query: Option<FindQuery>, field_paths: &[String]) -> Self {
        // Leaves only: `address.city`, not `address` as well.
        let fields = field_paths
            .iter()
            .filter(|path| {
                !field_paths
                    .iter()
                    .any(|other| other.len() > path.len() && other.starts_with(&format!("{path}.")))
            })
            .map(|path| (path.clone(), true))
            .collect();
        Self {
            path: default_path(&namespace, "json"),
            namespace,
            kind: Kind::Export {
                whole_collection: query.is_none(),
                query: query.map(Box::new),
                json_mode: JsonMode::Relaxed,
                fields,
            },
            format: Format::JsonArray,
            picker: None,
            running: None,
            outcome: None,
        }
    }

    pub fn import(namespace: Namespace) -> Self {
        Self {
            namespace,
            kind: Kind::Import { csv_as_text: false },
            path: String::new(),
            format: Format::JsonArray,
            picker: None,
            running: None,
            outcome: None,
        }
    }

    pub fn started(&mut self, request: RequestId, progress: Arc<Progress>) {
        self.running = Some((request, progress));
    }

    pub fn finished(&mut self, result: Result<String, BackendError>) {
        self.running = None;
        self.outcome = Some(result.map_err(|error| error.message));
    }

    pub fn show(&mut self, ctx: &egui::Context) -> Outcome {
        if let Some(picker) = &self.picker
            && let Ok(choice) = picker.try_recv()
        {
            self.picker = None;
            if let Some(path) = choice {
                self.set_path(path);
            }
        }
        if self.running.is_some() || self.picker.is_some() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }

        let mut outcome = Outcome::Open;
        let is_export = matches!(self.kind, Kind::Export { .. });
        let modal =
            egui::Modal::new(egui::Id::unique(("transfer_dialog", is_export))).show(ctx, |ui| {
                ui.set_width(560.0);
                ui.heading(if is_export {
                    "Export documents"
                } else {
                    "Import documents"
                });
                ui.weak(format!(
                    "{}.{}",
                    self.namespace.database, self.namespace.collection
                ));
                ui.add_space(6.0);

                let idle = self.running.is_none() && self.outcome.is_none();
                ui.add_enabled_ui(idle, |ui| self.settings(ui, ctx));

                if let Some((_, progress)) = &self.running {
                    progress_bar(ui, progress, is_export);
                }
                match &self.outcome {
                    Some(Ok(message)) => {
                        ui.label(message);
                    }
                    Some(Err(message)) => {
                        ui.colored_label(ui.visuals().error_fg_color, message);
                    }
                    None => {}
                }

                ui.separator();
                ui.horizontal(|ui| match (&self.running, &self.outcome) {
                    (Some((request, _)), _) => {
                        if ui.button("Cancel").clicked() {
                            outcome = Outcome::Cancel(*request);
                        }
                    }
                    (None, Some(_)) => {
                        if ui.button("Close").clicked() {
                            outcome = Outcome::Close;
                        }
                    }
                    (None, None) => {
                        let label = if is_export { "Export" } else { "Import" };
                        let ready = !self.path.trim().is_empty();
                        if ui.add_enabled(ready, Button::new(label)).clicked() {
                            let (command, progress) = self.command();
                            outcome = Outcome::Send(Box::new(command), progress);
                        }
                        if ui.button("Cancel").clicked() {
                            outcome = Outcome::Close;
                        }
                    }
                });
            });
        if modal.should_close() && self.running.is_none() {
            outcome = Outcome::Close;
        }
        outcome
    }

    fn settings(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let is_export = matches!(self.kind, Kind::Export { .. });
        ui.horizontal(|ui| {
            let label = ui.label("File");
            let response = ui
                .add(TextEdit::singleline(&mut self.path).desired_width(380.0))
                .labelled_by(label.id);
            if response.changed()
                && let Some(format) = Format::from_path(self.path.as_ref())
            {
                self.format = format;
            }
            if ui
                .add_enabled(self.picker.is_none(), Button::new("Choose…"))
                .clicked()
            {
                self.picker = Some(pick_file(ctx, is_export, &self.path));
            }
        });
        ui.horizontal(|ui| {
            ui.label("Format");
            let formats: &[(Format, &str)] = if is_export {
                &[
                    (Format::JsonArray, "JSON array"),
                    (Format::Ndjson, "JSON lines"),
                    (Format::Csv, "CSV"),
                ]
            } else {
                &[
                    (Format::JsonArray, "JSON (array or lines)"),
                    (Format::Csv, "CSV"),
                ]
            };
            for &(format, name) in formats {
                let selected = self.format == format
                    || (!is_export && format == Format::JsonArray && self.format == Format::Ndjson);
                if ui.selectable_label(selected, name).clicked() {
                    self.set_format(format);
                }
            }
        });

        match &mut self.kind {
            Kind::Export {
                query,
                whole_collection,
                json_mode,
                fields,
            } => {
                ui.horizontal(|ui| {
                    ui.label("Documents");
                    ui.add_enabled_ui(query.is_some(), |ui| {
                        ui.radio_value(whole_collection, false, "Current query");
                    });
                    ui.radio_value(whole_collection, true, "Whole collection");
                });
                if self.format == Format::Csv {
                    ui.label("Columns");
                    egui::ScrollArea::vertical()
                        .max_height(160.0)
                        .show(ui, |ui| {
                            for (path, include) in fields.iter_mut() {
                                ui.checkbox(include, path.as_str());
                            }
                        });
                    if fields.is_empty() {
                        ui.weak("No fields were sampled from this collection.");
                    }
                } else {
                    ui.horizontal(|ui| {
                        ui.label("Extended JSON");
                        ui.radio_value(json_mode, JsonMode::Relaxed, "Relaxed (readable)");
                        ui.radio_value(json_mode, JsonMode::Canonical, "Canonical (exact types)");
                    });
                }
            }
            Kind::Import { csv_as_text } => {
                if self.format == Format::Csv {
                    ui.checkbox(csv_as_text, "Keep all values as text");
                    ui.weak(
                        "Otherwise numbers, true/false and ObjectId _ids are detected. \
                         Dotted headers like address.city make sub-documents.",
                    );
                } else {
                    ui.weak("Accepts a JSON array or one document per line, with Extended JSON.");
                }
            }
        }
    }

    fn set_path(&mut self, path: PathBuf) {
        if let Some(format) = Format::from_path(&path) {
            self.format = format;
        }
        self.path = path.display().to_string();
    }

    /// Switching format also switches the export file's extension.
    fn set_format(&mut self, format: Format) {
        self.format = format;
        if matches!(self.kind, Kind::Export { .. }) {
            let extension = match format {
                Format::JsonArray => "json",
                Format::Ndjson => "ndjson",
                Format::Csv => "csv",
            };
            let path = PathBuf::from(self.path.trim());
            if !self.path.trim().is_empty() {
                self.path = path.with_extension(extension).display().to_string();
            }
        }
    }

    fn command(&self) -> (Command, Arc<Progress>) {
        let progress = Arc::new(Progress::default());
        let path = PathBuf::from(self.path.trim());
        let command = match &self.kind {
            Kind::Export {
                query,
                whole_collection,
                json_mode,
                fields,
            } => Command::Export(Box::new(ExportJob {
                namespace: self.namespace.clone(),
                query: if *whole_collection {
                    FindQuery::default()
                } else {
                    query.as_deref().cloned().unwrap_or_default()
                },
                path,
                format: self.format,
                json_mode: *json_mode,
                fields: fields
                    .iter()
                    .filter(|(_, include)| *include)
                    .map(|(path, _)| path.clone())
                    .collect(),
                progress: Arc::clone(&progress),
            })),
            Kind::Import { csv_as_text } => Command::Import(Box::new(ImportJob {
                namespace: self.namespace.clone(),
                path,
                format: self.format,
                csv_as_text: *csv_as_text,
                progress: Arc::clone(&progress),
            })),
        };
        (command, progress)
    }

    pub fn path(&self) -> &str {
        self.path.trim()
    }
}

pub fn import_message(summary: &ImportSummary) -> String {
    let mut message = format!("Imported {} documents.", summary.inserted);
    if summary.failed > 0 {
        message.push_str(&format!(" {} were rejected", summary.failed));
        if let Some(error) = &summary.first_error {
            message.push_str(&format!(", e.g. {error}"));
        }
        message.push('.');
    }
    message
}

fn progress_bar(ui: &mut egui::Ui, progress: &Progress, is_export: bool) {
    let done = progress.done.load(Ordering::Relaxed);
    let total = progress.total.load(Ordering::Relaxed);
    let text = if is_export {
        match total {
            0 => format!("{done} documents"),
            total => format!("{done} of {total} documents"),
        }
    } else {
        format!("{:.1} of {:.1} MB", mb(done), mb(total))
    };
    let mut bar = egui::ProgressBar::new(if total > 0 {
        done as f32 / total as f32
    } else {
        0.0
    })
    .text(text);
    if total == 0 {
        bar = bar.animate(true);
    }
    ui.add(bar);
}

fn mb(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

/// `~/<collection>.json`, or the current folder without a home.
fn default_path(namespace: &Namespace, extension: &str) -> String {
    let dir = directories::UserDirs::new()
        .map(|dirs| dirs.home_dir().to_owned())
        .unwrap_or_default();
    dir.join(format!("{}.{extension}", namespace.collection))
        .display()
        .to_string()
}

/// Opens the native file dialog on its own thread, so the UI keeps drawing.
fn pick_file(ctx: &egui::Context, save: bool, current: &str) -> Receiver<Option<PathBuf>> {
    let (sender, receiver) = crossbeam_channel::bounded(1);
    let ctx = ctx.clone();
    let current = PathBuf::from(current.trim());
    std::thread::spawn(move || {
        let mut dialog = rfd::FileDialog::new()
            .add_filter("JSON or CSV", &["json", "ndjson", "jsonl", "csv"])
            .add_filter("All files", &["*"]);
        if let Some(dir) = current.parent().filter(|dir| dir.is_dir()) {
            dialog = dialog.set_directory(dir);
        }
        let choice = if save {
            if let Some(name) = current.file_name() {
                dialog = dialog.set_file_name(name.to_string_lossy());
            }
            dialog.save_file()
        } else {
            dialog.pick_file()
        };
        let _ = sender.send(choice);
        ctx.request_repaint();
    });
    receiver
}
