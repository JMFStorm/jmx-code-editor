//! Dark theme. There is intentionally no light mode.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgba(pub u8, pub u8, pub u8, pub u8);

const fn hex(v: u32) -> Rgba {
    Rgba((v >> 16) as u8, (v >> 8) as u8, v as u8, 255)
}

const fn hexa(v: u32, a: u8) -> Rgba {
    Rgba((v >> 16) as u8, (v >> 8) as u8, v as u8, a)
}

impl Rgba {
    /// Color as linear floats, for rendering into an sRGB surface.
    pub fn linear(self) -> [f32; 4] {
        fn lin(c: u8) -> f32 {
            let c = c as f32 / 255.0;
            if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        }
        [lin(self.0), lin(self.1), lin(self.2), self.3 as f32 / 255.0]
    }

    /// Color as plain sRGB floats, for non-sRGB surfaces.
    pub fn srgb(self) -> [f32; 4] {
        [
            self.0 as f32 / 255.0,
            self.1 as f32 / 255.0,
            self.2 as f32 / 255.0,
            self.3 as f32 / 255.0,
        ]
    }

    pub fn glyphon(self) -> glyphon::Color {
        glyphon::Color::rgba(self.0, self.1, self.2, self.3)
    }
}

pub struct Theme {
    pub bg: Rgba,
    pub fg: Rgba,
    pub fg_dim: Rgba,
    pub gutter_bg: Rgba,
    pub gutter_fg: Rgba,
    pub gutter_fg_active: Rgba,
    pub current_line: Rgba,
    pub selection: Rgba,
    pub find_match: Rgba,
    pub find_match_active: Rgba,
    pub cursor: Rgba,
    pub accent: Rgba,
    pub accent_fg: Rgba,
    pub tabbar_bg: Rgba,
    pub tab_active_bg: Rgba,
    pub tab_fg: Rgba,
    pub tab_fg_active: Rgba,
    pub statusbar_bg: Rgba,
    pub statusbar_fg: Rgba,
    pub minibuffer_bg: Rgba,
    pub prompt_fg: Rgba,
    pub warning: Rgba,
    pub error: Rgba,
    pub modified: Rgba,
    pub scrollbar: Rgba,
    pub border: Rgba,
    pub toolbar_bg: Rgba,
    pub button_hover: Rgba,
    pub button_fg: Rgba,
    pub overlay_bg: Rgba,
    pub overlay_border: Rgba,
    pub overlay_title_bg: Rgba,
    pub picker_dir: Rgba,
    pub picker_selected: Rgba,
    pub shadow: Rgba,
}

pub const DARK: Theme = Theme {
    bg: hex(0x14161b),
    fg: hex(0xd4d4d4),
    fg_dim: hex(0x7f848e),
    gutter_bg: hex(0x14161b),
    gutter_fg: hex(0x4b5263),
    gutter_fg_active: hex(0xc8ccd4),
    current_line: hex(0x1c1f26),
    selection: hex(0x264f78),
    find_match: hexa(0xe5c07b, 70),
    find_match_active: hexa(0xe5c07b, 150),
    cursor: hex(0x61afef),
    accent: hex(0x61afef),
    accent_fg: hex(0x0e1013),
    tabbar_bg: hex(0x0e1013),
    tab_active_bg: hex(0x14161b),
    tab_fg: hex(0x7f848e),
    tab_fg_active: hex(0xe6e6e6),
    statusbar_bg: hex(0x0e1013),
    statusbar_fg: hex(0xabb2bf),
    minibuffer_bg: hex(0x1a1d23),
    prompt_fg: hex(0x98c379),
    warning: hex(0xe5c07b),
    error: hex(0xe06c75),
    modified: hex(0xe5c07b),
    scrollbar: hexa(0x5c6370, 90),
    border: hex(0x262a33),
    toolbar_bg: hex(0x0b0d10),
    button_hover: hex(0x23272f),
    button_fg: hex(0xc8ccd4),
    overlay_bg: hex(0x1a1d23),
    overlay_border: hex(0x3a3f4b),
    overlay_title_bg: hex(0x111317),
    picker_dir: hex(0x61afef),
    picker_selected: hex(0x2c313c),
    shadow: hexa(0x000000, 110),
};
