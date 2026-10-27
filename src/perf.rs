//! Opt-in frame-time instrumentation. Enabled by setting `FILEMAN_PERF=1`
//! (or toggled at runtime with Ctrl+Shift+F12). When on, an overlay shows the
//! last/avg/p95/max `ui()` time and repaints per second, and a summary line is
//! appended to `%APPDATA%\FileMan\perf.log` every 5 s, plus a one-off
//! "first frame" line measuring cold start. Costs ~nothing when off.

use eframe::egui;
use std::io::Write;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

static START: OnceLock<Instant> = OnceLock::new();

/// Call first thing in `main` so cold-start timing includes startup work.
pub fn mark_start() {
    START.get_or_init(Instant::now);
}

const RING: usize = 240;
const LOG_EVERY: Duration = Duration::from_secs(5);

pub struct Perf {
    pub enabled: bool,
    process_start: Instant,
    frame_start: Option<Instant>,
    ring: Vec<f32>,
    next: usize,
    frames_in_window: u32,
    window_start: Instant,
    last_log: Instant,
    first_frame_logged: bool,
    fps: f32,
}

impl Perf {
    pub fn new() -> Self {
        let now = Instant::now();
        let process_start = *START.get_or_init(Instant::now);
        Perf {
            enabled: std::env::var_os("FILEMAN_PERF").is_some_and(|v| v != "0"),
            process_start,
            frame_start: None,
            ring: Vec::with_capacity(RING),
            next: 0,
            frames_in_window: 0,
            window_start: now,
            last_log: now,
            first_frame_logged: false,
            fps: 0.0,
        }
    }

    pub fn begin(&mut self, ctx: &egui::Context) {
        if ctx.input(|i| {
            i.modifiers.ctrl && i.modifiers.shift && i.key_pressed(egui::Key::F12)
        }) {
            self.enabled = !self.enabled;
        }
        if self.enabled {
            self.frame_start = Some(Instant::now());
        }
    }

    pub fn end(&mut self, ctx: &egui::Context) {
        if !self.enabled {
            return;
        }
        let Some(start) = self.frame_start.take() else {
            return;
        };
        let ms = start.elapsed().as_secs_f32() * 1000.0;
        if !self.first_frame_logged {
            self.first_frame_logged = true;
            let since = self.process_start.elapsed().as_secs_f32() * 1000.0;
            log_line(&format!(
                "first_frame process_start_to_first_ui_done={since:.0}ms first_ui={ms:.1}ms"
            ));
        }
        if self.ring.len() < RING {
            self.ring.push(ms);
        } else {
            self.ring[self.next] = ms;
        }
        self.next = (self.next + 1) % RING;
        self.frames_in_window += 1;
        let win = self.window_start.elapsed();
        if win >= Duration::from_secs(1) {
            self.fps = self.frames_in_window as f32 / win.as_secs_f32();
            self.frames_in_window = 0;
            self.window_start = Instant::now();
        }
        let (avg, p95, max) = self.stats();
        if self.last_log.elapsed() >= LOG_EVERY {
            self.last_log = Instant::now();
            log_line(&format!(
                "frames/s={:.1} ui_ms avg={avg:.2} p95={p95:.2} max={max:.2}",
                self.fps
            ));
        }
        egui::Area::new(egui::Id::new("perf_overlay"))
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::RIGHT_BOTTOM, [-8.0, -8.0])
            .interactable(false)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.monospace(format!(
                        "ui {ms:5.2} ms  avg {avg:5.2}  p95 {p95:5.2}  max {max:5.2}\n{:.1} frames/s",
                        self.fps
                    ));
                });
            });
    }

    fn stats(&self) -> (f32, f32, f32) {
        if self.ring.is_empty() {
            return (0.0, 0.0, 0.0);
        }
        let mut v = self.ring.clone();
        v.sort_by(|a, b| a.total_cmp(b));
        let avg = v.iter().sum::<f32>() / v.len() as f32;
        let p95 = v[((v.len() as f32 * 0.95) as usize).min(v.len() - 1)];
        (avg, p95, *v.last().unwrap())
    }
}

fn log_line(line: &str) {
    let Some(appdata) = std::env::var_os("APPDATA") else {
        return;
    };
    let path = std::path::PathBuf::from(appdata).join("FileMan").join("perf.log");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{line}");
    }
}
