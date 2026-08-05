use std::ops::Range;
use std::path::Path;

use ego_tree::NodeRef;
use pulldown_cmark::{Alignment, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use scraper::Html;
use scraper::node::Node;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::{
    AdapterInput, AdapterOutput, CanonicalDocument, DocumentFormat, DocumentMetadata,
    FormatAdapter, LoadError, Section, TextStyle, TextStyleKind, TocEntry,
};

const MAX_TABLE_WIDTH: usize = 100;

pub(super) struct MarkdownAdapter;

impl FormatAdapter for MarkdownAdapter {
    fn format(&self) -> DocumentFormat { DocumentFormat::Markdown }

    fn probe(&self, path: &Path, _prefix: &[u8]) -> u8 {
        path.extension()
            .and_then(|extension| extension.to_str())
            .filter(|extension| {
                extension.eq_ignore_ascii_case("md") || extension.eq_ignore_ascii_case("markdown")
            })
            .map_or(0, |_| 95)
    }

    fn load(&self, input: AdapterInput<'_>) -> Result<AdapterOutput, LoadError> {
        let bytes = input.bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(input.bytes);
        let source = std::str::from_utf8(bytes).map_err(|_| LoadError::InvalidMarkdownEncoding)?;
        if source.len() > input.limits.max_text_bytes {
            return Err(LoadError::TextTooLarge { limit: input.limits.max_text_bytes });
        }
        let source = normalise_newlines(source);
        let fallback_title =
            input.path.file_stem().and_then(|name| name.to_str()).unwrap_or("Untitled").to_owned();

        let mut options = Options::empty();
        options.insert(
            Options::ENABLE_TABLES
                | Options::ENABLE_STRIKETHROUGH
                | Options::ENABLE_TASKLISTS
                | Options::ENABLE_YAML_STYLE_METADATA_BLOCKS,
        );
        let mut renderer = MarkdownRenderer::default();
        for event in Parser::new_ext(&source, options) {
            renderer.handle(event);
        }
        let rendered = renderer.finish(fallback_title);
        if rendered.text.len() > input.limits.max_text_bytes {
            return Err(LoadError::TextTooLarge { limit: input.limits.max_text_bytes });
        }

        let end = rendered.text.len();
        Ok(AdapterOutput {
            document: CanonicalDocument::new(
                rendered.text,
                rendered.metadata,
                vec![Section { title: rendered.section_title, range: 0..end }],
                rendered.toc,
                rendered.styles,
            ),
            warnings: Vec::new(),
        })
    }
}

#[derive(Default)]
struct MarkdownRenderer {
    sink: StyledText,
    toc: Vec<TocEntry>,
    headings: Vec<String>,
    heading: Option<HeadingFrame>,
    lists: Vec<ListFrame>,
    pending_item_prefix: Option<String>,
    quote_depth: usize,
    code_block: Option<CodeBlockFrame>,
    table: Option<TableBuilder>,
    metadata_block: Option<String>,
    metadata: FrontMatter,
}

struct HeadingFrame {
    level: u8,
    start: usize,
    label: String,
}

struct ListFrame {
    next: Option<u64>,
}

struct CodeBlockFrame {
    at_line_start: bool,
}

struct RenderedMarkdown {
    text: String,
    styles: Vec<TextStyle>,
    toc: Vec<TocEntry>,
    metadata: DocumentMetadata,
    section_title: String,
}

impl MarkdownRenderer {
    fn handle(&mut self, event: Event<'_>) {
        if self.metadata_block.is_some() {
            self.handle_metadata(event);
            return;
        }

        if self.table.is_some() {
            if matches!(event, Event::End(TagEnd::Table)) {
                let table = self.table.take().expect("table state exists");
                self.sink.ensure_newlines(2);
                table.render_into(&mut self.sink);
                self.sink.ensure_newlines(2);
            } else if let Some(table) = self.table.as_mut() {
                table.handle(event);
            }
            return;
        }

        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) => {
                self.flush_item_prefix();
                self.push_heading_label(&text);
                if self.code_block.is_some() {
                    self.push_code_block_text(&text);
                } else {
                    self.sink.push_text(&text);
                }
            }
            Event::Code(code) => {
                self.flush_item_prefix();
                self.push_heading_label(&code);
                self.sink.push_styled(&code, TextStyleKind::Code);
            }
            Event::Html(html) => self.push_html(&html, true),
            Event::InlineHtml(html) => self.push_html(&html, false),
            Event::SoftBreak => {
                self.flush_item_prefix();
                self.push_heading_label(" ");
                self.sink.push_space();
            }
            Event::HardBreak => {
                self.flush_item_prefix();
                self.push_heading_label(" ");
                self.sink.ensure_newlines(1);
                self.push_line_prefix();
            }
            Event::Rule => {
                self.sink.ensure_newlines(2);
                self.push_line_prefix();
                self.sink.push_literal("────────────────────────");
                self.sink.ensure_newlines(2);
            }
            Event::TaskListMarker(checked) => {
                self.flush_item_prefix();
                self.sink.push_literal(if checked { "[x] " } else { "[ ] " });
            }
            _ => {}
        }
    }

    fn start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph => {
                if let Some(prefix) = self.pending_item_prefix.take() {
                    self.sink.ensure_newlines(1);
                    self.sink.push_literal(&prefix);
                } else {
                    self.sink.ensure_newlines(2);
                    self.push_line_prefix();
                }
            }
            Tag::Heading { level, .. } => {
                self.flush_item_prefix();
                self.sink.ensure_newlines(2);
                self.push_line_prefix();
                self.heading = Some(HeadingFrame {
                    level: heading_level(level),
                    start: self.sink.len(),
                    label: String::new(),
                });
            }
            Tag::BlockQuote(_) => {
                self.flush_item_prefix();
                self.sink.ensure_newlines(2);
                self.quote_depth += 1;
                self.sink.start_style(TextStyleKind::Quote);
            }
            Tag::CodeBlock(_) => {
                self.flush_item_prefix();
                self.sink.ensure_newlines(2);
                self.sink.start_style(TextStyleKind::Code);
                self.code_block = Some(CodeBlockFrame { at_line_start: true });
            }
            Tag::List(start) => {
                self.flush_item_prefix();
                if self.lists.is_empty() {
                    self.sink.ensure_newlines(2);
                }
                self.lists.push(ListFrame { next: start });
            }
            Tag::Item => {
                self.sink.ensure_newlines(1);
                let depth = self.lists.len().saturating_sub(1);
                let marker = self.lists.last_mut().map_or_else(
                    || "- ".to_owned(),
                    |list| match list.next.as_mut() {
                        Some(next) => {
                            let marker = format!("{next}. ");
                            *next = next.saturating_add(1);
                            marker
                        }
                        None => "- ".to_owned(),
                    },
                );
                self.pending_item_prefix = Some(format!(
                    "{}{}{}",
                    quote_prefix(self.quote_depth),
                    "  ".repeat(depth),
                    marker
                ));
            }
            Tag::Emphasis => {
                self.flush_item_prefix();
                self.sink.start_style(TextStyleKind::Emphasis);
            }
            Tag::Strong => {
                self.flush_item_prefix();
                self.sink.start_style(TextStyleKind::Strong);
            }
            Tag::Strikethrough => {
                self.flush_item_prefix();
                self.sink.start_style(TextStyleKind::Strikethrough);
            }
            Tag::Link { dest_url, .. } => {
                self.flush_item_prefix();
                self.sink.start_link(dest_url.as_ref());
            }
            Tag::Image { dest_url, .. } => {
                self.flush_item_prefix();
                self.sink.start_image(dest_url.as_ref());
            }
            Tag::Table(alignments) => {
                self.flush_item_prefix();
                self.table = Some(TableBuilder::new(alignments));
            }
            Tag::MetadataBlock(_) => self.metadata_block = Some(String::new()),
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => {
                self.sink.ensure_newlines(if self.lists.is_empty() { 2 } else { 1 });
            }
            TagEnd::Heading(_) => self.finish_heading(),
            TagEnd::BlockQuote(_) => {
                self.sink.end_style(TextStyleKind::Quote);
                self.quote_depth = self.quote_depth.saturating_sub(1);
                self.sink.ensure_newlines(2);
            }
            TagEnd::CodeBlock => {
                self.code_block = None;
                self.sink.end_style(TextStyleKind::Code);
                self.sink.ensure_newlines(2);
            }
            TagEnd::List(_) => {
                self.lists.pop();
                self.pending_item_prefix = None;
                if self.lists.is_empty() {
                    self.sink.ensure_newlines(2);
                }
            }
            TagEnd::Item => {
                self.pending_item_prefix = None;
                self.sink.ensure_newlines(1);
            }
            TagEnd::Emphasis => self.sink.end_style(TextStyleKind::Emphasis),
            TagEnd::Strong => self.sink.end_style(TextStyleKind::Strong),
            TagEnd::Strikethrough => self.sink.end_style(TextStyleKind::Strikethrough),
            TagEnd::Link => self.sink.end_link(),
            TagEnd::Image => self.sink.end_image(),
            _ => {}
        }
    }

    fn handle_metadata(&mut self, event: Event<'_>) {
        match event {
            Event::End(TagEnd::MetadataBlock(_)) => {
                if let Some(raw) = self.metadata_block.take() {
                    self.metadata.merge(parse_front_matter(&raw));
                }
            }
            Event::Text(text) | Event::Code(text) => {
                if let Some(raw) = self.metadata_block.as_mut() {
                    raw.push_str(&text);
                }
            }
            Event::SoftBreak | Event::HardBreak => {
                if let Some(raw) = self.metadata_block.as_mut() {
                    raw.push('\n');
                }
            }
            _ => {}
        }
    }

    fn finish_heading(&mut self) {
        let Some(heading) = self.heading.take() else {
            return;
        };
        let end = self.sink.len();
        if heading.start < end {
            self.sink.styles.push(TextStyle {
                range: heading.start..end,
                kind: TextStyleKind::Heading(heading.level),
            });
        }
        let label = collapse_whitespace(&heading.label);
        if !label.is_empty() && heading.start < end {
            self.headings.push(label.clone());
            self.toc.push(TocEntry {
                label,
                offset: heading.start,
                depth: heading.level.saturating_sub(1),
            });
        }
        self.sink.ensure_newlines(2);
    }

    fn push_heading_label(&mut self, value: &str) {
        if let Some(heading) = self.heading.as_mut() {
            heading.label.push_str(value);
        }
    }

    fn flush_item_prefix(&mut self) {
        if let Some(prefix) = self.pending_item_prefix.take() {
            self.sink.ensure_newlines(1);
            self.sink.push_literal(&prefix);
        }
    }

    fn push_line_prefix(&mut self) {
        if self.quote_depth > 0 {
            self.sink.push_literal(&quote_prefix(self.quote_depth));
        }
    }

    fn push_code_block_text(&mut self, value: &str) {
        for character in value.replace("\r\n", "\n").replace('\r', "\n").chars() {
            let at_line_start = self.code_block.as_ref().is_some_and(|block| block.at_line_start);
            if at_line_start {
                self.push_line_prefix();
                self.sink.push_literal("    ");
                if let Some(block) = self.code_block.as_mut() {
                    block.at_line_start = false;
                }
            }
            if character == '\n' {
                self.sink.ensure_newlines(1);
                if let Some(block) = self.code_block.as_mut() {
                    block.at_line_start = true;
                }
            } else if character == '\t' {
                self.sink.push_literal("    ");
            } else {
                self.sink.push_char(character);
            }
        }
    }

    fn push_html(&mut self, html: &str, block: bool) {
        if html.trim().to_ascii_lowercase().starts_with("<br") {
            self.sink.ensure_newlines(1);
            self.push_line_prefix();
            return;
        }
        let visible = visible_html_text(html);
        if visible.is_empty() {
            return;
        }
        self.push_heading_label(&visible);
        if block {
            self.sink.ensure_newlines(2);
        }
        self.sink.push_text(&visible);
        if block {
            self.sink.ensure_newlines(2);
        }
    }

    fn finish(mut self, fallback_title: String) -> RenderedMarkdown {
        self.sink.finish();
        let first_heading = self.headings.first().cloned();
        let title = self.metadata.title.take().or(first_heading).unwrap_or(fallback_title);
        let metadata = DocumentMetadata {
            title: Some(title.clone()),
            author: self.metadata.author,
            language: self.metadata.language,
        };
        RenderedMarkdown {
            text: self.sink.text,
            styles: self.sink.styles,
            toc: self.toc,
            metadata,
            section_title: title,
        }
    }
}

