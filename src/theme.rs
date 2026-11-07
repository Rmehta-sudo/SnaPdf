//! Light and dark palettes. Interface code asks for colours through these
//! functions, so switching theme repaints everything with no restart. Most
//! UI greys are authored once for the dark theme and translated by
//! `ui_rgb`; surfaces with a specific role have explicit values.

use eframe::egui::{
    self, Color32, CornerRadius, FontFamily, FontId, Stroke, TextStyle, Theme, ThemePreference,
    Visuals,
};
use std::sync::atomic::{AtomicU8, Ordering};

/// 0 is dark, 1 is light.
static STYLE: AtomicU8 = AtomicU8::new(0);

/// The surfaces of one light style. Generic greys are mapped relative to
/// `toolbar`, and `tint` warms or cools every mapped grey slightly.
struct LightPalette {
    toolbar: [u8; 3],
    strip: [u8; 3],
    canvas: [u8; 3],
    sidebar: [u8; 3],
    address: [u8; 3],
    panel: [u8; 3],
    field: [u8; 3],
    tint: [i16; 3],
}

const LIGHT_STYLES: [LightPalette; 1] = [
    // Cool slate: soft blue-grey surfaces.
    LightPalette {
        toolbar: [234, 237, 241],
        strip: [219, 224, 230],
        canvas: [203, 209, 217],
        sidebar: [228, 232, 237],
        address: [223, 228, 234],
        panel: [231, 235, 239],
        field: [244, 246, 249],
        tint: [-3, 0, 4],
    },
];

pub fn is_light() -> bool {
    style() != 0
}

/// 0 for dark, 1 for light.
pub fn style() -> u8 {
    STYLE.load(Ordering::Relaxed)
}

fn light_palette() -> Option<&'static LightPalette> {
    LIGHT_STYLES.get(usize::from(style()).checked_sub(1)?)
}

fn rgb(channels: [u8; 3]) -> Color32 {
    Color32::from_rgb(channels[0], channels[1], channels[2])
}

fn pick(dark: Color32, light: impl Fn(&LightPalette) -> Color32) -> Color32 {
    light_palette().map_or(dark, light)
}

/// Where a dark-theme grey level lands in the light theme. Surfaces sit
/// around the toolbar's level (43 dark, 250 light) and move darker the
/// further they were from it, so hover and pressed states still read as
/// changes; text-range greys (above 110) invert to dark text.
fn light_level(level: u8, top: f32) -> u8 {
    let level = f32::from(level);
    let light = if level <= 110.0 {
        top - (level - 43.0).abs()
    } else {
        280.0 - 1.05 * level
    };
    light.clamp(18.0, 255.0) as u8
}

/// A colour authored for the dark theme, adapted to the current theme:
/// near-greys (including faintly tinted ones) are remapped, real colours
/// pass through unchanged.
pub fn adapt(color: Color32) -> Color32 {
    let Some(palette) = light_palette() else {
        return color;
    };
    let [r, g, b, a] = color.to_array();
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    if max - min > 16 {
        return color;
    }
    let average = ((u16::from(r) + u16::from(g) + u16::from(b)) / 3) as i16;
    let top = f32::from(palette.toolbar[1]);
    let level = i16::from(light_level(average as u8, top));
    // Only surfaces take the style's warmth; dark text stays neutral.
    let tinted = level > 150;
    let shift = |channel: u8, index: usize| {
        let tint = if tinted { palette.tint[index] } else { 0 };
        (level + i16::from(channel) - average + tint).clamp(0, 255) as u8
    };
    Color32::from_rgba_unmultiplied(shift(r, 0), shift(g, 1), shift(b, 2), a)
}

pub fn ui_rgb(r: u8, g: u8, b: u8) -> Color32 {
    adapt(Color32::from_rgb(r, g, b))
}

pub fn sidebar_bg() -> Color32 {
    pick(Color32::from_gray(38), |palette| rgb(palette.sidebar))
}

pub fn canvas_bg() -> Color32 {
    pick(Color32::from_gray(26), |palette| rgb(palette.canvas))
}

pub fn tab_strip_bg() -> Color32 {
    pick(Color32::from_gray(24), |palette| rgb(palette.strip))
}

pub fn toolbar_bg() -> Color32 {
    pick(Color32::from_gray(43), |palette| rgb(palette.toolbar))
}

pub fn address_bg() -> Color32 {
    pick(Color32::from_gray(31), |palette| rgb(palette.address))
}

pub fn panel_bg() -> Color32 {
    pick(Color32::from_gray(29), |palette| rgb(palette.panel))
}

/// Neutral fill for a selected sidebar row.
pub fn accent() -> Color32 {
    pick(Color32::from_gray(105), |_| Color32::from_gray(150))
}

/// The one colour that marks what is active: selected tool, focus, primary
/// buttons.
pub fn accent_blue() -> Color32 {
    pick(Color32::from_rgb(76, 141, 255), |_| {
        Color32::from_rgb(15, 108, 189)
    })
}

