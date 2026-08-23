mod app;
mod archive;
mod db;
mod fs_entry;
mod fs_ops;
mod pane;
mod progress;
mod search;
mod session;
mod tab;
mod tree;

use eframe::egui;

fn db_path() -> std::path::PathBuf {
    let appdata = std::env::var("APPDATA").expect("APPDATA env var not set");
    let dir = std::path::PathBuf::from(appdata).join("FileMan");
    std::fs::create_dir_all(&dir).expect("failed to create app data dir");
    dir.join("fileman.db")
}

fn main() -> eframe::Result<()> {
    let conn = db::open_db(&db_path()).expect("failed to open database");
    let loaded = session::load_session(&conn).ok().flatten();

    let mut viewport = egui::ViewportBuilder::default().with_inner_size([1200.0, 800.0]);
    if let Some(loaded) = &loaded {
        if let Some(window) = &loaded.window {
            viewport = viewport.with_inner_size([window.width, window.height]);
            if let (Some(x), Some(y)) = (window.pos_x, window.pos_y) {
                viewport = viewport.with_position([x, y]);
            }
        }
    }

    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    // Font loading (embedded default + any saved system-font preference) is
    // applied reactively from FileManApp::ui on the first frame — see
    // app::apply_fonts — since the font family can also change at runtime
    // via Settings.
    eframe::run_native(
        "Speed FileMan",
        options,
        Box::new(move |_cc| Ok(Box::new(app::FileManApp::new(conn, loaded)))),
    )
}
