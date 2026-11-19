//! Look and feel: a dark "Nordic night" palette with frost-blue and gold accents,
//! plus a few small widgets (pills, section headers, cards) shared by the GUI.

use egui::{Color32, CornerRadius, FontFamily, FontId, Margin, RichText, Shadow, Stroke, TextStyle};

pub const BG_DEEP: Color32 = Color32::from_rgb(0x10, 0x13, 0x18);
pub const PANEL: Color32 = Color32::from_rgb(0x16, 0x1a, 0x20);
pub const SURFACE: Color32 = Color32::from_rgb(0x1d, 0x22, 0x2a);
pub const WIDGET: Color32 = Color32::from_rgb(0x25, 0x2b, 0x35);
pub const WIDGET_HOVER: Color32 = Color32::from_rgb(0x2f, 0x37, 0x44);
pub const BORDER: Color32 = Color32::from_rgb(0x2c, 0x33, 0x3e);
pub const TEXT: Color32 = Color32::from_rgb(0xd8, 0xde, 0xe9);
pub const TEXT_WEAK: Color32 = Color32::from_rgb(0x84, 0x8d, 0x9c);
pub const ON_ACCENT: Color32 = Color32::from_rgb(0x12, 0x15, 0x1a);

pub const FROST: Color32 = Color32::from_rgb(0x88, 0xc0, 0xd0);
pub const GOLD: Color32 = Color32::from_rgb(0xd8, 0xb0, 0x6a);
pub const GREEN: Color32 = Color32::from_rgb(0x9b, 0xc8, 0x8a);
pub const RED: Color32 = Color32::from_rgb(0xdb, 0x76, 0x78);
pub const YELLOW: Color32 = Color32::from_rgb(0xeb, 0xcb, 0x8b);
pub const PURPLE: Color32 = Color32::from_rgb(0xb4, 0x8e, 0xad);

pub fn apply(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Dark);
    ctx.all_styles_mut(|style| {
        style.text_styles = [
            (TextStyle::Heading, FontId::new(20.0, FontFamily::Proportional)),
            (TextStyle::Body, FontId::new(14.0, FontFamily::Proportional)),
            (TextStyle::Button, FontId::new(14.0, FontFamily::Proportional)),
            (TextStyle::Small, FontId::new(11.0, FontFamily::Proportional)),
            (TextStyle::Monospace, FontId::new(12.5, FontFamily::Monospace)),
        ]
        .into();

        let s = &mut style.spacing;
        s.item_spacing = egui::vec2(8.0, 6.0);
        s.button_padding = egui::vec2(12.0, 5.0);
        s.interact_size.y = 24.0;
        s.window_margin = Margin::same(18);
        s.menu_margin = Margin::same(6);
        s.indent = 18.0;

        let v = &mut style.visuals;
        v.dark_mode = true;
        v.override_text_color = None;
        v.weak_text_color = Some(TEXT_WEAK);
        v.panel_fill = PANEL;
        v.window_fill = SURFACE;
        v.window_stroke = Stroke::new(1.0, BORDER);
        v.window_shadow = Shadow { offset: [0, 8], blur: 28, spread: 0, color: Color32::from_black_alpha(140) };
        v.popup_shadow = Shadow { offset: [0, 4], blur: 14, spread: 0, color: Color32::from_black_alpha(120) };
        v.window_corner_radius = CornerRadius::ZERO;
        v.menu_corner_radius = CornerRadius::ZERO;
        v.extreme_bg_color = BG_DEEP;
        v.faint_bg_color = Color32::from_rgb(0x1a, 0x1f, 0x26);
        v.code_bg_color = BG_DEEP;
        v.hyperlink_color = FROST;
        v.warn_fg_color = YELLOW;
        v.error_fg_color = RED;
        v.selection.bg_fill = FROST.gamma_multiply(0.22);
        v.selection.stroke = Stroke::new(1.0, FROST);
        v.striped = true;

        let w = &mut v.widgets;
        w.noninteractive.bg_fill = SURFACE;
        w.noninteractive.weak_bg_fill = SURFACE;
        w.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
        w.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
        w.inactive.bg_fill = WIDGET;
        w.inactive.weak_bg_fill = WIDGET;
        w.inactive.bg_stroke = Stroke::NONE;
        w.inactive.fg_stroke = Stroke::new(1.0, TEXT);
        w.hovered.bg_fill = WIDGET_HOVER;
        w.hovered.weak_bg_fill = WIDGET_HOVER;
        w.hovered.bg_stroke = Stroke::new(1.0, FROST.gamma_multiply(0.5));
        w.hovered.fg_stroke = Stroke::new(1.5, Color32::WHITE);
        w.active.bg_fill = FROST.gamma_multiply(0.35);
        w.active.weak_bg_fill = FROST.gamma_multiply(0.35);
        w.active.bg_stroke = Stroke::new(1.0, FROST);
        w.active.fg_stroke = Stroke::new(1.5, Color32::WHITE);
        w.open.bg_fill = WIDGET_HOVER;
        w.open.weak_bg_fill = WIDGET_HOVER;
        w.open.bg_stroke = Stroke::new(1.0, FROST.gamma_multiply(0.5));
        w.open.fg_stroke = Stroke::new(1.0, TEXT);
        // Square widgets throughout.
        for wv in [&mut w.noninteractive, &mut w.inactive, &mut w.hovered, &mut w.active, &mut w.open] {
            wv.corner_radius = CornerRadius::ZERO;
            wv.expansion = 0.0;
        }
    });
}