#[derive(Default)]
struct StyledText {
    text: String,
    styles: Vec<TextStyle>,
    open_styles: Vec<(TextStyleKind, usize)>,
    links: Vec<LinkFrame>,
    image: Option<ImageFrame>,
}

struct LinkFrame {
    start: usize,
    destination: String,
}

struct ImageFrame {
    destination: String,
    alt: String,
}

impl StyledText {
    fn len(&self) -> usize { self.text.len() }

    fn push_text(&mut self, value: &str) {
        for character in value.replace("\r\n", "\n").replace('\r', "\n").chars() {
            self.push_char(character);
        }
    }

    fn push_char(&mut self, character: char) {
        if character.is_control() && !matches!(character, '\n' | '\t') {
            return;
        }
        if let Some(image) = self.image.as_mut() {
            image.alt.push(character);
        } else {
            self.text.push(character);
        }
    }

    fn push_literal(&mut self, value: &str) { self.push_text(value); }

    fn push_space(&mut self) {
        if let Some(image) = self.image.as_mut() {
            if !image.alt.ends_with(char::is_whitespace) {
                image.alt.push(' ');
            }
        } else if !self.text.is_empty() && !self.text.ends_with(char::is_whitespace) {
            self.text.push(' ');
        }
    }

    fn ensure_newlines(&mut self, count: usize) {
        if self.image.is_some() {
            self.push_space();
            return;
        }
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

    fn start_style(&mut self, kind: TextStyleKind) {
        if self.image.is_none() {
            self.open_styles.push((kind, self.len()));
        }
    }

    fn end_style(&mut self, kind: TextStyleKind) {
        let Some(index) = self.open_styles.iter().rposition(|(open, _)| *open == kind) else {
            return;
        };
        let (_, start) = self.open_styles.remove(index);
        if start < self.len() {
            self.styles.push(TextStyle { range: start..self.len(), kind });
        }
    }

    fn push_styled(&mut self, value: &str, kind: TextStyleKind) {
        self.start_style(kind);
        self.push_text(value);
        self.end_style(kind);
    }

    fn start_link(&mut self, destination: &str) {
        if self.image.is_none() {
            self.links
                .push(LinkFrame { start: self.len(), destination: sanitise_inline(destination) });
        }
    }

    fn end_link(&mut self) {
        let Some(link) = self.links.pop() else {
            return;
        };
        let end = self.len();
        if link.start < end {
            self.styles.push(TextStyle { range: link.start..end, kind: TextStyleKind::Link });
        }
        let label = self.text.get(link.start..end).map(str::trim).unwrap_or_default();
        if !link.destination.is_empty() && label != link.destination {
            self.push_literal(" <");
            self.push_literal(&link.destination);
            self.push_literal(">");
        }
    }

    fn start_image(&mut self, destination: &str) {
        if self.image.is_none() {
            self.image =
                Some(ImageFrame { destination: sanitise_inline(destination), alt: String::new() });
        }
    }

    fn end_image(&mut self) {
        let Some(image) = self.image.take() else {
            return;
        };
        let alt = collapse_whitespace(&image.alt);
        if alt.is_empty() {
            self.push_literal("[Image]");
        } else {
            self.push_literal("[Image: ");
            self.push_literal(&alt);
            self.push_literal("]");
        }
        if !image.destination.is_empty() {
            self.push_literal(" <");
            self.push_literal(&image.destination);
            self.push_literal(">");
        }
    }

    fn finish(&mut self) {
        while self.text.ends_with(char::is_whitespace) {
            self.text.pop();
        }
        let len = self.text.len();
        for style in &mut self.styles {
            style.range.end = style.range.end.min(len);
        }
        self.styles.retain(|style| style.range.start < style.range.end);
        self.open_styles.clear();
        self.links.clear();
        self.image = None;
    }
}

struct TableBuilder {
    alignments: Vec<Alignment>,
    rows: Vec<TableRow>,
    current_row: Option<TableRow>,
    current_cell: Option<StyledText>,
    in_head: bool,
}

struct TableRow {
    cells: Vec<StyledText>,
    header: bool,
}

impl TableBuilder {
    fn new(alignments: Vec<Alignment>) -> Self {
        Self { alignments, rows: Vec::new(), current_row: None, current_cell: None, in_head: false }
    }

