use super::*;

/// Draws a settings-nav icon with plain epaint shapes so it never depends on
/// emoji/font coverage. All coordinates are fractions of the icon rect.
pub(super) fn paint_nav_icon(
    painter: &egui::Painter,
    r: egui::Rect,
    page: SettingsPage,
    color: egui::Color32,
) {
    let stroke = egui::Stroke::new(1.4, color);
    let c = r.center();
    let rad = r.width() * 0.42;
    let p = |fx: f32, fy: f32| egui::pos2(r.left() + r.width() * fx, r.top() + r.height() * fy);
    match page {
        SettingsPage::Appearance => {
            // Half-filled circle: light/dark theme toggle.
            painter.circle_stroke(c, rad, stroke);
            // Right half as a filled fan (no circle_sector helper here).
            let fill_r = rad - 1.2;
            let steps = 20;
            let mut pts = vec![c];
            for k in 0..=steps {
                let a =
                    -std::f32::consts::FRAC_PI_2 + std::f32::consts::PI * (k as f32 / steps as f32);
                pts.push(c + fill_r * egui::vec2(a.cos(), a.sin()));
            }
            painter.add(egui::Shape::convex_polygon(pts, color, egui::Stroke::NONE));
        }
        SettingsPage::Shortcuts => {
            // Keyboard: outlined body, space bar, two keys.
            painter.rect_stroke(
                r.shrink2(egui::vec2(0.5, 3.5)),
                3.0,
                stroke,
                egui::StrokeKind::Inside,
            );
            painter.rect_filled(
                egui::Rect::from_min_size(p(0.22, 0.62), egui::vec2(r.width() * 0.56, 2.4)),
                1.0,
                color,
            );
            painter.circle_filled(p(0.33, 0.38), 1.4, color);
            painter.circle_filled(p(0.52, 0.38), 1.4, color);
            painter.circle_filled(p(0.71, 0.38), 1.4, color);
        }
        SettingsPage::Toolbar => {
            // Three descending bars.
            for (i, w) in [0.86f32, 0.62, 0.40].into_iter().enumerate() {
                let y = r.top() + 3.0 + i as f32 * 4.6;
                painter.rect_filled(
                    egui::Rect::from_min_size(
                        egui::pos2(r.left() + 2.0, y),
                        egui::vec2((r.width() - 4.0) * w, 2.6),
                    ),
                    1.2,
                    color,
                );
            }
        }
        SettingsPage::CustomActions => {
            // Lightning bolt zigzag.
            let pts = [
                p(0.66, 0.02),
                p(0.30, 0.55),
                p(0.54, 0.55),
                p(0.36, 0.98),
                p(0.74, 0.42),
                p(0.50, 0.42),
                p(0.72, 0.02),
            ];
            painter.add(egui::Shape::line(pts.to_vec(), stroke));
        }
        SettingsPage::AppLauncher => {
            // Rocket / launch icon: triangle body + two fins + circle exhaust.
            // Body (upward triangle).
            painter.add(egui::Shape::convex_polygon(
                vec![p(0.50, 0.04), p(0.30, 0.70), p(0.70, 0.70)],
                color,
                egui::Stroke::NONE,
            ));
            // Left fin.
            painter.add(egui::Shape::line(
                vec![p(0.30, 0.70), p(0.16, 0.92), p(0.38, 0.78)],
                stroke,
            ));
            // Right fin.
            painter.add(egui::Shape::line(
                vec![p(0.70, 0.70), p(0.84, 0.92), p(0.62, 0.78)],
                stroke,
            ));
            // Exhaust circle.
            painter.circle_filled(p(0.50, 0.88), r.width() * 0.10, color);
        }
        SettingsPage::FileLauncher => {
            // Document with a small arrow: file + launch.
            // Document body (folded-corner rectangle).
            painter.add(egui::Shape::line(
                vec![
                    p(0.20, 0.06),
                    p(0.58, 0.06),
                    p(0.76, 0.24),
                    p(0.76, 0.94),
                    p(0.20, 0.94),
                    p(0.20, 0.06),
                ],
                stroke,
            ));
            painter.line_segment([p(0.58, 0.06), p(0.58, 0.24)], stroke);
            painter.line_segment([p(0.58, 0.24), p(0.76, 0.24)], stroke);
            // Small right-pointing triangle (launch) in the lower-left.
            painter.add(egui::Shape::convex_polygon(
                vec![p(0.24, 0.50), p(0.24, 0.82), p(0.48, 0.66)],
                color,
                egui::Stroke::NONE,
            ));
        }
        SettingsPage::FileTypes => {
            // Small file/document with a folded corner.
            painter.add(egui::Shape::line(
                vec![
                    p(0.22, 0.06),
                    p(0.60, 0.06),
                    p(0.78, 0.24),
                    p(0.78, 0.94),
                    p(0.22, 0.94),
                    p(0.22, 0.06),
                ],
                stroke,
            ));
            painter.line_segment([p(0.60, 0.06), p(0.60, 0.24)], stroke);
            painter.line_segment([p(0.60, 0.24), p(0.78, 0.24)], stroke);
        }
        SettingsPage::ViewMode => {
            // Three horizontal lines (list layout icon).
            for (i, w) in [1.0f32, 0.75, 0.5].into_iter().enumerate() {
                let y = r.top() + 3.0 + i as f32 * 4.6;
                painter.rect_filled(
                    egui::Rect::from_min_size(
                        egui::pos2(r.left() + 2.0, y),
                        egui::vec2((r.width() - 4.0) * w, 2.6),
                    ),
                    1.2,
                    color,
                );
            }
        }
        SettingsPage::Advanced => {
            // Gear: ring + teeth stubs + hub.
            painter.circle_stroke(c, rad - 2.2, stroke);
            for k in 0..8u32 {
                let a = k as f32 * std::f32::consts::TAU / 8.0;
                let dx = a.cos();
                let dy = a.sin();
                painter.line_segment(
                    [
                        c + rad * 0.62 * egui::vec2(dx, dy),
                        c + (rad - 0.6) * egui::vec2(dx, dy),
                    ],
                    stroke,
                );
            }
            painter.circle_filled(c, 2.2, color);
        }
        SettingsPage::About => {
            // Info glyph: ring, dot, stem.
            painter.circle_stroke(c, rad, stroke);
            painter.circle_filled(p(0.5, 0.28), 1.5, color);
            painter.line_segment([p(0.5, 0.46), p(0.5, 0.76)], stroke);
        }
    }
}

