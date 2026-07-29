//! Loading and normalising supported book formats.
//!
//! The rest of Peep crosses this module through [`open_document`] and the
//! format-neutral [`CanonicalDocument`]. Format-specific recovery stays in
//! private adapters.

mod epub;
mod txt;

use std::fs::File;
use std::io::{BufReader, Read};
use std::ops::Range;
use std::path::{Path, PathBuf};

use thiserror::Error;

use self::epub::EpubAdapter;
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
    Txt,
}

impl std::fmt::Display for DocumentFormat {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Epub => formatter.write_str("EPUB"),
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
    Emphasis,
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
        toc.retain(|entry| entry.offset <= text_len && text.is_char_boundary(entry.offset));
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
    let adapters: [&dyn FormatAdapter; 2] = [&EpubAdapter, &TxtAdapter];
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

pub(crate) fn detect_chapter_headings(text: &str) -> Vec<TocEntry> {
    let mut entries = Vec::new();
    let mut byte_offset = 0;
    for line in text.split('\n') {
        let line_len = line.len();
        let trimmed = line.trim_start();
        if let Some(label) = match_chapter_heading(trimmed) {
            let leading_spaces = line_len - trimmed.len();
            entries.push(TocEntry { label: label.to_owned(), offset: byte_offset + leading_spaces, depth: 0 });
        }
        byte_offset += line_len + 1; // +1 for '\n'
    }
    // Fix up the last entry: text doesn't end with '\n'
    if entries.last().is_some_and(|entry| entry.offset > text.len()) {
        let last = entries.last_mut().unwrap();
        last.offset = last.offset.min(text.len());
    }
    entries
}

fn match_chapter_heading(line: &str) -> Option<&str> {
    // Scan the whole line for "第X{unit}" patterns and pick the
    // best one.  "章" (chapter) outranks "卷" (volume) so that
    // lines like "第一卷 第一章 心事一灯知" produce chapter entries
    // rather than volume entries.
    let mut best: Option<(usize, &str)> = None;
    let mut search = line;
    while let Some(pos) = search.find('第') {
        let after_di = &search[pos + 3..]; // skip UTF-8 "第" (3 bytes)
        let Some(num_end) = chinese_number_end(after_di) else {
            search = after_di;
            continue;
        };
        let after_num = &after_di[num_end..];
        let unit_bytes = after_num
            .strip_prefix('章')
            .or_else(|| after_num.strip_prefix('回'))
            .or_else(|| after_num.strip_prefix('节'))
            .or_else(|| after_num.strip_prefix('節'))
            .or_else(|| after_num.strip_prefix('卷'))
            .or_else(|| after_num.strip_prefix('集'))
            .or_else(|| after_num.strip_prefix('部'));
        if unit_bytes.is_some() {
            let unit_char = after_num.chars().next().unwrap();
            let priority = unit_priority(unit_char);
            let match_start = &search[pos..];
            let total_len = 3 + num_end + unit_char.len_utf8(); // 第 + number + unit
            let label = &match_start[..total_len.min(match_start.len())];
            // Translate the match_start offset to the original line
            let line_offset = line.len() - search.len() + pos;
            let line_label = &line[line_offset..line_offset + label.len()];
            if best.as_ref().is_none_or(|(prev_prio, _)| priority > *prev_prio) {
                best = Some((priority, line_label));
            }
        }
        // Advance past this "第" to look for more chapter markers
        search = &search[pos + 3..];
    }

    if let Some((_, label)) = best {
        return Some(label);
    }

    // Special chapters: 序章, 终章, 尾声, 楔子, 引子, 番外, 代序, 跋
    for &special in &["序章", "终章", "尾声", "楔子", "引子", "番外", "代序", "跋", "尾聲"] {
        if line.starts_with(special) {
            return Some(special);
        }
    }

    // English: "Chapter N" / "CHAPTER N"
    if let Some(rest) = line.strip_prefix("Chapter ")
        .or_else(|| line.strip_prefix("CHAPTER "))
    {
        let digits_end = rest.chars().take_while(|c| c.is_ascii_digit()).count();
        if digits_end > 0 {
            return Some(&line[.."Chapter ".len() + digits_end]);
        }
    }

    None
}

fn unit_priority(unit: char) -> usize {
    match unit {
        '章' => 7,
        '回' => 6,
        '节' | '節' => 5,
        '卷' => 4,
        '集' => 3,
        '部' => 2,
        _ => 0,
    }
}

/// Returns the byte length of leading Chinese numerals + Arabic digits in `s`.
/// Returns `None` if no numeral is found.
fn chinese_number_end(s: &str) -> Option<usize> {
    let chinese_digits = [
        '零', '一', '二', '三', '四', '五', '六', '七', '八', '九', '十', '百', '千', '万',
        '壹', '贰', '叁', '肆', '伍', '陆', '柒', '捌', '玖', '拾', '佰', '仟',
    ];
    let mut chars = s.chars();
    let first = chars.next()?;
    if !first.is_ascii_digit() && !chinese_digits.contains(&first) {
        return None;
    }
    let mut end = first.len_utf8();
    for ch in chars {
        if ch.is_ascii_digit() || chinese_digits.contains(&ch) {
            end += ch.len_utf8();
        } else {
            break;
        }
    }
    Some(end)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_chinese_chapter_headings() {
        let text = "第一章 相遇\n正文开始\n第二章 离别\n";
        let toc = detect_chapter_headings(text);
        assert_eq!(toc.len(), 2);
        assert_eq!(toc[0].label(), "第一章");
        assert_eq!(toc[0].offset(), 0);
        assert_eq!(toc[1].label(), "第二章");
    }

    #[test]
    fn detects_arabic_chapter_numbers() {
        let toc = detect_chapter_headings("第1章\n内容\n第12回");
        assert_eq!(toc.len(), 2);
        assert_eq!(toc[0].label(), "第1章");
        assert_eq!(toc[1].label(), "第12回");
    }

    #[test]
    fn detects_special_chapters() {
        let toc = detect_chapter_headings("序章\n正文\n尾声\n后记");
        assert_eq!(toc.len(), 2);
        assert_eq!(toc[0].label(), "序章");
        assert_eq!(toc[1].label(), "尾声");
    }

    #[test]
    fn detects_english_chapters() {
        let toc = detect_chapter_headings("Chapter 1\nContent\nChapter 12\nMore");
        assert_eq!(toc.len(), 2);
        assert_eq!(toc[0].label(), "Chapter 1");
        assert_eq!(toc[1].label(), "Chapter 12");
    }

    #[test]
    fn ignores_false_positives() {
        // Chapter markers mid-line or without proper structure are not headings
        let toc = detect_chapter_headings("这是第一次尝试\n今天读了序章部分\n第一眼见到她");
        assert_eq!(toc.len(), 0);
    }

    #[test]
    fn prefers_chapter_over_volume() {
        // "第一卷 第一章" should produce "第一章", not "第一卷"
        let toc = detect_chapter_headings("第一卷 最后一战 第一章 心事一灯知\n第二卷 去日重来 第一章 痛作无家别");
        assert_eq!(toc.len(), 2);
        assert!(toc[0].label().starts_with("第一章"));
        assert!(toc[1].label().starts_with("第一章"));
    }
}
