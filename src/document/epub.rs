use std::cmp::Ordering;
use std::collections::HashMap;
use std::io::{Cursor, Read};
use std::path::Path;

use ego_tree::NodeRef;
use percent_encoding::percent_decode_str;
use scraper::node::Node;
use scraper::{Html, Selector};
use zip::ZipArchive;

use super::{
    AdapterInput, AdapterOutput, CanonicalDocument, DocumentFormat, DocumentMetadata,
    FormatAdapter, LoadError, LoadWarning, Section, TextStyle, TextStyleKind, TocEntry,
    detect_chapter_headings,
};

pub(super) struct EpubAdapter;

impl FormatAdapter for EpubAdapter {
    fn format(&self) -> DocumentFormat { DocumentFormat::Epub }

    fn probe(&self, path: &Path, prefix: &[u8]) -> u8 {
        if !prefix.starts_with(b"PK\x03\x04") {
            return 0;
        }
        if path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("epub"))
        {
            100
        } else {
            70
        }
    }

    fn load(&self, input: AdapterInput<'_>) -> Result<AdapterOutput, LoadError> {
        let cursor = Cursor::new(input.bytes);
        let archive =
            ZipArchive::new(cursor).map_err(|error| LoadError::InvalidEpub(error.to_string()))?;
        let mut archive = ArchiveReader::new(archive, input.limits.max_text_bytes)?;
        let mut warnings = Vec::new();

        let container_path = "META-INF/container.xml";
        let opf_from_container = archive
            .read_text(container_path, 1024 * 1024)?
            .and_then(|xml| parse_container_path(&xml));
        let mut opf_path = if let Some(path) = opf_from_container {
            path
        } else if let Some(path) = archive.first_with_extension("opf") {
            warnings.push(LoadWarning::new(
                "epub.container_recovered",
                "META-INF/container.xml was missing or invalid; located the OPF package directly",
            ));
            path
        } else {
            warnings.push(LoadWarning::new(
                "epub.package_missing",
                "the OPF package was missing; recovered readable HTML files by filename",
            ));
            return load_without_package(archive, input.path, warnings);
        };

        let package_xml = if let Some(xml) = archive.read_text(&opf_path, 4 * 1024 * 1024)? {
            xml
        } else if let Some(recovered_path) = archive.first_with_extension("opf") {
            warnings.push(LoadWarning::new(
                "epub.package_path_recovered",
                format!("package file {opf_path} was missing; used {recovered_path} instead"),
            ));
            opf_path = recovered_path;
            archive.read_text(&opf_path, 4 * 1024 * 1024)?.ok_or_else(|| {
                LoadError::InvalidEpub(format!("package file {opf_path} is missing"))
            })?
        } else {
            warnings.push(LoadWarning::new(
                "epub.package_missing",
                "the declared OPF package was missing; recovered readable HTML files by filename",
            ));
            return load_without_package(archive, input.path, warnings);
        };
        let package = match parse_package(&package_xml, &opf_path) {
            Ok(package) => package,
            Err(error) if error.contains("fixed-layout") => {
                return Err(LoadError::InvalidEpub(error));
            }
            Err(error) => {
                warnings.push(LoadWarning::new(
                    "epub.package_recovered",
                    format!(
                        "the OPF package was invalid ({error}); recovered HTML files by filename"
                    ),
                ));
                return load_without_package(archive, input.path, warnings);
            }
        };

        let mut content_paths = package.spine.clone();
        if content_paths.is_empty() {
            content_paths = package.html_items.clone();
            content_paths.sort_by(|left, right| natural_cmp(left, right));
            warnings.push(LoadWarning::new(
                "epub.spine_recovered",
                "the EPUB spine was missing or empty; ordered manifest HTML items by filename",
            ));
        }
        if content_paths.is_empty() {
            return Err(LoadError::InvalidEpub(
                "the archive contains no readable HTML content".to_owned(),
            ));
        }

        let assembled = assemble_document(
            &mut archive,
            &content_paths,
            &package.metadata,
            input.limits.max_text_bytes,
        )?;
        warnings.extend(assembled.warnings);

        let mut toc = package
            .nav_path
            .as_deref()
            .and_then(|path| {
                archive
                    .read_text(path, 8 * 1024 * 1024)
                    .ok()
                    .flatten()
                    .map(|html| parse_nav_document(&html, path, &assembled.path_offsets))
            })
            .unwrap_or_default();
        if toc.is_empty() {
            toc = package
                .ncx_path
                .as_deref()
                .and_then(|path| {
                    archive
                        .read_text(path, 8 * 1024 * 1024)
                        .ok()
                        .flatten()
                        .map(|xml| parse_ncx(&xml, path, &assembled.path_offsets))
                })
                .unwrap_or_default();
        }
        // When the NCX contains fragment references that were stripped
        // (e.g. Calibre filepos anchors), many entries collapse to the
        // same byte offset.  Discard the NCX result and fall back to
        // heading-based navigation.
        if toc_has_collapsed_offsets(&toc) {
            toc.clear();
        }
        if toc.is_empty() {
            toc = assembled.heading_toc;
            if !toc.is_empty() {
                warnings.push(LoadWarning::new(
                    "epub.toc_recovered",
                    "the EPUB navigation was missing or ambiguous; built a table of contents from headings",
                ));
            }
        }
        if toc.is_empty() {
            toc = detect_chapter_headings(&assembled.text);
            if !toc.is_empty() {
                warnings.push(LoadWarning::new(
                    "epub.toc_recovered",
                    "the EPUB had no usable navigation; detected chapter headings from text",
                ));
            }
        }

        Ok(AdapterOutput {
            document: CanonicalDocument::new(
                assembled.text,
                package.metadata,
                assembled.sections,
                toc,
                assembled.styles,
            ),
            warnings,
        })
    }
}

