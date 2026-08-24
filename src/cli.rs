use std::path::PathBuf;

use clap::Parser;

use crate::theme::ThemeChoice;

#[derive(Clone, Debug, Parser)]
#[command(
    name = "peep",
    version,
    about = "Read EPUB, TXT, and Markdown files in a quiet terminal interface"
)]
pub struct Cli {
    /// Do not capture mouse events; keeps native terminal text selection.
    #[arg(long)]
    pub no_mouse: bool,

    /// Override the saved color theme for this run.
    #[arg(long, value_enum)]
    pub theme: Option<ThemeChoice>,

    /// EPUB, TXT, Markdown file, or directory to open. Reopens the most recent
    /// file when omitted.
    pub file: Option<PathBuf>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_no_mouse_and_unicode_path() {
        let cli = Cli::try_parse_from(["peep", "--no-mouse", "小说.epub"]).unwrap();
        assert!(cli.no_mouse);
        assert_eq!(cli.theme, None);
        assert_eq!(cli.file, Some(PathBuf::from("小说.epub")));
    }

    #[test]
    fn accepts_an_explicit_theme_override() {
        let cli = Cli::try_parse_from(["peep", "--theme", "light"]).unwrap();
        assert_eq!(cli.theme, Some(ThemeChoice::Light));
    }
}
