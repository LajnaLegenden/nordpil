use eframe::egui;
use nordpil_backend::{Backend, Bridge, Outcome};

use crate::spike_language::SpikeLanguage;
use crate::widgets::code_editor::CodeEditor;

pub struct NordpilApp {
    backend: Bridge<Backend>,
    filter: String,
    filter_editor: CodeEditor,
    submitted_filter: Option<String>,
}

impl NordpilApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> std::io::Result<Self> {
        let ctx = cc.egui_ctx.clone();
        let backend = Bridge::new(Backend::default(), move || ctx.request_repaint())?;
        Ok(Self {
            backend,
            filter: String::new(),
            filter_editor: CodeEditor::new(egui::Id::unique("filter"), false)
                .hint("{ field: 'value' }"),
            submitted_filter: None,
        })
    }
}

impl eframe::App for NordpilApp {
    fn logic(&mut self, _ctx: &egui::Context, _frame: &mut eframe::Frame) {
        for event in self.backend.drain() {
            match event.outcome {
                Outcome::Done(response) => match response {},
                Outcome::Cancelled => {}
            }
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Panel::left("sidebar")
            .resizable(true)
            .default_size(240.0)
            .show(ui, |ui| {
                ui.heading("Connections");
            });

        egui::CentralPanel::default().show(ui, |ui| {
            ui.horizontal(|ui| {
                let label = ui.label("Filter");
                let output = self
                    .filter_editor
                    .show(ui, &mut self.filter, &SpikeLanguage);
                output.response.labelled_by(label.id);
                if output.submitted {
                    self.submitted_filter = Some(self.filter.clone());
                }
            });
            if let Some(filter) = &self.submitted_filter {
                ui.label(format!("Submitted: {filter}"));
            }
        });
    }
}