struct ArchiveReader<'a> {
    archive: ZipArchive<Cursor<&'a [u8]>>,
    names: HashMap<String, String>,
    text_budget_remaining: usize,
}

impl<'a> ArchiveReader<'a> {
    fn new(
        mut archive: ZipArchive<Cursor<&'a [u8]>>,
        text_budget: usize,
    ) -> Result<Self, LoadError> {
        let mut names = HashMap::new();
        for index in 0..archive.len() {
            let file = archive
                .by_index(index)
                .map_err(|error| LoadError::InvalidEpub(error.to_string()))?;
            if !file.is_dir() {
                names.insert(normalise_archive_name(file.name()), file.name().to_owned());
            }
        }
        Ok(Self { archive, names, text_budget_remaining: text_budget })
    }

    fn first_with_extension(&self, extension: &str) -> Option<String> {
        let suffix = format!(".{extension}");
        let mut matches = self
            .names
            .keys()
            .filter(|name| name.to_ascii_lowercase().ends_with(&suffix))
            .cloned()
            .collect::<Vec<_>>();
        matches.sort_by(|left, right| natural_cmp(left, right));
        matches.into_iter().next()
    }

    fn html_files(&self) -> Vec<String> {
        let mut matches = self
            .names
            .keys()
            .filter(|name| {
                let lower = name.to_ascii_lowercase();
                lower.ends_with(".xhtml") || lower.ends_with(".html") || lower.ends_with(".htm")
            })
            .cloned()
            .collect::<Vec<_>>();
        matches.sort_by(|left, right| natural_cmp(left, right));
        matches
    }