    fn handle(&mut self, event: Event<'_>) {
        match event {
            Event::Start(Tag::TableHead) => {
                self.in_head = true;
                self.current_row = Some(TableRow { cells: Vec::new(), header: true });
            }
            Event::End(TagEnd::TableHead) => {
                self.finish_row();
                self.in_head = false;
            }
            Event::Start(Tag::TableRow) => {
                self.current_row = Some(TableRow { cells: Vec::new(), header: self.in_head });
            }
            Event::End(TagEnd::TableRow) => self.finish_row(),
            Event::Start(Tag::TableCell) => self.current_cell = Some(StyledText::default()),
            Event::End(TagEnd::TableCell) => {
                if let Some(mut cell) = self.current_cell.take() {
                    cell.finish();
                    if let Some(row) = self.current_row.as_mut() {
                        row.cells.push(cell);
                    }
                }
            }
            Event::Text(text) => self.with_cell(|cell| cell.push_text(&text)),
            Event::Code(code) => {
                self.with_cell(|cell| cell.push_styled(&code, TextStyleKind::Code));
            }
            Event::SoftBreak => self.with_cell(StyledText::push_space),
            Event::HardBreak => self.with_cell(|cell| cell.ensure_newlines(1)),
            Event::Start(Tag::Emphasis) => {
                self.with_cell(|cell| cell.start_style(TextStyleKind::Emphasis));
            }
            Event::End(TagEnd::Emphasis) => {
                self.with_cell(|cell| cell.end_style(TextStyleKind::Emphasis));
            }
            Event::Start(Tag::Strong) => {
                self.with_cell(|cell| cell.start_style(TextStyleKind::Strong));
            }
            Event::End(TagEnd::Strong) => {
                self.with_cell(|cell| cell.end_style(TextStyleKind::Strong));
            }
            Event::Start(Tag::Strikethrough) => {
                self.with_cell(|cell| cell.start_style(TextStyleKind::Strikethrough));
            }
            Event::End(TagEnd::Strikethrough) => {
                self.with_cell(|cell| cell.end_style(TextStyleKind::Strikethrough));
            }
            Event::Start(Tag::Link { dest_url, .. }) => {
                self.with_cell(|cell| cell.start_link(dest_url.as_ref()));
            }
            Event::End(TagEnd::Link) => self.with_cell(StyledText::end_link),
            Event::Start(Tag::Image { dest_url, .. }) => {
                self.with_cell(|cell| cell.start_image(dest_url.as_ref()));
            }
            Event::End(TagEnd::Image) => self.with_cell(StyledText::end_image),
            Event::Html(html) | Event::InlineHtml(html) => {
                let visible = visible_html_text(&html);
                self.with_cell(|cell| cell.push_text(&visible));
            }
            Event::TaskListMarker(checked) => {
                self.with_cell(|cell| cell.push_literal(if checked { "[x] " } else { "[ ] " }));
            }
            _ => {}
        }
    }

