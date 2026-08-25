#![windows_subsystem = "windows"]

mod actions;
mod app;
mod archive;
mod config;
mod db;
mod fs_entry;
mod fs_ops;
mod icon_cache;
mod migrate;
mod monitor;
mod native_drag;
mod pane;
mod progress;
mod search;
mod session;
mod tab;
mod taskbar;
mod tips;
mod tree;
mod user;
mod win_default;

use eframe::egui;

fn db_path() -> std::path::PathBuf {
    let appdata = std::env::var("APPDATA").expect("APPDATA env var not set");
    let dir = std::path::PathBuf::from(appdata).join("FileMan");
    std::fs::create_dir_all(&dir).expect("failed to create app data dir");
    dir.join("fileman.db")
}

fn main() -> eframe::Result<()> {
    // Must happen before the window is created so Windows doesn't group this
    // instance's taskbar button with other running instances (see taskbar.rs).
    let instance_slot = taskbar::claim_instance_slot_and_set_identity();

    // OLE on the main thread: required before a drag can be handed to other
    // applications (see native_drag.rs). Cheap no-op when already up.
    native_drag::init_ole();

    let conn = db::open_db(&db_path()).expect("failed to open database");
    let user_id = user::default_user_id(&conn);
    let loaded = session::load_session(&conn, user_id).ok().flatten();

    // When registered as the default folder explorer, Explorer launches us
    // with the clicked folder as the first argument.
    let startup_dir = std::env::args()
        .nth(1)
        .map(std::path::PathBuf::from)
        .filter(|p| p.is_dir());

    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([1200.0, 800.0])
        .with_maximized(true);
    if let Some(loaded) = &loaded {
        if let Some(window) = &loaded.window {
            viewport = viewport.with_inner_size([window.width, window.height]);
            let size = (window.width, window.height);
            let monitors = monitor::list_monitors();
            // Match the saved monitor by its stable device name; if it's no
            // longer connected (unplugged, layout changed), fall back to the
            // primary monitor rather than restoring off-screen.
            let target = window
                .monitor_name
                .as_deref()
                .and_then(|saved| monitors.iter().find(|m| m.name == saved))
                .or_else(|| monitors.iter().find(|m| m.is_primary))
                .or_else(|| monitors.first());
            match (target, window.pos_x, window.pos_y) {
                (Some(mon), Some(x), Some(y)) => {
                    let pos = monitor::clamp_into_work_area((x, y), size, mon.work);
                    viewport = viewport.with_position([pos.0, pos.1]);
                }
                (None, Some(x), Some(y)) => {
                    viewport = viewport.with_position([x, y]);
                }
                _ => {}
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
        Box::new(move |_cc| {
            Ok(Box::new(app::FileManApp::new(
                conn,
                user_id,
                loaded,
                instance_slot,
                startup_dir,
            )))
        }),
    )
}
