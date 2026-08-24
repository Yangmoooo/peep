use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::{fs, io};

const SUPPORTED_EXTENSIONS: &[&str] = &["epub", "txt", "md", "markdown"];

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct FileEntry {
    pub(crate) path: PathBuf,
    pub(crate) name: String,
    pub(crate) is_directory: bool,
    pub(crate) is_parent: bool,
}

impl FileEntry {
    pub(crate) fn label(&self) -> String {
        if self.is_parent {
            "../".to_owned()
        } else if self.is_directory {
            format!("{}/", self.name)
        } else {
            self.name.clone()
        }
    }
}

pub(crate) fn read_directory(directory: &Path) -> io::Result<(PathBuf, Vec<FileEntry>)> {
    let directory = fs::canonicalize(directory)?;
    if !directory.is_dir() {
        return Err(io::Error::new(io::ErrorKind::NotADirectory, "path is not a directory"));
    }

    let mut entries = Vec::new();
    if let Some(parent) = directory.parent().filter(|parent| *parent != directory) {
        entries.push(FileEntry {
            path: parent.to_path_buf(),
            name: "..".to_owned(),
            is_directory: true,
            is_parent: true,
        });
    }

    for entry in fs::read_dir(&directory)? {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => continue,
        };
        let path = entry.path();
        let metadata = match fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(_) => continue,
        };
        let is_directory = metadata.is_dir();
        if !is_directory && (!metadata.is_file() || !is_supported_file(&path)) {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        entries.push(FileEntry { path, name, is_directory, is_parent: false });
    }

    entries.sort_by(compare_entries);
    Ok((directory, entries))
}

pub(crate) fn is_supported_file(path: &Path) -> bool {
    path.extension().and_then(|extension| extension.to_str()).is_some_and(|extension| {
        SUPPORTED_EXTENSIONS.iter().any(|supported| extension.eq_ignore_ascii_case(supported))
    })
}

fn compare_entries(left: &FileEntry, right: &FileEntry) -> Ordering {
    if left.is_parent != right.is_parent {
        return right.is_parent.cmp(&left.is_parent);
    }
    if left.is_directory != right.is_directory {
        return right.is_directory.cmp(&left.is_directory);
    }
    let left_lower = left.name.to_lowercase();
    let right_lower = right.name.to_lowercase();
    left_lower.cmp(&right_lower).then_with(|| left.name.cmp(&right.name))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn lists_directories_and_supported_files_in_stable_order() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir(directory.path().join("subdir")).unwrap();
        fs::write(directory.path().join("z.txt"), "text").unwrap();
        fs::write(directory.path().join("A.EPUB"), "not really epub").unwrap();
        fs::write(directory.path().join("ignored.pdf"), "pdf").unwrap();

        let (resolved, entries) = read_directory(directory.path()).unwrap();
        assert_eq!(resolved, fs::canonicalize(directory.path()).unwrap());
        assert_eq!(
            entries.iter().map(FileEntry::label).collect::<Vec<_>>(),
            ["../", "subdir/", "A.EPUB", "z.txt"]
        );
    }

    #[test]
    fn accepts_supported_extensions_case_insensitively() {
        assert!(is_supported_file(Path::new("book.EpUb")));
        assert!(is_supported_file(Path::new("notes.markdown")));
        assert!(!is_supported_file(Path::new("cover.png")));
    }
}