    fn with_cell(&mut self, update: impl FnOnce(&mut StyledText)) {
        if let Some(cell) = self.current_cell.as_mut() {
            update(cell);
        }
    }

    fn finish_row(&mut self) {
        if let Some(row) = self.current_row.take() {
            self.rows.push(row);
        }
    }

    fn render_into(mut self, sink: &mut StyledText) {
        self.finish_row();
        let columns = self
            .alignments
            .len()
            .max(self.rows.iter().map(|row| row.cells.len()).max().unwrap_or(0));
        if columns == 0 {
            return;
        }
        for row in &mut self.rows {
            row.cells.resize_with(columns, StyledText::default);
        }
        let widths = table_column_widths(&self.rows, columns);
        push_table_border(sink, '┌', '┬', '┐', '─', &widths);
        for (index, row) in self.rows.iter().enumerate() {
            push_table_row(sink, row, &widths, &self.alignments);
            if index + 1 < self.rows.len() {
                if row.header {
                    push_table_border(sink, '╞', '╪', '╡', '═', &widths);
                } else {
                    push_table_border(sink, '├', '┼', '┤', '─', &widths);
                }
            }
        }
        push_table_border(sink, '└', '┴', '┘', '─', &widths);
    }
}

fn table_column_widths(rows: &[TableRow], columns: usize) -> Vec<usize> {
    let mut widths = vec![1_usize; columns];
    let mut minimums = vec![1_usize; columns];
    for row in rows {
        for (column, cell) in row.cells.iter().enumerate() {
            let natural =
                cell.text.split('\n').map(UnicodeWidthStr::width).max().unwrap_or(1).max(1);
            widths[column] = widths[column].max(natural);
            minimums[column] = minimums[column].max(
                cell.text.graphemes(true).map(UnicodeWidthStr::width).max().unwrap_or(1).max(1),
            );
        }
    }

    let overhead = columns.saturating_mul(3).saturating_add(1);
    let available = MAX_TABLE_WIDTH.saturating_sub(overhead).max(minimums.iter().sum());
    while widths.iter().sum::<usize>() > available {
        let Some((index, _)) = widths
            .iter()
            .enumerate()
            .filter(|(index, width)| **width > minimums[*index])
            .max_by_key(|(_, width)| **width)
        else {
            break;
        };
        widths[index] -= 1;
    }
    widths
}

fn push_table_border(
    sink: &mut StyledText,
    left: char,
    middle: char,
    right: char,
    fill: char,
    widths: &[usize],
) {
    sink.push_char(left);
    for (index, width) in widths.iter().enumerate() {
        sink.push_literal(&fill.to_string().repeat(width.saturating_add(2)));
        sink.push_char(if index + 1 == widths.len() { right } else { middle });
    }
    sink.ensure_newlines(1);
}

fn push_table_row(
    sink: &mut StyledText,
    row: &TableRow,
    widths: &[usize],
    alignments: &[Alignment],
) {
    let wrapped = row
        .cells
        .iter()
        .zip(widths)
        .map(|(cell, width)| wrap_ranges(&cell.text, *width))
        .collect::<Vec<_>>();
    let height = wrapped.iter().map(Vec::len).max().unwrap_or(1);
    for line_index in 0..height {
        sink.push_literal("│ ");
        for (column, width) in widths.iter().enumerate() {
            let cell = &row.cells[column];
            let range = wrapped[column].get(line_index).cloned().unwrap_or(0..0);
            let value = cell.text.get(range.clone()).unwrap_or_default();
            let used = UnicodeWidthStr::width(value);
            let (left, right) = alignment_padding(
                alignments.get(column).copied().unwrap_or(Alignment::None),
                width.saturating_sub(used),
            );
            sink.push_literal(&" ".repeat(left));
            let target_start = sink.len();
            sink.push_literal(value);
            if row.header && !value.is_empty() {
                sink.styles.push(TextStyle {
                    range: target_start..target_start + value.len(),
                    kind: TextStyleKind::Strong,
                });
            }
            for style in &cell.styles {
                if let Some(intersection) = range_intersection(&range, &style.range) {
                    sink.styles.push(TextStyle {
                        range: target_start + intersection.start - range.start
                            ..target_start + intersection.end - range.start,
                        kind: style.kind,
                    });
                }
            }
            sink.push_literal(&" ".repeat(right));
            sink.push_literal(if column + 1 == widths.len() { " │" } else { " │ " });
        }
        sink.ensure_newlines(1);
    }
}

fn wrap_ranges(text: &str, width: usize) -> Vec<Range<usize>> {
    let width = width.max(1);
    if text.is_empty() {
        return std::iter::once(0..0).collect();
    }
    let mut ranges = Vec::new();
    let mut hard_start = 0_usize;
    for hard_line in text.split_inclusive('\n') {
        let content = hard_line.strip_suffix('\n').unwrap_or(hard_line);
        if content.is_empty() {
            ranges.push(hard_start..hard_start);
        } else {
            let mut start = hard_start;
            let mut used = 0_usize;
            for (relative, grapheme) in content.grapheme_indices(true) {
                let grapheme_width = UnicodeWidthStr::width(grapheme);
                if used > 0 && used.saturating_add(grapheme_width) > width {
                    let split = hard_start + relative;
                    ranges.push(start..split);
                    start = split;
                    used = 0;
                }
                used = used.saturating_add(grapheme_width);
            }
            ranges.push(start..hard_start + content.len());
        }
        hard_start += hard_line.len();
    }
    ranges
}

fn alignment_padding(alignment: Alignment, available: usize) -> (usize, usize) {
    match alignment {
        Alignment::Right => (available, 0),
        Alignment::Center => (available / 2, available - available / 2),
        Alignment::None | Alignment::Left => (0, available),
    }
}

fn range_intersection(left: &Range<usize>, right: &Range<usize>) -> Option<Range<usize>> {
    let start = left.start.max(right.start);
    let end = left.end.min(right.end);
    (start < end).then_some(start..end)
}

#[derive(Default)]
struct FrontMatter {
    title: Option<String>,
    author: Option<String>,
    language: Option<String>,
}

impl FrontMatter {
    fn merge(&mut self, other: Self) {
        self.title = self.title.take().or(other.title);
        self.author = self.author.take().or(other.author);
        self.language = self.language.take().or(other.language);
    }
}

fn parse_front_matter(raw: &str) -> FrontMatter {
    let mut metadata = FrontMatter::default();
    for line in raw.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = unquote_scalar(value.trim());
        if value.is_empty() {
            continue;
        }
        match key.trim().to_ascii_lowercase().as_str() {
            "title" => {
                metadata.title.get_or_insert(value);
            }
            "author" => {
                metadata.author.get_or_insert(value);
            }
            "language" | "lang" => {
                metadata.language.get_or_insert(value);
            }
            _ => {}
        }
    }
    metadata
}

