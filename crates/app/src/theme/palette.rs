//! Color tokens and the two built-in Nordic frost themes.

use eframe::egui::Color32;

/// A color as written in theme files: sRGB, alpha not premultiplied.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Rgba(pub [u8; 4]);

impl Rgba {
    pub const fn rgb(hex: u32) -> Self {
        Self([(hex >> 16) as u8, (hex >> 8) as u8, hex as u8, 255])
    }

    /// `alpha` from 0 to 1.
    pub fn translucent(hex: u32, alpha: f32) -> Self {
        Self::rgb(hex).with_alpha((alpha * 255.0).round() as u8)
    }

    pub fn with_alpha(self, alpha: u8) -> Self {
        let [r, g, b, _] = self.0;
        Self([r, g, b, alpha])
    }

    pub fn color32(self) -> Color32 {
        let [r, g, b, a] = self.0;
        Color32::from_rgba_unmultiplied(r, g, b, a)
    }
}

macro_rules! tokens {
    ($( $(#[$meta:meta])* $field:ident: $group:ident $(from $parent:ident)?, )*) => {
        /// Every color token. Components use these, never raw colors.
        #[derive(Debug, Clone, Copy, Default, PartialEq)]
        pub struct Tokens<C> {
            $( $(#[$meta])* pub $field: C, )*
        }

        impl<C: Copy> Tokens<C> {
            pub fn map<D>(&self, f: impl Fn(C) -> D) -> Tokens<D> {
                Tokens { $( $field: f(self.$field), )* }
            }
        }
    };
}

tokens! {
    /// Window background behind panels; the top bar.
    app: Surfaces,
    /// Sidebar and tab bar.
    side: Surfaces,
    /// Content area and editor ground.
    main: Surfaces,
    /// Cards, tables, dialogs.
    surface: Surfaces,
    /// Buttons, inputs, popups, table headers.
    raised: Surfaces,
    hover: Surfaces,
    /// Laid over icon buttons and rows under the pointer.
    hover_tint: Surfaces,
    /// Segmented control track and its active item.
    track: Surfaces,
    active: Surfaces,
    line: Surfaces,
    line_strong: Surfaces,
    scrim: Surfaces,
    shadow: Surfaces,
    text: Text,
    muted: Text,
    subtle: Text,
    accent: Accent,
    /// Text on an accent fill.
    accent_ink: Accent,
    /// Selected rows and options.
    accent_soft: Accent from accent,
    /// Text on `accent_soft`.
    accent_text: Accent,
    focus: Accent from accent,
    danger: Status,
    danger_soft: Status from danger,
    warn: Status,
    syntax_key: Syntax,
    syntax_string: Syntax,
    syntax_number: Syntax,
    syntax_constructor: Syntax,
    syntax_operator: Syntax,
    conn_red: Connections,
    conn_orange: Connections,
    conn_yellow: Connections,
    conn_green: Connections,
    conn_teal: Connections,
    conn_blue: Connections,
    conn_purple: Connections,
    conn_pink: Connections,
}

/// A theme's colors as written in files.
pub type Colors = Tokens<Rgba>;

/// A theme's colors, ready to paint with.
pub type Palette = Tokens<Color32>;

impl Colors {
    pub fn palette(&self) -> Palette {
        self.map(Rgba::color32)
    }
}

/// Connection colors look the same in both built-in themes.
fn connections(colors: Colors) -> Colors {
    Colors {
        conn_red: Rgba::rgb(0xe5636f),
        conn_orange: Rgba::rgb(0xe8964a),
        conn_yellow: Rgba::rgb(0xe3c15a),
        conn_green: Rgba::rgb(0x7fbf72),
        conn_teal: Rgba::rgb(0x4fb8ad),
        conn_blue: Rgba::rgb(0x5b9be0),
        conn_purple: Rgba::rgb(0xa184d6),
        conn_pink: Rgba::rgb(0xdf79b4),
        ..colors
    }
}

pub fn nordic_dark() -> Colors {
    connections(Colors {
        app: Rgba::rgb(0x0e1115),
        side: Rgba::rgb(0x11151a),
        main: Rgba::rgb(0x161a20),
        surface: Rgba::rgb(0x1b2028),
        raised: Rgba::rgb(0x212731),
        hover: Rgba::rgb(0x262d38),
        hover_tint: Rgba([255, 255, 255, 22]),
        track: Rgba::rgb(0x0f1318),
        active: Rgba::rgb(0x2c3441),
        line: Rgba::rgb(0x262c36),
        line_strong: Rgba::rgb(0x343c4a),
        scrim: Rgba::translucent(0x06090d, 0.64),
        shadow: Rgba::translucent(0x000000, 0.55),
        text: Rgba::rgb(0xe5e9f0),
        muted: Rgba::rgb(0xa7b0bf),
        subtle: Rgba::rgb(0x828d9f),
        accent: Rgba::rgb(0x88c0d0),
        accent_ink: Rgba::rgb(0x0b1a20),
        accent_soft: Rgba::translucent(0x88c0d0, 0.14),
        accent_text: Rgba::rgb(0xa8d6e3),
        focus: Rgba::translucent(0x88c0d0, 0.32),
        danger: Rgba::rgb(0xef8a8a),
        danger_soft: Rgba::translucent(0xef8a8a, 0.12),
        warn: Rgba::rgb(0xebcb8b),
        syntax_key: Rgba::rgb(0x81a1c1),
        syntax_string: Rgba::rgb(0xa3be8c),
        syntax_number: Rgba::rgb(0xd08770),
        syntax_constructor: Rgba::rgb(0x8fbcbb),
        syntax_operator: Rgba::rgb(0xb48ead),
        ..Colors::default()
    })
}

pub fn nordic_light() -> Colors {
    connections(Colors {
        app: Rgba::rgb(0xe8ecf1),
        side: Rgba::rgb(0xf1f4f7),
        main: Rgba::rgb(0xf8f9fb),
        surface: Rgba::rgb(0xffffff),
        raised: Rgba::rgb(0xeef1f5),
        hover: Rgba::rgb(0xe7ebf0),
        hover_tint: Rgba([0, 0, 0, 18]),
        track: Rgba::rgb(0xe4e8ee),
        active: Rgba::rgb(0xffffff),
        line: Rgba::rgb(0xdfe4ea),
        line_strong: Rgba::rgb(0xc9d1db),
        scrim: Rgba::translucent(0x1c232e, 0.32),
        shadow: Rgba::translucent(0x182434, 0.16),
        text: Rgba::rgb(0x1c232e),
        muted: Rgba::rgb(0x475266),
        subtle: Rgba::rgb(0x5d6879),
        accent: Rgba::rgb(0x2b7a91),
        accent_ink: Rgba::rgb(0xffffff),
        accent_soft: Rgba::translucent(0x2b7a91, 0.11),
        accent_text: Rgba::rgb(0x1b6176),
        focus: Rgba::translucent(0x2b7a91, 0.25),
        danger: Rgba::rgb(0xc2414d),
        danger_soft: Rgba::translucent(0xc2414d, 0.09),
        warn: Rgba::rgb(0x9a6a12),
        syntax_key: Rgba::rgb(0x3b6a9c),
        syntax_string: Rgba::rgb(0x4c7a34),
        syntax_number: Rgba::rgb(0xb4532a),
        syntax_constructor: Rgba::rgb(0x1f7a78),
        syntax_operator: Rgba::rgb(0x8a4b9e),
        ..Colors::default()
    })
}
