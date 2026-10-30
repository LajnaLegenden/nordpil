use eframe::egui;

pub struct NordpilApp {}

impl NordpilApp {
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        Self {}
    }
}

impl eframe::App for NordpilApp {
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
