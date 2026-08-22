use eframe::egui;

mod fs_entry;
mod tab;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions::default();
    eframe::run_native(
        "Speed FileMan",
        options,
        Box::new(|_cc| Ok(Box::new(FileManApp::default()))),
    )
}

#[derive(Default)]
struct FileManApp {}

impl eframe::App for FileManApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ui, |ui| {
            ui.label("Speed FileMan");
        });
    }
}
