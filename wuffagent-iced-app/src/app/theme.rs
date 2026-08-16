use iced::theme::Palette;
use iced::Theme;

/// Map the application's theme name to an iced Theme.
///
/// Supports "dark" and "light" theme names.
pub fn iced_theme(name: &str) -> Theme {
    match name {
        "light" => Theme::Light,
        _ => Theme::Dark,
    }
}

/// Get the palette colors for the current theme name.
pub fn palette(name: &str) -> Palette {
    match name {
        "light" => Palette {
            background: iced::Color::from_rgb(0.95, 0.95, 0.95),
            text: iced::Color::from_rgb(0.1, 0.1, 0.1),
            primary: iced::Color::from_rgb(0.2, 0.5, 0.9),
            success: iced::Color::from_rgb(0.2, 0.7, 0.3),
            warning: iced::Color::from_rgb(0.9, 0.6, 0.1),
            danger: iced::Color::from_rgb(0.9, 0.2, 0.2),
        },
        _ => Palette {
            background: iced::Color::from_rgb(0.1, 0.1, 0.1),
            text: iced::Color::from_rgb(0.9, 0.9, 0.9),
            primary: iced::Color::from_rgb(0.3, 0.6, 1.0),
            success: iced::Color::from_rgb(0.2, 0.8, 0.4),
            warning: iced::Color::from_rgb(1.0, 0.7, 0.2),
            danger: iced::Color::from_rgb(1.0, 0.3, 0.3),
        },
    }
}
