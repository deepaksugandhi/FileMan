use super::*;

pub(super) fn help_heading(ui: &mut egui::Ui, text: &str) {
    ui.add_space(8.0);
    ui.label(egui::RichText::new(text).strong().heading());
    ui.add_space(2.0);
}

pub(super) fn help_content(ui: &mut egui::Ui) {
    let w = |ui: &mut egui::Ui, text: &str| {
        ui.label(egui::RichText::new(text).weak());
    };

    help_heading(ui, "Getting Started");
    w(
        ui,
        "FileMan is a dual-pane file manager. The left panel shows a folder tree with your Favourites at the top. The center area has two independent file browsers, each with tabs, an address bar, a filter, and navigation buttons.",
    );
    ui.add_space(4.0);
    w(
        ui,
        "Switch users via the dropdown in the top-right corner. Each user has independent settings, favourites, toolbar layout, and shortcuts.",
    );

    help_heading(ui, "Navigation");
    w(
        ui,
        "Address Bar — type a path and press Enter to navigate directly.",
    );
    w(ui, "Back (Alt+Left) — return to the previous folder.");
    w(ui, "Forward (Alt+Right) — go forward after going back.");
    w(ui, "Up (Backspace) — go to the parent folder.");
    w(
        ui,
        "Tabs — each pane supports multiple tabs. Open a new tab with + Tab, or close one with the x on hover. Pinned tabs resist accidental navigation.",
    );
    w(
        ui,
        "🕒 Recent — the first toolbar button. Shows recently opened files and folders; click one to jump straight there, or Clear Recent to wipe the list.",
    );

    help_heading(ui, "View Modes");
    w(ui, "Switch between layouts via Settings > View:");
    w(
        ui,
        "  Details — columns for name, date, type, size. Click headers to sort.",
    );
    w(ui, "  List — compact single-column list.");
    w(ui, "  Icons — large icon grid for image-heavy folders.");
    w(
        ui,
        "The filter box (next to the Up button) narrows visible files by name. Click the red x to clear.",
    );

    help_heading(ui, "File Operations");
    w(
        ui,
        "Toolbar buttons provide quick access to Copy (Ctrl+C), Cut (Ctrl+X), Paste (Ctrl+V), Delete (Del), Rename (F2), New Folder, New File, Find (Ctrl+F), and Refresh (F5).",
    );
    w(
        ui,
        "Right-click any file or folder for the context menu with additional options: Extract, Copy Filename, Copy Folder Path, Open With, Open in Windows Explorer, and Add to Favourites.",
    );
    w(
        ui,
        "The \"Windows Explorer\" submenu covers your whole selection, not just the row you clicked — right-click any selected file with several others selected and pick, say, \"Combine files in Foxit PDF\" to combine all of them at once.",
    );
    w(
        ui,
        "Creating a single new folder selects it automatically — press Enter to open it right away. Enter also opens whatever single file or folder is currently selected.",
    );
    w(
        ui,
        "Files dragged in from a mail client (e.g. an Outlook attachment) are accepted the same as files dragged from Explorer.",
    );

    help_heading(ui, "Favourites");
    w(
        ui,
        "Right-click a folder and select Add to Favourites to pin it to the Folder Tree. Right-click a favourite to remove it.",
    );

    help_heading(ui, "Custom Actions");
    w(
        ui,
        "Custom actions let you open files with any application. Go to Settings > Custom Actions to add one. Each action shows as a 🔍 icon button on the second toolbar row.",
    );

    help_heading(ui, "App Launcher & File Launcher");
    w(
        ui,
        "App Launcher (Settings > App Launcher) lets you configure apps as ⚡ quick-launch buttons. File Launcher (Settings > File Launcher) does the same for specific files, opened with 📄 via their default app.",
    );
    w(
        ui,
        "Both show a search box on the toolbar's second row — type to filter, then click a result in the dropdown to launch it.",
    );

    help_heading(ui, "Settings");
    w(
        ui,
        "Appearance — theme (Light/Dark), font family, font size, tab layout (horizontal/vertical).",
    );
    w(
        ui,
        "Keyboard Shortcuts — click Rebind next to any action, then press the new key combination.",
    );
    w(
        ui,
        "Toolbar — reorder or toggle which buttons appear on the main row.",
    );
    w(
        ui,
        "View — choose the default listing layout (Details, List, or Icons).",
    );
    w(
        ui,
        "Advanced — set FileMan as the default folder explorer, or export/import all settings via JSON.",
    );

    help_heading(ui, "Keyboard Shortcuts");
    w(ui, "Ctrl+C Copy | Ctrl+X Cut | Ctrl+V Paste | Ctrl+F Find");
    w(
        ui,
        "F2 Rename | F3 Copy Filename | F4 Copy Folder Path | F5 Refresh",
    );
    w(
        ui,
        "Backspace Go Up | Delete Delete | Alt+Left Back | Alt+Right Forward",
    );
    w(
        ui,
        "Enter Confirm dialog / Open selected file or folder | Escape Cancel / Close dialog",
    );

    help_heading(ui, "Tips");
    w(
        ui,
        "- Pinned tabs won't navigate away when you double-click a folder.",
    );
    w(
        ui,
        "- The filter is per-tab, so each pane filters independently.",
    );
    w(ui, "- Drag the pane divider to resize left/right panes.");
    w(
        ui,
        "- Press Esc to close any dialog including this Help window.",
    );
    w(
        ui,
        "- Use Export/Import in Advanced settings to transfer your setup to another machine.",
    );
}
