use ratatui::style::Color;

#[derive(Clone, Copy, Debug)]
pub struct Palette {
    pub background: Color,
    pub panel_bg: Color,
    pub panel_focused_bg: Color,
    pub panel_selected_bg: Color,
    pub border_idle: Color,
    pub border_focused: Color,
    pub text_primary: Color,
    pub text_secondary: Color,
    pub accent: Color,
    pub success: Color,
    pub error: Color,
    pub comment_depth: [Color; 6],
}

impl Palette {
    pub fn terminal_default() -> Self {
        Self {
            background: Color::Reset,
            panel_bg: Color::Reset,
            panel_focused_bg: Color::DarkGray,
            panel_selected_bg: Color::DarkGray,
            border_idle: Color::DarkGray,
            border_focused: Color::LightBlue,
            text_primary: Color::White,
            text_secondary: Color::Gray,
            accent: Color::LightBlue,
            success: Color::LightGreen,
            error: Color::LightRed,
            comment_depth: [
                Color::LightRed,
                Color::LightGreen,
                Color::LightMagenta,
                Color::LightYellow,
                Color::LightCyan,
                Color::LightBlue,
            ],
        }
    }
}

pub fn palette_for(name: &str) -> Palette {
    let trimmed = name.trim();
    if !trimmed.is_empty() && !trimmed.eq_ignore_ascii_case("default") {
        eprintln!(
            "Theme \"{}\" is not supported yet; falling back to terminal colors.",
            trimmed
        );
    }
    Palette::terminal_default()
}
