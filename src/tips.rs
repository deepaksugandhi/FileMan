//! Rotating feature-tips card, pinned to the bottom-left corner of the
//! window. Shows one hint at a time, advances automatically every few
//! seconds (pausing while the pointer rests on it), and can be hidden for
//! the session or disabled outright (persisted via `config`, key
//! [`KEY_TIPS_ENABLED`]; re-enable from Settings → Appearance).
//!
//! The card deliberately uses an indigo-and-amber palette unrelated to the
//! app's grey Windows-style chrome, so it reads as a distinct floating
//! surface rather than part of the main UI.

use eframe::egui;
use std::time::{Duration, Instant, UNIX_EPOCH};

/// Seconds each tip stays on screen before rotating to the next.
const ROTATE_SECS: f32 = 8.0;

/// Config key storing whether tips are enabled ("true"/"false").
pub const KEY_TIPS_ENABLED: &str = "tips_enabled";

/// The tips themselves — every entry must describe a real FileMan feature.
const TIPS: &[&str] = &[
    "Ctrl+C / Ctrl+X / Ctrl+V copy, cut and paste files — pasting lands in whichever folder you're browsing.",
    "Use F3 to copy full file path & F4 to copy folder path.",
    "Delete sends the selection to the Recycle Bin, so slips of the mouse are always recoverable.",
    "Create a unique profile for each user/scenario. Click dropdown besides Settings button.",
    "Press Ctrl+F to search a folder tree recursively, or just press * to jump straight into the filter box and narrow the list by name.",
	"Rename tabs for easy identification. They appear witha green dot.",
    "Right-click any tab to duplicate or close it — handy for comparing two folders side by side.",
    "Drag the divider between the two panes to give either side more room.",
    "Click ☆ Add to Favourites to pin the current folder to the sidebar; right-click it there to remove.",
    "Custom Actions open selected files with your favourite apps — icons are pulled straight from each exe.",
    "Every shortcut is rebindable: Settings → Keyboard Shortcuts, click Rebind, then press the new keys.",
    "You decide what lives on the toolbar — Settings → Toolbar shows or hides any button.",
    "Hold Ctrl and scroll the mouse wheel to zoom the entire interface, text included.",
    "Switching PCs? Export your settings from Settings → Advanced and import them on the other machine.",
    "Make FileMan the default folder explorer in Settings → Advanced and folders will open here, not Explorer.",
    "Prefer stacked tabs? Switch between horizontal and vertical tab strips under Settings → Appearance.",
    "Large copies and moves run in the background with a progress window — you can keep browsing meanwhile.",
    "Pin your favourite apps or files as quick-launch buttons in Settings → App Launcher / File Launcher, then find them fast by typing into the search box on the second toolbar row.",
    "Just created a folder? It's already selected — press Enter to jump straight in.",
    "Click 🕒 Recent on the toolbar to jump back to files and folders you've opened recently.",
    "Select several files, then use the \"Windows Explorer\" right-click submenu — commands like combining PDFs see your whole selection, not just the file you clicked.",
    "Click a tab in the other pane to switch focus — you don't need to click on a file row first.",
];

/// What the user did with the card during the last [`TipsCard::draw`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TipAction {
    /// Nothing happened.
    None,
    /// The ✕ button hid the card until the next launch (no persistence needed).
    Close,
    /// "Turn off" was pressed — persist the disabled state.
    Disable,
}

/// State for the tips card: which tip is showing and when it appeared.
pub struct TipsCard {
    index: usize,
    shown_at: Instant,
    /// False once the ✕ button hides the card for the rest of the session.
    visible: bool,
    /// Whether the pointer rested on the card last frame (pauses rotation).
    hovered: bool,
}