    fn read_text(
        &mut self,
        name: &str,
        per_file_limit: usize,
    ) -> Result<Option<String>, LoadError> {
        let normalised = normalise_archive_name(name);
        let Some(actual_name) = self.names.get(&normalised).cloned() else {
            return Ok(None);
        };
        let mut file = self
            .archive
            .by_name(&actual_name)
            .map_err(|error| LoadError::InvalidEpub(error.to_string()))?;
        let allowed = per_file_limit.min(self.text_budget_remaining);
        if file.size() > allowed as u64 {
            return Err(LoadError::TextTooLarge { limit: self.text_budget_remaining });
        }
        let mut bytes = Vec::with_capacity(file.size() as usize);
        file.by_ref()
            .take(allowed.saturating_add(1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|error| LoadError::InvalidEpub(error.to_string()))?;
        if bytes.len() > allowed {
            return Err(LoadError::TextTooLarge { limit: self.text_budget_remaining });
        }
        self.text_budget_remaining = self.text_budget_remaining.saturating_sub(bytes.len());
        Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
    }
}

#[derive(Debug)]
struct Package {
    metadata: DocumentMetadata,
    spine: Vec<String>,
    html_items: Vec<String>,
    nav_path: Option<String>,
    ncx_path: Option<String>,
}

#[derive(Clone, Debug)]
struct ManifestItem {
    href: String,
    media_type: String,
    properties: String,
}

fn parse_container_path(xml: &str) -> Option<String> {
    let document = roxmltree::Document::parse(xml).ok()?;
    let path = document
        .descendants()
        .find(|node| node.is_element() && node.tag_name().name() == "rootfile")?
        .attribute("full-path")?;
    normalise_relative_archive_path("", path)
}

fn parse_package(xml: &str, opf_path: &str) -> Result<Package, String> {
    let document = roxmltree::Document::parse(xml).map_err(|error| error.to_string())?;
    let base = archive_parent(opf_path);
    let mut metadata = DocumentMetadata::default();
    let text_of = |name: &str| {
        document
            .descendants()
            .find(|node| node.is_element() && node.tag_name().name() == name)
            .and_then(|node| node.text())
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_owned)
    };
    metadata.title = text_of("title");
    metadata.author = text_of("creator");
    metadata.language = text_of("language");

    let fixed_layout = document.descendants().any(|node| {
        node.is_element()
            && node.tag_name().name() == "meta"
            && node.attribute("property") == Some("rendition:layout")
            && (node.text().is_some_and(|value| value.trim() == "pre-paginated")
                || node.attribute("content") == Some("pre-paginated"))
    });
    if fixed_layout {
        return Err("fixed-layout EPUB is not supported; this format lays out text as fixed-position pages (similar to a PDF) and cannot be reflowed as plain text".to_owned());
    }

    let mut manifest = HashMap::new();
    for item in
        document.descendants().filter(|node| node.is_element() && node.tag_name().name() == "item")
    {
        let (Some(id), Some(href)) = (item.attribute("id"), item.attribute("href")) else {
            continue;
        };
        let Some(href) = normalise_relative_archive_path(base, href) else {
            continue;
        };
        manifest.insert(
            id.to_owned(),
            ManifestItem {
                href,
                media_type: item.attribute("media-type").unwrap_or_default().to_owned(),
                properties: item.attribute("properties").unwrap_or_default().to_owned(),
            },
        );
    }

    let mut spine = Vec::new();
    for itemref in document
        .descendants()
        .filter(|node| node.is_element() && node.tag_name().name() == "itemref")
    {
        let Some(idref) = itemref.attribute("idref") else {
            continue;
        };
        if let Some(item) = manifest.get(idref)
            && is_html_item(item)
        {
            spine.push(item.href.clone());
        }
    }

    let mut html_items = manifest
        .values()
        .filter(|item| {
            is_html_item(item)
                && !item.properties.split_whitespace().any(|property| property == "nav")
        })
        .map(|item| item.href.clone())
        .collect::<Vec<_>>();
    html_items.sort_by(|left, right| natural_cmp(left, right));

    let nav_path = manifest
        .values()
        .find(|item| item.properties.split_whitespace().any(|value| value == "nav"))
        .map(|item| item.href.clone());

    let spine_toc_id = document
        .descendants()
        .find(|node| node.is_element() && node.tag_name().name() == "spine")
        .and_then(|node| node.attribute("toc"));
    let ncx_path =
        spine_toc_id.and_then(|id| manifest.get(id)).map(|item| item.href.clone()).or_else(|| {
            manifest
                .values()
                .find(|item| item.media_type == "application/x-dtbncx+xml")
                .map(|item| item.href.clone())
        });

