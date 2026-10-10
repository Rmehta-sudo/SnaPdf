use eframe::egui::{
    self, Color32, CornerRadius, FontFamily, FontId, Stroke, TextStyle, Theme, ThemePreference,
    Visuals,
};

pub const SIDEBAR_BG: Color32 = Color32::from_rgb(20, 22, 26);
pub const CANVAS_BG: Color32 = Color32::from_rgb(30, 32, 37);
pub const PANEL_BG: Color32 = Color32::from_rgb(28, 30, 35);
pub const ACCENT: Color32 = Color32::from_rgb(104, 116, 214);
pub const MUTED: Color32 = Color32::from_rgb(150, 154, 166);

pub fn apply(ctx: &egui::Context) {
    ctx.set_theme(ThemePreference::Dark);
    let mut style = (*ctx.style_of(Theme::Dark)).clone();
    style.spacing.item_spacing = egui::vec2(8.0, 8.0);
    style.spacing.button_padding = egui::vec2(11.0, 7.0);
    style.visuals = Visuals::dark();
    style.visuals.panel_fill = PANEL_BG;
    style.visuals.window_fill = PANEL_BG;
    style.visuals.extreme_bg_color = Color32::from_rgb(16, 18, 21);
    style.visuals.faint_bg_color = Color32::from_rgb(38, 40, 46);
    style.visuals.selection.bg_fill = ACCENT.gamma_multiply(0.65);
    style.visuals.window_corner_radius = CornerRadius::same(10);
    style.visuals.menu_corner_radius = CornerRadius::same(10);
    style.visuals.widgets.inactive.corner_radius = CornerRadius::same(8);
    style.visuals.widgets.hovered.corner_radius = CornerRadius::same(8);
    style.visuals.widgets.active.corner_radius = CornerRadius::same(8);
    style.visuals.widgets.inactive.bg_fill = Color32::from_rgb(40, 42, 48);
    style.visuals.widgets.inactive.weak_bg_fill = Color32::from_rgb(36, 38, 44);
    style.visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, Color32::from_rgb(54, 57, 66));
    style.visuals.widgets.hovered.bg_fill = Color32::from_rgb(50, 53, 62);
    style.visuals.widgets.hovered.weak_bg_fill = Color32::from_rgb(47, 50, 58);
    style.visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, Color32::from_rgb(78, 84, 104));
    style.visuals.widgets.active.bg_fill = ACCENT.gamma_multiply(0.72);
    style.visuals.widgets.active.bg_stroke = Stroke::new(1.0, ACCENT);
    style.visuals.interact_cursor = Some(egui::CursorIcon::PointingHand);
    style.scroll_animation =
        egui::style::ScrollAnimation::new(1_900.0, egui::Rangef::new(0.06, 0.18));
    style.text_styles.insert(
        TextStyle::Heading,
        FontId::new(20.0, FontFamily::Proportional),
    );
    style
        .text_styles
        .insert(TextStyle::Body, FontId::new(14.0, FontFamily::Proportional));
    ctx.set_style_of(Theme::Dark, style);
}
