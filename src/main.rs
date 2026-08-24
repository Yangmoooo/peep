use std::process::ExitCode;

use clap::Parser;
use peep::app::App;
use peep::cli::Cli;
use peep::state::StateStore;
use peep::terminal;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("peep: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let store = StateStore::for_current_user()?;
    let initial_file = cli.file.clone().or_else(|| store.last_opened());
    let cwd = std::env::current_dir()?;
    let mut app = App::new(cwd, store);
    if let Some(theme) = cli.theme {
        app.override_theme(theme);
    }
    if let Some(path) = initial_file {
        app.open_path(path);
    }
    terminal::run(&mut app, !cli.no_mouse)?;
    Ok(())
}