    Ok(Package { metadata, spine, html_items, nav_path, ncx_path })
}

fn is_html_item(item: &ManifestItem) -> bool {
    item.media_type == "application/xhtml+xml"
        || item.media_type == "text/html"
        || item.href.to_ascii_lowercase().ends_with(".xhtml")
        || item.href.to_ascii_lowercase().ends_with(".html")
        || item.href.to_ascii_lowercase().ends_with(".htm")
}

struct AssembledDocument {
    text: String,
    sections: Vec<Section>,
    styles: Vec<TextStyle>,
    heading_toc: Vec<TocEntry>,
    path_offsets: HashMap<String, usize>,
    warnings: Vec<LoadWarning>,
}

fn assemble_document(
    archive: &mut ArchiveReader<'_>,
    paths: &[String],
    metadata: &DocumentMetadata,
    text_limit: usize,
) -> Result<AssembledDocument, LoadError> {
    let mut text = String::new();
    let mut sections = Vec::new();
    let mut styles = Vec::new();
    let mut heading_toc = Vec::new();
    let mut path_offsets = HashMap::new();
    let mut warnings = Vec::new();

    for path in paths {
        let Some(html) = archive.read_text(path, archive.text_budget_remaining)? else {
            warnings.push(LoadWarning::new(
                "epub.spine_item_missing",
                format!("spine item {path} is missing"),
            ));
            continue;
        };
        let rendered = render_html(&html);
        if rendered.text.trim().is_empty() {
            continue;
        }

        if !text.is_empty() {
            ensure_newlines(&mut text, 2);
        }
        let start = text.len();
        path_offsets.insert(normalise_archive_name(path), start);
        text.push_str(&rendered.text);
        if text.len() > text_limit {
            return Err(LoadError::TextTooLarge { limit: text_limit });
        }
        let end = text.len();

        let title = rendered
            .headings
            .first()
            .map(|heading| heading.label.clone())
            .or_else(|| metadata.title().filter(|_| paths.len() == 1).map(str::to_owned))
            .unwrap_or_else(|| file_stem(path));
        sections.push(Section { title, range: start..end });
        styles.extend(rendered.styles.into_iter().map(|mut style| {
            style.range.start += start;
            style.range.end += start;
            style
        }));
        heading_toc.extend(rendered.headings.into_iter().map(|heading| TocEntry {
            label: heading.label,
            offset: start + heading.offset,
            depth: heading.depth,
        }));
    }

    if sections.is_empty() {
        return Err(LoadError::InvalidEpub("the EPUB contains no readable text".to_owned()));
    }
    Ok(AssembledDocument { text, sections, styles, heading_toc, path_offsets, warnings })
}

fn load_without_package(
    mut archive: ArchiveReader<'_>,
    source_path: &Path,
    mut warnings: Vec<LoadWarning>,
) -> Result<AdapterOutput, LoadError> {
    let paths = archive.html_files();
    if paths.is_empty() {
        return Err(LoadError::InvalidEpub(
            "the archive contains no readable HTML content".to_owned(),
        ));
    }
    warnings.push(LoadWarning::new(
        "epub.spine_recovered",
        "ordered HTML content by filename because no usable spine was available",
    ));
    let title =
        source_path.file_stem().and_then(|name| name.to_str()).unwrap_or("Untitled").to_owned();
    let metadata = DocumentMetadata { title: Some(title), ..DocumentMetadata::default() };
    let text_limit = archive.text_budget_remaining;
    let assembled = assemble_document(&mut archive, &paths, &metadata, text_limit)?;
    warnings.extend(assembled.warnings);
    if !assembled.heading_toc.is_empty() {
        warnings.push(LoadWarning::new(
            "epub.toc_recovered",
            "built a table of contents from headings",
        ));
    }
    Ok(AdapterOutput {
        document: CanonicalDocument::new(
            assembled.text,
            metadata,
            assembled.sections,
            assembled.heading_toc,
            assembled.styles,
        ),
        warnings,
    })
}

