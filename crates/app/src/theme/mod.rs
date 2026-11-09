//! The look: palettes for the dark and light themes, fonts and sizes.
//! Built-in values come from the design canvas's foundations board.

use std::sync::{Arc, LazyLock};

use eframe::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, FontTweak, Margin,
    Shadow, Stroke, Style, TextStyle, Theme, ThemePreference, Visuals, vec2,
};
use eframe::epaint::text::VariationCoords;
use nordpil_backend::settings::{Settings, TextSize};
use nordpil_query::Highlight;

mod palette;
mod resolve;

pub use palette::{Palette, nordic_dark, nordic_light};
pub use resolve::{Themes, builtin_name};

static DARK: LazyLock<Arc<Palette>> = LazyLock::new(|| Arc::new(nordic_dark().palette()));
static LIGHT: LazyLock<Arc<Palette>> = LazyLock::new(|| Arc::new(nordic_light().palette()));

fn palette_id(dark: bool) -> egui::Id {
    egui::Id::unique(("nordpil-palette", dark))
}

/// The palette of the theme `ui` is drawn in.
pub fn palette(ui: &egui::Ui) -> Arc<Palette> {
    of(ui.ctx(), ui.visuals().dark_mode)
}

/// The palette of the dark or the light theme.
pub fn of(ctx: &egui::Context, dark: bool) -> Arc<Palette> {
    ctx.data(|data| data.get_temp(palette_id(dark)))
        .unwrap_or_else(|| builtin(dark))
}

/// The palette of the theme the window shows now.
pub fn current(ctx: &egui::Context) -> Arc<Palette> {
    of(ctx, ctx.global_style().visuals.dark_mode)
}

/// A built-in Nordic frost palette.
pub fn builtin(dark: bool) -> Arc<Palette> {
    Arc::clone(if dark { &DARK } else { &LIGHT })
}

pub fn syntax(kind: Highlight, p: &Palette) -> Color32 {
    match kind {
        Highlight::Punctuation => p.subtle,
        Highlight::Comment => p.subtle,
        Highlight::Error => p.danger,
        Highlight::Key => p.syntax_key,
        Highlight::String | Highlight::Regex => p.syntax_string,
        Highlight::Number | Highlight::Keyword => p.syntax_number,
        Highlight::Operator => p.syntax_operator,
        Highlight::Function => p.syntax_constructor,
    }
}

/// Text in the medium (500) weight.
pub fn medium() -> FontFamily {
    FontFamily::Name("medium".into())
}

/// Text in the semibold (600) weight.
pub fn semibold() -> FontFamily {
    FontFamily::Name("semibold".into())
}

/// Dialog titles.
pub fn title_style() -> TextStyle {
    TextStyle::Name("title".into())
}

/// Uppercase section labels such as "Connections".
pub fn section_style() -> TextStyle {
    TextStyle::Name("section".into())
}

/// Sets fonts and the built-in palettes. Call once at startup.
pub fn install(ctx: &egui::Context) {
    install_fonts(ctx);
    set_palettes(ctx, builtin(true), builtin(false));
}

/// Paints the dark and the light theme with these palettes from now on.
pub fn set_palettes(ctx: &egui::Context, dark: Arc<Palette>, light: Arc<Palette>) {
    ctx.style_mut_of(Theme::Dark, |style| apply(style, &dark, true));
    ctx.style_mut_of(Theme::Light, |style| apply(style, &light, false));
    ctx.data_mut(|data| {
        data.insert_temp(palette_id(true), dark);
        data.insert_temp(palette_id(false), light);
    });
}

/// Applies the theme mode and text size settings. Palettes are set apart,
/// with [`set_palettes`].
pub fn apply_mode(ctx: &egui::Context, settings: &Settings) {
    use nordpil_backend::settings::ThemeMode as Setting;
    ctx.set_theme(match settings.themes.mode {
        Setting::System => ThemePreference::System,
        Setting::Dark => ThemePreference::Dark,
        Setting::Light => ThemePreference::Light,
    });
    ctx.set_zoom_factor(zoom(settings.text_size));
}

/// Body text of 12.5, 14 or 15 points.
pub fn zoom(size: TextSize) -> f32 {
    match size {
        TextSize::Compact => 0.9,
        TextSize::Comfortable => 1.0,
        TextSize::Large => 1.08,
    }
}

const GEIST: &[u8] = include_bytes!("../../assets/fonts/Geist.ttf");
const GEIST_MONO: &[u8] = include_bytes!("../../assets/fonts/GeistMono.ttf");
const LUCIDE: &[u8] = include_bytes!("../../assets/fonts/lucide.ttf");

fn install_fonts(ctx: &egui::Context) {
    let weight = |font: &'static [u8], weight: f32| {
        Arc::new(FontData::from_static(font).tweak(FontTweak {
            coords: VariationCoords::new([("wght", weight)]),
            ..FontTweak::default()
        }))
    };
    let mut fonts = FontDefinitions::default();
    for (name, data) in [
        ("geist", weight(GEIST, 400.0)),
        ("geist-medium", weight(GEIST, 500.0)),
        ("geist-semibold", weight(GEIST, 600.0)),
        ("geist-mono", weight(GEIST_MONO, 400.0)),
        (
            "lucide",
            Arc::new(FontData::from_static(LUCIDE).tweak(FontTweak {
                scale: 1.1,
                y_offset_factor: 0.08,
                ..FontTweak::default()
            })),
        ),
    ] {
        fonts.font_data.insert(name.to_owned(), data);
    }
    // egui's own fonts stay as fallbacks for other scripts and symbols.
    let fallbacks = fonts
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    let family = |first: &str| {
        let mut list = vec![first.to_owned(), "lucide".to_owned()];
        list.extend(fallbacks.iter().cloned());
        list
    };
    fonts
        .families
        .insert(FontFamily::Proportional, family("geist"));
    fonts.families.insert(medium(), family("geist-medium"));
    fonts.families.insert(semibold(), family("geist-semibold"));
    let mono_fallbacks = fonts
        .families
        .get(&FontFamily::Monospace)
        .cloned()
        .unwrap_or_default();
    let mut mono = vec!["geist-mono".to_owned(), "lucide".to_owned()];
    mono.extend(mono_fallbacks);
    fonts.families.insert(FontFamily::Monospace, mono);
    ctx.set_fonts(fonts);
}

