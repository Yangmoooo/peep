use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::theme::ThemeChoice;

const STATE_SCHEMA: u32 = 1;
const BOOKMARK_SCHEMA: u32 = 1;
const PREFERENCES_SCHEMA: u32 = 1;
const HISTORY_SCHEMA: u32 = 1;
const MAX_RECENT_BOOKS: usize = 100;

static LAST_UPDATED_UNIX_MS: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug)]
pub struct StateStore {
    root: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Bookmark {
    pub position: usize,
    pub label: Option<String>,
    pub created_unix_ms: u128,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SavedBook {
    pub position: usize,
    pub bookmarks: Vec<Bookmark>,
    pub matched_by_fingerprint: bool,
    pub warnings: Vec<StateWarning>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecentBook {
    pub path: PathBuf,
    pub position: usize,
    pub updated_unix_ms: u128,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RecentBooks {
    pub books: Vec<RecentBook>,
    pub warnings: Vec<StateWarning>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SavedHistory {
    pub commands: Vec<String>,
    pub searches: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StateWarning {
    code: &'static str,
    message: String,
}

impl StateWarning {
    pub fn code(&self) -> &str { self.code }

    pub fn message(&self) -> &str { &self.message }

    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self { code, message: message.into() }
    }
}

#[derive(Debug, Error)]
pub enum StateError {
    #[error("cannot determine the application state directory")]
    NoStateDirectory,
    #[error("cannot create state directory {path}: {source}")]
    CreateDirectory {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("cannot write state file {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("cannot serialise state: {0}")]
    Serialise(#[from] serde_json::Error),
    #[error(
        "state file {path} uses schema {found}, newer than the supported schema {supported}; it was not overwritten"
    )]
    NewerSchema { path: PathBuf, found: u32, supported: u32 },
    #[error("bookmark file {path} is unreadable and was not overwritten: {reason}")]
    UnreadableBookmarks { path: PathBuf, reason: String },
    #[error("state file {path} is unreadable and was not overwritten: {reason}")]
    UnreadableRecord { path: PathBuf, reason: String },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct BookState {
    schema: u32,
    path: PathBuf,
    fingerprint: String,
    position: usize,
    updated_unix_ms: u128,
    #[serde(flatten)]
    unknown: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct BookmarkRecord {
    schema: u32,
    path: PathBuf,
    fingerprint: String,
    updated_unix_ms: u128,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    bookmarks: Vec<StoredBookmark>,
    #[serde(flatten)]
    unknown: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredBookmark {
    position: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    label: Option<String>,
    created_unix_ms: u128,
    #[serde(flatten)]
    unknown: BTreeMap<String, Value>,
}

impl From<StoredBookmark> for Bookmark {
    fn from(value: StoredBookmark) -> Self {
        Self {
            position: value.position,
            label: value.label,
            created_unix_ms: value.created_unix_ms,
        }
    }
}

impl From<&Bookmark> for StoredBookmark {
    fn from(value: &Bookmark) -> Self {
        Self {
            position: value.position,
            label: value.label.clone(),
            created_unix_ms: value.created_unix_ms,
            unknown: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct LastOpened {
    schema: u32,
    path: PathBuf,
    #[serde(flatten)]
    unknown: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct PreferencesRecord {
    schema: u32,
    #[serde(default)]
    theme: ThemeChoice,
    #[serde(flatten)]
    unknown: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct HistoryRecord {
    schema: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    commands: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    searches: Vec<String>,
    #[serde(flatten)]
    unknown: BTreeMap<String, Value>,
}

enum Record<T> {
    Missing,
    Valid(T),
    Invalid(String),
}

impl StateStore {
    pub fn for_current_user() -> Result<Self, StateError> {
        let directories = ProjectDirs::from("", "", "peep").ok_or(StateError::NoStateDirectory)?;
        let root =
            directories.state_dir().unwrap_or_else(|| directories.data_local_dir()).to_path_buf();
        Self::at(root)
    }

    pub fn at(root: impl Into<PathBuf>) -> Result<Self, StateError> {
        let root = root.into();
        for directory in [root.join("books"), root.join("bookmarks")] {
            fs::create_dir_all(&directory)
                .map_err(|source| StateError::CreateDirectory { path: directory, source })?;
        }
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path { &self.root }

    pub fn load_book(&self, path: &Path, fingerprint: &str) -> SavedBook {
        let path = canonical_or_owned(path);
        let mut saved = SavedBook::default();

        let (position, progress_by_fingerprint, progress_warnings) =
            self.load_progress(&path, fingerprint);
        saved.position = position;
        saved.warnings.extend(progress_warnings);

        let (bookmarks, bookmarks_by_fingerprint, bookmark_warnings) =
            self.load_bookmarks(&path, fingerprint);
        saved.bookmarks = normalize_bookmarks(bookmarks);
        saved.warnings.extend(bookmark_warnings);
        saved.matched_by_fingerprint = progress_by_fingerprint || bookmarks_by_fingerprint;
        saved
    }

    pub fn save_progress(
        &self,
        path: &Path,
        fingerprint: &str,
        position: usize,
    ) -> Result<(), StateError> {
        let path = canonical_or_owned(path);
        let state_path = self.book_state_path(&path);
        if let Some(found) = newer_schema(&state_path, STATE_SCHEMA) {
            return Err(StateError::NewerSchema {
                path: state_path,
                found,
                supported: STATE_SCHEMA,
            });
        }
        let unknown = match read_record::<BookState>(&state_path) {
            Record::Valid(state) if state.schema == STATE_SCHEMA => state.unknown,
            Record::Missing | Record::Invalid(_) | Record::Valid(_) => BTreeMap::new(),
        };
        let state = BookState {
            schema: STATE_SCHEMA,
            path: path.clone(),
            fingerprint: fingerprint.to_owned(),
            position,
            updated_unix_ms: next_updated_unix_ms(),
            unknown,
        };
        self.write_json(&state_path, &state)
    }

    pub fn save_bookmarks(
        &self,
        path: &Path,
        fingerprint: &str,
        bookmarks: &[Bookmark],
    ) -> Result<(), StateError> {
        let path = canonical_or_owned(path);
        let state_path = self.bookmark_state_path(&path);
        match read_schema(&state_path) {
            Record::Valid(found) if found > BOOKMARK_SCHEMA => {
                return Err(StateError::NewerSchema {
                    path: state_path,
                    found,
                    supported: BOOKMARK_SCHEMA,
                });
            }
            Record::Invalid(reason) => {
                return Err(StateError::UnreadableBookmarks { path: state_path, reason });
            }
            Record::Missing | Record::Valid(_) => {}
        }
        let previous = match read_record::<BookmarkRecord>(&state_path) {
            Record::Valid(state) if state.schema == BOOKMARK_SCHEMA => Some(state),
            Record::Invalid(reason) => {
                return Err(StateError::UnreadableBookmarks { path: state_path, reason });
            }
            Record::Missing | Record::Valid(_) => self.best_bookmark_record(fingerprint),
        };
        let (unknown, mut bookmark_unknown) = previous.map_or_else(
            || (BTreeMap::new(), BTreeMap::new()),
            |record| {
                let bookmark_unknown = record
                    .bookmarks
                    .into_iter()
                    .map(|bookmark| (bookmark.position, bookmark.unknown))
                    .collect::<BTreeMap<_, _>>();
                (record.unknown, bookmark_unknown)
            },
        );
        let bookmarks = normalize_bookmarks(bookmarks.to_vec())
            .iter()
            .map(|bookmark| {
                let mut stored = StoredBookmark::from(bookmark);
                stored.unknown = bookmark_unknown.remove(&bookmark.position).unwrap_or_default();
                stored
            })
            .collect();
        let state = BookmarkRecord {
            schema: BOOKMARK_SCHEMA,
            path: path.clone(),
            fingerprint: fingerprint.to_owned(),
            updated_unix_ms: next_updated_unix_ms(),
            bookmarks,
            unknown,
        };
        self.write_json(&state_path, &state)
    }

    pub fn recent_books(&self, limit: usize) -> RecentBooks {
        let mut warnings = Vec::new();
        let mut candidates = Vec::new();
        let directory = self.root.join("books");
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) => {
                warnings.push(StateWarning::new(
                    "state.recent_unreadable",
                    format!("cannot read {}: {error}", directory.display()),
                ));
                return RecentBooks { books: Vec::new(), warnings };
            }
        };
        for entry in entries.flatten() {
            let record_path = entry.path();
            if !is_json_file(&record_path) {
                continue;
            }
            match read_record::<BookState>(&record_path) {
                Record::Valid(state) if state.schema == STATE_SCHEMA && state.path.is_file() => {
                    candidates.push(state)
                }
                Record::Valid(state) if state.schema > STATE_SCHEMA => {
                    warnings.push(StateWarning::new(
                        "state.newer_progress_schema",
                        format!(
                            "{} uses unsupported schema {}",
                            record_path.display(),
                            state.schema
                        ),
                    ))
                }
                Record::Invalid(reason) => warnings.push(StateWarning::new(
                    "state.progress_unreadable",
                    format!("cannot read {}: {reason}", record_path.display()),
                )),
                Record::Missing | Record::Valid(_) => {}
            }
        }
        candidates.sort_by_key(|state| std::cmp::Reverse(state.updated_unix_ms));
        let mut paths = HashSet::new();
        let mut fingerprints = HashSet::new();
        let books = candidates
            .into_iter()
            .filter(|state| {
                paths.insert(path_key(&state.path))
                    && fingerprints.insert(state.fingerprint.clone())
            })
            .take(limit.min(MAX_RECENT_BOOKS))
            .map(|state| RecentBook {
                path: state.path,
                position: state.position,
                updated_unix_ms: state.updated_unix_ms,
            })
            .collect();
        RecentBooks { books, warnings }
    }

    pub fn last_opened(&self) -> Option<PathBuf> {
        let state = match read_record::<LastOpened>(&self.root.join("last-opened.json")) {
            Record::Valid(state) => state,
            Record::Missing | Record::Invalid(_) => return None,
        };
        (state.schema == STATE_SCHEMA).then_some(state.path)
    }

    pub fn save_last_opened(&self, path: &Path) -> Result<(), StateError> {
        let state_path = self.root.join("last-opened.json");
        if let Some(found) = newer_schema(&state_path, STATE_SCHEMA) {
            return Err(StateError::NewerSchema {
                path: state_path,
                found,
                supported: STATE_SCHEMA,
            });
        }
        let unknown = match read_record::<LastOpened>(&state_path) {
            Record::Valid(state) if state.schema == STATE_SCHEMA => state.unknown,
            Record::Missing | Record::Invalid(_) | Record::Valid(_) => BTreeMap::new(),
        };
        self.write_json(
            &state_path,
            &LastOpened { schema: STATE_SCHEMA, path: canonical_or_owned(path), unknown },
        )
    }

    pub fn load_theme(&self) -> ThemeChoice {
        match read_record::<PreferencesRecord>(&self.preferences_path()) {
            Record::Valid(preferences) if preferences.schema == PREFERENCES_SCHEMA => {
                preferences.theme
            }
            Record::Missing | Record::Invalid(_) | Record::Valid(_) => ThemeChoice::Auto,
        }
    }

    pub fn save_theme(&self, theme: ThemeChoice) -> Result<(), StateError> {
        let path = self.preferences_path();
        match read_schema(&path) {
            Record::Valid(found) if found > PREFERENCES_SCHEMA => {
                return Err(StateError::NewerSchema { path, found, supported: PREFERENCES_SCHEMA });
            }
            Record::Invalid(reason) => {
                return Err(StateError::UnreadableRecord { path, reason });
            }
            Record::Missing | Record::Valid(_) => {}
        }
        let unknown = match read_record::<PreferencesRecord>(&path) {
            Record::Valid(preferences) if preferences.schema == PREFERENCES_SCHEMA => {
                preferences.unknown
            }
            Record::Missing | Record::Invalid(_) | Record::Valid(_) => BTreeMap::new(),
        };
        self.write_json(&path, &PreferencesRecord { schema: PREFERENCES_SCHEMA, theme, unknown })
    }

    pub fn load_history(&self) -> SavedHistory {
        match read_record::<HistoryRecord>(&self.history_path()) {
            Record::Valid(history) if history.schema == HISTORY_SCHEMA => {
                SavedHistory { commands: history.commands, searches: history.searches }
            }
            Record::Missing | Record::Invalid(_) | Record::Valid(_) => SavedHistory::default(),
        }
    }

    pub fn save_history(&self, history: &SavedHistory) -> Result<(), StateError> {
        let path = self.history_path();
        match read_schema(&path) {
            Record::Valid(found) if found > HISTORY_SCHEMA => {
                return Err(StateError::NewerSchema { path, found, supported: HISTORY_SCHEMA });
            }
            Record::Invalid(reason) => {
                return Err(StateError::UnreadableRecord { path, reason });
            }
            Record::Missing | Record::Valid(_) => {}
        }
        let unknown = match read_record::<HistoryRecord>(&path) {
            Record::Valid(history) if history.schema == HISTORY_SCHEMA => history.unknown,
            Record::Missing | Record::Invalid(_) | Record::Valid(_) => BTreeMap::new(),
        };
        self.write_json(
            &path,
            &HistoryRecord {
                schema: HISTORY_SCHEMA,
                commands: history.commands.clone(),
                searches: history.searches.clone(),
                unknown,
            },
        )
    }

    fn load_progress(&self, path: &Path, fingerprint: &str) -> (usize, bool, Vec<StateWarning>) {
        let exact_path = self.book_state_path(path);
        let mut warnings = Vec::new();
        match read_record::<BookState>(&exact_path) {
            Record::Valid(state)
                if state.schema == STATE_SCHEMA && path_key(&state.path) == path_key(path) =>
            {
                return (state.position, false, warnings);
            }
            Record::Valid(state) if state.schema > STATE_SCHEMA => {
                warnings.push(StateWarning::new(
                    "state.newer_progress_schema",
                    format!("{} uses unsupported schema {}", exact_path.display(), state.schema),
                ))
            }
            Record::Invalid(reason) => warnings.push(StateWarning::new(
                "state.progress_unreadable",
                format!("cannot read {}: {reason}", exact_path.display()),
            )),
            Record::Missing | Record::Valid(_) => {}
        }

        if let Some(state) = self.best_book_state(fingerprint) {
            return (state.position, true, warnings);
        }
        (0, false, warnings)
    }

    fn load_bookmarks(
        &self,
        path: &Path,
        fingerprint: &str,
    ) -> (Vec<Bookmark>, bool, Vec<StateWarning>) {
        let exact_path = self.bookmark_state_path(path);
        let mut warnings = Vec::new();
        match read_record::<BookmarkRecord>(&exact_path) {
            Record::Valid(state)
                if state.schema == BOOKMARK_SCHEMA && path_key(&state.path) == path_key(path) =>
            {
                if state.fingerprint != fingerprint {
                    warnings.push(StateWarning::new(
                        "state.bookmarks_content_changed",
                        "the file changed since its bookmarks were saved; bookmark positions may have shifted",
                    ));
                }
                return (
                    state.bookmarks.into_iter().map(Bookmark::from).collect(),
                    false,
                    warnings,
                );
            }
            Record::Valid(state) if state.schema > BOOKMARK_SCHEMA => {
                warnings.push(StateWarning::new(
                    "state.newer_bookmark_schema",
                    format!("{} uses unsupported schema {}", exact_path.display(), state.schema),
                ))
            }
            Record::Invalid(reason) => warnings.push(StateWarning::new(
                "state.bookmarks_unreadable",
                format!("cannot read {}: {reason}", exact_path.display()),
            )),
            Record::Missing | Record::Valid(_) => {}
        }

        if let Some(state) = self.best_bookmark_record(fingerprint) {
            return (state.bookmarks.into_iter().map(Bookmark::from).collect(), true, warnings);
        }
        (Vec::new(), false, warnings)
    }

    fn best_book_state(&self, fingerprint: &str) -> Option<BookState> {
        let entries = fs::read_dir(self.root.join("books")).ok()?;
        entries
            .flatten()
            .filter(|entry| is_json_file(&entry.path()))
            .filter_map(|entry| match read_record::<BookState>(&entry.path()) {
                Record::Valid(state)
                    if state.schema == STATE_SCHEMA && state.fingerprint == fingerprint =>
                {
                    Some(state)
                }
                Record::Missing | Record::Valid(_) | Record::Invalid(_) => None,
            })
            .max_by_key(|state| state.updated_unix_ms)
    }

    fn best_bookmark_record(&self, fingerprint: &str) -> Option<BookmarkRecord> {
        let entries = fs::read_dir(self.root.join("bookmarks")).ok()?;
        entries
            .flatten()
            .filter(|entry| is_json_file(&entry.path()))
            .filter_map(|entry| match read_record::<BookmarkRecord>(&entry.path()) {
                Record::Valid(state)
                    if state.schema == BOOKMARK_SCHEMA && state.fingerprint == fingerprint =>
                {
                    Some(state)
                }
                Record::Missing | Record::Valid(_) | Record::Invalid(_) => None,
            })
            .max_by_key(|state| state.updated_unix_ms)
    }

    fn book_state_path(&self, path: &Path) -> PathBuf { self.record_path("books", path) }

    fn bookmark_state_path(&self, path: &Path) -> PathBuf { self.record_path("bookmarks", path) }

    fn preferences_path(&self) -> PathBuf { self.root.join("preferences.json") }

    fn history_path(&self) -> PathBuf { self.root.join("history.json") }

    fn record_path(&self, directory: &str, path: &Path) -> PathBuf {
        let path = canonical_or_owned(path);
        let digest = blake3::hash(path_key(&path).as_bytes()).to_hex();
        self.root.join(directory).join(format!("{digest}.json"))
    }

    fn write_json<T: Serialize>(&self, path: &Path, value: &T) -> Result<(), StateError> {
        let bytes = serde_json::to_vec_pretty(value)?;
        let parent = path.parent().unwrap_or(&self.root);
        fs::create_dir_all(parent)
            .map_err(|source| StateError::CreateDirectory { path: parent.to_path_buf(), source })?;
        let mut temporary = tempfile::NamedTempFile::new_in(parent)
            .map_err(|source| StateError::Write { path: path.to_path_buf(), source })?;
        temporary
            .write_all(&bytes)
            .and_then(|()| temporary.as_file().sync_all())
            .map_err(|source| StateError::Write { path: path.to_path_buf(), source })?;
        temporary
            .persist(path)
            .map_err(|error| StateError::Write { path: path.to_path_buf(), source: error.error })?;
        Ok(())
    }
}

fn read_record<T: for<'de> Deserialize<'de>>(path: &Path) -> Record<T> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Record::Missing,
        Err(error) => return Record::Invalid(error.to_string()),
    };
    match serde_json::from_slice(&bytes) {
        Ok(value) => Record::Valid(value),
        Err(error) => Record::Invalid(error.to_string()),
    }
}

fn read_schema(path: &Path) -> Record<u32> {
    match read_record::<Value>(path) {
        Record::Valid(value) => value
            .get("schema")
            .and_then(Value::as_u64)
            .and_then(|schema| u32::try_from(schema).ok())
            .map_or_else(|| Record::Invalid("missing or invalid schema".to_owned()), Record::Valid),
        Record::Missing => Record::Missing,
        Record::Invalid(reason) => Record::Invalid(reason),
    }
}

fn newer_schema(path: &Path, supported: u32) -> Option<u32> {
    match read_schema(path) {
        Record::Valid(found) if found > supported => Some(found),
        Record::Missing | Record::Valid(_) | Record::Invalid(_) => None,
    }
}

fn normalize_bookmarks(mut bookmarks: Vec<Bookmark>) -> Vec<Bookmark> {
    bookmarks.sort_by_key(|bookmark| (bookmark.position, bookmark.created_unix_ms));
    bookmarks.dedup_by(|next, current| {
        if next.position == current.position {
            *current = next.clone();
            true
        } else {
            false
        }
    });
    bookmarks
}

fn is_json_file(path: &Path) -> bool {
    path.extension().and_then(|value| value.to_str()) == Some("json")
}

fn canonical_or_owned(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn path_key(path: &Path) -> String {
    let value = path.to_string_lossy();
    if cfg!(windows) { value.to_lowercase() } else { value.into_owned() }
}

pub fn now_unix_ms() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis()
}

fn next_updated_unix_ms() -> u128 {
    let current = now_unix_ms().try_into().unwrap_or(u64::MAX);
    u128::from(monotonic_unix_ms(current, &LAST_UPDATED_UNIX_MS))
}

fn monotonic_unix_ms(current: u64, last: &AtomicU64) -> u64 {
    let mut previous = last.load(Ordering::Relaxed);
    loop {
        let next = current.max(previous.saturating_add(1));
        match last.compare_exchange_weak(previous, next, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return next,
            Err(observed) => previous = observed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bookmark(position: usize, label: Option<&str>, created_unix_ms: u128) -> Bookmark {
        Bookmark { position, label: label.map(str::to_owned), created_unix_ms }
    }

    #[test]
    fn update_timestamps_are_strictly_increasing_within_the_same_millisecond() {
        let last = AtomicU64::new(100);

        assert_eq!(monotonic_unix_ms(100, &last), 101);
        assert_eq!(monotonic_unix_ms(100, &last), 102);
        assert_eq!(monotonic_unix_ms(200, &last), 200);
    }

    #[test]
    fn reads_v1_progress_and_resumes_by_exact_path() {
        let directory = tempfile::tempdir().unwrap();
        let book = directory.path().join("book.txt");
        fs::write(&book, "text").unwrap();
        let store = StateStore::at(directory.path().join("state")).unwrap();
        let path = canonical_or_owned(&book);
        let raw = serde_json::json!({
            "schema": 1,
            "path": path,
            "fingerprint": "old",
            "position": 42,
            "updated_unix_ms": 1
        });
        fs::write(store.book_state_path(&book), serde_json::to_vec(&raw).unwrap()).unwrap();

        let saved = store.load_book(&book, "changed");
        assert_eq!(saved.position, 42);
        assert!(!saved.matched_by_fingerprint);
        assert!(saved.bookmarks.is_empty());
    }

    #[test]
    fn resumes_progress_and_bookmarks_after_a_move() {
        let directory = tempfile::tempdir().unwrap();
        let first = directory.path().join("before.txt");
        let second = directory.path().join("after.txt");
        fs::write(&first, "same").unwrap();
        fs::write(&second, "same").unwrap();
        let store = StateStore::at(directory.path().join("state")).unwrap();
        store.save_progress(&first, "same-hash", 3).unwrap();
        store.save_bookmarks(&first, "same-hash", &[bookmark(2, Some("here"), 1)]).unwrap();

        let saved = store.load_book(&second, "same-hash");
        assert_eq!(saved.position, 3);
        assert_eq!(saved.bookmarks, [bookmark(2, Some("here"), 1)]);
        assert!(saved.matched_by_fingerprint);
    }

    #[test]
    fn keeps_bookmarks_out_of_progress_files() {
        let directory = tempfile::tempdir().unwrap();
        let book = directory.path().join("book.txt");
        fs::write(&book, "text").unwrap();
        let store = StateStore::at(directory.path().join("state")).unwrap();
        store.save_bookmarks(&book, "hash", &[bookmark(1, None, 1)]).unwrap();
        let bookmark_bytes = fs::read(store.bookmark_state_path(&book)).unwrap();

        store.save_progress(&book, "hash", 2).unwrap();

        assert_eq!(fs::read(store.bookmark_state_path(&book)).unwrap(), bookmark_bytes);
        let progress = fs::read_to_string(store.book_state_path(&book)).unwrap();
        assert!(!progress.contains("bookmarks"));
    }

    #[test]
    fn refuses_to_overwrite_unreadable_or_newer_bookmarks() {
        let directory = tempfile::tempdir().unwrap();
        let book = directory.path().join("book.txt");
        fs::write(&book, "text").unwrap();
        let store = StateStore::at(directory.path().join("state")).unwrap();
        let path = store.bookmark_state_path(&book);
        fs::write(&path, "not json").unwrap();
        assert!(matches!(
            store.save_bookmarks(&book, "hash", &[]),
            Err(StateError::UnreadableBookmarks { .. })
        ));
        assert_eq!(fs::read_to_string(&path).unwrap(), "not json");

        fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({
                "schema": 2,
                "path": book,
                "fingerprint": "hash",
                "updated_unix_ms": 1,
                "bookmarks": []
            }))
            .unwrap(),
        )
        .unwrap();
        assert!(matches!(
            store.save_bookmarks(&book, "hash", &[]),
            Err(StateError::NewerSchema { .. })
        ));
    }

    #[test]
    fn refuses_to_overwrite_a_newer_progress_schema_even_if_its_shape_changed() {
        let directory = tempfile::tempdir().unwrap();
        let book = directory.path().join("book.txt");
        fs::write(&book, "text").unwrap();
        let store = StateStore::at(directory.path().join("state")).unwrap();
        let path = store.book_state_path(&book);
        fs::write(&path, r#"{"schema":2,"future_shape":true}"#).unwrap();

        assert!(matches!(
            store.save_progress(&book, "hash", 1),
            Err(StateError::NewerSchema { found: 2, .. })
        ));
        assert_eq!(fs::read_to_string(path).unwrap(), r#"{"schema":2,"future_shape":true}"#);
    }

    #[test]
    fn preserves_unknown_bookmark_fields() {
        let directory = tempfile::tempdir().unwrap();
        let book = directory.path().join("book.txt");
        fs::write(&book, "text").unwrap();
        let store = StateStore::at(directory.path().join("state")).unwrap();
        let path = store.bookmark_state_path(&book);
        fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({
                "schema": 1,
                "path": canonical_or_owned(&book),
                "fingerprint": "hash",
                "updated_unix_ms": 1,
                "future_field": { "kept": true },
                "bookmarks": [{
                    "position": 1,
                    "created_unix_ms": 1,
                    "future_bookmark_field": "also kept"
                }]
            }))
            .unwrap(),
        )
        .unwrap();

        store.save_bookmarks(&book, "hash", &[bookmark(1, None, 1)]).unwrap();

        let value: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        assert_eq!(value["future_field"]["kept"], true);
        assert_eq!(value["bookmarks"][0]["future_bookmark_field"], "also kept");
    }

    #[test]
    fn recent_books_are_sorted_deduplicated_and_skip_missing_paths() {
        let directory = tempfile::tempdir().unwrap();
        let first = directory.path().join("first.txt");
        let second = directory.path().join("second.txt");
        let moved = directory.path().join("moved.txt");
        fs::write(&first, "first").unwrap();
        fs::write(&second, "second").unwrap();
        fs::write(&moved, "first").unwrap();
        let store = StateStore::at(directory.path().join("state")).unwrap();
        store.save_progress(&first, "same", 1).unwrap();
        store.save_progress(&second, "other", 2).unwrap();
        store.save_progress(&moved, "same", 3).unwrap();
        let missing = directory.path().join("missing.txt");
        store.save_progress(&missing, "missing", 4).unwrap();

        let recent = store.recent_books(100);
        assert_eq!(recent.books.len(), 2);
        assert_eq!(recent.books[0].path, canonical_or_owned(&moved));
        assert_eq!(recent.books[1].path, canonical_or_owned(&second));
    }

    #[test]
    fn recent_books_enforces_its_interface_limit() {
        let directory = tempfile::tempdir().unwrap();
        let store = StateStore::at(directory.path().join("state")).unwrap();
        for number in 0..=MAX_RECENT_BOOKS {
            let book = directory.path().join(format!("book-{number}.txt"));
            fs::write(&book, number.to_string()).unwrap();
            store.save_progress(&book, &format!("hash-{number}"), number).unwrap();
        }

        assert_eq!(store.recent_books(usize::MAX).books.len(), MAX_RECENT_BOOKS);
    }

    #[test]
    fn remembers_last_opened_path() {
        let directory = tempfile::tempdir().unwrap();
        let store = StateStore::at(directory.path().join("state")).unwrap();
        let path = directory.path().join("book.epub");

        store.save_last_opened(&path).unwrap();

        assert_eq!(store.last_opened(), Some(path));
    }

    #[test]
    fn theme_preference_is_independent_and_preserves_unknown_fields() {
        let directory = tempfile::tempdir().unwrap();
        let store = StateStore::at(directory.path().join("state")).unwrap();
        let path = store.preferences_path();
        fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({
                "schema": 1,
                "theme": "light",
                "future_field": { "kept": true }
            }))
            .unwrap(),
        )
        .unwrap();

        assert_eq!(store.load_theme(), ThemeChoice::Light);
        store.save_theme(ThemeChoice::Dark).unwrap();

        let value: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        assert_eq!(value["theme"], "dark");
        assert_eq!(value["future_field"]["kept"], true);
    }

    #[test]
    fn refuses_to_overwrite_unreadable_or_newer_theme_preferences() {
        let directory = tempfile::tempdir().unwrap();
        let store = StateStore::at(directory.path().join("state")).unwrap();
        let path = store.preferences_path();
        fs::write(&path, "not json").unwrap();
        assert!(matches!(
            store.save_theme(ThemeChoice::Light),
            Err(StateError::UnreadableRecord { .. })
        ));
        assert_eq!(fs::read_to_string(&path).unwrap(), "not json");

        fs::write(&path, r#"{"schema":2,"theme":"future"}"#).unwrap();
        assert!(matches!(
            store.save_theme(ThemeChoice::Dark),
            Err(StateError::NewerSchema { found: 2, .. })
        ));
    }

    #[test]
    fn history_is_independent_and_preserves_unknown_fields() {
        let directory = tempfile::tempdir().unwrap();
        let store = StateStore::at(directory.path().join("state")).unwrap();
        let path = store.history_path();
        fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({
                "schema": 1,
                "commands": ["toc"],
                "searches": ["第一章"],
                "future_field": { "kept": true }
            }))
            .unwrap(),
        )
        .unwrap();

        let mut history = store.load_history();
        assert_eq!(history.commands, ["toc"]);
        assert_eq!(history.searches, ["第一章"]);
        history.commands.push("recent".to_owned());
        store.save_history(&history).unwrap();

        let value: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        assert_eq!(value["commands"], serde_json::json!(["toc", "recent"]));
        assert_eq!(value["future_field"]["kept"], true);
    }

    #[test]
    fn refuses_to_overwrite_unreadable_or_newer_history() {
        let directory = tempfile::tempdir().unwrap();
        let store = StateStore::at(directory.path().join("state")).unwrap();
        let path = store.history_path();
        fs::write(&path, "not json").unwrap();
        assert!(matches!(
            store.save_history(&SavedHistory::default()),
            Err(StateError::UnreadableRecord { .. })
        ));
        assert_eq!(fs::read_to_string(&path).unwrap(), "not json");

        fs::write(&path, r#"{"schema":2,"entries":[]}"#).unwrap();
        assert!(matches!(
            store.save_history(&SavedHistory::default()),
            Err(StateError::NewerSchema { found: 2, .. })
        ));
    }

    #[test]
    fn separate_books_do_not_overwrite_each_other() {
        let directory = tempfile::tempdir().unwrap();
        let first = directory.path().join("first.txt");
        let second = directory.path().join("second.txt");
        fs::write(&first, "first").unwrap();
        fs::write(&second, "second").unwrap();
        let store = StateStore::at(directory.path().join("state")).unwrap();

        store.save_progress(&first, "first-hash", 3).unwrap();
        store.save_progress(&second, "second-hash", 7).unwrap();

        assert_eq!(store.load_book(&first, "first-hash").position, 3);
        assert_eq!(store.load_book(&second, "second-hash").position, 7);
    }
}
