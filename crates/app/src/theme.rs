//! The look: color tokens for the dark and light themes, fonts and sizes.
//! Values come from the design canvas's foundations board.

use std::sync::{Arc, LazyLock};

use eframe::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, FontTweak, Margin,
    Shadow, Stroke, Style, TextStyle, Theme, ThemePreference, Visuals, vec2,
};
use eframe::epaint::text::VariationCoords;
use nordpil_backend::settings::{Settings, TextSize};
use nordpil_query::Highlight;

/// Color tokens. Components use these, never raw colors.
pub struct Palette {
    /// Window background behind panels; the top bar.
    pub app: Color32,
    /// Sidebar and tab bar.
    pub side: Color32,
    /// Content area and editor ground.
    pub main: Color32,
    /// Cards, tables, dialogs.
    pub surface: Color32,
    /// Buttons, inputs, popups, table headers.
    pub raised: Color32,
    pub hover: Color32,
    /// Segmented control track and its active item.
    pub track: Color32,
    pub active: Color32,
    pub line: Color32,
    pub line_strong: Color32,
    pub text: Color32,
    pub muted: Color32,
    pub subtle: Color32,
    pub accent: Color32,
    /// Text on an accent fill.
    pub accent_ink: Color32,
    /// Selected rows and options.
    pub accent_soft: Color32,
    /// Text on `accent_soft`.
    pub accent_text: Color32,
    pub focus: Color32,
    pub danger: Color32,
    pub danger_soft: Color32,
    pub warn: Color32,
    pub scrim: Color32,
    pub shadow: Color32,
    pub syntax_key: Color32,
    pub syntax_string: Color32,
    pub syntax_number: Color32,
    pub syntax_constructor: Color32,
    pub syntax_operator: Color32,
}

const fn rgb(hex: u32) -> Color32 {
    Color32::from_rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

fn rgba(hex: u32, alpha: f32) -> Color32 {
    let [_, r, g, b] = hex.to_be_bytes();
    Color32::from_rgba_unmultiplied(r, g, b, (alpha * 255.0).round() as u8)
}

static DARK: LazyLock<Palette> = LazyLock::new(|| Palette {
    app: rgb(0x0e1115),
    side: rgb(0x11151a),
    main: rgb(0x161a20),
    surface: rgb(0x1b2028),
    raised: rgb(0x212731),
    hover: rgb(0x262d38),
    track: rgb(0x0f1318),
    active: rgb(0x2c3441),
    line: rgb(0x262c36),
    line_strong: rgb(0x343c4a),
    text: rgb(0xe5e9f0),
    muted: rgb(0xa7b0bf),
    subtle: rgb(0x828d9f),
    accent: rgb(0x88c0d0),
    accent_ink: rgb(0x0b1a20),
    accent_soft: rgba(0x88c0d0, 0.14),
    accent_text: rgb(0xa8d6e3),
    focus: rgba(0x88c0d0, 0.32),
    danger: rgb(0xef8a8a),
    danger_soft: rgba(0xef8a8a, 0.12),
    warn: rgb(0xebcb8b),
    scrim: rgba(0x06090d, 0.64),
    shadow: rgba(0x000000, 0.55),
    syntax_key: rgb(0x81a1c1),
    syntax_string: rgb(0xa3be8c),
    syntax_number: rgb(0xd08770),
    syntax_constructor: rgb(0x8fbcbb),
    syntax_operator: rgb(0xb48ead),
});

static LIGHT: LazyLock<Palette> = LazyLock::new(|| Palette {
    app: rgb(0xe8ecf1),
    side: rgb(0xf1f4f7),
    main: rgb(0xf8f9fb),
    surface: rgb(0xffffff),
    raised: rgb(0xeef1f5),
    hover: rgb(0xe7ebf0),
    track: rgb(0xe4e8ee),
    active: rgb(0xffffff),
    line: rgb(0xdfe4ea),
    line_strong: rgb(0xc9d1db),
    text: rgb(0x1c232e),
    muted: rgb(0x475266),
    subtle: rgb(0x5d6879),
    accent: rgb(0x2b7a91),
    accent_ink: rgb(0xffffff),
    accent_soft: rgba(0x2b7a91, 0.11),
    accent_text: rgb(0x1b6176),
    focus: rgba(0x2b7a91, 0.25),
    danger: rgb(0xc2414d),
    danger_soft: rgba(0xc2414d, 0.09),
    warn: rgb(0x9a6a12),
    scrim: rgba(0x1c232e, 0.32),
    shadow: rgba(0x182434, 0.16),
    syntax_key: rgb(0x3b6a9c),
    syntax_string: rgb(0x4c7a34),
    syntax_number: rgb(0xb4532a),
    syntax_constructor: rgb(0x1f7a78),
    syntax_operator: rgb(0x8a4b9e),
});

/// The palette of the theme `ui` is drawn in.
pub fn palette(ui: &egui::Ui) -> &'static Palette {
    of(ui.visuals())
}

pub fn of(visuals: &Visuals) -> &'static Palette {
    if visuals.dark_mode { &DARK } else { &LIGHT }
}

pub fn syntax(kind: Highlight, visuals: &Visuals) -> Color32 {
    let p = of(visuals);
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

/// Sets fonts and both themes' styles. Call once at startup.
pub fn install(ctx: &egui::Context) {
    install_fonts(ctx);
    ctx.style_mut_of(Theme::Dark, |style| apply(style, &DARK, true));
    ctx.style_mut_of(Theme::Light, |style| apply(style, &LIGHT, false));
}

/// Applies the theme and text size settings.
pub fn apply_settings(ctx: &egui::Context, settings: &Settings) {
    use nordpil_backend::settings::Theme as Setting;
    ctx.set_theme(match settings.theme {
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

const GEIST: &[u8] = include_bytes!("../assets/fonts/Geist.ttf");
const GEIST_MONO: &[u8] = include_bytes!("../assets/fonts/GeistMono.ttf");
const LUCIDE: &[u8] = include_bytes!("../assets/fonts/lucide.ttf");

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