#[derive(Debug)]
struct RenderedHtml {
    text: String,
    styles: Vec<TextStyle>,
    headings: Vec<RenderedHeading>,
}

#[derive(Debug)]
struct RenderedHeading {
    label: String,
    offset: usize,
    depth: u8,
}

#[derive(Default)]
struct HtmlRenderer {
    text: String,
    styles: Vec<TextStyle>,
    headings: Vec<RenderedHeading>,
    pending_space: bool,
    pre_depth: usize,
}

impl HtmlRenderer {
    fn walk(&mut self, node: NodeRef<'_, Node>) {
        match node.value() {
            Node::Text(text) => self.push_text(text),
            Node::Element(element) => {
                let name = element.name();
                if matches!(name, "head" | "script" | "style" | "noscript" | "svg" | "nav") {
                    return;
                }
                if name == "br" {
                    self.ensure_newlines(1);
                    return;
                }
                if name == "img" {
                    self.push_literal("[Image]");
                    return;
                }

                let heading_level = heading_level(name);
                let paragraph_block =
                    matches!(name, "p" | "blockquote" | "pre" | "li") || heading_level.is_some();
                if paragraph_block {
                    self.ensure_newlines(2);
                } else if name == "tr" {
                    self.ensure_newlines(1);
                }
                if name == "li" {
                    self.push_literal("- ");
                }

                let style_start = self.text.len();
                if name == "pre" {
                    self.pre_depth += 1;
                }
                for child in node.children() {
                    self.walk(child);
                }
                if name == "pre" {
                    self.pre_depth = self.pre_depth.saturating_sub(1);
                }
                let style_end = self.text.len();

                let style_kind = match name {
                    "em" | "i" => Some(TextStyleKind::Emphasis),
                    "strong" | "b" => Some(TextStyleKind::Strong),
                    _ => heading_level.map(TextStyleKind::Heading),
                };
                if let Some(kind) = style_kind
                    && style_start < style_end
                {
                    self.styles.push(TextStyle { range: style_start..style_end, kind });
                }
                if let Some(depth) = heading_level {
                    let label = self.text[style_start..style_end].trim().to_owned();
                    if !label.is_empty() {
                        self.headings.push(RenderedHeading { label, offset: style_start, depth });
                    }
                }

                if matches!(name, "td" | "th") {
                    self.push_literal("\t");
                }
                if paragraph_block {
                    self.ensure_newlines(2);
                } else if name == "tr" {
                    self.ensure_newlines(1);
                }
            }
            _ => {
                for child in node.children() {
                    self.walk(child);
                }
            }
        }
    }

    fn push_text(&mut self, value: &str) {
        if self.pre_depth > 0 {
            self.text.push_str(&value.replace("\r\n", "\n").replace('\r', "\n"));
            return;
        }
        for character in value.chars() {
            if character.is_whitespace() {
                self.pending_space = true;
                continue;
            }
            if self.pending_space
                && !self.text.is_empty()
                && !self.text.ends_with(['\n', '\t', ' '])
            {
                self.text.push(' ');
            }
            self.pending_space = false;
            self.text.push(character);
        }
    }

    fn push_literal(&mut self, value: &str) {
        if self.pending_space && !self.text.is_empty() && !self.text.ends_with(['\n', '\t', ' ']) {
            self.text.push(' ');
        }
        self.pending_space = false;
        self.text.push_str(value);
    }

    fn ensure_newlines(&mut self, count: usize) {
        self.pending_space = false;
        while self.text.ends_with([' ', '\t']) {
            self.text.pop();
        }
        if self.text.is_empty() {
            return;
        }
        let existing = self.text.chars().rev().take_while(|character| *character == '\n').count();
        for _ in existing..count {
            self.text.push('\n');
        }
    }

    fn finish(mut self) -> RenderedHtml {
        while self.text.ends_with(char::is_whitespace) {
            self.text.pop();
        }
        let len = self.text.len();
        for style in &mut self.styles {
            style.range.end = style.range.end.min(len);
        }
        self.styles.retain(|style| style.range.start < style.range.end);
        self.headings.retain(|heading| heading.offset < len);
        RenderedHtml { text: self.text, styles: self.styles, headings: self.headings }
    }
}