/// A small coloured tag, e.g. `ESM` or `FOMOD`.
pub fn pill(ui: &mut egui::Ui, text: impl Into<String>, color: Color32) -> egui::Response {
    egui::Frame::NONE
        .fill(color.gamma_multiply(0.16))
        .stroke(Stroke::new(1.0, color.gamma_multiply(0.55)))
        .inner_margin(Margin::symmetric(5, 0))
        .show(ui, |ui| ui.label(RichText::new(text.into()).size(10.5).color(color).strong()))
        .response
}

/// Frame for the main content panels.
pub fn panel_frame(fill: Color32) -> egui::Frame {
    egui::Frame::NONE.fill(fill).inner_margin(Margin::symmetric(14, 12))
}

/// A flat raised box for grouped content.
pub fn card() -> egui::Frame {
    egui::Frame::NONE.fill(SURFACE).stroke(Stroke::new(1.0, BORDER)).inner_margin(Margin::symmetric(12, 8))
}

/// A filled call-to-action button.
pub fn accent_button(text: impl Into<String>, color: Color32) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text.into()).strong().color(ON_ACCENT)).fill(color)
}

/// A button without a background, for icon actions in table rows.
pub fn icon_button(icon: &str) -> egui::Button<'static> {
    egui::Button::new(RichText::new(icon.to_string()).size(12.0)).frame_when_inactive(false).min_size(egui::vec2(22.0, 20.0))
}

/// A tab in a tab strip: label plus a muted count, underlined in frost when selected.
pub fn tab(ui: &mut egui::Ui, selected: bool, label: &str, count: usize) -> egui::Response {
    let color = if selected { FROST } else { TEXT_WEAK };
    let mut job = egui::text::LayoutJob::default();
    RichText::new(label.to_uppercase()).size(13.0).strong().color(color).extra_letter_spacing(1.5).append_to(
        &mut job,
        ui.style(),
        egui::FontSelection::Default,
        egui::Align::Center,
    );
    RichText::new(format!("  {count}")).small().color(TEXT_WEAK).append_to(&mut job, ui.style(), egui::FontSelection::Default, egui::Align::Center);
    let resp = ui.add(egui::Button::new(job).frame_when_inactive(false).min_size(egui::vec2(0.0, 30.0)));
    if selected {
        let r = resp.rect;
        ui.painter().hline(r.x_range(), r.bottom() - 1.0, Stroke::new(2.0, FROST));
    }
    resp
}
