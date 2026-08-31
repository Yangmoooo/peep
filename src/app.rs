use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use directories::UserDirs;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::document::{DocumentSource, LoadOptions, LoadedDocument, open_document};
use crate::file_picker::{FileEntry, read_directory};
use crate::filter::FilteredList;
use crate::history::InputHistory;
use crate::search::{
    SearchAnalysis, SearchDirection, SearchError, SearchHit, SearchKind, SearchQuery, analyze,
    find_next,
};
use crate::state::{Bookmark, RecentBook, SavedHistory, StateStore, StateWarning, now_unix_ms};
use crate::theme::ThemeChoice;
use crate::viewport::{Viewport, VisualLine};

const SAVE_DEBOUNCE: Duration = Duration::from_millis(750);
const BOOKMARK_LABEL_LIMIT: usize = 120;
const RECENT_BOOK_LIMIT: usize = 100;
const SEARCH_PREVIEW_LIMIT: usize = 200;
const SEARCH_CONTEXT_WIDTH: usize = 72;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InputMode {
    Normal,
    Command,
    Search,
    Filter,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OverlayKind {
    Help,
    Info,
    Toc,
    Bookmarks,
    Recent,
    SearchResults,
    Files,
}

impl OverlayKind {
    pub fn is_list(self) -> bool {
        matches!(
            self,
            Self::Toc | Self::Bookmarks | Self::Recent | Self::SearchResults | Self::Files
        )
    }

    fn is_filterable(self) -> bool {
        matches!(self, Self::Toc | Self::Bookmarks | Self::Recent | Self::Files)
    }
}

#[derive(Clone, Debug)]
pub struct OverlayState {
    pub kind: OverlayKind,
    pub selected: usize,
    filter: Option<FilteredList>,
}

impl OverlayState {
    pub fn new(kind: OverlayKind, selected: usize) -> Self { Self { kind, selected, filter: None } }
}

pub struct App {
    pub(crate) cwd: PathBuf,
    pub(crate) loaded: Option<Arc<LoadedDocument>>,
    pub(crate) viewport: Option<Viewport>,
    pub(crate) input_mode: InputMode,
    pub(crate) input: String,
    input_cursor: usize,
    command_history: InputHistory,
    search_history: InputHistory,
    pub(crate) overlay: Option<OverlayState>,
    file_picker: Option<FilePickerState>,
    pub(crate) message: Option<String>,
    pub(crate) loading_path: Option<PathBuf>,
    pub(crate) current_match: Option<Range<usize>>,
    theme_choice: ThemeChoice,
    search_session: Option<SearchSession>,
    bookmarks: Vec<Bookmark>,
    recent_books: Vec<RecentBook>,
    state_warnings: Vec<StateWarning>,
    store: StateStore,
    load_options: LoadOptions,
    load_task: Option<(u64, mpsc::Receiver<LoadTaskResult>)>,
    load_generation: u64,
    search_task: Option<(u64, mpsc::Receiver<SearchTaskResult>)>,
    search_generation: u64,
    viewport_height: usize,
    overlay_max_position: usize,
    overlay_page_rows: usize,
    overlay_list_offset: usize,
    dirty_progress: bool,
    last_move: Instant,
    should_quit: bool,
    completion: Option<CompletionState>,
}

struct LoadTaskResult {
    generation: u64,
    result: Result<LoadedDocument, crate::document::LoadError>,
}

struct SearchTaskResult {
    generation: u64,
    payload: SearchTaskPayload,
}

enum SearchTaskPayload {
    Analyze { query: SearchQuery, result: Result<SearchAnalysis, SearchError> },
    Step { direction: SearchDirection, result: Result<Option<Range<usize>>, SearchError> },
}

#[derive(Clone, Debug)]
struct SearchSession {
    query: SearchQuery,
    current: Option<SearchHit>,
    total: usize,
    previews: Vec<SearchHit>,
}

struct SearchContext {
    text: String,
    emphasis: Range<usize>,
}

#[derive(Clone, Debug)]
struct FilePickerState {
    directory: PathBuf,
    entries: Vec<FileEntry>,
}

#[derive(Clone, Debug)]
struct CompletionState {
    candidates: Vec<String>,
    next: usize,
}

impl App {
    pub fn new(cwd: PathBuf, store: StateStore) -> Self {
        let theme_choice = store.load_theme();
        let history = store.load_history();
        Self {
            cwd,
            loaded: None,
            viewport: None,
            input_mode: InputMode::Normal,
            input: String::new(),
            input_cursor: 0,
            command_history: InputHistory::new(history.commands),
            search_history: InputHistory::new(history.searches),
            overlay: None,
            file_picker: None,
            message: None,
            loading_path: None,
            current_match: None,
            theme_choice,
            search_session: None,
            bookmarks: Vec::new(),
            recent_books: Vec::new(),
            state_warnings: Vec::new(),
            store,
            load_options: LoadOptions::default(),
            load_task: None,
            load_generation: 0,
            search_task: None,
            search_generation: 0,
            viewport_height: 1,
            overlay_max_position: 0,
            overlay_page_rows: 1,
            overlay_list_offset: 0,
            dirty_progress: false,
            last_move: Instant::now(),
            should_quit: false,
            completion: None,
        }
    }

    pub fn should_quit(&self) -> bool { self.should_quit }

    pub fn document(&self) -> Option<&LoadedDocument> { self.loaded.as_deref() }

    pub fn current_match(&self) -> Option<Range<usize>> { self.current_match.clone() }

    pub fn input_mode(&self) -> InputMode { self.input_mode }

    pub fn input(&self) -> &str { &self.input }

    pub fn input_cursor(&self) -> usize { self.input_cursor }

    pub fn theme_choice(&self) -> ThemeChoice { self.theme_choice }

    pub fn override_theme(&mut self, theme: ThemeChoice) { self.theme_choice = theme; }

    pub fn overlay(&self) -> Option<&OverlayState> { self.overlay.as_ref() }

    pub(crate) fn overlay_list_offset(&self) -> usize { self.overlay_list_offset }

    pub(crate) fn set_overlay_list_offset(&mut self, offset: usize) {
        self.overlay_list_offset = offset;
    }

    pub fn set_overlay_layout(&mut self, content_rows: usize, viewport_rows: usize) {
        self.overlay_page_rows = viewport_rows.max(1);
        let is_list = self.overlay.as_ref().is_some_and(|overlay| overlay.kind.is_list());
        self.overlay_max_position = self.overlay.as_ref().map_or(0, |overlay| {
            if overlay.kind.is_list() {
                content_rows.saturating_sub(1)
            } else {
                content_rows.saturating_sub(viewport_rows)
            }
        });
        if let Some(overlay) = self.overlay.as_mut() {
            overlay.selected = overlay.selected.min(self.overlay_max_position);
        }
        self.overlay_list_offset = if is_list {
            self.overlay_list_offset.min(content_rows.saturating_sub(viewport_rows))
        } else {
            0
        };
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
        self.file_picker = None;
        self.current_match = None;
        self.search_generation = self.search_generation.wrapping_add(1);
        self.search_task = None;
        self.search_session = None;
    }

    pub fn open_path(&mut self, path: PathBuf) {
        if path.is_dir() {
            self.show_file_picker(path);
        } else {
            self.start_load(path);
        }
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
                match task.payload {
                    SearchTaskPayload::Analyze { query, result } => {
                        self.finish_search_analysis(query, result);
                    }
                    SearchTaskPayload::Step { direction, result } => {
                        self.finish_search_step(direction, result);
                    }
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
            InputMode::Filter => self.input_mode = InputMode::Normal,
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
            InputMode::Search => self.input.clone(),
            InputMode::Filter => self
                .overlay
                .as_ref()
                .and_then(|overlay| overlay.filter.as_ref())
                .map_or_else(String::new, |filter| filter.query().to_owned()),
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

    pub fn composer_prompt(&self) -> &'static str {
        match self.input_mode {
            InputMode::Search | InputMode::Filter => "⌕ ",
            InputMode::Normal | InputMode::Command => "› ",
        }
    }

    pub fn composer_cursor_width(&self) -> usize {
        match self.input_mode {
            InputMode::Command | InputMode::Search => {
                let prefix = &self.input[..self.input_cursor.min(self.input.len())];
                UnicodeWidthStr::width(prefix)
            }
            InputMode::Filter => self
                .overlay
                .as_ref()
                .and_then(|overlay| overlay.filter.as_ref())
                .map_or(0, |filter| UnicodeWidthStr::width(filter.query())),
            InputMode::Normal => 0,
        }
    }

    pub fn overlay_title(&self) -> Option<String> {
        let overlay = self.overlay.as_ref()?;
        match overlay.kind {
            OverlayKind::Help => Some("Help".to_owned()),
            OverlayKind::Info => Some("Document info".to_owned()),
            OverlayKind::Toc => {
                let total = self.loaded.as_ref().map_or(0, |loaded| loaded.document().toc().len());
                if let Some(filter) = overlay.filter.as_ref().filter(|filter| filter.is_active()) {
                    return Some(filtered_overlay_title(
                        "Table of contents",
                        overlay.selected,
                        filter.len(),
                        total,
                    ));
                }
                let current =
                    if total == 0 { 0 } else { overlay.selected.min(total.saturating_sub(1)) + 1 };
                Some(format!("Table of contents · {current}/{total}"))
            }
            OverlayKind::Bookmarks => {
                let total = self.bookmarks.len();
                if let Some(filter) = overlay.filter.as_ref().filter(|filter| filter.is_active()) {
                    return Some(filtered_overlay_title(
                        "Bookmarks",
                        overlay.selected,
                        filter.len(),
                        total,
                    ));
                }
                let current =
                    if total == 0 { 0 } else { overlay.selected.min(total.saturating_sub(1)) + 1 };
                Some(format!("Bookmarks · {current}/{total}"))
            }
            OverlayKind::Recent => {
                let total = self.recent_books.len();
                if let Some(filter) = overlay.filter.as_ref().filter(|filter| filter.is_active()) {
                    return Some(filtered_overlay_title(
                        "Recent books",
                        overlay.selected,
                        filter.len(),
                        total,
                    ));
                }
                let current =
                    if total == 0 { 0 } else { overlay.selected.min(total.saturating_sub(1)) + 1 };
                Some(format!("Recent books · {current}/{total}"))
            }
            OverlayKind::SearchResults => {
                let session = self.search_session.as_ref()?;
                let current = session
                    .previews
                    .get(overlay.selected)
                    .map_or(0, |hit| hit.ordinal.saturating_add(1));
                Some(format!("Search results · {current}/{}", session.total))
            }
            OverlayKind::Files => {
                let total = self.file_picker.as_ref().map_or(0, |picker| picker.entries.len());
                let directory = self.file_picker.as_ref().map_or_else(
                    || "Files".to_owned(),
                    |picker| picker.directory.display().to_string(),
                );
                if let Some(filter) = overlay.filter.as_ref().filter(|filter| filter.is_active()) {
                    return Some(filtered_overlay_title(
                        &format!("Open · {directory}"),
                        overlay.selected,
                        filter.len(),
                        total,
                    ));
                }
                let current =
                    if total == 0 { 0 } else { overlay.selected.min(total.saturating_sub(1)) + 1 };
                Some(format!("Open · {directory} · {current}/{total}"))
            }
        }
    }

    pub fn overlay_items(&self) -> Vec<String> {
        let Some(overlay) = &self.overlay else {
            return Vec::new();
        };
        let items = self.unfiltered_overlay_items(overlay.kind);
        let items =
            if let Some(filter) = overlay.filter.as_ref() { filter.items(&items) } else { items };
        if !items.is_empty() {
            return items;
        }
        vec![match overlay.kind {
            OverlayKind::Toc => "No matching sections".to_owned(),
            OverlayKind::Bookmarks
                if overlay.filter.as_ref().is_some_and(|filter| filter.is_active()) =>
            {
                "No matching bookmarks".to_owned()
            }
            OverlayKind::Bookmarks => "No bookmarks yet; use :mark [label] to add one".to_owned(),
            OverlayKind::Recent
                if overlay.filter.as_ref().is_some_and(|filter| filter.is_active()) =>
            {
                "No matching recent books".to_owned()
            }
            OverlayKind::Recent => "No readable recent books".to_owned(),
            OverlayKind::SearchResults => "No search results".to_owned(),
            OverlayKind::Files => "No supported files in this directory".to_owned(),
            OverlayKind::Help | OverlayKind::Info => return items,
        }]
    }

    fn unfiltered_overlay_items(&self, kind: OverlayKind) -> Vec<String> {
        match kind {
            OverlayKind::Help => vec![
                "j/k or ↑/↓       scroll one line".to_owned(),
                "Ctrl-d/Ctrl-u    scroll half a page".to_owned(),
                "Space/b or →/←   scroll one page".to_owned(),
                "PgDn/PgUp        scroll one page".to_owned(),
                "g/G or Home/End  start/end".to_owned(),
                "[/]              previous/next section".to_owned(),
                "/text, n/N       search".to_owned(),
                ":exact/:re       exact/regex search".to_owned(),
                ":results         browse search results".to_owned(),
                ":toc             browse table of contents".to_owned(),
                ":mark/:marks     add/browse bookmarks".to_owned(),
                ":recent          browse recent books".to_owned(),
                ":e <directory>   choose a supported file".to_owned(),
                "/ in a list      filter toc/bookmarks/recent/files".to_owned(),
                "Tab              complete commands and paths".to_owned(),
                "↑/↓ in input     browse saved input history".to_owned(),
                ":theme           choose auto/light/dark colors".to_owned(),
                ":history clear   clear command/search history".to_owned(),
                "Enter/Esc        jump/close a list".to_owned(),
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
            OverlayKind::Bookmarks => self.bookmark_items(),
            OverlayKind::Recent => self.recent_items(),
            OverlayKind::SearchResults => self.search_result_items(),
            OverlayKind::Files => self
                .file_picker
                .as_ref()
                .map(|picker| picker.entries.iter().map(FileEntry::label).collect())
                .unwrap_or_default(),
        }
    }

    pub(crate) fn overlay_item_emphasis(&self, index: usize) -> Option<Range<usize>> {
        let overlay = self.overlay.as_ref()?;
        if overlay.kind != OverlayKind::SearchResults {
            return None;
        }
        let loaded = self.loaded.as_ref()?;
        let session = self.search_session.as_ref()?;
        let hit = session.previews.get(index)?;
        let prefix = format!("{}/{}  ", hit.ordinal.saturating_add(1), session.total);
        let context = search_context(loaded.document().text(), &hit.range, SEARCH_CONTEXT_WIDTH);
        Some(
            prefix.len().saturating_add(context.emphasis.start)
                ..prefix.len().saturating_add(context.emphasis.end),
        )
    }

    pub fn shutdown(&mut self) { self.save_progress(); }

    fn install_document(&mut self, loaded: LoadedDocument) {
        let saved = self.store.load_book(loaded.path(), loaded.fingerprint());
        let position = saved.position;
        self.bookmarks = saved
            .bookmarks
            .into_iter()
            .map(|mut bookmark| {
                bookmark.position = floor_char_boundary(
                    loaded.document().text(),
                    bookmark.position.min(loaded.document().text().len()),
                );
                bookmark
            })
            .collect();
        self.bookmarks.sort_by_key(|bookmark| (bookmark.position, bookmark.created_unix_ms));
        self.bookmarks.dedup_by_key(|bookmark| bookmark.position);
        self.state_warnings = saved.warnings;
        if let Err(error) = self.store.save_last_opened(loaded.path()) {
            self.message = Some(error.to_string());
        } else if saved.matched_by_fingerprint {
            self.message = Some("Restored reading state after the file was moved".to_owned());
        } else if loaded.warnings().is_empty() && self.state_warnings.is_empty() {
            self.message = None;
        } else {
            let warning_count = loaded.warnings().len() + self.state_warnings.len();
            self.message = Some(format!(
                "Opened with {} recovery warning{}; use :info for details",
                warning_count,
                if warning_count == 1 { "" } else { "s" }
            ));
        }
        let mut viewport = Viewport::new(loaded.document().text(), position);
        viewport.set_width(80);
        viewport.goto_byte(loaded.document().text(), position);
        self.loaded = Some(Arc::new(loaded));
        self.viewport = Some(viewport);
        self.current_match = None;
        self.search_session = None;
        self.recent_books.clear();
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
        if event.code == KeyCode::Tab {
            if self.input_mode == InputMode::Command {
                self.complete_input();
            }
            return;
        }
        self.completion = None;
        match event.code {
            KeyCode::Esc => {
                self.input.clear();
                self.input_cursor = 0;
                self.reset_active_history_navigation();
                self.input_mode = InputMode::Normal;
            }
            KeyCode::Up => self.navigate_input_history(true),
            KeyCode::Down => self.navigate_input_history(false),
            KeyCode::Enter => {
                let input = std::mem::take(&mut self.input);
                self.input_cursor = 0;
                let mode = std::mem::replace(&mut self.input_mode, InputMode::Normal);
                match mode {
                    InputMode::Command => {
                        let clears_history =
                            matches!(parse_command(&input), Ok(Command::ClearHistory(_)));
                        self.execute_command(&input);
                        if !clears_history {
                            self.record_input_history(InputMode::Command, &input);
                        }
                    }
                    InputMode::Search => {
                        self.start_search(
                            SearchQuery::new(SearchKind::LooseLiteral, input.clone()),
                            SearchDirection::Forward,
                            None,
                        );
                        self.record_input_history(InputMode::Search, &input);
                    }
                    InputMode::Normal | InputMode::Filter => {}
                }
            }
            KeyCode::Backspace => {
                let cursor = self.input_cursor;
                if let Some((start, _)) = self.input[..cursor].grapheme_indices(true).next_back() {
                    self.input.replace_range(start..cursor, "");
                    self.input_cursor = start;
                }
            }
            KeyCode::Delete => {
                let cursor = self.input_cursor;
                if let Some((_, grapheme)) = self.input[cursor..].grapheme_indices(true).next() {
                    let end = cursor + grapheme.len();
                    self.input.replace_range(cursor..end, "");
                }
            }
            KeyCode::Left => self.move_input_cursor_left(),
            KeyCode::Right => self.move_input_cursor_right(),
            KeyCode::Home => self.input_cursor = 0,
            KeyCode::End => self.input_cursor = self.input.len(),
            KeyCode::Char('u') if event.modifiers.contains(KeyModifiers::CONTROL) => {
                self.input.clear();
                self.input_cursor = 0;
            }
            KeyCode::Char(character)
                if !event.modifiers.contains(KeyModifiers::CONTROL)
                    && !event.modifiers.contains(KeyModifiers::ALT) =>
            {
                self.input.insert(self.input_cursor, character);
                self.input_cursor += character.len_utf8();
            }
            _ => {}
        }
    }

    fn navigate_input_history(&mut self, previous: bool) {
        let current = self.input.clone();
        let replacement = match self.input_mode {
            InputMode::Command if previous => self.command_history.previous(&current),
            InputMode::Command => self.command_history.next(),
            InputMode::Search if previous => self.search_history.previous(&current),
            InputMode::Search => self.search_history.next(),
            InputMode::Normal | InputMode::Filter => None,
        };
        if let Some(replacement) = replacement {
            self.input = replacement;
            self.input_cursor = self.input.len();
        }
    }

    fn move_input_cursor_left(&mut self) {
        if let Some((start, _)) = self.input[..self.input_cursor].grapheme_indices(true).next_back()
        {
            self.input_cursor = start;
        }
    }

    fn move_input_cursor_right(&mut self) {
        if let Some((_, grapheme)) = self.input[self.input_cursor..].grapheme_indices(true).next() {
            self.input_cursor += grapheme.len();
        }
    }

    fn complete_input(&mut self) {
        if let Some(mut completion) = self.completion.take()
            && !completion.candidates.is_empty()
        {
            let index = completion.next % completion.candidates.len();
            self.input = completion.candidates[index].clone();
            self.input_cursor = self.input.len();
            completion.next = index.saturating_add(1);
            self.completion = Some(completion);
            return;
        }

        let candidates = completion_candidates(&self.input, self.input_cursor, &self.cwd);
        if let Some(first) = candidates.first() {
            self.input = first.clone();
            self.input_cursor = self.input.len();
            self.completion = if candidates.len() == 1 && input_ends_with_separator(&self.input) {
                None
            } else {
                Some(CompletionState { candidates, next: 1 })
            };
        }
    }

    fn reset_active_history_navigation(&mut self) {
        match self.input_mode {
            InputMode::Command => self.command_history.reset_navigation(),
            InputMode::Search => self.search_history.reset_navigation(),
            InputMode::Normal | InputMode::Filter => {}
        }
    }

    fn record_input_history(&mut self, mode: InputMode, value: &str) {
        let changed = match mode {
            InputMode::Command => self.command_history.record(value),
            InputMode::Search => self.search_history.record(value),
            InputMode::Normal | InputMode::Filter => false,
        };
        if changed {
            self.save_input_history();
        }
    }

    fn save_input_history(&mut self) {
        let history = SavedHistory {
            commands: self.command_history.entries().to_vec(),
            searches: self.search_history.entries().to_vec(),
        };
        if let Err(error) = self.store.save_history(&history) {
            let suffix = format!("history was not saved: {error}");
            self.message = Some(
                self.message
                    .take()
                    .map_or(suffix.clone(), |message| format!("{message}; {suffix}")),
            );
        }
    }

    fn handle_overlay_key(&mut self, event: KeyEvent) {
        let Some(mut overlay) = self.overlay.take() else {
            return;
        };
        if self.input_mode == InputMode::Filter {
            self.handle_filter_key(event, overlay);
            return;
        }
        let page = self.overlay_page_rows.saturating_sub(1).max(1);
        let half_page = (self.overlay_page_rows / 2).max(1);
        match event.code {
            KeyCode::Esc => {
                self.input_mode = InputMode::Normal;
                self.file_picker = None;
                return;
            }
            KeyCode::Char('/') if overlay.filter.is_some() => {
                self.input_mode = InputMode::Filter;
                self.message = None;
                self.overlay = Some(overlay);
                return;
            }
            KeyCode::Enter if overlay.kind == OverlayKind::Toc => {
                let selected = selected_original_index(&overlay);
                let offset = self.loaded.as_ref().and_then(|loaded| {
                    selected.and_then(|index| {
                        loaded.document().toc().get(index).map(|entry| entry.offset())
                    })
                });
                if let Some(offset) = offset {
                    self.goto_byte(offset);
                } else {
                    self.overlay = Some(overlay);
                }
                return;
            }
            KeyCode::Enter if overlay.kind == OverlayKind::Bookmarks => {
                let offset = selected_original_index(&overlay)
                    .and_then(|index| self.bookmarks.get(index))
                    .map(|bookmark| bookmark.position);
                if let Some(offset) = offset {
                    self.goto_byte(offset);
                } else {
                    self.overlay = Some(overlay);
                }
                return;
            }
            KeyCode::Enter if overlay.kind == OverlayKind::Recent => {
                let path = selected_original_index(&overlay)
                    .and_then(|index| self.recent_books.get(index))
                    .map(|book| book.path.clone());
                if let Some(path) = path {
                    self.start_load(path);
                } else {
                    self.overlay = Some(overlay);
                }
                return;
            }
            KeyCode::Enter if overlay.kind == OverlayKind::Files => {
                let entry = selected_original_index(&overlay)
                    .and_then(|index| self.file_picker.as_ref()?.entries.get(index))
                    .cloned();
                match entry {
                    Some(entry) if entry.is_directory => self.show_file_picker(entry.path),
                    Some(entry) => self.start_load(entry.path),
                    None => self.overlay = Some(overlay),
                }
                return;
            }
            KeyCode::Enter if overlay.kind == OverlayKind::SearchResults => {
                let hit = self
                    .search_session
                    .as_ref()
                    .and_then(|session| session.previews.get(overlay.selected))
                    .cloned();
                if let Some(hit) = hit {
                    let ordinal = hit.ordinal;
                    let total = self.search_session.as_ref().map_or(0, |session| session.total);
                    if let Some(session) = self.search_session.as_mut() {
                        session.current = Some(hit.clone());
                    }
                    self.goto_search_hit(hit.range);
                    self.message = Some(format!("Match {}/{}", ordinal.saturating_add(1), total));
                } else {
                    self.overlay = Some(overlay);
                }
                return;
            }
            KeyCode::Char('x') if overlay.kind == OverlayKind::Bookmarks => {
                if let Some(index) = selected_original_index(&overlay) {
                    self.delete_bookmark(index);
                }
                if let Some(filter) = overlay.filter.as_mut() {
                    filter.refresh(&self.bookmark_items());
                    overlay.selected = overlay.selected.min(filter.len().saturating_sub(1));
                    self.overlay_max_position = filter.len().saturating_sub(1);
                } else {
                    overlay.selected = overlay.selected.min(self.bookmarks.len().saturating_sub(1));
                    self.overlay_max_position = self.bookmarks.len().saturating_sub(1);
                }
                self.overlay = Some(overlay);
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

    fn handle_filter_key(&mut self, event: KeyEvent, mut overlay: OverlayState) {
        match event.code {
            KeyCode::Esc | KeyCode::Enter => {
                self.input_mode = InputMode::Normal;
            }
            KeyCode::Backspace => {
                let mut query = overlay
                    .filter
                    .as_ref()
                    .map_or_else(String::new, |filter| filter.query().into());
                if let Some((start, _)) = query.grapheme_indices(true).next_back() {
                    query.truncate(start);
                }
                self.update_overlay_filter(&mut overlay, query);
            }
            KeyCode::Char('u') if event.modifiers.contains(KeyModifiers::CONTROL) => {
                self.update_overlay_filter(&mut overlay, String::new());
            }
            KeyCode::Char(character)
                if !event.modifiers.contains(KeyModifiers::CONTROL)
                    && !event.modifiers.contains(KeyModifiers::ALT) =>
            {
                let mut query = overlay
                    .filter
                    .as_ref()
                    .map_or_else(String::new, |filter| filter.query().into());
                query.push(character);
                self.update_overlay_filter(&mut overlay, query);
            }
            _ => {}
        }
        self.overlay = Some(overlay);
    }

    fn update_overlay_filter(&mut self, overlay: &mut OverlayState, query: String) {
        let labels = self.unfiltered_overlay_items(overlay.kind);
        if let Some(filter) = overlay.filter.as_mut() {
            overlay.selected = filter.update(query, &labels, overlay.selected);
            self.overlay_max_position = filter.len().saturating_sub(1);
            self.overlay_list_offset = 0;
        }
    }

    fn begin_input(&mut self, mode: InputMode) {
        self.input_mode = mode;
        self.input.clear();
        self.input_cursor = 0;
        self.completion = None;
        self.reset_active_history_navigation();
        self.message = None;
    }

    fn show_overlay(&mut self, kind: OverlayKind, selected: usize) {
        let total = self.unfiltered_overlay_items(kind).len();
        let mut overlay = OverlayState::new(kind, selected);
        if kind.is_filterable() && total > 0 {
            overlay.filter = Some(FilteredList::new(total));
        }
        self.input_mode = InputMode::Normal;
        self.overlay = Some(overlay);
        self.file_picker = None;
        self.overlay_list_offset = 0;
    }

    fn show_file_picker(&mut self, directory: PathBuf) {
        match read_directory(&directory) {
            Ok((directory, entries)) => {
                let mut overlay = OverlayState::new(OverlayKind::Files, 0);
                if !entries.is_empty() {
                    overlay.filter = Some(FilteredList::new(entries.len()));
                }
                self.file_picker = Some(FilePickerState { directory, entries });
                self.input_mode = InputMode::Normal;
                self.overlay = Some(overlay);
                self.overlay_list_offset = 0;
                self.overlay_max_position = self
                    .file_picker
                    .as_ref()
                    .map_or(0, |picker| picker.entries.len().saturating_sub(1));
                self.message = None;
            }
            Err(error) => {
                self.file_picker = None;
                self.overlay = None;
                self.message =
                    Some(format!("Cannot open directory {}: {error}", directory.display()));
            }
        }
    }

    fn execute_command(&mut self, raw: &str) {
        match parse_command(raw) {
            Ok(Command::Open(path)) => self.open_path(self.resolve_path(&path)),
            Ok(Command::Quit) => self.should_quit = true,
            Ok(Command::Toc) => {
                if self.loaded.as_ref().is_some_and(|loaded| !loaded.document().toc().is_empty()) {
                    self.show_overlay(OverlayKind::Toc, self.current_toc_index());
                } else {
                    self.message = Some("This document has no table of contents".to_owned());
                }
            }
            Ok(Command::Goto(percent)) => self.goto_percent(percent),
            Ok(Command::Mark(label)) => self.add_bookmark(label),
            Ok(Command::Marks) => {
                if self.loaded.is_some() {
                    let selected = self.current_bookmark_index();
                    self.show_overlay(OverlayKind::Bookmarks, selected);
                } else {
                    self.message = Some("No document is open".to_owned());
                }
            }
            Ok(Command::Recent) => {
                let recent = self.store.recent_books(RECENT_BOOK_LIMIT);
                let warning_count = recent.warnings.len();
                self.recent_books = recent.books;
                self.state_warnings.extend(recent.warnings);
                if warning_count > 0 {
                    self.message = Some(format!(
                        "Skipped {warning_count} unreadable recent state record{}",
                        if warning_count == 1 { "" } else { "s" }
                    ));
                }
                self.show_overlay(OverlayKind::Recent, 0);
            }
            Ok(Command::Exact(pattern)) => self.start_search(
                SearchQuery::new(SearchKind::ExactLiteral, pattern),
                SearchDirection::Forward,
                None,
            ),
            Ok(Command::Regex(pattern)) => self.start_search(
                SearchQuery::new(SearchKind::Regex, pattern),
                SearchDirection::Forward,
                None,
            ),
            Ok(Command::Results) => {
                if let Some(session) = self.search_session.as_ref()
                    && !session.previews.is_empty()
                {
                    let selected = session.current.as_ref().and_then(|current| {
                        session.previews.iter().position(|hit| hit.ordinal == current.ordinal)
                    });
                    self.show_overlay(OverlayKind::SearchResults, selected.unwrap_or(0));
                } else {
                    self.message = Some("No search results".to_owned());
                }
            }
            Ok(Command::Info) => {
                if self.loaded.is_some() {
                    self.show_overlay(OverlayKind::Info, 0);
                } else {
                    self.message = Some("No document is open".to_owned());
                }
            }
            Ok(Command::Help) => {
                self.show_overlay(OverlayKind::Help, 0);
            }
            Ok(Command::Theme(None)) => {
                self.message = Some(format!("Theme: {}", self.theme_choice));
            }
            Ok(Command::Theme(Some(theme))) => {
                self.theme_choice = theme;
                self.message = Some(match self.store.save_theme(theme) {
                    Ok(()) => format!("Theme: {theme}"),
                    Err(error) => format!("Theme changed to {theme}, but was not saved: {error}"),
                });
            }
            Ok(Command::ClearHistory(scope)) => self.clear_input_history(scope),
            Err(error) => self.message = Some(error),
        }
    }

    fn clear_input_history(&mut self, scope: HistoryScope) {
        match scope {
            HistoryScope::All => {
                self.command_history.clear();
                self.search_history.clear();
            }
            HistoryScope::Commands => self.command_history.clear(),
            HistoryScope::Searches => self.search_history.clear(),
        }
        self.message = Some(match scope {
            HistoryScope::All => "History cleared".to_owned(),
            HistoryScope::Commands => "Command history cleared".to_owned(),
            HistoryScope::Searches => "Search history cleared".to_owned(),
        });
        self.save_input_history();
    }

    fn start_search(
        &mut self,
        query: SearchQuery,
        direction: SearchDirection,
        from: Option<usize>,
    ) {
        if query.pattern.is_empty() {
            self.message = Some("Search text cannot be empty".to_owned());
            return;
        }
        let Some(loaded) = self.loaded.clone() else {
            self.message = Some("No document is open".to_owned());
            return;
        };
        let default_from = self.viewport.as_ref().map_or(0, Viewport::anchor);
        let from = from.unwrap_or(default_from);
        self.search_generation = self.search_generation.wrapping_add(1);
        let generation = self.search_generation;
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let result =
                analyze(loaded.document().text(), &query, from, direction, SEARCH_PREVIEW_LIMIT);
            let _ = sender.send(SearchTaskResult {
                generation,
                payload: SearchTaskPayload::Analyze { query, result },
            });
        });
        self.search_task = Some((generation, receiver));
        self.message = Some("Searching…".to_owned());
    }

    fn repeat_search(&mut self, direction: SearchDirection) {
        let Some(session) = self.search_session.as_ref() else {
            self.message = Some("No previous search".to_owned());
            return;
        };
        let query = session.query.clone();
        let from = match (direction, self.current_match.as_ref()) {
            (SearchDirection::Forward, Some(found)) => found.end,
            (SearchDirection::Backward, Some(found)) => found.start,
            _ => self.viewport.as_ref().map_or(0, Viewport::anchor),
        };
        if self.current_match.is_none() || session.current.is_none() {
            self.start_search(query, direction, Some(from));
            return;
        }
        let Some(loaded) = self.loaded.clone() else {
            return;
        };
        self.search_generation = self.search_generation.wrapping_add(1);
        let generation = self.search_generation;
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let result = find_next(loaded.document().text(), &query, from, direction);
            let _ = sender.send(SearchTaskResult {
                generation,
                payload: SearchTaskPayload::Step { direction, result },
            });
        });
        self.search_task = Some((generation, receiver));
        self.message = Some("Searching…".to_owned());
    }

    fn finish_search_analysis(
        &mut self,
        query: SearchQuery,
        result: Result<SearchAnalysis, SearchError>,
    ) {
        match result {
            Ok(analysis) => {
                let current = analysis.current.clone();
                self.search_session = Some(SearchSession {
                    query,
                    current: analysis.current,
                    total: analysis.total,
                    previews: analysis.previews,
                });
                if let Some(hit) = current {
                    self.goto_search_hit(hit.range);
                    self.message =
                        Some(format!("Match {}/{}", hit.ordinal.saturating_add(1), analysis.total));
                } else {
                    self.current_match = None;
                    self.message = Some("No matches".to_owned());
                }
            }
            Err(error) => self.message = Some(error.to_string()),
        }
    }

    fn finish_search_step(
        &mut self,
        direction: SearchDirection,
        result: Result<Option<Range<usize>>, SearchError>,
    ) {
        match result {
            Ok(Some(range)) => {
                let Some(session) = self.search_session.as_mut() else {
                    return;
                };
                let ordinal = match (direction, session.current.as_ref(), session.total) {
                    (_, _, 0) => 0,
                    (SearchDirection::Forward, Some(current), total) => {
                        current.ordinal.saturating_add(1) % total
                    }
                    (SearchDirection::Backward, Some(current), total) => {
                        current.ordinal.checked_sub(1).unwrap_or(total.saturating_sub(1))
                    }
                    _ => 0,
                };
                let hit = SearchHit { range: range.clone(), ordinal };
                session.current = Some(hit.clone());
                insert_search_preview(&mut session.previews, hit, SEARCH_PREVIEW_LIMIT);
                let total = session.total;
                self.goto_search_hit(range);
                self.message = Some(format!("Match {}/{}", ordinal.saturating_add(1), total));
            }
            Ok(None) => {
                self.current_match = None;
                self.message = Some("No matches".to_owned());
            }
            Err(error) => self.message = Some(error.to_string()),
        }
    }

    fn goto_search_hit(&mut self, range: Range<usize>) {
        if let (Some(loaded), Some(viewport)) = (self.loaded.as_ref(), self.viewport.as_mut()) {
            viewport.goto_byte(loaded.document().text(), range.start);
            self.current_match = Some(range);
            self.mark_moved();
        }
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

    fn add_bookmark(&mut self, label: Option<String>) {
        let Some(loaded) = self.loaded.as_ref() else {
            self.message = Some("No document is open".to_owned());
            return;
        };
        let label = label.map(|label| label.trim().to_owned()).filter(|label| !label.is_empty());
        if label.as_ref().is_some_and(|label| label.chars().count() > BOOKMARK_LABEL_LIMIT) {
            self.message =
                Some(format!("Bookmark labels cannot exceed {BOOKMARK_LABEL_LIMIT} characters"));
            return;
        }
        let position = self.viewport.as_ref().map_or(0, Viewport::anchor);
        let previous = self.bookmarks.clone();
        if let Some(bookmark) =
            self.bookmarks.iter_mut().find(|bookmark| bookmark.position == position)
        {
            bookmark.label = label;
        } else {
            self.bookmarks.push(Bookmark { position, label, created_unix_ms: now_unix_ms() });
            self.bookmarks.sort_by_key(|bookmark| bookmark.position);
        }
        match self.store.save_bookmarks(loaded.path(), loaded.fingerprint(), &self.bookmarks) {
            Ok(()) => self.message = Some("Bookmark saved".to_owned()),
            Err(error) => {
                self.bookmarks = previous;
                self.message = Some(error.to_string());
            }
        }
    }

    fn delete_bookmark(&mut self, index: usize) {
        if index >= self.bookmarks.len() {
            return;
        }
        let Some(loaded) = self.loaded.as_ref() else {
            return;
        };
        let previous = self.bookmarks.clone();
        self.bookmarks.remove(index);
        match self.store.save_bookmarks(loaded.path(), loaded.fingerprint(), &self.bookmarks) {
            Ok(()) => self.message = Some("Bookmark deleted".to_owned()),
            Err(error) => {
                self.bookmarks = previous;
                self.message = Some(error.to_string());
            }
        }
    }

    fn current_bookmark_index(&self) -> usize {
        let anchor = self.viewport.as_ref().map_or(0, Viewport::anchor);
        self.bookmarks.partition_point(|bookmark| bookmark.position <= anchor).saturating_sub(1)
    }

    fn bookmark_items(&self) -> Vec<String> {
        let Some(loaded) = self.loaded.as_ref() else {
            return Vec::new();
        };
        if self.bookmarks.is_empty() {
            return Vec::new();
        }
        let text = loaded.document().text();
        let total_chars = loaded.document().total_chars().max(1);
        let mut current_byte = 0;
        let mut current_chars = 0;
        self.bookmarks
            .iter()
            .map(|bookmark| {
                let position = floor_char_boundary(text, bookmark.position.min(text.len()));
                current_chars += text[current_byte..position].chars().count();
                current_byte = position;
                let percent = current_chars as f64 * 100.0 / total_chars as f64;
                let label = bookmark
                    .label
                    .clone()
                    .unwrap_or_else(|| bookmark_fallback_label(loaded, position));
                format!("{label} · {percent:.1}%")
            })
            .collect()
    }

    fn recent_items(&self) -> Vec<String> {
        if self.recent_books.is_empty() {
            return Vec::new();
        }
        let now = now_unix_ms();
        self.recent_books
            .iter()
            .map(|book| {
                let name = book.path.file_name().map_or_else(
                    || book.path.display().to_string(),
                    |name| name.to_string_lossy().into_owned(),
                );
                let parent = book.path.parent().map_or_else(String::new, compact_path);
                let age = relative_age(now.saturating_sub(book.updated_unix_ms));
                if parent.is_empty() {
                    format!("{name} · {age}")
                } else {
                    format!("{name} · {parent} · {age}")
                }
            })
            .collect()
    }

    fn search_result_items(&self) -> Vec<String> {
        let (Some(loaded), Some(session)) = (self.loaded.as_ref(), self.search_session.as_ref())
        else {
            return Vec::new();
        };
        if session.previews.is_empty() {
            return Vec::new();
        }
        session
            .previews
            .iter()
            .map(|hit| {
                format!(
                    "{}/{}  {}",
                    hit.ordinal.saturating_add(1),
                    session.total,
                    search_context(loaded.document().text(), &hit.range, SEARCH_CONTEXT_WIDTH,)
                        .text
                )
            })
            .collect()
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
        for warning in &self.state_warnings {
            lines.push(format!("State warning [{}]: {}", warning.code(), warning.message()));
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
    Mark(Option<String>),
    Marks,
    Recent,
    Exact(String),
    Regex(String),
    Results,
    Info,
    Help,
    Theme(Option<ThemeChoice>),
    ClearHistory(HistoryScope),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HistoryScope {
    All,
    Commands,
    Searches,
}

const COMMAND_COMPLETIONS: &[&str] = &[
    "e",
    "open",
    "q",
    "quit",
    "toc",
    "mark",
    "marks",
    "bookmarks",
    "recent",
    "exact",
    "re",
    "regex",
    "results",
    "info",
    "help",
    "h",
    "theme",
    "history",
    "goto",
];

fn completion_candidates(input: &str, cursor: usize, cwd: &Path) -> Vec<String> {
    let cursor = cursor.min(input.len());
    if !input.is_char_boundary(cursor) {
        return Vec::new();
    }
    let before = &input[..cursor];
    let Some(command_end) = before.find(char::is_whitespace) else {
        let prefix = before.to_ascii_lowercase();
        return COMMAND_COMPLETIONS
            .iter()
            .filter(|command| command.starts_with(&prefix))
            .map(|command| (*command).to_owned())
            .collect();
    };
    let command = before[..command_end].to_ascii_lowercase();
    if command != "e" && command != "open" {
        return Vec::new();
    }
    let argument_start = before[command_end..]
        .char_indices()
        .find(|(_, character)| !character.is_whitespace())
        .map_or(cursor, |(offset, _)| command_end + offset);
    if argument_start > cursor {
        return Vec::new();
    }
    let raw_path = &before[argument_start..];
    complete_path_candidates(raw_path, cwd)
        .into_iter()
        .map(|replacement| {
            let mut candidate = input.to_owned();
            candidate.replace_range(argument_start..cursor, &replacement);
            candidate
        })
        .collect()
}

fn complete_path_candidates(raw: &str, cwd: &Path) -> Vec<String> {
    let quote = raw.chars().next().filter(|character| *character == '\'' || *character == '"');
    let unquoted = quote.map_or(raw, |quote| &raw[quote.len_utf8()..]);
    let separator = unquoted
        .char_indices()
        .rev()
        .find(|(_, character)| *character == '/' || *character == '\\');
    let (base, fragment) = separator
        .map_or(("", unquoted), |(offset, _)| (&unquoted[..=offset], &unquoted[offset + 1..]));
    let directory = resolve_completion_path(base, cwd);
    let Ok((_, entries)) = read_directory(&directory) else {
        return Vec::new();
    };
    let separator = base
        .chars()
        .last()
        .filter(|character| *character == '/' || *character == '\\')
        .unwrap_or(std::path::MAIN_SEPARATOR);
    entries
        .into_iter()
        .filter(|entry| !entry.is_parent && path_name_matches(&entry.name, fragment))
        .map(|entry| {
            let mut replacement = format!("{base}{}", entry.name);
            if entry.is_directory {
                replacement.push(separator);
            }
            quote.map_or_else(|| replacement.clone(), |quote| format!("{quote}{replacement}"))
        })
        .collect()
}

fn resolve_completion_path(raw: &str, cwd: &Path) -> PathBuf {
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
    if path.is_absolute() { path } else { cwd.join(path) }
}

fn path_name_matches(name: &str, fragment: &str) -> bool {
    if cfg!(windows) {
        name.to_lowercase().starts_with(&fragment.to_lowercase())
    } else {
        name.starts_with(fragment)
    }
}

fn input_ends_with_separator(input: &str) -> bool { input.trim_end().ends_with(['/', '\\']) }

fn parse_command(raw: &str) -> Result<Command, String> {
    let raw = raw.trim();
    let (name, arguments) =
        raw.split_once(char::is_whitespace).map_or((raw, ""), |(name, rest)| (name, rest.trim()));
    match name {
        "e" | "open" if !arguments.is_empty() => Ok(Command::Open(arguments.to_owned())),
        "e" | "open" => Err("Usage: :e <path>".to_owned()),
        "q" | "quit" => Ok(Command::Quit),
        "toc" => Ok(Command::Toc),
        "mark" => Ok(Command::Mark((!arguments.is_empty()).then(|| arguments.to_owned()))),
        "marks" | "bookmarks" => Ok(Command::Marks),
        "recent" => Ok(Command::Recent),
        "exact" if !arguments.is_empty() => Ok(Command::Exact(arguments.to_owned())),
        "exact" => Err("Usage: :exact <text>".to_owned()),
        "re" | "regex" if !arguments.is_empty() => Ok(Command::Regex(arguments.to_owned())),
        "re" | "regex" => Err("Usage: :re <pattern>".to_owned()),
        "results" => Ok(Command::Results),
        "info" => Ok(Command::Info),
        "help" | "h" => Ok(Command::Help),
        "theme" if arguments.is_empty() => Ok(Command::Theme(None)),
        "theme" => arguments
            .parse::<ThemeChoice>()
            .map(Some)
            .map(Command::Theme)
            .map_err(|()| "Usage: :theme <auto|light|dark>".to_owned()),
        "history" => match arguments {
            "clear" | "clear all" => Ok(Command::ClearHistory(HistoryScope::All)),
            "clear commands" => Ok(Command::ClearHistory(HistoryScope::Commands)),
            "clear searches" => Ok(Command::ClearHistory(HistoryScope::Searches)),
            _ => Err("Usage: :history clear [commands|searches|all]".to_owned()),
        },
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

fn selected_original_index(overlay: &OverlayState) -> Option<usize> {
    overlay
        .filter
        .as_ref()
        .map_or(Some(overlay.selected), |filter| filter.original_index(overlay.selected))
}

fn filtered_overlay_title(name: &str, selected: usize, matches: usize, total: usize) -> String {
    let current = if matches == 0 { 0 } else { selected.min(matches - 1) + 1 };
    format!("{name} · {current}/{matches} matches · {total} total")
}

fn floor_char_boundary(text: &str, mut offset: usize) -> usize {
    while offset > 0 && !text.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

fn bookmark_fallback_label(loaded: &LoadedDocument, position: usize) -> String {
    if let Some(entry) =
        loaded.document().toc().iter().rev().find(|entry| entry.offset() <= position)
    {
        return entry.label().to_owned();
    }
    let text = loaded.document().text();
    let start = text[..position].rfind('\n').map_or(0, |offset| offset + 1);
    let end = text[position..].find('\n').map_or(text.len(), |offset| position + offset);
    let line = text[start..end].split_whitespace().collect::<Vec<_>>().join(" ");
    if line.is_empty() {
        return "Untitled bookmark".to_owned();
    }
    let mut characters = line.chars();
    let preview = characters.by_ref().take(40).collect::<String>();
    if characters.next().is_some() { format!("{preview}…") } else { preview }
}

fn compact_path(path: &Path) -> String {
    if let Some(home) = UserDirs::new().map(|directories| directories.home_dir().to_path_buf())
        && let Ok(relative) = path.strip_prefix(home)
    {
        return if relative.as_os_str().is_empty() {
            "~".to_owned()
        } else {
            format!("~/{}", relative.display())
        };
    }
    path.display().to_string()
}

fn relative_age(milliseconds: u128) -> String {
    let seconds = milliseconds / 1_000;
    if seconds < 60 {
        "just now".to_owned()
    } else if seconds < 60 * 60 {
        format!("{}m ago", seconds / 60)
    } else if seconds < 24 * 60 * 60 {
        format!("{}h ago", seconds / (60 * 60))
    } else {
        format!("{}d ago", seconds / (24 * 60 * 60))
    }
}

fn insert_search_preview(previews: &mut Vec<SearchHit>, hit: SearchHit, limit: usize) {
    if limit == 0 {
        previews.clear();
        return;
    }
    let current = hit.ordinal;
    if let Some(existing) = previews.iter_mut().find(|preview| preview.ordinal == current) {
        *existing = hit;
    } else {
        previews.push(hit);
        previews.sort_by_key(|preview| preview.ordinal);
    }
    while previews.len() > limit {
        let first_distance = current.saturating_sub(previews[0].ordinal);
        let last_distance = previews.last().unwrap().ordinal.saturating_sub(current);
        if first_distance > last_distance {
            previews.remove(0);
        } else {
            previews.pop();
        }
    }
}

fn search_context(text: &str, range: &Range<usize>, width: usize) -> SearchContext {
    let start = floor_char_boundary(text, range.start.min(text.len()));
    let raw_end = floor_char_boundary(text, range.end.min(text.len()));
    let line_start = text[..start].rfind('\n').map_or(0, |offset| offset + 1);
    let line_end = text[start..].find('\n').map_or(text.len(), |offset| start + offset);
    let end = raw_end.max(start).min(line_end);
    let side = width.saturating_sub(8) / 2;
    let before_full = &text[line_start..start];
    let before_chars = before_full.chars().count();
    let mut before = before_full.chars().rev().take(side).collect::<Vec<_>>();
    before.reverse();
    let before = before.into_iter().collect::<String>();
    let matched = text[start..end].chars().take(width / 2).collect::<String>();
    let after_full = &text[end..line_end];
    let after = after_full.chars().take(side).collect::<String>();
    let mut text = String::new();
    if before_chars > side {
        text.push('…');
    }
    text.push_str(&before);
    let emphasis_start = text.len();
    text.push_str(&matched);
    let emphasis_end = text.len();
    text.push_str(&after);
    if after_full.chars().count() > side {
        text.push('…');
    }
    SearchContext { text, emphasis: emphasis_start..emphasis_end }
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

    fn wait_for_search(app: &mut App) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while app.search_task.is_some() && Instant::now() < deadline {
            app.poll_tasks();
            thread::yield_now();
        }
        assert!(app.search_task.is_none(), "search did not finish before the test deadline");
    }

    #[test]
    fn parses_open_path_without_splitting_spaces() {
        assert_eq!(
            parse_command("e books/my novel.epub").unwrap(),
            Command::Open("books/my novel.epub".to_owned())
        );
    }

    #[test]
    fn command_completion_completes_commands_and_paths() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("alpha.txt"), "text").unwrap();
        fs::write(directory.path().join("beta.md"), "# Markdown").unwrap();
        fs::write(directory.path().join("ignored.pdf"), "pdf").unwrap();
        fs::create_dir(directory.path().join("nested")).unwrap();
        fs::write(directory.path().join("nested/inside.txt"), "text").unwrap();
        let store = StateStore::at(directory.path().join("state")).unwrap();
        let mut app = App::new(directory.path().to_path_buf(), store);

        app.handle_key(KeyEvent::new(KeyCode::Char(':'), KeyModifiers::NONE));
        for character in "th".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE));
        }
        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.input(), "theme");

        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Char(':'), KeyModifiers::NONE));
        for character in "e a".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE));
        }
        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.input(), "e alpha.txt");

        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Char(':'), KeyModifiers::NONE));
        for character in "e n".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE));
        }
        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        let nested_prefix = format!("e nested{}", std::path::MAIN_SEPARATOR);
        assert_eq!(app.input(), nested_prefix);
        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.input(), format!("{nested_prefix}inside.txt"));
    }

    #[test]
    fn command_input_supports_grapheme_cursor_editing() {
        let directory = tempfile::tempdir().unwrap();
        let store = StateStore::at(directory.path().join("state")).unwrap();
        let mut app = App::new(directory.path().to_path_buf(), store);
        app.handle_key(KeyEvent::new(KeyCode::Char(':'), KeyModifiers::NONE));
        for character in "ab中文".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE));
        }
        app.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(app.input(), "ab文");
        app.handle_key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Char('前'), KeyModifiers::NONE));
        assert_eq!(app.input(), "前ab文");
        app.handle_key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
        assert_eq!(app.input_cursor(), app.input().len());
    }

    #[test]
    fn opening_a_directory_shows_supported_files_and_opens_the_selection() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir(directory.path().join("nested")).unwrap();
        fs::write(directory.path().join("book.txt"), "text").unwrap();
        fs::write(directory.path().join("notes.markdown"), "# Notes").unwrap();
        fs::write(directory.path().join("ignored.pdf"), "pdf").unwrap();
        let store = StateStore::at(directory.path().join("state")).unwrap();
        let mut app = App::new(directory.path().to_path_buf(), store);

        app.open_path(directory.path().to_path_buf());
        assert_eq!(app.overlay().map(|overlay| overlay.kind), Some(OverlayKind::Files));
        let items = app.overlay_items();
        assert!(items.contains(&"nested/".to_owned()));
        assert!(items.contains(&"book.txt".to_owned()));
        assert!(items.contains(&"notes.markdown".to_owned()));
        assert!(!items.contains(&"ignored.pdf".to_owned()));

        let selected = items.iter().position(|item| item == "book.txt").unwrap();
        app.overlay.as_mut().unwrap().selected = selected;
        app.set_overlay_layout(items.len(), items.len());
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.overlay().is_none());
        let expected = fs::canonicalize(directory.path().join("book.txt")).unwrap();
        assert_eq!(app.loading_path.as_deref(), Some(expected.as_path()));
    }

    #[test]
    fn parses_and_validates_percentage() {
        assert_eq!(parse_command("goto 38%").unwrap(), Command::Goto(38.0));
        assert!(parse_command("goto 101%").is_err());
    }

    #[test]
    fn parses_v2_commands_without_splitting_arguments() {
        assert_eq!(
            parse_command("mark 重要 转折").unwrap(),
            Command::Mark(Some("重要 转折".to_owned()))
        );
        assert_eq!(parse_command("mark").unwrap(), Command::Mark(None));
        assert_eq!(
            parse_command("exact 第一章 风起").unwrap(),
            Command::Exact("第一章 风起".to_owned())
        );
        assert_eq!(
            parse_command("re 第[一二]章").unwrap(),
            Command::Regex("第[一二]章".to_owned())
        );
        assert_eq!(parse_command("marks").unwrap(), Command::Marks);
        assert_eq!(parse_command("recent").unwrap(), Command::Recent);
        assert_eq!(parse_command("results").unwrap(), Command::Results);
        assert_eq!(parse_command("theme").unwrap(), Command::Theme(None));
        assert_eq!(parse_command("theme light").unwrap(), Command::Theme(Some(ThemeChoice::Light)));
        assert!(parse_command("theme sepia").is_err());
        assert_eq!(
            parse_command("history clear").unwrap(),
            Command::ClearHistory(HistoryScope::All)
        );
        assert_eq!(
            parse_command("history clear searches").unwrap(),
            Command::ClearHistory(HistoryScope::Searches)
        );
        assert!(parse_command("history").is_err());
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
    fn slash_focuses_search_input_without_becoming_visible_text() {
        let directory = tempfile::tempdir().unwrap();
        let store = StateStore::at(directory.path().join("state")).unwrap();
        let mut app = App::new(directory.path().to_path_buf(), store);

        app.handle_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
        assert_eq!(app.input_mode(), InputMode::Search);
        assert_eq!(app.composer_text(), "");
        assert_eq!(app.composer_prompt(), "⌕ ");

        app.handle_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE));
        assert_eq!(app.composer_text(), "q");
        assert_eq!(app.composer_cursor_width(), 1);
    }

    #[test]
    fn theme_command_applies_and_persists_the_preference() {
        let directory = tempfile::tempdir().unwrap();
        let store = StateStore::at(directory.path().join("state")).unwrap();
        let mut app = App::new(directory.path().to_path_buf(), store.clone());

        app.execute_command("theme light");

        assert_eq!(app.theme_choice(), ThemeChoice::Light);
        assert_eq!(store.load_theme(), ThemeChoice::Light);
        assert_eq!(app.message.as_deref(), Some("Theme: light"));
    }

    #[test]
    fn command_history_restores_a_cross_session_draft_with_up_and_down() {
        let directory = tempfile::tempdir().unwrap();
        let store = StateStore::at(directory.path().join("state")).unwrap();
        let mut first = App::new(directory.path().to_path_buf(), store.clone());
        first.handle_key(KeyEvent::new(KeyCode::Char(':'), KeyModifiers::NONE));
        for character in "unknown-command".chars() {
            first.handle_key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE));
        }
        first.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        let mut second = App::new(directory.path().to_path_buf(), store);
        second.handle_key(KeyEvent::new(KeyCode::Char(':'), KeyModifiers::NONE));
        for character in "draft".chars() {
            second.handle_key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE));
        }
        second.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(second.composer_text(), "unknown-command");
        second.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(second.composer_text(), "draft");
    }

    #[test]
    fn search_history_is_persisted_separately_and_can_be_cleared() {
        let directory = tempfile::tempdir().unwrap();
        let book = directory.path().join("book.txt");
        fs::write(&book, "第一章 风起").unwrap();
        let store = StateStore::at(directory.path().join("state")).unwrap();
        let loaded =
            open_document(DocumentSource::from_path(book), LoadOptions::default()).unwrap();
        let mut first = App::new(directory.path().to_path_buf(), store.clone());
        first.install_document(loaded);
        first.handle_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
        for character in "风起".chars() {
            first.handle_key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE));
        }
        first.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        let mut second = App::new(directory.path().to_path_buf(), store.clone());
        second.handle_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
        second.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(second.composer_text(), "风起");
        second.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        second.execute_command("history clear searches");

        let mut third = App::new(directory.path().to_path_buf(), store);
        third.handle_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
        third.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(third.composer_text(), "");
    }

    #[test]
    fn scrolls_non_toc_overlays_with_line_page_and_end_keys() {
        let directory = tempfile::tempdir().unwrap();
        let store = StateStore::at(directory.path().join("state")).unwrap();
        let mut app = App::new(directory.path().to_path_buf(), store);
        app.overlay = Some(OverlayState::new(OverlayKind::Info, 0));
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

        app.overlay = Some(OverlayState::new(OverlayKind::Toc, 0));
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
        app.overlay = Some(OverlayState::new(OverlayKind::Toc, usize::MAX));

        app.set_overlay_layout(3, 0);
        assert_eq!(app.overlay().unwrap().selected, 2);
        app.handle_key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE));
        assert_eq!(app.overlay().unwrap().selected, 2);
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert!(app.overlay().is_none());
        assert_eq!(app.viewport.as_ref().unwrap().anchor(), target);
    }

    #[test]
    fn toc_filter_keeps_visible_and_original_indices_separate() {
        let text = (1..=12)
            .map(|number| format!("第{number}章 标题{number}\n正文。\n"))
            .collect::<String>();
        let (_directory, mut app) = app_with_text(&text);
        let target = app.document().unwrap().document().toc()[11].offset();
        app.execute_command("toc");

        app.handle_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
        for character in "第１２章标题１２".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE));
        }

        assert_eq!(app.input_mode(), InputMode::Filter);
        assert_eq!(app.composer_text(), "第１２章标题１２");
        assert_eq!(app.overlay_items(), ["第12章 标题12"]);
        assert_eq!(
            app.overlay_title().as_deref(),
            Some("Table of contents · 1/1 matches · 12 total")
        );

        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.input_mode(), InputMode::Normal);
        assert!(app.overlay().is_some());
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.overlay().is_none());
        assert_eq!(app.viewport.as_ref().unwrap().anchor(), target);
    }

    #[test]
    fn unmatched_filter_stays_open_and_can_be_cleared() {
        let (_directory, mut app) = app_with_text("第一章 风起\n正文。");
        app.execute_command("toc");
        app.handle_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
        for character in "没有".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE));
        }
        assert_eq!(app.overlay_items(), ["No matching sections"]);

        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.overlay().is_some());

        app.handle_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(app.overlay_items(), ["第一章 风起"]);
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
        app.overlay = Some(OverlayState::new(OverlayKind::Help, 0));
        let help = app.overlay_items().join("\n");

        assert!(help.contains("→/←"));
        assert!(help.contains("PgDn/PgUp"));
        assert!(help.contains("Enter/Esc"));
        assert!(help.contains(":mark/:marks"));
        assert!(help.contains(":recent"));
        assert!(help.contains(":exact/:re"));
        assert!(help.contains(":history clear"));
    }

    #[test]
    fn bookmarks_are_persisted_updated_browsed_and_deleted() {
        let text = "第一章 风起\n正文。\n第二章 云涌\n正文。";
        let (_directory, mut app) = app_with_text(text);
        let path = app.document().unwrap().path().to_path_buf();
        let fingerprint = app.document().unwrap().fingerprint().to_owned();

        app.execute_command("mark 开始");
        assert_eq!(app.bookmarks.len(), 1);
        assert_eq!(app.bookmarks[0].label.as_deref(), Some("开始"));
        app.execute_command("mark 新标签");
        assert_eq!(app.bookmarks.len(), 1);
        assert_eq!(app.bookmarks[0].label.as_deref(), Some("新标签"));

        let second = text.find("第二章").unwrap();
        app.goto_byte(second);
        app.execute_command("mark");
        assert_eq!(app.bookmarks.len(), 2);
        assert_eq!(app.store.load_book(&path, &fingerprint).bookmarks.len(), 2);

        app.execute_command("marks");
        assert_eq!(app.overlay().unwrap().kind, OverlayKind::Bookmarks);
        assert_eq!(app.overlay_title().as_deref(), Some("Bookmarks · 2/2"));
        assert!(app.overlay_items()[1].contains("第二章 云涌"));
        app.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        assert_eq!(app.bookmarks.len(), 1);
        assert_eq!(app.store.load_book(&path, &fingerprint).bookmarks.len(), 1);

        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.overlay().is_none());
        assert_eq!(app.viewport.as_ref().unwrap().anchor(), 0);
    }

    #[test]
    fn filtered_bookmark_deletion_uses_the_original_index() {
        let text = "第一章 风起\n正文。\n第二章 云涌\n正文。";
        let (_directory, mut app) = app_with_text(text);
        app.execute_command("mark keep");
        app.goto_byte(text.find("第二章").unwrap());
        app.execute_command("mark delete-me");
        app.execute_command("marks");

        app.handle_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
        for character in "delete-me".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE));
        }
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));

        assert_eq!(app.bookmarks.len(), 1);
        assert_eq!(app.bookmarks[0].label.as_deref(), Some("keep"));
        assert_eq!(app.overlay_items(), ["No matching bookmarks"]);
    }

    #[test]
    fn empty_bookmarks_overlay_stays_open_on_enter() {
        let (_directory, mut app) = app_with_text("正文。");

        app.execute_command("marks");
        assert_eq!(app.overlay().unwrap().kind, OverlayKind::Bookmarks);
        assert_eq!(app.overlay_items(), ["No bookmarks yet; use :mark [label] to add one"]);

        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert_eq!(app.overlay().unwrap().kind, OverlayKind::Bookmarks);
    }

    #[test]
    fn recent_overlay_opens_the_latest_existing_book() {
        let directory = tempfile::tempdir().unwrap();
        let first = directory.path().join("first.txt");
        let second = directory.path().join("second.txt");
        fs::write(&first, "first").unwrap();
        fs::write(&second, "second").unwrap();
        let store = StateStore::at(directory.path().join("state")).unwrap();
        store.save_progress(&first, "first", 0).unwrap();
        thread::sleep(Duration::from_millis(2));
        store.save_progress(&second, "second", 0).unwrap();
        let mut app = App::new(directory.path().to_path_buf(), store);

        app.execute_command("recent");
        assert_eq!(app.overlay().unwrap().kind, OverlayKind::Recent);
        assert!(app.overlay_items()[0].starts_with("second.txt"));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        let deadline = Instant::now() + Duration::from_secs(2);
        while app.document().is_none() && Instant::now() < deadline {
            app.poll_tasks();
            thread::yield_now();
        }
        assert_eq!(app.document().unwrap().path(), fs::canonicalize(second).unwrap());
    }

    #[test]
    fn empty_recent_overlay_stays_open_on_enter() {
        let directory = tempfile::tempdir().unwrap();
        let store = StateStore::at(directory.path().join("state")).unwrap();
        let mut app = App::new(directory.path().to_path_buf(), store);

        app.execute_command("recent");
        assert_eq!(app.overlay().unwrap().kind, OverlayKind::Recent);
        assert_eq!(app.overlay_items(), ["No readable recent books"]);

        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert_eq!(app.overlay().unwrap().kind, OverlayKind::Recent);
    }

    #[test]
    fn loose_search_builds_a_session_and_results_overlay() {
        let text = "第一章：风起\n正文。\n第一章 风起\n结尾。";
        let (_directory, mut app) = app_with_text(text);

        app.start_search(
            SearchQuery::new(SearchKind::LooseLiteral, "第一章风起"),
            SearchDirection::Forward,
            Some(0),
        );
        wait_for_search(&mut app);

        let session = app.search_session.as_ref().unwrap();
        assert_eq!(session.total, 2);
        assert_eq!(session.current.as_ref().unwrap().ordinal, 0);
        assert_eq!(&text[app.current_match().unwrap()], "第一章：风起");
        assert_eq!(app.message.as_deref(), Some("Match 1/2"));

        app.execute_command("results");
        assert_eq!(app.overlay().unwrap().kind, OverlayKind::SearchResults);
        let items = app.overlay_items();
        assert_eq!(items.len(), 2);
        let emphasis = app.overlay_item_emphasis(0).unwrap();
        assert_eq!(&items[0][emphasis], "第一章：风起");
        assert!(!items[0].contains('‹') && !items[0].contains('›'));
        app.overlay.as_mut().unwrap().selected = 1;
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(&text[app.current_match().unwrap()], "第一章 风起");

        app.handle_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE));
        wait_for_search(&mut app);
        assert_eq!(&text[app.current_match().unwrap()], "第一章：风起");
        assert_eq!(app.message.as_deref(), Some("Match 1/2"));
    }

    #[test]
    fn exact_and_regex_commands_report_results_and_errors() {
        let text = "CHAPTER 1\nchapter 2";
        let (_directory, mut app) = app_with_text(text);

        app.execute_command("exact chapter 2");
        wait_for_search(&mut app);
        assert_eq!(&text[app.current_match().unwrap()], "chapter 2");
        assert_eq!(app.search_session.as_ref().unwrap().total, 1);

        app.execute_command("re (?i)chapter [12]");
        wait_for_search(&mut app);
        assert_eq!(app.search_session.as_ref().unwrap().total, 2);

        app.execute_command("re [");
        wait_for_search(&mut app);
        assert!(app.message.as_deref().unwrap().starts_with("invalid regular expression"));
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
        assert_eq!(store.load_book(&book, &fingerprint).position, 0);
    }
}
