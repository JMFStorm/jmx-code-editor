/// Editor settings. Hardcoded defaults for now; a TOML config file comes later.
#[derive(Debug, Clone)]
pub struct Config {
    /// Font size in logical pixels.
    pub font_size: f32,
    pub default_font_size: f32,
    /// Line height as a multiple of the font size.
    pub line_height: f32,
    /// Width of a tab stop in columns.
    pub tab_width: usize,
    pub line_numbers: bool,
    pub cursor_blink_ms: u64,
    /// Preferred monospace fonts, first installed one wins.
    pub font_families: Vec<&'static str>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            font_size: 15.0,
            default_font_size: 15.0,
            line_height: 1.4,
            tab_width: 4,
            line_numbers: true,
            cursor_blink_ms: 530,
            font_families: vec![
                "JetBrains Mono",
                "Cascadia Code",
                "Cascadia Mono",
                "Fira Code",
                "Source Code Pro",
                "Hack",
                "Consolas",
                "Menlo",
                "Ubuntu Mono",
                "DejaVu Sans Mono",
                "Liberation Mono",
                "Noto Sans Mono",
            ],
        }
    }
}

pub const MIN_FONT_SIZE: f32 = 8.0;
pub const MAX_FONT_SIZE: f32 = 48.0;
