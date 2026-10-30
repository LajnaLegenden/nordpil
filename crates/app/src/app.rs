use eframe::egui;
use nordpil_backend::{Backend, Bridge, Outcome};

pub struct NordpilApp {
    backend: Bridge<Backend>,
}

impl NordpilApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> std::io::Result<Self> {
        let ctx = cc.egui_ctx.clone();
        let backend = Bridge::new(Backend::default(), move || ctx.request_repaint())?;
        Ok(Self { backend })
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
            ui.label("No connection selected.");
        });
    }
}
