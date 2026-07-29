use std::io;
use std::time::Duration;

use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    BeginSynchronizedUpdate, EndSynchronizedUpdate, EnterAlternateScreen, LeaveAlternateScreen,
    disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use crate::app::App;
use crate::{terminal_palette, ui};

const EVENT_POLL: Duration = Duration::from_millis(50);
const PALETTE_PROBE_TIMEOUT: Duration = Duration::from_millis(100);

pub fn run(app: &mut App, capture_mouse: bool) -> io::Result<()> {
    let mut session = TerminalSession::enter(capture_mouse)?;
    let result = run_loop(&mut session.terminal, app, capture_mouse);
    app.shutdown();
    result
}

fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
    capture_mouse: bool,
) -> io::Result<()> {
    while !app.should_quit() {
        app.poll_tasks();
        draw_frame(terminal, app)?;

        if !event::poll(EVENT_POLL)? {
            continue;
        }
        match event::read()? {
            Event::Key(key) if accepts_key_kind(key.kind) => app.handle_key(key),
            Event::Mouse(mouse) if capture_mouse => match mouse.kind {
                MouseEventKind::ScrollDown => app.scroll_mouse(3),
                MouseEventKind::ScrollUp => app.scroll_mouse(-3),
                _ => {}
            },
            Event::Resize(..) => {}
            Event::Paste(text) if app.input_mode() != crate::app::InputMode::Normal => {
                for character in text.chars() {
                    app.handle_key(crossterm::event::KeyEvent::new(
                        crossterm::event::KeyCode::Char(character),
                        crossterm::event::KeyModifiers::NONE,
                    ));
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn draw_frame(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
) -> io::Result<()> {
    execute!(terminal.backend_mut(), BeginSynchronizedUpdate)?;
    let draw_result = terminal.draw(|frame| ui::render(frame, app)).map(|_| ());
    let end_result = execute!(terminal.backend_mut(), EndSynchronizedUpdate);
    draw_result?;
    end_result
}

fn accepts_key_kind(kind: KeyEventKind) -> bool {
    matches!(kind, KeyEventKind::Press | KeyEventKind::Repeat)
}

struct TerminalSession {
    terminal: Terminal<CrosstermBackend<io::Stdout>>,
    capture_mouse: bool,
}

impl TerminalSession {
    fn enter(capture_mouse: bool) -> io::Result<Self> {
        enable_raw_mode()?;
        terminal_palette::set_default_colors(terminal_palette::detect_default_colors(
            PALETTE_PROBE_TIMEOUT,
        ));
        let mut stdout = io::stdout();
        let enter_result = if capture_mouse {
            execute!(stdout, EnterAlternateScreen, EnableMouseCapture)
        } else {
            execute!(stdout, EnterAlternateScreen)
        };
        if let Err(error) = enter_result {
            let _ = disable_raw_mode();
            return Err(error);
        }

        match Terminal::new(CrosstermBackend::new(stdout)) {
            Ok(mut terminal) => {
                terminal.clear()?;
                Ok(Self { terminal, capture_mouse })
            }
            Err(error) => {
                let mut stdout = io::stdout();
                if capture_mouse {
                    let _ = execute!(stdout, DisableMouseCapture, LeaveAlternateScreen);
                } else {
                    let _ = execute!(stdout, LeaveAlternateScreen);
                }
                let _ = disable_raw_mode();
                Err(error)
            }
        }
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        if self.capture_mouse {
            let _ =
                execute!(self.terminal.backend_mut(), DisableMouseCapture, LeaveAlternateScreen);
        } else {
            let _ = execute!(self.terminal.backend_mut(), LeaveAlternateScreen);
        }
        let _ = disable_raw_mode();
        let _ = self.terminal.show_cursor();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_press_and_repeat_but_not_release() {
        assert!(accepts_key_kind(KeyEventKind::Press));
        assert!(accepts_key_kind(KeyEventKind::Repeat));
        assert!(!accepts_key_kind(KeyEventKind::Release));
    }
}
