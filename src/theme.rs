use eframe::egui::{
    self, Color32, CornerRadius, FontFamily, FontId, Stroke, TextStyle, Theme, ThemePreference,
    Visuals,
};

pub const SIDEBAR_BG: Color32 = Color32::from_rgb(35, 35, 35);
pub const CANVAS_BG: Color32 = Color32::from_rgb(30, 30, 30);
pub const PANEL_BG: Color32 = Color32::from_rgb(29, 29, 29);
pub const ACCENT: Color32 = Color32::from_rgb(105, 105, 105);
pub const MUTED: Color32 = Color32::from_rgb(154, 154, 154);

pub fn apply(ctx: &egui::Context) {
    ctx.set_theme(ThemePreference::Dark);
    let mut style = (*ctx.style_of(Theme::Dark)).clone();
    style.spacing.item_spacing = egui::vec2(6.0, 6.0);
    style.spacing.button_padding = egui::vec2(9.0, 6.0);
    style.visuals = Visuals::dark();
    style.visuals.panel_fill = PANEL_BG;
    style.visuals.window_fill = PANEL_BG;
    style.visuals.extreme_bg_color = Color32::from_rgb(23, 23, 23);
    style.visuals.faint_bg_color = Color32::from_rgb(42, 42, 42);
    style.visuals.selection.bg_fill = Color32::from_rgb(69, 69, 69);
    style.visuals.window_corner_radius = CornerRadius::same(7);
    style.visuals.menu_corner_radius = CornerRadius::same(7);
    style.visuals.widgets.inactive.corner_radius = CornerRadius::same(5);
    style.visuals.widgets.hovered.corner_radius = CornerRadius::same(5);
    style.visuals.widgets.active.corner_radius = CornerRadius::same(5);
    style.visuals.widgets.inactive.bg_fill = Color32::from_rgb(42, 42, 42);
    style.visuals.widgets.inactive.weak_bg_fill = Color32::from_rgb(36, 36, 36);
    style.visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, Color32::from_rgb(55, 55, 55));
    style.visuals.widgets.hovered.bg_fill = Color32::from_rgb(51, 51, 51);
    style.visuals.widgets.hovered.weak_bg_fill = Color32::from_rgb(47, 47, 47);
    style.visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, Color32::from_rgb(77, 77, 77));
    style.visuals.widgets.active.bg_fill = Color32::from_rgb(61, 61, 61);
    style.visuals.widgets.active.bg_stroke = Stroke::new(1.0, Color32::from_rgb(93, 93, 93));
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
