//! Loading and normalising supported book formats.
//!
//! The rest of Peep crosses this module through [`open_document`] and the
//! format-neutral [`CanonicalDocument`]. Format-specific recovery stays in
//! private adapters.

mod chapter;
mod epub;
mod markdown;
mod toc;
mod txt;

use std::fs::File;
use std::io::{BufReader, Read};
use std::ops::Range;
use std::path::{Path, PathBuf};

use thiserror::Error;

pub(crate) use self::chapter::detect_chapter_headings;
use self::epub::EpubAdapter;
use self::markdown::MarkdownAdapter;
use self::txt::TxtAdapter;

const READ_BUFFER_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug)]
pub struct DocumentSource {
    path: PathBuf,
}

impl DocumentSource {
    pub fn from_path(path: impl Into<PathBuf>) -> Self { Self { path: path.into() } }

    pub fn path(&self) -> &Path { &self.path }
}

impl<P: Into<PathBuf>> From<P> for DocumentSource {
    fn from(path: P) -> Self { Self::from_path(path) }
}

#[derive(Clone, Copy, Debug)]
pub struct LoadOptions {
    pub max_input_bytes: usize,
    pub max_text_bytes: usize,
}

impl Default for LoadOptions {
    fn default() -> Self {
        Self {
            // Large covers are common; only decoded text has the 100 MiB
            // product guarantee.
            max_input_bytes: 512 * 1024 * 1024,
            max_text_bytes: 100 * 1024 * 1024,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DocumentFormat {
    Epub,
    Markdown,
    Txt,
}

impl std::fmt::Display for DocumentFormat {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Epub => formatter.write_str("EPUB"),
            Self::Markdown => formatter.write_str("MD"),
            Self::Txt => formatter.write_str("TXT"),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct DocumentMetadata {
    title: Option<String>,
    author: Option<String>,
    language: Option<String>,
}

impl DocumentMetadata {
    pub fn title(&self) -> Option<&str> { self.title.as_deref() }

    pub fn author(&self) -> Option<&str> { self.author.as_deref() }

    pub fn language(&self) -> Option<&str> { self.language.as_deref() }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Section {
    title: String,
    range: Range<usize>,
}

impl Section {
    pub fn title(&self) -> &str { &self.title }

    pub fn range(&self) -> Range<usize> { self.range.clone() }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TocEntry {
    label: String,
    offset: usize,
    depth: u8,
}

impl TocEntry {
    pub fn label(&self) -> &str { &self.label }

    pub fn offset(&self) -> usize { self.offset }

    pub fn depth(&self) -> u8 { self.depth }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TextStyleKind {
    Code,
    Emphasis,
    Link,
    Quote,
    Strikethrough,
    Strong,
    Heading(u8),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TextStyle {
    range: Range<usize>,
    kind: TextStyleKind,
}

impl TextStyle {
    pub fn range(&self) -> Range<usize> { self.range.clone() }

    pub fn kind(&self) -> TextStyleKind { self.kind }
}

#[derive(Clone, Debug)]
pub struct CanonicalDocument {
    text: String,
    total_chars: usize,
    metadata: DocumentMetadata,
    sections: Vec<Section>,
    toc: Vec<TocEntry>,
    styles: Vec<TextStyle>,
}

impl CanonicalDocument {
    pub fn text(&self) -> &str { &self.text }

    pub fn total_chars(&self) -> usize { self.total_chars }

    pub fn metadata(&self) -> &DocumentMetadata { &self.metadata }

    pub fn sections(&self) -> &[Section] { &self.sections }

    pub fn toc(&self) -> &[TocEntry] { &self.toc }

    pub fn styles(&self) -> &[TextStyle] { &self.styles }

    fn new(
        text: String,
        metadata: DocumentMetadata,
        sections: Vec<Section>,
        mut toc: Vec<TocEntry>,
        mut styles: Vec<TextStyle>,
    ) -> Self {
        let text_len = text.len();
        toc.retain(|entry| {
            entry.offset < text_len
                && text.is_char_boundary(entry.offset)
                && !entry.label.trim().is_empty()
                && (toc::label_is_landmark(&entry.label)
                    || toc::label_is_visible_at(&text, &entry.label, entry.offset))
        });
        toc.sort_by_key(|entry| entry.offset);
        toc = toc.into_iter().fold(Vec::<TocEntry>::new(), |mut entries, entry| {
            let keep = entries.last().is_none_or(|previous| {
                previous.offset != entry.offset
                    || previous.depth != entry.depth
                    || toc::labels_compatible(&previous.label, &entry.label)
            });
            if keep {
                entries.push(entry);
            }
            entries
        });
        styles.retain(|style| {
            style.range.start <= style.range.end
                && style.range.end <= text_len
                && text.is_char_boundary(style.range.start)
                && text.is_char_boundary(style.range.end)
        });
        Self { total_chars: text.chars().count(), text, metadata, sections, toc, styles }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LoadWarning {
    code: &'static str,
    message: String,
}

impl LoadWarning {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self { code, message: message.into() }
    }

    pub fn code(&self) -> &str { self.code }

    pub fn message(&self) -> &str { &self.message }
}

#[derive(Clone, Debug)]
pub struct LoadedDocument {
    path: PathBuf,
    fingerprint: String,
    format: DocumentFormat,
    document: CanonicalDocument,
    warnings: Vec<LoadWarning>,
}

impl LoadedDocument {
    pub fn path(&self) -> &Path { &self.path }

    pub fn fingerprint(&self) -> &str { &self.fingerprint }

    pub fn format(&self) -> DocumentFormat { self.format }

    pub fn document(&self) -> &CanonicalDocument { &self.document }

    pub fn warnings(&self) -> &[LoadWarning] { &self.warnings }
}

#[derive(Debug, Error)]
pub enum LoadError {
    #[error("cannot open {path}: {source}")]
    Open {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("cannot read {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{path} is larger than the {limit} byte input limit")]
    InputTooLarge { path: PathBuf, limit: usize },
    #[error("the decoded text is larger than the {limit} byte limit")]
    TextTooLarge { limit: usize },
    #[error("unsupported or unrecognised document format: {0}")]
    UnsupportedFormat(PathBuf),
    #[error("TXT is neither valid UTF-8 nor valid GB18030")]
    InvalidTextEncoding,
    #[error("Markdown is not valid UTF-8")]
    InvalidMarkdownEncoding,
    #[error("invalid EPUB: {0}")]
    InvalidEpub(String),
}

struct AdapterInput<'a> {
    path: &'a Path,
    bytes: &'a [u8],
    limits: LoadOptions,
}

struct AdapterOutput {
    document: CanonicalDocument,
    warnings: Vec<LoadWarning>,
}

trait FormatAdapter {
    fn format(&self) -> DocumentFormat;
    fn probe(&self, path: &Path, prefix: &[u8]) -> u8;
    fn load(&self, input: AdapterInput<'_>) -> Result<AdapterOutput, LoadError>;
}

pub fn open_document(
    source: DocumentSource,
    options: LoadOptions,
) -> Result<LoadedDocument, LoadError> {
    let requested_path = source.path;
    let file = File::open(&requested_path)
        .map_err(|source| LoadError::Open { path: requested_path.clone(), source })?;
    let metadata = file
        .metadata()
        .map_err(|source| LoadError::Read { path: requested_path.clone(), source })?;
    if metadata.len() > options.max_input_bytes as u64 {
        return Err(LoadError::InputTooLarge {
            path: requested_path,
            limit: options.max_input_bytes,
        });
    }

    let mut reader = BufReader::with_capacity(READ_BUFFER_BYTES, file);
    let mut bytes = Vec::with_capacity(metadata.len().min(options.max_input_bytes as u64) as usize);
    let mut hasher = blake3::Hasher::new();
    let mut buffer = vec![0_u8; READ_BUFFER_BYTES];
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|source| LoadError::Read { path: requested_path.clone(), source })?;
        if read == 0 {
            break;
        }
        if bytes.len().saturating_add(read) > options.max_input_bytes {
            return Err(LoadError::InputTooLarge {
                path: requested_path,
                limit: options.max_input_bytes,
            });
        }
        hasher.update(&buffer[..read]);
        bytes.extend_from_slice(&buffer[..read]);
    }

    let prefix = &bytes[..bytes.len().min(4096)];
    let adapters: [&dyn FormatAdapter; 3] = [&EpubAdapter, &MarkdownAdapter, &TxtAdapter];
    let adapter = adapters
        .into_iter()
        .max_by_key(|adapter| adapter.probe(&requested_path, prefix))
        .filter(|adapter| adapter.probe(&requested_path, prefix) > 0)
        .ok_or_else(|| LoadError::UnsupportedFormat(requested_path.clone()))?;

    let output =
        adapter.load(AdapterInput { path: &requested_path, bytes: &bytes, limits: options })?;
    let path = std::fs::canonicalize(&requested_path).unwrap_or(requested_path);

    Ok(LoadedDocument {
        path,
        fingerprint: hasher.finalize().to_hex().to_string(),
        format: adapter.format(),
        document: output.document,
        warnings: output.warnings,
    })
}

fn plain_document(
    text: String,
    metadata: DocumentMetadata,
    section_title: String,
    toc: Vec<TocEntry>,
) -> CanonicalDocument {
    let end = text.len();
    CanonicalDocument::new(
        text,
        metadata,
        vec![Section { title: section_title, range: 0..end }],
        toc,
        Vec::new(),
    )
}
