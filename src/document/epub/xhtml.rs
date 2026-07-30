use std::collections::HashMap;

use ego_tree::NodeRef;
use scraper::Html;
use scraper::node::Node;

use super::{TextStyle, TextStyleKind, restricted_xml_options};

#[derive(Debug)]
pub(super) struct RenderedContent {
    pub(super) text: String,
    pub(super) styles: Vec<TextStyle>,
    pub(super) headings: Vec<RenderedHeading>,
    pub(super) anchors: HashMap<String, usize>,
}

#[derive(Clone, Debug)]
pub(super) struct RenderedHeading {
    pub(super) label: String,
    pub(super) offset: usize,
    pub(super) depth: u8,
}

#[derive(Default)]
struct RenderSink {
    text: String,
    styles: Vec<TextStyle>,
    headings: Vec<RenderedHeading>,
    anchors: HashMap<String, usize>,
    pending_space: bool,
    pre_depth: usize,
}

struct ElementFrame {
    style_start: usize,
    heading_level: Option<u8>,
    paragraph_block: bool,
    table_row: bool,
    preformatted: bool,
    table_cell: bool,
}

impl RenderSink {
    fn enter_element<'a>(
        &mut self,
        name: &str,
        anchors: impl IntoIterator<Item = &'a str>,
    ) -> ElementFrame {
        let heading_level = heading_level(name);
        let paragraph_block =
            matches!(name, "p" | "blockquote" | "pre" | "li") || heading_level.is_some();
        let table_row = name == "tr";
        if paragraph_block {
            self.ensure_newlines(2);
        } else if table_row {
            self.ensure_newlines(1);
        }
        if name == "li" {
            self.push_literal("- ");
        }

        for anchor in anchors {
            if !anchor.is_empty() {
                self.anchors.entry(anchor.to_owned()).or_insert(self.text.len());
            }
        }

        let style_start = self.text.len();
        let preformatted = name == "pre";
        if preformatted {
            self.pre_depth += 1;
        }
        ElementFrame {
            style_start,
            heading_level,
            paragraph_block,
            table_row,
            preformatted,
            table_cell: matches!(name, "td" | "th"),
        }
    }

    fn exit_element(&mut self, frame: ElementFrame, name: &str) {
        if frame.preformatted {
            self.pre_depth = self.pre_depth.saturating_sub(1);
        }
        let style_end = self.text.len();
        let style_kind = match name {
            "em" | "i" => Some(TextStyleKind::Emphasis),
            "strong" | "b" => Some(TextStyleKind::Strong),
            _ => frame.heading_level.map(TextStyleKind::Heading),
        };
        if let Some(kind) = style_kind
            && frame.style_start < style_end
        {
            self.styles.push(TextStyle { range: frame.style_start..style_end, kind });
        }
        if let Some(depth) = frame.heading_level {
            let rendered = &self.text[frame.style_start..style_end];
            let label = rendered.trim();
            if !label.is_empty() {
                let leading = rendered.len() - rendered.trim_start().len();
                self.headings.push(RenderedHeading {
                    label: label.to_owned(),
                    offset: frame.style_start + leading,
                    depth,
                });
            }
        }

        if frame.table_cell {
            self.push_literal("\t");
        }
        if frame.paragraph_block {
            self.ensure_newlines(2);
        } else if frame.table_row {
            self.ensure_newlines(1);
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

    fn finish(mut self) -> RenderedContent {
        while self.text.ends_with(char::is_whitespace) {
            self.text.pop();
        }
        let len = self.text.len();
        for style in &mut self.styles {
            style.range.end = style.range.end.min(len);
        }
        self.styles.retain(|style| style.range.start < style.range.end);
        self.headings.retain(|heading| heading.offset < len);
        self.anchors.retain(|_, offset| *offset <= len);
        RenderedContent {
            text: self.text,
            styles: self.styles,
            headings: self.headings,
            anchors: self.anchors,
        }
    }
}

pub(super) fn render(source: &str) -> RenderedContent {
    if let Ok(document) = roxmltree::Document::parse_with_options(source, restricted_xml_options())
    {
        let mut sink = RenderSink::default();
        walk_xml(document.root(), &mut sink);
        return sink.finish();
    }

    let document = Html::parse_document(source);
    let mut sink = RenderSink::default();
    walk_html(document.tree.root(), &mut sink);
    sink.finish()
}

fn walk_xml(node: roxmltree::Node<'_, '_>, sink: &mut RenderSink) {
    if node.is_text() {
        if let Some(text) = node.text() {
            sink.push_text(text);
        }
        return;
    }
    if !node.is_element() {
        for child in node.children() {
            walk_xml(child, sink);
        }
        return;
    }

    let name = node.tag_name().name().to_ascii_lowercase();
    if skipped_element(&name) {
        return;
    }
    if name == "br" {
        sink.ensure_newlines(1);
        return;
    }
    if name == "img" {
        sink.push_literal("[Image]");
        return;
    }
    let anchors = [node.attribute("id"), (name == "a").then(|| node.attribute("name")).flatten()]
        .into_iter()
        .flatten();
    let frame = sink.enter_element(&name, anchors);
    for child in node.children() {
        walk_xml(child, sink);
    }
    sink.exit_element(frame, &name);
}

fn walk_html(node: NodeRef<'_, Node>, sink: &mut RenderSink) {
    match node.value() {
        Node::Text(text) => sink.push_text(text),
        Node::Element(element) => {
            let name = element.name();
            if skipped_element(name) {
                return;
            }
            if name == "br" {
                sink.ensure_newlines(1);
                return;
            }
            if name == "img" {
                sink.push_literal("[Image]");
                return;
            }
            let anchors =
                [element.attr("id"), (name == "a").then(|| element.attr("name")).flatten()]
                    .into_iter()
                    .flatten();
            let frame = sink.enter_element(name, anchors);
            for child in node.children() {
                walk_html(child, sink);
            }
            sink.exit_element(frame, name);
        }
        _ => {
            for child in node.children() {
                walk_html(child, sink);
            }
        }
    }
}

fn skipped_element(name: &str) -> bool {
    matches!(name, "head" | "script" | "style" | "noscript" | "svg" | "nav")
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