pub fn accent_blue_text() -> Color32 {
    pick(Color32::from_rgb(150, 192, 255), |_| {
        Color32::from_rgb(12, 92, 165)
    })
}

pub const ON_ACCENT: Color32 = Color32::WHITE;

/// Raised cards and text fields: the brightest surface of a style.
pub fn field_bg() -> Color32 {
    pick(Color32::from_gray(42), |palette| rgb(palette.field))
}

pub fn muted() -> Color32 {
    pick(Color32::from_gray(168), |_| Color32::from_gray(108))
}

/// Text on a selected or emphasised row: white on dark, near-black on light.
pub fn strong_text() -> Color32 {
    pick(Color32::WHITE, |_| Color32::from_gray(18))
}

pub fn success_text() -> Color32 {
    pick(Color32::from_rgb(120, 200, 140), |_| {
        Color32::from_rgb(22, 128, 61)
    })
}

pub fn error_text() -> Color32 {
    pick(Color32::from_rgb(235, 120, 110), |_| {
        Color32::from_rgb(190, 40, 30)
    })
}

pub fn warning_text() -> Color32 {
    pick(Color32::from_rgb(255, 196, 92), |_| {
        Color32::from_rgb(166, 98, 0)
    })
}

/// Switch palettes and restyle egui's own widgets to match.
pub fn apply(ctx: &egui::Context, palette_style: u8) {
    STYLE.store(
        palette_style.min(LIGHT_STYLES.len() as u8),
        Ordering::Relaxed,
    );
    let light = is_light();
    let theme = if light { Theme::Light } else { Theme::Dark };
    ctx.set_theme(if light {
        ThemePreference::Light
    } else {
        ThemePreference::Dark
    });
    let mut style = (*ctx.style_of(theme)).clone();
    style.spacing.item_spacing = egui::vec2(6.0, 6.0);
    style.spacing.button_padding = egui::vec2(9.0, 6.0);
    style.visuals = if light {
        Visuals::light()
    } else {
        Visuals::dark()
    };
    let gray = |level: u8| adapt(Color32::from_gray(level));
    style.visuals.panel_fill = panel_bg();
    style.visuals.window_fill = panel_bg();
    style.visuals.extreme_bg_color = pick(Color32::from_gray(23), |palette| rgb(palette.field));
    style.visuals.faint_bg_color = gray(42);
    style.visuals.selection.bg_fill = accent_blue().gamma_multiply(if light { 0.25 } else { 0.45 });
    style.visuals.selection.stroke = Stroke::new(1.0, accent_blue());
    style.visuals.hyperlink_color = accent_blue_text();
    style.visuals.window_corner_radius = CornerRadius::same(7);
    style.visuals.menu_corner_radius = CornerRadius::same(7);
    style.visuals.widgets.inactive.corner_radius = CornerRadius::same(5);
    style.visuals.widgets.hovered.corner_radius = CornerRadius::same(5);
    style.visuals.widgets.active.corner_radius = CornerRadius::same(5);
    style.visuals.widgets.inactive.bg_fill = gray(42);
    style.visuals.widgets.inactive.weak_bg_fill = gray(36);
    style.visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, gray(55));
    style.visuals.widgets.hovered.bg_fill = gray(51);
    style.visuals.widgets.hovered.weak_bg_fill = gray(47);
    style.visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, gray(77));
    style.visuals.widgets.active.bg_fill = gray(61);
    style.visuals.widgets.active.bg_stroke = Stroke::new(1.0, gray(93));
    if light {
        style.visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, gray(60));
        style.visuals.widgets.noninteractive.fg_stroke.color = Color32::from_gray(40);
        style.visuals.widgets.inactive.fg_stroke.color = Color32::from_gray(30);
        style.visuals.widgets.hovered.fg_stroke.color = Color32::from_gray(10);
    }
    style.scroll_animation =
        egui::style::ScrollAnimation::new(1_900.0, egui::Rangef::new(0.06, 0.18));
    style.text_styles.insert(
        TextStyle::Heading,
        FontId::new(20.0, FontFamily::Proportional),
    );
    style
        .text_styles
        .insert(TextStyle::Body, FontId::new(14.0, FontFamily::Proportional));
    // egui's 10 pt default for small text is hard to read in hints and labels.
    style.text_styles.insert(
        TextStyle::Small,
        FontId::new(11.5, FontFamily::Proportional),
    );
    ctx.set_style_of(theme, style);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn light_mapping_keeps_surfaces_light_and_text_dark() {
        // Toolbar, hover, and divider greys stay light and get darker in step.
        assert_eq!(light_level(43, 238.0), 238);
        assert!(light_level(54, 238.0) < light_level(43, 238.0));
        assert!(light_level(70, 238.0) < light_level(54, 238.0));
        // Text greys become dark text, brighter text darker.
        assert!(light_level(240, 238.0) < 40);
        assert!(light_level(200, 238.0) < light_level(170, 238.0));
    }
}