fn apply(style: &mut Style, p: &Palette, dark: bool) {
    style.text_styles = [
        (TextStyle::Heading, FontId::new(21.0, semibold())),
        (TextStyle::Body, FontId::proportional(14.0)),
        (TextStyle::Button, FontId::proportional(14.0)),
        (TextStyle::Monospace, FontId::monospace(13.5)),
        (TextStyle::Small, FontId::proportional(12.5)),
        (title_style(), FontId::new(18.0, semibold())),
        (section_style(), FontId::new(11.5, medium())),
    ]
    .into();

    let spacing = &mut style.spacing;
    spacing.item_spacing = vec2(8.0, 6.0);
    spacing.button_padding = vec2(12.0, 5.0);
    spacing.interact_size = vec2(36.0, 30.0);
    spacing.window_margin = Margin::same(22);
    spacing.menu_margin = Margin::same(6);
    spacing.indent = 18.0;
    spacing.icon_width = 18.0;
    spacing.icon_width_inner = 10.0;
    spacing.icon_spacing = 8.0;
    spacing.combo_height = 320.0;
    spacing.scroll.bar_width = 8.0;

    style.visuals = visuals(p, dark);
}

fn visuals(p: &Palette, dark: bool) -> Visuals {
    let mut v = if dark {
        Visuals::dark()
    } else {
        Visuals::light()
    };
    let radius = CornerRadius::same(8);
    v.override_text_color = None;
    v.weak_text_color = Some(p.subtle);
    v.hyperlink_color = p.accent;
    v.faint_bg_color = p.raised;
    v.extreme_bg_color = p.main;
    v.text_edit_bg_color = Some(p.surface);
    v.code_bg_color = p.raised;
    v.warn_fg_color = p.warn;
    v.error_fg_color = p.danger;
    v.panel_fill = p.main;
    v.window_fill = p.surface;
    v.window_stroke = Stroke::new(1.0, p.line_strong);
    v.window_corner_radius = CornerRadius::same(14);
    v.menu_corner_radius = CornerRadius::same(10);
    v.window_shadow = Shadow {
        offset: [0, 16],
        blur: 44,
        spread: 0,
        color: p.shadow,
    };
    v.popup_shadow = Shadow {
        offset: [0, 8],
        blur: 24,
        spread: 0,
        color: p.shadow,
    };
    v.selection.bg_fill = p.accent_soft;
    v.selection.stroke = Stroke::new(1.0, p.accent_text);
    v.text_cursor.stroke = Stroke::new(2.0, p.accent);
    v.collapsing_header_frame = false;
    v.indent_has_left_vline = true;
    v.striped = false;
    v.button_frame = true;

    let w = &mut v.widgets;
    w.noninteractive.bg_fill = p.surface;
    w.noninteractive.weak_bg_fill = p.surface;
    w.noninteractive.bg_stroke = Stroke::new(1.0, p.line);
    w.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
    w.noninteractive.corner_radius = radius;

    w.inactive.bg_fill = p.raised;
    w.inactive.weak_bg_fill = p.raised;
    w.inactive.bg_stroke = Stroke::new(1.0, p.line);
    w.inactive.fg_stroke = Stroke::new(1.0, p.text);
    w.inactive.corner_radius = radius;
    w.inactive.expansion = 0.0;

    w.hovered.bg_fill = p.hover;
    w.hovered.weak_bg_fill = p.hover;
    w.hovered.bg_stroke = Stroke::new(1.0, p.line_strong);
    w.hovered.fg_stroke = Stroke::new(1.0, p.text);
    w.hovered.corner_radius = radius;
    w.hovered.expansion = 0.0;

    w.active.bg_fill = p.active;
    w.active.weak_bg_fill = p.active;
    w.active.bg_stroke = Stroke::new(1.0, p.accent);
    w.active.fg_stroke = Stroke::new(1.0, p.text);
    w.active.corner_radius = radius;
    w.active.expansion = 0.0;

    w.open.bg_fill = p.hover;
    w.open.weak_bg_fill = p.hover;
    w.open.bg_stroke = Stroke::new(1.0, p.line_strong);
    w.open.fg_stroke = Stroke::new(1.0, p.text);
    w.open.corner_radius = radius;
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_in_hover_tints_match_the_old_fixed_tints() {
        assert_eq!(builtin(true).hover_tint, Color32::from_white_alpha(22));
        assert_eq!(builtin(false).hover_tint, Color32::from_black_alpha(18));
    }

    #[test]
    fn palettes_set_on_a_context_are_read_back_by_theme() {
        let ctx = egui::Context::default();
        assert_eq!(*of(&ctx, true), *builtin(true));
        let mut light = *builtin(false);
        light.accent = Color32::RED;
        set_palettes(&ctx, builtin(true), Arc::new(light));
        assert_eq!(of(&ctx, false).accent, Color32::RED);
        assert_eq!(*of(&ctx, true), *builtin(true));
    }
}
