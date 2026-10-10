use eframe::egui::{
    self, Color32, CornerRadius, FontFamily, FontId, Stroke, TextStyle, Theme, ThemePreference,
    Visuals,
};

pub const SIDEBAR_BG: Color32 = Color32::from_rgb(20, 22, 26);
pub const CANVAS_BG: Color32 = Color32::from_rgb(34, 36, 41);
pub const PANEL_BG: Color32 = Color32::from_rgb(27, 29, 34);
pub const ACCENT: Color32 = Color32::from_rgb(120, 119, 198);
pub const MUTED: Color32 = Color32::from_rgb(155, 159, 170);

pub fn apply(ctx: &egui::Context) {
    ctx.set_theme(ThemePreference::Dark);
    let mut style = (*ctx.style_of(Theme::Dark)).clone();
    style.spacing.item_spacing = egui::vec2(8.0, 7.0);
    style.spacing.button_padding = egui::vec2(10.0, 6.0);
    style.visuals = Visuals::dark();
    style.visuals.panel_fill = PANEL_BG;
    style.visuals.window_fill = PANEL_BG;
    style.visuals.extreme_bg_color = Color32::from_rgb(16, 18, 21);
    style.visuals.faint_bg_color = Color32::from_rgb(38, 40, 46);
    style.visuals.selection.bg_fill = ACCENT.gamma_multiply(0.65);
    style.visuals.widgets.inactive.corner_radius = CornerRadius::same(6);
    style.visuals.widgets.hovered.corner_radius = CornerRadius::same(6);
    style.visuals.widgets.active.corner_radius = CornerRadius::same(6);
    style.visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, Color32::from_rgb(56, 59, 68));
    style.visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, ACCENT);
    style.text_styles.insert(
        TextStyle::Heading,
        FontId::new(20.0, FontFamily::Proportional),
    );
    style
        .text_styles
        .insert(TextStyle::Body, FontId::new(14.0, FontFamily::Proportional));
    ctx.set_style_of(Theme::Dark, style);
}