fn render_html(source: &str) -> RenderedHtml {
    let document = Html::parse_document(source);
    let mut renderer = HtmlRenderer::default();
    renderer.walk(document.tree.root());
    renderer.finish()
}

fn heading_level(name: &str) -> Option<u8> {
    match name {
        "h1" => Some(1),
        "h2" => Some(2),
        "h3" => Some(3),
        "h4" => Some(4),
        "h5" => Some(5),
        "h6" => Some(6),
        _ => None,
    }
}

fn parse_nav_document(
    source: &str,
    nav_path: &str,
    path_offsets: &HashMap<String, usize>,
) -> Vec<TocEntry> {
    let document = Html::parse_document(source);
    let nav_selector = Selector::parse("nav").expect("static selector");
    let link_selector = Selector::parse("a[href]").expect("static selector");
    let nav = document
        .select(&nav_selector)
        .find(|element| {
            element.value().attrs().any(|(name, value)| {
                name.ends_with("type") && value.split_whitespace().any(|part| part == "toc")
            })
        })
        .or_else(|| document.select(&nav_selector).next());
    let Some(nav) = nav else {
        return Vec::new();
    };

    nav.select(&link_selector)
        .filter_map(|link| {
            let href = link.value().attr("href")?;
            let path = normalise_relative_archive_path(archive_parent(nav_path), href)?;
            let offset = *path_offsets.get(&path)?;
            let label = collapse_whitespace(&link.text().collect::<String>());
            (!label.is_empty()).then_some(TocEntry { label, offset, depth: 0 })
        })
        .collect()
}

fn parse_ncx(source: &str, ncx_path: &str, path_offsets: &HashMap<String, usize>) -> Vec<TocEntry> {
    let Ok(document) = roxmltree::Document::parse(source) else {
        return Vec::new();
    };
    document
        .descendants()
        .filter(|node| node.is_element() && node.tag_name().name() == "navPoint")
        .filter_map(|point| {
            let label = point
                .descendants()
                .find(|node| node.is_element() && node.tag_name().name() == "text")?
                .text()
                .map(collapse_whitespace)?;
            let href = point
                .descendants()
                .find(|node| node.is_element() && node.tag_name().name() == "content")?
                .attribute("src")?;
            let path = normalise_relative_archive_path(archive_parent(ncx_path), href)?;
            let offset = *path_offsets.get(&path)?;
            let depth = point
                .ancestors()
                .filter(|node| node.is_element() && node.tag_name().name() == "navPoint")
                .count()
                .saturating_sub(1)
                .min(u8::MAX as usize) as u8;
            Some(TocEntry { label, offset, depth })
        })
        .collect()
}