fn unquote_scalar(value: &str) -> String {
    let quoted = (value.starts_with('"') && value.ends_with('"'))
        || (value.starts_with('\'') && value.ends_with('\''));
    if quoted && value.len() >= 2 {
        value[1..value.len() - 1].trim().to_owned()
    } else {
        value.to_owned()
    }
}

fn visible_html_text(source: &str) -> String {
    let document = Html::parse_fragment(source);
    let mut result = String::new();
    walk_html(document.tree.root(), &mut result);
    result
        .lines()
        .map(collapse_whitespace)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn walk_html(node: NodeRef<'_, Node>, output: &mut String) {
    match node.value() {
        Node::Text(text) => output.push_str(text),
        Node::Element(element) => {
            let name = element.name();
            if matches!(name, "head" | "script" | "style" | "noscript" | "svg") {
                return;
            }
            if name == "br" {
                output.push('\n');
                return;
            }
            let block = matches!(
                name,
                "address"
                    | "article"
                    | "aside"
                    | "blockquote"
                    | "div"
                    | "footer"
                    | "header"
                    | "li"
                    | "main"
                    | "p"
                    | "section"
            );
            if block && !output.ends_with(char::is_whitespace) {
                output.push('\n');
            }
            for child in node.children() {
                walk_html(child, output);
            }
            if block && !output.ends_with(char::is_whitespace) {
                output.push('\n');
            }
        }
        _ => {
            for child in node.children() {
                walk_html(child, output);
            }
        }
    }
}

fn heading_level(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

fn quote_prefix(depth: usize) -> String { "│ ".repeat(depth) }

fn collapse_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn sanitise_inline(value: &str) -> String {
    value.chars().filter(|character| !character.is_control()).collect::<String>().trim().to_owned()
}

fn normalise_newlines(value: &str) -> String {
    if value.contains('\r') {
        value.replace("\r\n", "\n").replace('\r', "\n")
    } else {
        value.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(source: &str) -> RenderedMarkdown {
        let mut options = Options::empty();
        options.insert(
            Options::ENABLE_TABLES
                | Options::ENABLE_STRIKETHROUGH
                | Options::ENABLE_TASKLISTS
                | Options::ENABLE_YAML_STYLE_METADATA_BLOCKS,
        );
        let mut renderer = MarkdownRenderer::default();
        for event in Parser::new_ext(source, options) {
            renderer.handle(event);
        }
        renderer.finish("fallback".to_owned())
    }

    #[test]
    fn headings_create_toc_and_inline_styles() {
        let rendered = render("# 文档标题\n\n这是 **粗体**、*斜体* 和 `code`。\n");
        assert_eq!(rendered.metadata.title(), Some("文档标题"));
        assert_eq!(rendered.toc.len(), 1);
        assert_eq!(rendered.toc[0].label(), "文档标题");
        assert!(rendered.styles.iter().any(|style| style.kind() == TextStyleKind::Strong));
        assert!(rendered.styles.iter().any(|style| style.kind() == TextStyleKind::Emphasis));
        assert!(rendered.styles.iter().any(|style| style.kind() == TextStyleKind::Code));
    }

    #[test]
    fn front_matter_is_hidden_and_supplies_metadata() {
        let rendered = render("---\ntitle: '设计说明'\nauthor: 测试者\n---\n\n正文\n");
        assert_eq!(rendered.metadata.title(), Some("设计说明"));
        assert_eq!(rendered.metadata.author(), Some("测试者"));
        assert!(!rendered.text.contains("author:"));
        assert_eq!(rendered.text, "正文");
    }

    #[test]
    fn renders_links_images_lists_and_quotes_as_readable_text() {
        let rendered = render(
            "> 引用\n\n- [x] 完成\n- [ ] 阅读 [文档](https://example.com)\n\n![架构图](assets/a.png)\n",
        );
        assert!(rendered.text.contains("│ 引用"));
        assert!(rendered.text.contains("- [x] 完成"), "{}", rendered.text);
        assert!(rendered.text.contains("文档 <https://example.com>"));
        assert!(rendered.text.contains("[Image: 架构图] <assets/a.png>"));
    }

    #[test]
    fn preserves_ordered_and_nested_list_structure() {
        let rendered = render("3. 父项\n   - 子项\n4. 下一项\n");
        assert!(rendered.text.contains("3. 父项\n  - 子项"), "{}", rendered.text);
        assert!(rendered.text.contains("4. 下一项"), "{}", rendered.text);
    }

    #[test]
    fn renders_cjk_table_with_borders_and_bounded_width() {
        let long = "中文说明".repeat(40);
        let source = format!("| 名称 | 说明 |\n| :--- | ---: |\n| Peep | {long} |\n");
        let rendered = render(&source);
        assert!(rendered.text.contains('┌'));
        assert!(rendered.text.contains("│ 名称"));
        assert!(rendered.text.lines().all(|line| UnicodeWidthStr::width(line) <= 100));
        assert!(rendered.text.lines().count() > 5);
    }

    #[test]
    fn strips_terminal_control_characters() {
        let rendered = render("普通\u{1b}[31m文字\u{7}\n");
        assert!(!rendered.text.contains('\u{1b}'));
        assert!(!rendered.text.contains('\u{7}'));
    }

    #[test]
    fn preserves_hard_breaks_and_code_block_style_at_document_end() {
        let rendered = render("第一行  \n第二行\n\n```text\ncode\n```\n");
        assert!(rendered.text.contains("第一行\n第二行"));
        let code = rendered.text.find("code").unwrap();
        assert!(
            rendered.styles.iter().any(|style| {
                style.kind() == TextStyleKind::Code && style.range().contains(&code)
            })
        );
    }

    #[test]
    fn extracts_visible_html_without_scripts() {
        let rendered =
            render("<div><p>第一段</p><script>danger()</script><p>第二段<br>下一行</p></div>\n");
        assert!(rendered.text.contains("第一段"));
        assert!(rendered.text.contains("第二段"));
        assert!(rendered.text.contains("下一行"));
        assert!(!rendered.text.contains("danger"));
        assert!(!rendered.text.contains("<div>"));
    }
}
