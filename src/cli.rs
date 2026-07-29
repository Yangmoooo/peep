use std::path::PathBuf;

use clap::Parser;

#[derive(Clone, Debug, Parser)]
#[command(name = "peep", version, about = "Read EPUB and TXT files in a quiet terminal interface")]
pub struct Cli {
    /// Do not capture mouse events; keeps native terminal text selection.
    #[arg(long)]
    pub no_mouse: bool,

    /// EPUB or TXT file to open. Reopens the most recent file when omitted.
    pub file: Option<PathBuf>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_no_mouse_and_unicode_path() {
        let cli = Cli::try_parse_from(["peep", "--no-mouse", "小说.epub"]).unwrap();
        assert!(cli.no_mouse);
        assert_eq!(cli.file, Some(PathBuf::from("小说.epub")));
    }
}
