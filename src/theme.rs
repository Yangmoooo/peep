use clap::ValueEnum;
use ratatui::style::{Color, Modifier, Style};
use serde::{Deserialize, Serialize};

use crate::terminal_palette::DefaultColors;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum ThemeChoice {
    #[default]
    Auto,
    Light,
    Dark,
}

impl std::fmt::Display for ThemeChoice {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Auto => "auto",
            Self::Light => "light",
            Self::Dark => "dark",
        })
    }
}

impl std::str::FromStr for ThemeChoice {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "auto" => Ok(Self::Auto),
            "light" => Ok(Self::Light),
            "dark" => Ok(Self::Dark),
            _ => Err(()),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Theme {
    pub(crate) body: Style,
    pub(crate) surface: Style,
    pub(crate) muted: Style,
    pub(crate) accent: Style,
    pub(crate) heading: Style,
    pub(crate) border: Style,
    pub(crate) selected: Style,
    pub(crate) matched: Style,
    pub(crate) surface_visible: bool,
}

impl Theme {
    pub(crate) fn resolve(choice: ThemeChoice, terminal: Option<DefaultColors>) -> Self {
        match choice {
            ThemeChoice::Auto => terminal.map_or_else(Self::fallback, Self::from_default_colors),
            ThemeChoice::Light => Self::from_colors((31, 35, 40), (255, 255, 255)),
            ThemeChoice::Dark => Self::from_colors((230, 237, 243), (13, 17, 23)),
        }
    }

    fn from_default_colors(colors: DefaultColors) -> Self {
        Self::from_colors(colors.foreground, colors.background)
    }

    fn from_colors(foreground: Rgb, background: Rgb) -> Self {
        let light = is_light(background);
        let surface = if light {
            blend((0, 0, 0), background, 0.04)
        } else {
            blend((255, 255, 255), background, 0.12)
        };
        let muted = blend(foreground, background, 0.55);
        let accent = if light { (0, 95, 135) } else { (86, 156, 214) };
        let matched = if light { (255, 214, 64) } else { (137, 101, 0) };
        Self {
            body: Style::default().fg(rgb(foreground)).bg(rgb(background)),
            surface: Style::default().fg(rgb(foreground)).bg(rgb(surface)),
            muted: Style::default().fg(rgb(muted)),
            accent: Style::default().fg(rgb(accent)),
            heading: Style::default().fg(rgb(accent)).add_modifier(Modifier::BOLD),
            border: Style::default().fg(rgb(muted)).bg(rgb(surface)),
            selected: Style::default()
                .fg(contrast_text(accent))
                .bg(rgb(accent))
                .add_modifier(Modifier::BOLD),
            matched: Style::default()
                .fg(contrast_text(matched))
                .bg(rgb(matched))
                .add_modifier(Modifier::BOLD),
            surface_visible: true,
        }
    }

    fn fallback() -> Self {
        Self {
            body: Style::default(),
            surface: Style::default(),
            muted: Style::default().fg(Color::DarkGray),
            accent: Style::default().fg(Color::Cyan),
            heading: Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            border: Style::default().fg(Color::DarkGray),
            selected: Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD),
            matched: Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD),
            surface_visible: false,
        }
    }
}

type Rgb = (u8, u8, u8);

fn is_light((red, green, blue): Rgb) -> bool {
    299 * u32::from(red) + 587 * u32::from(green) + 114 * u32::from(blue) > 128_000
}

fn contrast_text(background: Rgb) -> Color {
    if is_light(background) { Color::Black } else { Color::White }
}

fn blend(top: Rgb, bottom: Rgb, alpha: f32) -> Rgb {
    let channel =
        |top: u8, bottom: u8| (f32::from(top) * alpha + f32::from(bottom) * (1.0 - alpha)) as u8;
    (channel(top.0, bottom.0), channel(top.1, bottom.1), channel(top.2, bottom.2))
}

fn rgb((red, green, blue): Rgb) -> Color { Color::Rgb(red, green, blue) }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_derives_a_low_contrast_surface_for_light_and_dark_terminals() {
        let light = Theme::resolve(
            ThemeChoice::Auto,
            Some(DefaultColors { foreground: (0, 0, 0), background: (255, 255, 255) }),
        );
        assert_eq!(light.surface.bg, Some(Color::Rgb(244, 244, 244)));
        assert!(light.surface_visible);

        let dark = Theme::resolve(
            ThemeChoice::Auto,
            Some(DefaultColors { foreground: (255, 255, 255), background: (0, 0, 0) }),
        );
        assert_eq!(dark.surface.bg, Some(Color::Rgb(30, 30, 30)));
        assert!(dark.surface_visible);
    }

    #[test]
    fn auto_fallback_does_not_assume_a_terminal_background() {
        let theme = Theme::resolve(ThemeChoice::Auto, None);
        assert_eq!(theme.body.bg, None);
        assert_eq!(theme.surface.bg, None);
        assert!(!theme.surface_visible);
    }

    #[test]
    fn explicit_themes_have_readable_selection_contrast() {
        let light = Theme::resolve(ThemeChoice::Light, None);
        let dark = Theme::resolve(ThemeChoice::Dark, None);
        assert_ne!(light.selected.fg, light.selected.bg);
        assert_ne!(dark.selected.fg, dark.selected.bg);
        assert!(light.surface_visible && dark.surface_visible);
    }
}
