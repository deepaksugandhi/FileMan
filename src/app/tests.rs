use super::*;

#[test]
fn large_icons_view_is_virtualized_and_background_tabs_release_data() {
    let conn = Connection::open_in_memory().unwrap();
    crate::db::init_db(&conn).unwrap();
    let user = crate::user::default_user_id(&conn);
    let dir = tempfile::tempdir().unwrap();
    let mut app = FileManApp::new(conn, user, None, 0, Some(dir.path().into()));
    let entries: Vec<_> = (0..10_000).map(|i| crate::fs_entry::FsEntry {
        name: format!("folder-{i:05}"), path: dir.path().join(format!("folder-{i:05}")),
        is_dir: true, size: 0, modified: None, archive: false, readonly: false,
        hidden: false, system: false,
    }).collect();
    let tab = app.panes[0].active_tab_mut();
    tab.listing = std::sync::Arc::new(entries);
    tab.listing_dirty = false;
    tab.view_mode = ViewMode::Icons;
    let inactive = tab.clone();
    app.panes[0].tabs.push(inactive);
    let ctx = egui::Context::default();
    app.poll_listing(0, &ctx);
    assert!(app.panes[0].tabs[1].listing.is_empty());
    assert!(app.panes[0].tabs[1].listing_dirty);
    assert!(app.listing_jobs[0].is_none(), "inactive tabs must not be scanned");
    for _ in 0..2 {
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(640.0, 480.0))),
            ..Default::default()
        };
        let mut output = ctx.run_ui(raw, |ui| app.show_pane_content(ui, &ctx, 0));
        output.textures_delta.clear();
    }
    assert!(!app.dnd_folder_rects.is_empty());
    assert!(app.dnd_folder_rects.len() < 100, "only visible icons should create widgets");
    assert_eq!(app.panes[0].active_tab().listing.len(), 10_000);
    app.panes[0].set_active_tab(1);
    app.poll_listing(0, &ctx);
    assert!(app.panes[0].tabs[0].listing.is_empty());
    assert!(app.listing_jobs[0].is_some(), "activation starts a fresh listing");
}

#[test]
fn ensure_two_panes_pads_a_single_pane_up_to_two() {
    let panes = vec![Pane::new(PathBuf::from("D:\\one"))];
    let (panes, active_pane) = ensure_two_panes(panes, 0, ("name", true));
    assert_eq!(panes.len(), 2);
    assert_eq!(panes[0].tabs[0].path, PathBuf::from("D:\\one"));
    assert_eq!(panes[1].tabs[0].path, PathBuf::from("C:\\"));
    assert_eq!(active_pane, 0);
}

#[test]
fn ensure_two_panes_creates_two_fresh_panes_from_empty() {
    let (panes, active_pane) = ensure_two_panes(Vec::new(), 0, ("name", true));
    assert_eq!(panes.len(), 2);
    assert_eq!(panes[0].tabs[0].path, PathBuf::from("C:\\"));
    assert_eq!(panes[1].tabs[0].path, PathBuf::from("C:\\"));
    assert_eq!(active_pane, 0);
}

#[test]
fn ensure_two_panes_leaves_a_valid_two_pane_vector_unchanged() {
    let panes = vec![
        Pane::new(PathBuf::from("D:\\left")),
        Pane::new(PathBuf::from("E:\\right")),
    ];
    let (panes, active_pane) = ensure_two_panes(panes, 1, ("name", true));
    assert_eq!(panes.len(), 2);
    assert_eq!(panes[0].tabs[0].path, PathBuf::from("D:\\left"));
    assert_eq!(panes[1].tabs[0].path, PathBuf::from("E:\\right"));
    assert_eq!(active_pane, 1);
}

#[test]
fn ensure_two_panes_truncates_more_than_two_panes() {
    let panes = vec![
        Pane::new(PathBuf::from("D:\\one")),
        Pane::new(PathBuf::from("E:\\two")),
        Pane::new(PathBuf::from("F:\\three")),
    ];
    let (panes, _) = ensure_two_panes(panes, 0, ("name", true));
    assert_eq!(panes.len(), 2);
    assert_eq!(panes[0].tabs[0].path, PathBuf::from("D:\\one"));
    assert_eq!(panes[1].tabs[0].path, PathBuf::from("E:\\two"));
}

#[test]
fn ensure_two_panes_clamps_out_of_range_active_pane() {
    let panes = vec![Pane::new(PathBuf::from("C:\\"))];
    let (panes, active_pane) = ensure_two_panes(panes, 99, ("name", true));
    assert_eq!(panes.len(), 2);
    assert_eq!(active_pane, 1);
}

#[test]
fn ensure_two_panes_seeds_only_fresh_panes_with_default_sort() {
    // A restored session pane keeps its own saved sorting; only the
    // freshly padded pane picks up the universal default.
    let mut restored = Pane::new(PathBuf::from("D:\\kept"));
    restored.tabs[0].sort_col = "modified".to_string();
    restored.tabs[0].sort_asc = false;
    let (panes, _) = ensure_two_panes(vec![restored], 0, ("size", false));
    assert_eq!(panes[0].tabs[0].sort_col, "modified");
    assert!(!panes[0].tabs[0].sort_asc);
    assert_eq!(panes[1].tabs[0].sort_col, "size");
    assert!(!panes[1].tabs[0].sort_asc);

    let (fresh, _) = ensure_two_panes(Vec::new(), 0, ("archive", true));
    assert_eq!(fresh[0].tabs[0].sort_col, "archive");
    assert!(fresh[0].tabs[0].sort_asc);
    assert_eq!(fresh[1].tabs[0].sort_col, "archive");
    assert!(fresh[1].tabs[0].sort_asc);
}

