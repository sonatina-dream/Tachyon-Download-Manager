use gpui::{Hsla, Window, WindowAppearance, rgb, rgba};

fn c(hex: u32) -> Hsla {
    rgb(hex).into()
}

fn ca(hex: u32) -> Hsla {
    rgba(hex).into()
}

/// Colours for one appearance. Looked up per render so windows follow the system's
/// light/dark setting live.
#[derive(Clone, Copy)]
pub struct Theme {
    /// Lists, inputs and dialog bodies.
    pub bg: Hsla,
    /// Sidebar and secondary panels.
    pub surface: Hsla,
    /// Toolbar and table header.
    pub chrome: Hsla,
    pub border: Hsla,
    pub border_strong: Hsla,
    pub text: Hsla,
    pub muted: Hsla,
    pub faint: Hsla,
    pub accent: Hsla,
    pub accent_hover: Hsla,
    pub accent_soft: Hsla,
    pub on_accent: Hsla,
    pub hover: Hsla,
    pub pressed: Hsla,
    pub row_alt: Hsla,
    pub success: Hsla,
    pub danger: Hsla,
    pub warning: Hsla,
    pub track: Hsla,
    pub button: Hsla,
    pub button_hover: Hsla,
}

impl Theme {
    pub fn light() -> Self {
        Self {
            bg: c(0xffffff),
            surface: c(0xf4f4f6),
            chrome: c(0xf7f7f9),
            border: c(0xe3e3e8),
            border_strong: c(0xc9c9d0),
            text: c(0x1d1d1f),
            muted: c(0x6b6b72),
            faint: c(0xa1a1a8),
            accent: c(0x1f6feb),
            accent_hover: c(0x1a5fd0),
            accent_soft: ca(0x1f6feb24),
            on_accent: c(0xffffff),
            hover: ca(0x0000000b),
            pressed: ca(0x00000016),
            row_alt: c(0xfafafb),
            success: c(0x1f9d55),
            danger: c(0xd93025),
            warning: c(0xc77700),
            track: c(0xe7e7ec),
            button: c(0xffffff),
            button_hover: c(0xf2f2f5),
        }
    }

    pub fn dark() -> Self {
        Self {
            bg: c(0x1c1c1e),
            surface: c(0x232326),
            chrome: c(0x2a2a2d),
            border: c(0x37373b),
            border_strong: c(0x4a4a50),
            text: c(0xf2f2f5),
            muted: c(0xa0a0a8),
            faint: c(0x6c6c74),
            accent: c(0x3b82f6),
            accent_hover: c(0x5593f7),
            accent_soft: ca(0x3b82f633),
            on_accent: c(0xffffff),
            hover: ca(0xffffff0f),
            pressed: ca(0xffffff1c),
            row_alt: c(0x202023),
            success: c(0x34c27a),
            danger: c(0xff5f57),
            warning: c(0xf5a524),
            track: c(0x38383d),
            button: c(0x323236),
            button_hover: c(0x3b3b40),
        }
    }

    pub fn for_appearance(appearance: WindowAppearance) -> Self {
        match appearance {
            WindowAppearance::Dark | WindowAppearance::VibrantDark => Self::dark(),
            WindowAppearance::Light | WindowAppearance::VibrantLight => Self::light(),
        }
    }

    pub fn of(window: &Window) -> Self {
        Self::for_appearance(window.appearance())
    }
}