fn normalise_relative_archive_path(base: &str, href: &str) -> Option<String> {
    let href = href.split(['#', '?']).next().unwrap_or_default();
    let decoded = percent_decode_str(href).decode_utf8_lossy();
    if decoded.starts_with('/') || decoded.contains(':') {
        return None;
    }
    let mut parts =
        base.split('/').filter(|part| !part.is_empty()).map(str::to_owned).collect::<Vec<_>>();
    for part in decoded.replace('\\', "/").split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            other => parts.push(other.to_owned()),
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

fn normalise_archive_name(name: &str) -> String {
    name.trim_start_matches('/')
        .replace('\\', "/")
        .split('/')
        .fold(Vec::<&str>::new(), |mut parts, part| {
            match part {
                "" | "." => {}
                ".." => {
                    parts.pop();
                }
                other => parts.push(other),
            }
            parts
        })
        .join("/")
}

fn archive_parent(path: &str) -> &str { path.rsplit_once('/').map_or("", |(parent, _)| parent) }

fn file_stem(path: &str) -> String {
    path.rsplit('/')
        .next()
        .unwrap_or(path)
        .rsplit_once('.')
        .map_or_else(|| path.to_owned(), |(stem, _)| stem.to_owned())
}

fn collapse_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn ensure_newlines(text: &mut String, count: usize) {
    let existing = text.chars().rev().take_while(|character| *character == '\n').count();
    for _ in existing..count {
        text.push('\n');
    }
}

fn natural_cmp(left: &str, right: &str) -> Ordering {
    let left = left.as_bytes();
    let right = right.as_bytes();
    let (mut left_index, mut right_index) = (0, 0);
    while left_index < left.len() && right_index < right.len() {
        if left[left_index].is_ascii_digit() && right[right_index].is_ascii_digit() {
            let left_end = digit_end(left, left_index);
            let right_end = digit_end(right, right_index);
            let left_number = &left[left_index..left_end];
            let right_number = &right[right_index..right_end];
            let left_significant = trim_zeroes(left_number);
            let right_significant = trim_zeroes(right_number);
            let ordering = left_significant
                .len()
                .cmp(&right_significant.len())
                .then_with(|| left_significant.cmp(right_significant))
                .then_with(|| left_number.len().cmp(&right_number.len()));
            if ordering != Ordering::Equal {
                return ordering;
            }
            left_index = left_end;
            right_index = right_end;
        } else {
            let ordering =
                left[left_index].to_ascii_lowercase().cmp(&right[right_index].to_ascii_lowercase());
            if ordering != Ordering::Equal {
                return ordering;
            }
            left_index += 1;
            right_index += 1;
        }
    }
    left.len().cmp(&right.len())
}

fn digit_end(bytes: &[u8], start: usize) -> usize {
    let mut end = start;
    while end < bytes.len() && bytes[end].is_ascii_digit() {
        end += 1;
    }
    end
}

fn toc_has_collapsed_offsets(toc: &[TocEntry]) -> bool {
    if toc.len() <= 1 {
        return toc.is_empty();
    }
    // When every entry resolves to the same byte offset the NCX
    // fragments were stripped and the TOC is unusable.
    let first = toc[0].offset();
    toc.iter().all(|entry| entry.offset() == first)
}

fn trim_zeroes(bytes: &[u8]) -> &[u8] {
    let first_nonzero =
        bytes.iter().position(|byte| *byte != b'0').unwrap_or(bytes.len().saturating_sub(1));
    &bytes[first_nonzero..]
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Write};

    use zip::ZipWriter;
    use zip::write::SimpleFileOptions;

    use super::*;

    #[test]
    fn renders_html_blocks_images_and_styles() {
        let rendered = render_html(
            "<html><body><h1>第一章</h1><p>Hello <em>world</em>.</p><p><img/></p></body></html>",
        );
        assert_eq!(rendered.text, "第一章\n\nHello world.\n\n[Image]");
        assert!(rendered.styles.iter().any(|style| style.kind == TextStyleKind::Emphasis));
        assert_eq!(rendered.headings[0].label, "第一章");
    }

    #[test]
    fn opens_standard_epub_in_spine_order() {
        let bytes = make_epub(&[
            (
                "META-INF/container.xml",
                r#"<?xml version="1.0"?><container><rootfiles><rootfile full-path="OPS/book.opf"/></rootfiles></container>"#,
            ),
            (
                "OPS/book.opf",
                r#"<?xml version="1.0"?><package><metadata><title>Demo</title></metadata><manifest><item id="two" href="2.xhtml" media-type="application/xhtml+xml"/><item id="ten" href="10.xhtml" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="ten"/><itemref idref="two"/></spine></package>"#,
            ),
            ("OPS/2.xhtml", "<html><body><h1>Two</h1><p>second</p></body></html>"),
            ("OPS/10.xhtml", "<html><body><h1>Ten</h1><p>first</p></body></html>"),
        ]);
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(&bytes).unwrap();

        let loaded = super::super::open_document(
            super::super::DocumentSource::from_path(file.path()),
            super::super::LoadOptions::default(),
        )
        .unwrap();

        assert_eq!(loaded.format(), DocumentFormat::Epub);
        assert_eq!(loaded.document().metadata().title(), Some("Demo"));
        assert!(
            loaded.document().text().find("Ten").unwrap()
                < loaded.document().text().find("Two").unwrap()
        );
        assert_eq!(loaded.document().sections().len(), 2);
        assert_eq!(loaded.document().toc()[0].label(), "Ten");
    }

    #[test]
    fn recovers_epub_without_package() {
        let bytes = make_epub(&[
            ("text/10.xhtml", "<html><body><h1>Ten</h1></body></html>"),
            ("text/2.xhtml", "<html><body><h1>Two</h1></body></html>"),
        ]);
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(&bytes).unwrap();

        let loaded = super::super::open_document(
            super::super::DocumentSource::from_path(file.path()),
            super::super::LoadOptions::default(),
        )
        .unwrap();

        assert!(
            loaded.document().text().find("Two").unwrap()
                < loaded.document().text().find("Ten").unwrap()
        );
        assert!(loaded.warnings().iter().any(|warning| warning.code() == "epub.package_missing"));
    }

    #[test]
    fn recovers_when_container_points_to_a_missing_package() {
        let bytes = make_epub(&[
            (
                "META-INF/container.xml",
                r#"<container><rootfiles><rootfile full-path="missing.opf"/></rootfiles></container>"#,
            ),
            (
                "OPS/book.opf",
                r#"<package><manifest><item id="one" href="1.xhtml" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="one"/></spine></package>"#,
            ),
            ("OPS/1.xhtml", "<html><body><h1>Recovered</h1></body></html>"),
        ]);
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(&bytes).unwrap();

        let loaded = super::super::open_document(
            super::super::DocumentSource::from_path(file.path()),
            super::super::LoadOptions::default(),
        )
        .unwrap();

        assert!(loaded.document().text().contains("Recovered"));
        assert!(
            loaded.warnings().iter().any(|warning| warning.code() == "epub.package_path_recovered")
        );
    }

    #[test]
    fn rejects_fixed_layout_epub() {
        let bytes = make_epub(&[
            (
                "META-INF/container.xml",
                r#"<container><rootfiles><rootfile full-path="book.opf"/></rootfiles></container>"#,
            ),
            (
                "book.opf",
                r#"<package><metadata><meta property="rendition:layout">pre-paginated</meta></metadata><manifest><item id="one" href="1.xhtml" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="one"/></spine></package>"#,
            ),
            ("1.xhtml", "<html><body>fixed</body></html>"),
        ]);
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(&bytes).unwrap();

        let error = super::super::open_document(
            super::super::DocumentSource::from_path(file.path()),
            super::super::LoadOptions::default(),
        )
        .unwrap_err();

        assert!(error.to_string().contains("fixed-layout"));
    }

    #[test]
    fn natural_sort_orders_numeric_segments() {
        assert_eq!(natural_cmp("chapter2.xhtml", "chapter10.xhtml"), Ordering::Less);
    }

    #[test]
    fn collapsed_toc_detects_stripped_fragments() {
        let entries =
            vec![TocEntry { label: "a".into(), offset: 100, depth: 0 }, TocEntry { label: "b".into(), offset: 100, depth: 0 }];
        assert!(toc_has_collapsed_offsets(&entries));
    }

    #[test]
    fn collapsed_toc_passes_valid_toc() {
        let entries =
            vec![TocEntry { label: "a".into(), offset: 100, depth: 0 }, TocEntry { label: "b".into(), offset: 200, depth: 0 }];
        assert!(!toc_has_collapsed_offsets(&entries));
    }

    #[test]
    fn collapsed_toc_returns_false_for_single_entry() {
        let entries = vec![TocEntry { label: "a".into(), offset: 100, depth: 0 }];
        assert!(!toc_has_collapsed_offsets(&entries));
    }

    fn make_epub(entries: &[(&str, &str)]) -> Vec<u8> {
        let cursor = Cursor::new(Vec::new());
        let mut writer = ZipWriter::new(cursor);
        let options = SimpleFileOptions::default();
        for (name, contents) in entries {
            writer.start_file(*name, options).unwrap();
            writer.write_all(contents.as_bytes()).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }
}