#[test]
fn next_sort_flips_direction_on_same_column() {
    assert_eq!(next_sort("size", true, "size"), ("size".to_string(), false));
    assert_eq!(next_sort("size", false, "size"), ("size".to_string(), true));
}

#[test]
fn next_sort_switches_column_ascending() {
    assert_eq!(
        next_sort("name", false, "modified"),
        ("modified".to_string(), true)
    );
}

#[test]
fn parse_sort_col_accepts_known_columns_and_rejects_junk() {
    for col in ["name", "modified", "size", "archive"] {
        assert_eq!(parse_sort_col(col), Some(col));
    }
    assert_eq!(parse_sort_col("bogus"), None);
    assert_eq!(parse_sort_col(""), None);
}

#[test]
fn poll_subdirs_resolves_background_listing_then_caches() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp.path().join("sub_b")).unwrap();
    std::fs::create_dir(temp.path().join("sub_a")).unwrap();
    std::fs::write(temp.path().join("file.txt"), b"x").unwrap();

    let mut cache = crate::bounded_cache::BoundedCache::new(128, 8 * 1024 * 1024);
    let mut jobs: HashMap<PathBuf, mpsc::Receiver<std::io::Result<Vec<PathBuf>>>> =
        HashMap::new();
    let dir = temp.path().to_path_buf();

    let mut resolved = None;
    for _ in 0..1000 {
        if let Some(subdirs) = FileManApp::poll_subdirs(&mut cache, &mut jobs, &dir) {
            resolved = Some(subdirs);
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let subdirs = resolved.expect("background listing must resolve");
    let names: Vec<String> = subdirs
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["sub_a", "sub_b"], "only directories, sorted");
    assert!(cache.get(&dir).is_some(), "resolved listing is cached");
    assert!(jobs.is_empty(), "finished job is removed from the map");

    let cached = FileManApp::poll_subdirs(&mut cache, &mut jobs, &dir)
        .expect("cached listing resolves instantly");
    assert_eq!(cached.len(), 2);
    assert!(jobs.is_empty(), "cache hit must not spawn a job");
}

/// Headless reproduction of the address-bar flow: edit mode on, the
/// TextEdit acquires focus via the app's per-frame request_focus, the
/// user pastes a path, presses Enter — navigation must run and the bar
/// must drop back to breadcrumb mode. Uses the real egui::Context so
/// egui 0.36's own focus handling is exercised, not assumed.
#[test]
fn address_bar_enter_navigates_and_returns_to_breadcrumbs() {
    let ctx = egui::Context::default();
    let temp = std::env::temp_dir().join("fileman_addr_test");
    std::fs::create_dir_all(&temp).unwrap();
    let target = temp.display().to_string();

    let mut address_bar = String::new();
    let mut edit_mode = true;
    let mut navigated_to: Option<String> = None;
    let mut ever_focused = false;

    for frame in 0..6 {
        let mut raw = egui::RawInput::default();
        if frame == 4 {
            // The user pressed Enter (physical key event, like winit).
            raw.events.push(egui::Event::Key {
                key: egui::Key::Enter,
                physical_key: Some(egui::Key::Enter),
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            });
        }
        let mut full = ctx.run_ui(raw, |ui| {
            // --- exact show_pane_content address-bar fragment ---
            if edit_mode {
                let address_id = egui::Id::new(("address_bar", 0usize));
                let address_resp = ui.add(
                    egui::TextEdit::singleline(&mut address_bar)
                        .id(address_id)
                        .desired_width(f32::INFINITY)
                        .hint_text("Type a path and press Enter...")
                        .frame(
                            egui::Frame::new()
                                .fill(egui::Color32::TRANSPARENT)
                                .stroke(egui::Stroke::NONE),
                        ),
                );
                // pane_idx == self.active_pane in this scenario. The
                // app seeds focus only until it sticks (mirrors
                // `focused_address_pane != Some(pane_idx)`); re-asking
                // every frame would cancel egui's live lost_focus
                // signal on the Enter frame.
                if !ever_focused {
                    address_resp.request_focus();
                }
                if address_resp.has_focus() {
                    ever_focused = true;
                    // The app writes the pasted path here once focused
                    // (clipboard paste lands in the field).
                    if address_bar.is_empty() {
                        // Simulate Explorer's "Copy as path", which
                        // wraps the path in double quotes.
                        address_bar = format!("\"{target}\"");
                    }
                }
                if address_resp.lost_focus() {
                    if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        let typed = address_bar.trim().trim_matches('"').trim();
                        let t = std::path::PathBuf::from(typed);
                        navigated_to = Some(t.display().to_string());
                    }
                    edit_mode = false;
                }
            }
        });
        // egui asserts at drop if produced font textures were never
        // consumed by a renderer; headless tests have none.
        full.textures_delta.clear();
        if navigated_to.is_some() {
            break;
        }
    }

    assert!(ever_focused, "TextEdit must acquire keyboard focus");
    assert_eq!(
        navigated_to.as_deref(),
        Some(target.as_str()),
        "Enter must navigate"
    );
    assert!(!edit_mode, "bar must return to breadcrumbs after Enter");
}