impl TipsCard {
    /// Starts at a launch-varying tip so returning users don't always see
    /// the same first hint.
    pub fn new() -> Self {
        let start = std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.subsec_nanos() as usize % TIPS.len())
            .unwrap_or(0);
        Self {
            index: start,
            shown_at: Instant::now(),
            visible: true,
            hovered: false,
        }
    }

    /// Advances to the next tip and restarts the rotation timer.
    fn advance(&mut self) {
        self.index = (self.index + 1) % TIPS.len();
        self.shown_at = Instant::now();
    }

    /// Restores the card after it was hidden/disabled (Settings toggle),
    /// restarting the timer so the freshly shown tip gets its full period.
    pub fn set_visible(&mut self, visible: bool) {
        self.visible = visible;
        if visible {
            self.shown_at = Instant::now();
        }
    }

    /// Draws the card (when enabled and visible) and reports any button the
    /// user pressed. Schedules the repaint that fires exactly when the next
    /// rotation is due, so the timer works even while the app sits idle.
    pub fn draw(&mut self, ctx: &egui::Context, font_size: f32) -> TipAction {
        if !self.visible {
            return TipAction::None;
        }
        if !self.hovered && self.shown_at.elapsed() >= Duration::from_secs_f32(ROTATE_SECS) {
            self.advance();
        }

        // Indigo + amber in dark mode; cream paper + ochre in light mode.
        // Nothing here matches the app chrome, on purpose. Resolved inside
        // the card's Ui so it always follows the active theme.
        let mut action = TipAction::None;
        egui::Window::new("Tip")
            .id(egui::Id::new("tips_card"))
            .anchor(egui::Align2::LEFT_BOTTOM, [14.0, -14.0])
            .title_bar(false)
            .resizable(false)
            .collapsible(false)
            .show(ctx, |ui| {
                // Re-skin just this card so its buttons follow the tip
                // palette instead of the app-wide grey 3D treatment.
                ui.scope(|ui| {
                    let dark = ui.visuals().dark_mode;
                    let (
                        card_fill,
                        card_stroke,
                        title_col,
                        body_col,
                        accent_col,
                        track_col,
                        btn_fill,
                        btn_hover_fill,
                        btn_active_fill,
                        btn_text,
                    ) = if dark {
                        (
                            egui::Color32::from_rgb(34, 28, 58),
                            egui::Color32::from_rgb(128, 108, 212),
                            egui::Color32::from_rgb(255, 209, 102),
                            egui::Color32::from_rgb(232, 228, 248),
                            egui::Color32::from_rgb(255, 193, 92),
                            egui::Color32::from_rgb(62, 53, 98),
                            egui::Color32::from_rgb(52, 44, 86),
                            egui::Color32::from_rgb(66, 56, 106),
                            egui::Color32::from_rgb(42, 35, 72),
                            egui::Color32::from_rgb(238, 233, 252),
                        )
                    } else {
                        (
                            egui::Color32::from_rgb(255, 250, 235),
                            egui::Color32::from_rgb(214, 164, 70),
                            egui::Color32::from_rgb(146, 94, 6),
                            egui::Color32::from_rgb(76, 61, 34),
                            egui::Color32::from_rgb(198, 138, 26),
                            egui::Color32::from_rgb(238, 225, 194),
                            egui::Color32::from_rgb(250, 240, 216),
                            egui::Color32::from_rgb(245, 230, 197),
                            egui::Color32::from_rgb(237, 220, 181),
                            egui::Color32::from_rgb(96, 75, 32),
                        )
                    };
                    let v = &mut ui.style_mut().visuals.widgets;
                    for state in [&mut v.inactive, &mut v.hovered, &mut v.active] {
                        state.corner_radius = egui::CornerRadius::same(5);
                    }
                    v.inactive.bg_fill = btn_fill;
                    v.inactive.weak_bg_fill = btn_fill;
                    v.inactive.fg_stroke = egui::Stroke::new(1.0, btn_text);
                    v.hovered.bg_fill = btn_hover_fill;
                    v.hovered.weak_bg_fill = btn_hover_fill;
                    v.hovered.fg_stroke = egui::Stroke::new(1.0, btn_text);
                    v.hovered.expansion = 1.0;
                    v.active.bg_fill = btn_active_fill;
                    v.active.weak_bg_fill = btn_active_fill;
                    v.active.fg_stroke = egui::Stroke::new(1.0, btn_text);

                    let frame = egui::Frame::new()
                        .fill(card_fill)
                        .stroke(egui::Stroke::new(1.5, card_stroke))
                        .corner_radius(egui::CornerRadius::same(10))
                        .inner_margin(egui::Margin::same(12))
                        .show(ui, |ui| {
                            ui.set_min_width(300.0);

                            ui.horizontal(|ui| {
                                ui.label(
                                    egui::RichText::new("💡 Did you know?")
                                        .strong()
                                        .size(font_size)
                                        .color(title_col),
                                );
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        // Close button drawn as a vector ✕
                                        // (two strokes) so it never depends
                                        // on font/emoji glyph coverage — same
                                        // approach as the settings nav icons.
                                        let (close_rect, close_resp) = ui.allocate_exact_size(
                                            egui::vec2(20.0, 20.0),
                                            egui::Sense::click(),
                                        );
                                        let close_resp = close_resp
                                            .on_hover_text("Hide tips until FileMan restarts");
                                        if close_resp.clicked() {
                                            self.visible = false;
                                            action = TipAction::Close;
                                        }
                                        let bg = if close_resp.clicked() {
                                            btn_active_fill
                                        } else if close_resp.hovered() {
                                            btn_hover_fill
                                        } else {
                                            egui::Color32::TRANSPARENT
                                        };
                                        if bg != egui::Color32::TRANSPARENT {
                                            ui.painter().rect_filled(close_rect, 5.0, bg);
                                        }
                                        let c = close_rect.center();
                                        let r = close_rect.width() * 0.30;
                                        let arm = egui::Stroke::new(1.6, btn_text);
                                        ui.painter().line_segment(
                                            [
                                                c + r * egui::vec2(-1.0, -1.0),
                                                c + r * egui::vec2(1.0, 1.0),
                                            ],
                                            arm,
                                        );
                                        ui.painter().line_segment(
                                            [
                                                c + r * egui::vec2(-1.0, 1.0),
                                                c + r * egui::vec2(1.0, -1.0),
                                            ],
                                            arm,
                                        );
                                        ui.label(
                                            egui::RichText::new(format!(
                                                "{}/{}",
                                                self.index + 1,
                                                TIPS.len()
                                            ))
                                            .small()
                                            .color(body_col),
                                        );
                                    },
                                );
                            });
                            ui.add_space(2.0);
                            ui.label(
                                egui::RichText::new(TIPS[self.index])
                                    .size(font_size)
                                    .color(body_col),
                            );

                            ui.add_space(8.0);
                            ui.horizontal(|ui| {
                                if ui.button("Next ›").clicked() {
                                    self.advance();
                                }
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if ui
                                            .small_button("Turn off")
                                            .on_hover_text(
                                                "Disable tips permanently — \
                                                 re-enable in Settings → Appearance",
                                            )
                                            .clicked()
                                        {
                                            action = TipAction::Disable;
                                        }
                                    },
                                );
                            });

                            // Thin countdown line: fills toward the next
                            // automatic rotation.
                            let (rect, _) = ui.allocate_exact_size(
                                egui::vec2(ui.available_width(), 3.0),
                                egui::Sense::hover(),
                            );
                            let frac = rotation_fraction(self.shown_at.elapsed());
                            ui.painter().rect_filled(rect, 1.5, track_col);
                            ui.painter().rect_filled(
                                egui::Rect::from_min_size(
                                    rect.min,
                                    egui::vec2(rect.width() * frac, rect.height()),
                                ),
                                1.5,
                                accent_col,
                            );
                        });
                    // Purely geometric hit test: children (buttons) shouldn't
                    // un-pause the timer while the pointer is over the card.
                    self.hovered = frame.response.contains_pointer();
                });
            });

        // Wake up exactly when the next rotation (or progress-bar tick) is
        // due; the floor guards against a zero-duration busy loop.
        let remaining = Duration::from_secs_f32(ROTATE_SECS)
            .saturating_sub(self.shown_at.elapsed())
            .max(Duration::from_millis(50));
        ctx.request_repaint_after(remaining);
        action
    }
}

