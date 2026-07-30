use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use directories::UserDirs;
use unicode_segmentation::UnicodeSegmentation;

use crate::document::{DocumentSource, LoadOptions, LoadedDocument, open_document};
use crate::search::{SearchDirection, find_literal};
use crate::state::StateStore;
use crate::viewport::{Viewport, VisualLine};

const SAVE_DEBOUNCE: Duration = Duration::from_millis(750);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InputMode {
    Normal,
    Command,
    Search,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OverlayKind {
    Help,
    Info,
    Toc,
}

#[derive(Clone, Debug)]
pub struct OverlayState {
    pub kind: OverlayKind,
    pub selected: usize,
}

pub struct App {
    pub(crate) cwd: PathBuf,
    pub(crate) loaded: Option<Arc<LoadedDocument>>,
    pub(crate) viewport: Option<Viewport>,
    pub(crate) input_mode: InputMode,
    pub(crate) input: String,
    pub(crate) overlay: Option<OverlayState>,
    pub(crate) message: Option<String>,
    pub(crate) loading_path: Option<PathBuf>,
    pub(crate) current_match: Option<Range<usize>>,
    pub(crate) search_query: Option<String>,
    store: StateStore,
    load_options: LoadOptions,
    load_task: Option<(u64, mpsc::Receiver<LoadTaskResult>)>,
    load_generation: u64,
    search_task: Option<(u64, mpsc::Receiver<SearchTaskResult>)>,
    search_generation: u64,
    viewport_height: usize,
    overlay_max_position: usize,
    overlay_page_rows: usize,
    dirty_progress: bool,
    last_move: Instant,
    should_quit: bool,
}

struct LoadTaskResult {
    generation: u64,
    result: Result<LoadedDocument, crate::document::LoadError>,
}

struct SearchTaskResult {
    generation: u64,
    found: Option<Range<usize>>,
}

impl App {
    pub fn new(cwd: PathBuf, store: StateStore) -> Self {
        Self {
            cwd,
            loaded: None,
            viewport: None,
            input_mode: InputMode::Normal,
            input: String::new(),
            overlay: None,
            message: None,
            loading_path: None,
            current_match: None,
            search_query: None,
            store,
            load_options: LoadOptions::default(),
            load_task: None,
            load_generation: 0,
            search_task: None,
            search_generation: 0,
            viewport_height: 1,
            overlay_max_position: 0,
            overlay_page_rows: 1,
            dirty_progress: false,
            last_move: Instant::now(),
            should_quit: false,
        }
    }

    pub fn should_quit(&self) -> bool { self.should_quit }

    pub fn document(&self) -> Option<&LoadedDocument> { self.loaded.as_deref() }

    pub fn current_match(&self) -> Option<Range<usize>> { self.current_match.clone() }

    pub fn input_mode(&self) -> InputMode { self.input_mode }

    pub fn input(&self) -> &str { &self.input }

    pub fn overlay(&self) -> Option<&OverlayState> { self.overlay.as_ref() }

    pub fn set_overlay_layout(&mut self, content_rows: usize, viewport_rows: usize) {
        self.overlay_page_rows = viewport_rows.max(1);
        self.overlay_max_position = self.overlay.as_ref().map_or(0, |overlay| match overlay.kind {
            OverlayKind::Toc => content_rows.saturating_sub(1),
            OverlayKind::Help | OverlayKind::Info => content_rows.saturating_sub(viewport_rows),
        });
        if let Some(overlay) = self.overlay.as_mut() {
            overlay.selected = overlay.selected.min(self.overlay_max_position);
        }
    }

    pub fn cwd(&self) -> &Path { &self.cwd }

    pub fn start_load(&mut self, path: PathBuf) {
        self.save_progress();
        self.load_generation = self.load_generation.wrapping_add(1);
        let generation = self.load_generation;
        let options = self.load_options;
        let (sender, receiver) = mpsc::channel();
        let task_path = path.clone();
        thread::spawn(move || {
            let result = open_document(DocumentSource::from_path(task_path), options);
            let _ = sender.send(LoadTaskResult { generation, result });
        });
        self.load_task = Some((generation, receiver));
        self.loading_path = Some(path);
        self.message = None;
        self.overlay = None;
        self.current_match = None;
    }

    pub fn poll_tasks(&mut self) {
        let load_result =
            self.load_task.as_ref().and_then(|(_, receiver)| receiver.try_recv().ok());
        if let Some(task) = load_result {
            self.load_task = None;
            self.loading_path = None;
            if task.generation == self.load_generation {
                match task.result {
                    Ok(loaded) => self.install_document(loaded),
                    Err(error) => self.message = Some(error.to_string()),
                }
            }
        }

        let search_result =
            self.search_task.as_ref().and_then(|(_, receiver)| receiver.try_recv().ok());
        if let Some(task) = search_result {
            self.search_task = None;
            if task.generation == self.search_generation {
                if let Some(found) = task.found {
                    if let (Some(loaded), Some(viewport)) =
                        (self.loaded.as_ref(), self.viewport.as_mut())
                    {
                        viewport.goto_byte(loaded.document().text(), found.start);
                        self.current_match = Some(found);
                        self.message = None;
                        self.mark_moved();
                    }
                } else {
                    self.message = Some("No matches".to_owned());
                }
            }
        }
        self.maybe_save_progress();
    }

    pub fn handle_key(&mut self, event: KeyEvent) {
        if event.modifiers.contains(KeyModifiers::CONTROL) && event.code == KeyCode::Char('c') {
            self.should_quit = true;
            return;
        }
        if self.overlay.is_some() {
            self.handle_overlay_key(event);
            return;
        }
        match self.input_mode {
            InputMode::Normal => self.handle_normal_key(event),
            InputMode::Command | InputMode::Search => self.handle_input_key(event),
        }
    }

    pub fn scroll_mouse(&mut self, lines: isize) {
        if self.overlay.is_none() {
            self.scroll(lines);
            return;
        }
        let Some(overlay) = self.overlay.as_mut() else {
            return;
        };
        overlay.selected = if lines >= 0 {
            overlay.selected.saturating_add(lines as usize).min(self.overlay_max_position)
        } else {
            overlay.selected.saturating_sub(lines.unsigned_abs())
        };
    }

    pub fn visible_lines(&mut self, width: usize, height: usize) -> Vec<VisualLine> {
        self.viewport_height = height.max(1);
        let Some(loaded) = self.loaded.as_ref() else {
            return Vec::new();
        };
        let Some(viewport) = self.viewport.as_mut() else {
            return Vec::new();
        };
        viewport.set_width(width.max(1));
        viewport.visible_lines(loaded.document().text(), height)
    }

    pub fn progress_percent(&self) -> f64 {
        let (Some(loaded), Some(viewport)) = (self.loaded.as_ref(), self.viewport.as_ref()) else {
            return 0.0;
        };
        viewport.progress_percent(loaded.document().text())
    }

    pub fn progress_chars(&self) -> usize {
        let (Some(loaded), Some(viewport)) = (self.loaded.as_ref(), self.viewport.as_ref()) else {
            return 0;
        };
        viewport.progress_chars(loaded.document().text())
    }

    pub fn composer_text(&self) -> String {
        match self.input_mode {
            InputMode::Command => self.input.clone(),
            InputMode::Search => format!("/{}", self.input),
            InputMode::Normal => {
                if let Some(path) = &self.loading_path {
                    format!("Loading {}…", path.display())
                } else if let Some(message) = &self.message {
                    message.clone()
                } else {
                    "Ask anything…".to_owned()
                }
            }
        }
    }

    pub fn overlay_title(&self) -> Option<String> {
        let overlay = self.overlay.as_ref()?;
        match overlay.kind {
            OverlayKind::Help => Some("Help".to_owned()),
            OverlayKind::Info => Some("Document info".to_owned()),
            OverlayKind::Toc => {
                let total = self.loaded.as_ref().map_or(0, |loaded| loaded.document().toc().len());
                let current =
                    if total == 0 { 0 } else { overlay.selected.min(total.saturating_sub(1)) + 1 };
                Some(format!("Table of contents · {current}/{total}"))
            }
        }
    }

    pub fn overlay_items(&self) -> Vec<String> {
        let Some(overlay) = &self.overlay else {
            return Vec::new();
        };
        match overlay.kind {
            OverlayKind::Help => vec![
                "j/k or ↑/↓       scroll one line".to_owned(),
                "Ctrl-d/Ctrl-u    scroll half a page".to_owned(),
                "Space/b or →/←   scroll one page".to_owned(),
                "PgDn/PgUp        scroll one page".to_owned(),
                "g/G or Home/End  start/end".to_owned(),
                "[/]              previous/next chapter".to_owned(),
                "/text, n/N       search".to_owned(),
                ":toc             browse table of contents".to_owned(),
                "Enter/Esc        jump/close an open TOC".to_owned(),
                ":help            show this help".to_owned(),
                ":q or Ctrl-C     quit".to_owned(),
            ],
            OverlayKind::Info => self.info_lines(),
            OverlayKind::Toc => self
                .loaded
                .as_ref()
                .map(|loaded| {
                    loaded
                        .document()
                        .toc()
                        .iter()
                        .map(|entry| {
                            format!("{}{}", "  ".repeat(entry.depth() as usize), entry.label())
                        })
                        .collect()
                })
                .unwrap_or_default(),
        }
    }

    pub fn shutdown(&mut self) { self.save_progress(); }

    fn install_document(&mut self, loaded: LoadedDocument) {
        let resume = self.store.resume_position(loaded.path(), loaded.fingerprint());
        let position = resume.as_ref().map_or(0, |resume| resume.position);
        if let Err(error) = self.store.save_last_opened(loaded.path()) {
            self.message = Some(error.to_string());
        } else if resume.is_some_and(|resume| resume.matched_by_fingerprint) {
            self.message = Some("Restored progress after the file was moved".to_owned());
        } else if loaded.warnings().is_empty() {
            self.message = None;
        } else {
            self.message = Some(format!(
                "Opened with {} recovery warning{}; use :info for details",
                loaded.warnings().len(),
                if loaded.warnings().len() == 1 { "" } else { "s" }
            ));
        }
        let mut viewport = Viewport::new(loaded.document().text(), position);
        viewport.set_width(80);
        viewport.goto_byte(loaded.document().text(), position);
        self.loaded = Some(Arc::new(loaded));
        self.viewport = Some(viewport);
        self.current_match = None;
        self.search_query = None;
        // Persist even a freshly opened book at position zero. Besides resume
        // state, this records the fingerprint used to recognise a later move.
        self.dirty_progress = true;
        self.last_move = Instant::now();
    }

    fn handle_normal_key(&mut self, event: KeyEvent) {
        let page = self.viewport_height.max(1) as isize;
        let half_page = (self.viewport_height.max(2) / 2) as isize;
        match event.code {
            KeyCode::Char(':') => self.begin_input(InputMode::Command),
            KeyCode::Char('/') => self.begin_input(InputMode::Search),
            KeyCode::Char('j') | KeyCode::Down => self.scroll(1),
            KeyCode::Char('k') | KeyCode::Up => self.scroll(-1),
            KeyCode::Char('d') if event.modifiers.contains(KeyModifiers::CONTROL) => {
                self.scroll(half_page)
            }
            KeyCode::Char('u') if event.modifiers.contains(KeyModifiers::CONTROL) => {
                self.scroll(-half_page)
            }
            KeyCode::Char(' ') | KeyCode::PageDown | KeyCode::Right => self.scroll(page),
            KeyCode::Char('b') | KeyCode::PageUp | KeyCode::Left => self.scroll(-page),
            KeyCode::Char('g') | KeyCode::Home => self.goto_start(),
            KeyCode::Char('G') | KeyCode::End => self.goto_end(),
            KeyCode::Char(']') => self.goto_chapter(true),
            KeyCode::Char('[') => self.goto_chapter(false),
            KeyCode::Char('n') => self.repeat_search(SearchDirection::Forward),
            KeyCode::Char('N') => self.repeat_search(SearchDirection::Backward),
            KeyCode::Esc => self.message = None,
            _ => {}
        }
    }

    fn handle_input_key(&mut self, event: KeyEvent) {
        match event.code {
            KeyCode::Esc => {
                self.input.clear();
                self.input_mode = InputMode::Normal;
            }
            KeyCode::Enter => {
                let input = std::mem::take(&mut self.input);
                let mode = std::mem::replace(&mut self.input_mode, InputMode::Normal);
                match mode {
                    InputMode::Command => self.execute_command(&input),
                    InputMode::Search => self.start_search(input, SearchDirection::Forward, None),
                    InputMode::Normal => {}
                }
            }
            KeyCode::Backspace => {
                if let Some((start, _)) = self.input.grapheme_indices(true).next_back() {
                    self.input.truncate(start);
                }
            }
            KeyCode::Char('u') if event.modifiers.contains(KeyModifiers::CONTROL) => {
                self.input.clear();
            }
            KeyCode::Char(character)
                if !event.modifiers.contains(KeyModifiers::CONTROL)
                    && !event.modifiers.contains(KeyModifiers::ALT) =>
            {
                self.input.push(character);
            }
            _ => {}
        }
    }

    fn handle_overlay_key(&mut self, event: KeyEvent) {
        let Some(mut overlay) = self.overlay.take() else {
            return;
        };
        let page = self.overlay_page_rows.saturating_sub(1).max(1);
        let half_page = (self.overlay_page_rows / 2).max(1);
        match event.code {
            KeyCode::Esc => return,
            KeyCode::Enter if overlay.kind == OverlayKind::Toc => {
                let offset = self.loaded.as_ref().and_then(|loaded| {
                    loaded.document().toc().get(overlay.selected).map(|entry| entry.offset())
                });
                if let Some(offset) = offset {
                    self.goto_byte(offset);
                }
                return;
            }
            KeyCode::Char('j') | KeyCode::Down => {
                overlay.selected =
                    overlay.selected.saturating_add(1).min(self.overlay_max_position);
            }
            KeyCode::Char('k') | KeyCode::Up => {
                overlay.selected = overlay.selected.saturating_sub(1);
            }
            KeyCode::Char('d') if event.modifiers.contains(KeyModifiers::CONTROL) => {
                overlay.selected =
                    overlay.selected.saturating_add(half_page).min(self.overlay_max_position);
            }
            KeyCode::Char('u') if event.modifiers.contains(KeyModifiers::CONTROL) => {
                overlay.selected = overlay.selected.saturating_sub(half_page);
            }
            KeyCode::Char(' ') | KeyCode::PageDown | KeyCode::Right => {
                overlay.selected =
                    overlay.selected.saturating_add(page).min(self.overlay_max_position);
            }
            KeyCode::Char('b') | KeyCode::PageUp | KeyCode::Left => {
                overlay.selected = overlay.selected.saturating_sub(page);
            }
            KeyCode::Char('g') | KeyCode::Home => {
                overlay.selected = 0;
            }
            KeyCode::Char('G') | KeyCode::End => {
                overlay.selected = self.overlay_max_position;
            }
            _ => {}
        }
        self.overlay = Some(overlay);
    }

    fn begin_input(&mut self, mode: InputMode) {
        self.input_mode = mode;
        self.input.clear();
        self.message = None;
    }

    fn execute_command(&mut self, raw: &str) {
        match parse_command(raw) {
            Ok(Command::Open(path)) => self.start_load(self.resolve_path(&path)),
            Ok(Command::Quit) => self.should_quit = true,
            Ok(Command::Toc) => {
                if self.loaded.as_ref().is_some_and(|loaded| !loaded.document().toc().is_empty()) {
                    self.overlay = Some(OverlayState {
                        kind: OverlayKind::Toc,
                        selected: self.current_toc_index(),
                    });
                } else {
                    self.message = Some("This document has no table of contents".to_owned());
                }
            }
            Ok(Command::Goto(percent)) => self.goto_percent(percent),
            Ok(Command::Info) => {
                if self.loaded.is_some() {
                    self.overlay = Some(OverlayState { kind: OverlayKind::Info, selected: 0 });
                } else {
                    self.message = Some("No document is open".to_owned());
                }
            }
            Ok(Command::Help) => {
                self.overlay = Some(OverlayState { kind: OverlayKind::Help, selected: 0 });
            }
            Err(error) => self.message = Some(error),
        }
    }

    fn start_search(&mut self, query: String, direction: SearchDirection, from: Option<usize>) {
        if query.is_empty() {
            self.message = Some("Search text cannot be empty".to_owned());
            return;
        }
        let Some(loaded) = self.loaded.clone() else {
            self.message = Some("No document is open".to_owned());
            return;
        };
        let default_from = self.viewport.as_ref().map_or(0, Viewport::anchor);
        let from = from.unwrap_or(default_from);
        self.search_query = Some(query.clone());
        self.search_generation = self.search_generation.wrapping_add(1);
        let generation = self.search_generation;
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let found = find_literal(loaded.document().text(), &query, from, direction);
            let _ = sender.send(SearchTaskResult { generation, found });
        });
        self.search_task = Some((generation, receiver));
        self.message = Some("Searching…".to_owned());
    }

    fn repeat_search(&mut self, direction: SearchDirection) {
        let Some(query) = self.search_query.clone() else {
            self.message = Some("No previous search".to_owned());
            return;
        };
        let from = match (direction, self.current_match.as_ref()) {
            (SearchDirection::Forward, Some(found)) => found.end,
            (SearchDirection::Backward, Some(found)) => found.start,
            _ => self.viewport.as_ref().map_or(0, Viewport::anchor),
        };
        self.start_search(query, direction, Some(from));
    }

    fn scroll(&mut self, delta: isize) {
        if let (Some(loaded), Some(viewport)) = (self.loaded.as_ref(), self.viewport.as_mut()) {
            let before = viewport.anchor();
            viewport.scroll_by(loaded.document().text(), delta);
            if viewport.anchor() != before {
                self.current_match = None;
                self.mark_moved();
            }
        }
    }

    fn goto_start(&mut self) {
        if let Some(viewport) = self.viewport.as_mut() {
            viewport.goto_start();
            self.current_match = None;
            self.mark_moved();
        }
    }

    fn goto_end(&mut self) {
        if let (Some(loaded), Some(viewport)) = (self.loaded.as_ref(), self.viewport.as_mut()) {
            viewport.goto_end(loaded.document().text());
            self.current_match = None;
            self.mark_moved();
        }
    }

    fn goto_percent(&mut self, percent: f64) {
        if let (Some(loaded), Some(viewport)) = (self.loaded.as_ref(), self.viewport.as_mut()) {
            viewport.goto_percent(loaded.document().text(), percent);
            self.current_match = None;
            self.mark_moved();
        } else {
            self.message = Some("No document is open".to_owned());
        }
    }

    fn goto_byte(&mut self, offset: usize) {
        if let (Some(loaded), Some(viewport)) = (self.loaded.as_ref(), self.viewport.as_mut()) {
            viewport.goto_byte(loaded.document().text(), offset);
            self.current_match = None;
            self.mark_moved();
        }
    }

    fn goto_chapter(&mut self, forward: bool) {
        let Some(loaded) = self.loaded.as_ref() else {
            return;
        };
        let anchor = self.viewport.as_ref().map_or(0, Viewport::anchor);
        let offset = if forward {
            loaded
                .document()
                .toc()
                .iter()
                .find(|entry| entry.offset() > anchor)
                .map(|entry| entry.offset())
        } else {
            loaded
                .document()
                .toc()
                .iter()
                .rev()
                .find(|entry| entry.offset() < anchor)
                .map(|entry| entry.offset())
        };
        if let Some(offset) = offset {
            self.goto_byte(offset);
        }
    }

    fn current_toc_index(&self) -> usize {
        let Some(loaded) = self.loaded.as_ref() else {
            return 0;
        };
        let anchor = self.viewport.as_ref().map_or(0, Viewport::anchor);
        loaded.document().toc().partition_point(|entry| entry.offset() <= anchor).saturating_sub(1)
    }

    fn info_lines(&self) -> Vec<String> {
        let Some(loaded) = self.loaded.as_ref() else {
            return Vec::new();
        };
        let mut lines = vec![
            format!("Path: {}", loaded.path().display()),
            format!("Format: {}", loaded.format()),
            format!("Title: {}", loaded.document().metadata().title().unwrap_or("Unknown")),
            format!(
                "Position: {} / {} characters ({:.1}%)",
                self.progress_chars(),
                loaded.document().total_chars(),
                self.progress_percent()
            ),
        ];
        for warning in loaded.warnings() {
            lines.push(format!("Warning [{}]: {}", warning.code(), warning.message()));
        }
        lines
    }

    fn resolve_path(&self, raw: &str) -> PathBuf {
        let raw = unquote(raw.trim());
        let path = if raw == "~" || raw.starts_with("~/") || raw.starts_with("~\\") {
            UserDirs::new()
                .map(|directories| {
                    if raw.len() == 1 {
                        directories.home_dir().to_path_buf()
                    } else {
                        directories.home_dir().join(&raw[2..])
                    }
                })
                .unwrap_or_else(|| PathBuf::from(raw))
        } else {
            PathBuf::from(raw)
        };
        if path.is_absolute() { path } else { self.cwd.join(path) }
    }

    fn mark_moved(&mut self) {
        self.dirty_progress = true;
        self.last_move = Instant::now();
    }

    fn maybe_save_progress(&mut self) {
        if self.dirty_progress && self.last_move.elapsed() >= SAVE_DEBOUNCE {
            self.save_progress();
        }
    }

    fn save_progress(&mut self) {
        if !self.dirty_progress {
            return;
        }
        let (Some(loaded), Some(viewport)) = (self.loaded.as_ref(), self.viewport.as_ref()) else {
            return;
        };
        match self.store.save_progress(loaded.path(), loaded.fingerprint(), viewport.anchor()) {
            Ok(()) => self.dirty_progress = false,
            Err(error) => {
                self.message = Some(error.to_string());
                self.last_move = Instant::now();
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Command {
    Open(String),
    Quit,
    Toc,
    Goto(f64),
    Info,
    Help,
}

fn parse_command(raw: &str) -> Result<Command, String> {
    let raw = raw.trim();
    let (name, arguments) =
        raw.split_once(char::is_whitespace).map_or((raw, ""), |(name, rest)| (name, rest.trim()));
    match name {
        "e" | "open" if !arguments.is_empty() => Ok(Command::Open(arguments.to_owned())),
        "e" | "open" => Err("Usage: :e <path>".to_owned()),
        "q" | "quit" => Ok(Command::Quit),
        "toc" => Ok(Command::Toc),
        "info" => Ok(Command::Info),
        "help" | "h" => Ok(Command::Help),
        "goto" => {
            let value = arguments.strip_suffix('%').unwrap_or(arguments);
            let percent = value.parse::<f64>().map_err(|_| "Usage: :goto <0%-100%>".to_owned())?;
            if !(0.0..=100.0).contains(&percent) {
                return Err("Percentage must be between 0 and 100".to_owned());
            }
            Ok(Command::Goto(percent))
        }
        "" => Err("Command cannot be empty".to_owned()),
        other => Err(format!("Unknown command: {other}")),
    }
}

fn unquote(value: &str) -> &str {
    if value.len() >= 2
        && ((value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\'')))
    {
        &value[1..value.len() - 1]
    } else {
        value
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::Duration;

    use super::*;

    fn app_with_text(text: &str) -> (tempfile::TempDir, App) {
        let directory = tempfile::tempdir().unwrap();
        let book = directory.path().join("book.txt");
        fs::write(&book, text).unwrap();
        let store = StateStore::at(directory.path().join("state")).unwrap();
        let loaded =
            open_document(DocumentSource::from_path(book), LoadOptions::default()).unwrap();
        let mut app = App::new(directory.path().to_path_buf(), store);
        app.install_document(loaded);
        (directory, app)
    }

    #[test]
    fn parses_open_path_without_splitting_spaces() {
        assert_eq!(
            parse_command("e books/my novel.epub").unwrap(),
            Command::Open("books/my novel.epub".to_owned())
        );
    }

    #[test]
    fn parses_and_validates_percentage() {
        assert_eq!(parse_command("goto 38%").unwrap(), Command::Goto(38.0));
        assert!(parse_command("goto 101%").is_err());
    }

    #[test]
    fn removes_matching_path_quotes_only() {
        assert_eq!(unquote("\"a b.txt\""), "a b.txt");
        assert_eq!(unquote("'a b.txt'"), "a b.txt");
        assert_eq!(unquote("\"a b.txt"), "\"a b.txt");
    }

    #[test]
    fn colon_focuses_command_input_without_becoming_visible_text() {
        let directory = tempfile::tempdir().unwrap();
        let store = StateStore::at(directory.path().join("state")).unwrap();
        let mut app = App::new(directory.path().to_path_buf(), store);

        app.handle_key(KeyEvent::new(KeyCode::Char(':'), KeyModifiers::NONE));
        assert_eq!(app.input_mode(), InputMode::Command);
        assert_eq!(app.composer_text(), "");

        app.handle_key(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));
        assert_eq!(app.composer_text(), "i");
    }

    #[test]
    fn scrolls_non_toc_overlays_with_line_page_and_end_keys() {
        let directory = tempfile::tempdir().unwrap();
        let store = StateStore::at(directory.path().join("state")).unwrap();
        let mut app = App::new(directory.path().to_path_buf(), store);
        app.overlay = Some(OverlayState { kind: OverlayKind::Info, selected: 0 });
        app.set_overlay_layout(20, 5);

        app.scroll_mouse(3);
        assert_eq!(app.overlay().unwrap().selected, 3);
        app.scroll_mouse(-3);
        assert_eq!(app.overlay().unwrap().selected, 0);
        app.handle_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
        assert_eq!(app.overlay().unwrap().selected, 1);
        app.handle_key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE));
        assert_eq!(app.overlay().unwrap().selected, 5);
        app.handle_key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
        assert_eq!(app.overlay().unwrap().selected, 15);
        app.handle_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE));
        assert_eq!(app.overlay().unwrap().selected, 14);
        app.handle_key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
        assert_eq!(app.overlay().unwrap().selected, 0);
        app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(app.overlay().unwrap().selected, 4);
        app.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        assert_eq!(app.overlay().unwrap().selected, 0);
    }

    #[test]
    fn scrolls_toc_by_line_half_page_page_and_end_keys() {
        let text = (1..=20)
            .map(|number| format!("第{number}章 标题{number}\n正文。\n"))
            .collect::<String>();
        let (_directory, mut app) = app_with_text(&text);
        assert_eq!(app.document().unwrap().document().toc().len(), 20);

        app.overlay = Some(OverlayState { kind: OverlayKind::Toc, selected: 0 });
        app.set_overlay_layout(20, 5);
        assert_eq!(app.overlay_title().as_deref(), Some("Table of contents · 1/20"));

        app.handle_key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE));
        assert_eq!(app.overlay().unwrap().selected, 4);
        assert_eq!(app.overlay_title().as_deref(), Some("Table of contents · 5/20"));
        app.handle_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL));
        assert_eq!(app.overlay().unwrap().selected, 6);
        app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(app.overlay().unwrap().selected, 10);
        app.handle_key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
        assert_eq!(app.overlay().unwrap().selected, 19);
        assert_eq!(app.overlay_title().as_deref(), Some("Table of contents · 20/20"));
        app.handle_key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE));
        assert_eq!(app.overlay().unwrap().selected, 19);
        app.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        assert_eq!(app.overlay().unwrap().selected, 15);
        app.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(app.overlay().unwrap().selected, 13);
        app.handle_key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
        assert_eq!(app.overlay().unwrap().selected, 0);
        app.handle_key(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE));
        assert_eq!(app.overlay().unwrap().selected, 0);
    }

    #[test]
    fn toc_layout_clamps_selection_and_enter_jumps_to_it() {
        let text = (1..=4)
            .map(|number| format!("第{number}章 标题{number}\n正文。\n"))
            .collect::<String>();
        let (_directory, mut app) = app_with_text(&text);
        let target = app.document().unwrap().document().toc()[2].offset();
        app.overlay = Some(OverlayState { kind: OverlayKind::Toc, selected: usize::MAX });

        app.set_overlay_layout(3, 0);
        assert_eq!(app.overlay().unwrap().selected, 2);
        app.handle_key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE));
        assert_eq!(app.overlay().unwrap().selected, 2);
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert!(app.overlay().is_none());
        assert_eq!(app.viewport.as_ref().unwrap().anchor(), target);
    }

    #[test]
    fn left_and_right_scroll_the_document_only_in_normal_mode() {
        let text = (1..=20).map(|number| format!("line {number}\n")).collect::<String>();
        let (_directory, mut app) = app_with_text(&text);
        app.visible_lines(40, 4);

        app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert!(app.viewport.as_ref().unwrap().anchor() > 0);
        app.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        assert_eq!(app.viewport.as_ref().unwrap().anchor(), 0);

        app.handle_key(KeyEvent::new(KeyCode::Char(':'), KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(app.viewport.as_ref().unwrap().anchor(), 0);
    }

    #[test]
    fn help_lists_page_keys_and_toc_controls() {
        let directory = tempfile::tempdir().unwrap();
        let store = StateStore::at(directory.path().join("state")).unwrap();
        let mut app = App::new(directory.path().to_path_buf(), store);
        app.overlay = Some(OverlayState { kind: OverlayKind::Help, selected: 0 });
        let help = app.overlay_items().join("\n");

        assert!(help.contains("→/←"));
        assert!(help.contains("PgDn/PgUp"));
        assert!(help.contains("Enter/Esc"));
    }

    #[test]
    fn loads_a_document_in_the_background_and_persists_it() {
        let directory = tempfile::tempdir().unwrap();
        let book = directory.path().join("book.txt");
        fs::write(&book, "hello\nworld").unwrap();
        let store = StateStore::at(directory.path().join("state")).unwrap();
        let mut app = App::new(directory.path().to_path_buf(), store.clone());
        app.start_load(book.clone());

        let deadline = Instant::now() + Duration::from_secs(2);
        while app.document().is_none() && Instant::now() < deadline {
            app.poll_tasks();
            thread::yield_now();
        }

        assert_eq!(app.document().unwrap().document().text(), "hello\nworld");
        let fingerprint = app.document().unwrap().fingerprint().to_owned();
        app.shutdown();
        assert_eq!(store.resume_position(&book, &fingerprint).unwrap().position, 0);
    }
}
