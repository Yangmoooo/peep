use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use thiserror::Error;

const STATE_SCHEMA: u32 = 1;

#[derive(Clone, Debug)]
pub struct StateStore {
    root: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResumePosition {
    pub position: usize,
    pub matched_by_fingerprint: bool,
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
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct BookState {
    schema: u32,
    path: PathBuf,
    fingerprint: String,
    position: usize,
    updated_unix_ms: u128,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct LastOpened {
    schema: u32,
    path: PathBuf,
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
        let books = root.join("books");
        fs::create_dir_all(&books)
            .map_err(|source| StateError::CreateDirectory { path: books, source })?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path { &self.root }

    pub fn resume_position(&self, path: &Path, fingerprint: &str) -> Option<ResumePosition> {
        let path = canonical_or_owned(path);
        let exact_path = self.book_state_path(&path);
        if let Some(state) = read_json::<BookState>(&exact_path)
            && state.schema == STATE_SCHEMA
            && path_key(&state.path) == path_key(&path)
        {
            return Some(ResumePosition {
                position: state.position,
                matched_by_fingerprint: false,
            });
        }

        let mut best = None::<BookState>;
        let entries = fs::read_dir(self.root.join("books")).ok()?;
        for entry in entries.flatten() {
            if entry.path().extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let Some(state) = read_json::<BookState>(&entry.path()) else {
                continue;
            };
            if state.schema == STATE_SCHEMA
                && state.fingerprint == fingerprint
                && best
                    .as_ref()
                    .is_none_or(|current| state.updated_unix_ms > current.updated_unix_ms)
            {
                best = Some(state);
            }
        }
        best.map(|state| ResumePosition { position: state.position, matched_by_fingerprint: true })
    }

    pub fn save_progress(
        &self,
        path: &Path,
        fingerprint: &str,
        position: usize,
    ) -> Result<(), StateError> {
        let path = canonical_or_owned(path);
        let state = BookState {
            schema: STATE_SCHEMA,
            path: path.clone(),
            fingerprint: fingerprint.to_owned(),
            position,
            updated_unix_ms: now_unix_ms(),
        };
        self.write_json(&self.book_state_path(&path), &state)
    }

    pub fn last_opened(&self) -> Option<PathBuf> {
        let state = read_json::<LastOpened>(&self.root.join("last-opened.json"))?;
        (state.schema == STATE_SCHEMA).then_some(state.path)
    }

    pub fn save_last_opened(&self, path: &Path) -> Result<(), StateError> {
        self.write_json(
            &self.root.join("last-opened.json"),
            &LastOpened { schema: STATE_SCHEMA, path: canonical_or_owned(path) },
        )
    }

    fn book_state_path(&self, path: &Path) -> PathBuf {
        let digest = blake3::hash(path_key(path).as_bytes()).to_hex();
        self.root.join("books").join(format!("{digest}.json"))
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

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Option<T> {
    let bytes = fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn canonical_or_owned(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn path_key(path: &Path) -> String {
    let value = path.to_string_lossy();
    if cfg!(windows) { value.to_lowercase() } else { value.into_owned() }
}

fn now_unix_ms() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saves_and_resumes_by_exact_path() {
        let directory = tempfile::tempdir().unwrap();
        let book = directory.path().join("book.txt");
        fs::write(&book, "text").unwrap();
        let store = StateStore::at(directory.path().join("state")).unwrap();

        store.save_progress(&book, "fingerprint", 42).unwrap();

        assert_eq!(
            store.resume_position(&book, "changed"),
            Some(ResumePosition { position: 42, matched_by_fingerprint: false })
        );
    }

    #[test]
    fn resumes_renamed_file_by_fingerprint() {
        let directory = tempfile::tempdir().unwrap();
        let first = directory.path().join("before.txt");
        let second = directory.path().join("after.txt");
        fs::write(&first, "same").unwrap();
        fs::write(&second, "same").unwrap();
        let store = StateStore::at(directory.path().join("state")).unwrap();
        store.save_progress(&first, "same-hash", 9).unwrap();

        assert_eq!(
            store.resume_position(&second, "same-hash"),
            Some(ResumePosition { position: 9, matched_by_fingerprint: true })
        );
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
    fn separate_books_do_not_overwrite_each_other() {
        let directory = tempfile::tempdir().unwrap();
        let first = directory.path().join("first.txt");
        let second = directory.path().join("second.txt");
        fs::write(&first, "first").unwrap();
        fs::write(&second, "second").unwrap();
        let store = StateStore::at(directory.path().join("state")).unwrap();

        store.save_progress(&first, "first-hash", 3).unwrap();
        store.save_progress(&second, "second-hash", 7).unwrap();

        assert_eq!(store.resume_position(&first, "first-hash").unwrap().position, 3);
        assert_eq!(store.resume_position(&second, "second-hash").unwrap().position, 7);
    }
}