/// Fraction (0..=1) of the rotation period already elapsed.
fn rotation_fraction(elapsed: Duration) -> f32 {
    (elapsed.as_secs_f32() / ROTATE_SECS).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tips_are_non_empty_and_wrappable() {
        assert!(TIPS.len() > 1);
        assert!(TIPS.iter().all(|t| !t.trim().is_empty()));
    }

    #[test]
    fn advance_cycles_through_every_tip_before_repeating() {
        let mut card = TipsCard {
            index: 0,
            shown_at: Instant::now(),
            visible: true,
            hovered: false,
        };
        let mut seen = Vec::new();
        for _ in 0..TIPS.len() {
            seen.push(card.index);
            card.advance();
        }
        seen.sort_unstable();
        assert_eq!(seen, (0..TIPS.len()).collect::<Vec<_>>());
        assert_eq!(card.index, 0); // wrapped back to the start
    }

    #[test]
    fn rotation_fraction_clamps_to_unit_range() {
        assert_eq!(rotation_fraction(Duration::ZERO), 0.0);
        assert_eq!(rotation_fraction(Duration::from_secs(2)), 0.25);
        assert_eq!(rotation_fraction(Duration::from_secs(60)), 1.0);
    }

    #[test]
    fn set_visible_false_hides_until_restored() {
        let mut card = TipsCard::new();
        card.set_visible(false);
        // A hidden card must not report actions — draw short-circuits.
        assert_eq!(card.visible, false);
        card.set_visible(true);
        assert!(card.visible);
    }
}
