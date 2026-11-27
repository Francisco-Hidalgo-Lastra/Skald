//! Toast notifications, stacked in the bottom-right corner of the window.

use crate::theme;
use egui::{Color32, RichText};
use std::time::Duration;

/// Seconds a plain info toast stays up before it starts to fade.
const LIFETIME: f64 = 5.0;
/// Seconds the fade-out takes.
const FADE: f64 = 0.5;
/// Toasts on screen at once. A new toast past this drops the oldest.
const MAX: usize = 6;
const WIDTH: f32 = 380.0;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Info,
    Warn,
    Error,
}

impl Level {
    fn icon(self) -> (&'static str, Color32) {
        match self {
            Level::Info => ("›", theme::FROST),
            Level::Warn => ("⚠", theme::YELLOW),
            Level::Error => ("✖", theme::RED),
        }
    }
}

struct Toast {
    id: u64,
    level: Level,
    title: String,
    details: Vec<(Level, String)>,
    /// Time of the last frame the pointer was away from the toast. None until it is first drawn.
    since: Option<f64>,
}

impl Toast {
    /// Info toasts without details fade out by themselves. The rest stay until closed.
    fn sticky(&self) -> bool {
        self.level != Level::Info || !self.details.is_empty()
    }
}

#[derive(Default)]
pub struct Toasts {
    list: Vec<Toast>,
    next_id: u64,
}

impl Toasts {
    pub fn push(&mut self, level: Level, title: impl Into<String>, details: Vec<(Level, String)>) {
        self.list.push(Toast { id: self.next_id, level, title: title.into(), details, since: None });
        self.next_id += 1;
        if self.list.len() > MAX {
            self.list.remove(0);
        }
    }

    /// Draws the toasts with the bottom one `bottom` points above the window's bottom edge.
    pub fn show(&mut self, ctx: &egui::Context, bottom: f32) {
        if self.list.is_empty() {
            return;
        }
        let now = ctx.input(|i| i.time);
        let mut closed = None;
        egui::Area::new(egui::Id::unique("toasts"))
            .anchor(egui::Align2::RIGHT_BOTTOM, [-14.0, -bottom])
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                ui.set_width(WIDTH);
                for t in &mut self.list {
                    let age = now - *t.since.get_or_insert(now);
                    let (icon, color) = t.level.icon();
                    let frame = theme::card().fill(theme::WIDGET).stroke(egui::Stroke::new(1.0, color.gamma_multiply(0.6)));
                    let r = ui.scope(|ui| {
                        if !t.sticky() {
                            ui.multiply_opacity(((LIFETIME + FADE - age) / FADE).clamp(0.0, 1.0) as f32);
                        }
                        frame.show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.horizontal_top(|ui| {
                                ui.label(RichText::new(icon).strong().color(color));
                                ui.vertical(|ui| {
                                    ui.set_width(ui.available_width() - 28.0);
                                    ui.add(egui::Label::new(RichText::new(&t.title).color(theme::TEXT)).wrap());
                                    if !t.details.is_empty() {
                                        ui.add_space(2.0);
                                        egui::ScrollArea::vertical().id_salt(("toast", t.id)).max_height(180.0).show(ui, |ui| {
                                            ui.spacing_mut().item_spacing.y = 2.0;
                                            for (level, line) in &t.details {
                                                let color = if *level == Level::Info { theme::TEXT_WEAK } else { level.icon().1 };
                                                ui.add(egui::Label::new(RichText::new(line).small().color(color)).wrap());
                                            }
                                        });
                                    }
                                });
                                if ui.add(theme::icon_button("✖")).on_hover_text("Close").clicked() {
                                    closed = Some(t.id);
                                }
                            });
                        });
                    });
                    // Hovering keeps a toast up and resets its timer.
                    if ui.rect_contains_pointer(r.response.rect) {
                        t.since = Some(now);
                    }
                    ui.add_space(6.0);
                }
            });

        self.list.retain(|t| Some(t.id) != closed && (t.sticky() || now - t.since.unwrap_or(now) < LIFETIME + FADE));

        // Wake up when the next toast starts fading, then every frame while it fades.
        let wake = self
            .list
            .iter()
            .filter(|t| !t.sticky())
            .map(|t| (LIFETIME - (now - t.since.unwrap_or(now))).max(0.0))
            .fold(f64::INFINITY, f64::min);
        if wake.is_finite() {
            ctx.request_repaint_after(Duration::from_secs_f64(wake.max(1.0 / 60.0)));
        }
    }
}
