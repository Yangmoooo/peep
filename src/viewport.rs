use std::collections::HashMap;
use std::ops::Range;

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VisualLine {
    range: Range<usize>,
    char_start: usize,
}

impl VisualLine {
    pub fn range(&self) -> Range<usize> { self.range.clone() }

    pub fn char_start(&self) -> usize { self.char_start }
}

#[derive(Debug)]
pub struct Viewport {
    line_starts: Vec<usize>,
    line_char_starts: Vec<usize>,
    total_chars: usize,
    anchor: usize,
    width: usize,
    height: usize,
    wrapped: HashMap<usize, Vec<VisualLine>>,
    max_anchor: Option<usize>,
}

impl Viewport {
    pub fn new(text: &str, anchor: usize) -> Self {
        let (line_starts, line_char_starts, total_chars) = index_hard_lines(text);
        let anchor = floor_char_boundary(text, anchor.min(text.len()));
        Self {
            line_starts,
            line_char_starts,
            total_chars,
            anchor,
            width: 1,
            height: 1,
            wrapped: HashMap::new(),
            max_anchor: None,
        }
    }

    pub fn anchor(&self) -> usize { self.anchor }

    pub fn total_chars(&self) -> usize { self.total_chars }

    pub fn set_width(&mut self, width: usize) {
        let width = width.max(1);
        if self.width != width {
            self.width = width;
            self.wrapped.clear();
            self.max_anchor = None;
        }
    }

    fn set_height(&mut self, height: usize) {
        let height = height.max(1);
        if self.height != height {
            self.height = height;
            self.max_anchor = None;
        }
    }

    pub fn goto_byte(&mut self, text: &str, offset: usize) {
        let offset = floor_char_boundary(text, offset.min(text.len()));
        let (hard_line, segment) = self.locate(text, offset);
        self.anchor = self.wrap_hard_line(text, hard_line)[segment].range.start;
        self.clamp_anchor(text);
    }

    pub fn goto_percent(&mut self, text: &str, percent: f64) {
        if self.total_chars == 0 {
            self.anchor = 0;
            return;
        }
        let percent = percent.clamp(0.0, 100.0);
        let target = ((self.total_chars as f64) * percent / 100.0).round() as usize;
        let byte = self.byte_at_char(text, target.min(self.total_chars));
        self.goto_byte(text, byte);
    }

    pub fn goto_start(&mut self) { self.anchor = 0; }

    pub fn goto_end(&mut self, text: &str) { self.anchor = self.max_anchor(text); }

    pub fn scroll_by(&mut self, text: &str, delta: isize) {
        if delta == 0 {
            return;
        }
        let (mut hard_line, mut segment) = self.locate(text, self.anchor);
        if delta > 0 {
            for _ in 0..delta as usize {
                let segment_count = self.wrap_hard_line(text, hard_line).len();
                if segment + 1 < segment_count {
                    segment += 1;
                } else if hard_line + 1 < self.line_starts.len() {
                    hard_line += 1;
                    segment = 0;
                } else {
                    break;
                }
            }
        } else {
            for _ in 0..delta.unsigned_abs() {
                if segment > 0 {
                    segment -= 1;
                } else if hard_line > 0 {
                    hard_line -= 1;
                    segment = self.wrap_hard_line(text, hard_line).len().saturating_sub(1);
                } else {
                    break;
                }
            }
        }
        self.anchor = self.wrap_hard_line(text, hard_line)[segment].range.start;
        self.clamp_anchor(text);
    }

    pub fn visible_lines(&mut self, text: &str, height: usize) -> Vec<VisualLine> {
        if height == 0 {
            return Vec::new();
        }
        self.set_height(height);
        self.clamp_anchor(text);
        let (mut hard_line, mut segment) = self.locate(text, self.anchor);
        let mut lines = Vec::with_capacity(height);
        while lines.len() < height && hard_line < self.line_starts.len() {
            let wrapped = self.wrap_hard_line(text, hard_line);
            while segment < wrapped.len() && lines.len() < height {
                lines.push(wrapped[segment].clone());
                segment += 1;
            }
            hard_line += 1;
            segment = 0;
        }
        lines
    }

    pub fn progress_chars(&self, text: &str) -> usize { self.char_at_byte(text, self.anchor) }

    pub fn progress_percent(&self, text: &str) -> f64 {
        if self.total_chars == 0 {
            return 0.0;
        }
        self.progress_chars(text) as f64 * 100.0 / self.total_chars as f64
    }

    fn locate(&mut self, text: &str, offset: usize) -> (usize, usize) {
        let hard_line =
            self.line_starts.partition_point(|start| *start <= offset).saturating_sub(1);
        let lines = self.wrap_hard_line(text, hard_line);
        let segment = lines
            .iter()
            .position(|line| offset >= line.range.start && offset < line.range.end)
            .unwrap_or_else(|| {
                lines.partition_point(|line| line.range.start <= offset).saturating_sub(1)
            });
        (hard_line, segment.min(lines.len().saturating_sub(1)))
    }

    fn wrap_hard_line(&mut self, text: &str, index: usize) -> &Vec<VisualLine> {
        self.wrapped.entry(index).or_insert_with(|| {
            let start = self.line_starts[index];
            let end =
                self.line_starts.get(index + 1).map_or(text.len(), |next| next.saturating_sub(1));
            let slice = &text[start..end];
            if slice.is_empty() {
                return vec![VisualLine {
                    range: start..start,
                    char_start: self.line_char_starts[index],
                }];
            }

            let mut lines = Vec::new();
            let mut segment_start = start;
            let mut segment_char_start = self.line_char_starts[index];
            let mut segment_chars = 0_usize;
            let mut columns = 0_usize;
            for (relative, grapheme) in slice.grapheme_indices(true) {
                let width = UnicodeWidthStr::width(grapheme);
                if columns > 0 && columns.saturating_add(width) > self.width {
                    let split = start + relative;
                    lines.push(VisualLine {
                        range: segment_start..split,
                        char_start: segment_char_start,
                    });
                    segment_start = split;
                    segment_char_start += segment_chars;
                    segment_chars = 0;
                    columns = 0;
                }
                columns = columns.saturating_add(width);
                segment_chars += grapheme.chars().count();
            }
            lines.push(VisualLine { range: segment_start..end, char_start: segment_char_start });
            lines
        })
    }

    fn byte_at_char(&self, text: &str, char_offset: usize) -> usize {
        if char_offset >= self.total_chars {
            return text.len();
        }
        let hard_line =
            self.line_char_starts.partition_point(|start| *start <= char_offset).saturating_sub(1);
        let local = char_offset.saturating_sub(self.line_char_starts[hard_line]);
        let start = self.line_starts[hard_line];
        let end =
            self.line_starts.get(hard_line + 1).map_or(text.len(), |next| next.saturating_sub(1));
        text[start..end].char_indices().nth(local).map_or(end, |(offset, _)| start + offset)
    }

    fn char_at_byte(&self, text: &str, byte_offset: usize) -> usize {
        let byte_offset = floor_char_boundary(text, byte_offset.min(text.len()));
        let hard_line =
            self.line_starts.partition_point(|start| *start <= byte_offset).saturating_sub(1);
        let start = self.line_starts[hard_line];
        self.line_char_starts[hard_line] + text[start..byte_offset].chars().count()
    }

    fn clamp_anchor(&mut self, text: &str) { self.anchor = self.anchor.min(self.max_anchor(text)); }

    fn max_anchor(&mut self, text: &str) -> usize {
        if let Some(anchor) = self.max_anchor {
            return anchor;
        }
        let mut remaining = self.height;
        let mut anchor = 0;
        for index in (0..self.line_starts.len()).rev() {
            let line_count = self.wrap_hard_line(text, index).len();
            if remaining <= line_count {
                let segment = line_count - remaining;
                anchor = self.wrap_hard_line(text, index)[segment].range.start;
                break;
            }
            remaining -= line_count;
        }
        self.max_anchor = Some(anchor);
        anchor
    }
}

fn index_hard_lines(text: &str) -> (Vec<usize>, Vec<usize>, usize) {
    let mut line_starts = vec![0];
    let mut line_char_starts = vec![0];
    let mut chars = 0_usize;
    for (byte, character) in text.char_indices() {
        chars += 1;
        if character == '\n' {
            line_starts.push(byte + 1);
            line_char_starts.push(chars);
        }
    }
    (line_starts, line_char_starts, chars)
}

fn floor_char_boundary(text: &str, mut offset: usize) -> usize {
    while offset > 0 && !text.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_cjk_by_terminal_width() {
        let text = "中文测试";
        let mut viewport = Viewport::new(text, 0);
        viewport.set_width(4);
        let lines = viewport.visible_lines(text, 4);
        assert_eq!(&text[lines[0].range()], "中文");
        assert_eq!(&text[lines[1].range()], "测试");
    }

    #[test]
    fn never_splits_combining_grapheme() {
        let text = "a\u{301}b";
        let mut viewport = Viewport::new(text, 0);
        viewport.set_width(1);
        let lines = viewport.visible_lines(text, 4);
        assert_eq!(&text[lines[0].range()], "a\u{301}");
        assert_eq!(&text[lines[1].range()], "b");
    }

    #[test]
    fn scrolls_across_empty_hard_lines() {
        let text = "one\n\nthree";
        let mut viewport = Viewport::new(text, 0);
        viewport.set_width(20);
        viewport.scroll_by(text, 1);
        assert_eq!(viewport.anchor(), 4);
        viewport.scroll_by(text, 1);
        assert_eq!(viewport.anchor(), 5);
        viewport.scroll_by(text, -2);
        assert_eq!(viewport.anchor(), 0);
    }

    #[test]
    fn percent_is_independent_of_wrap_width() {
        let text = "abcdefghij";
        let mut viewport = Viewport::new(text, 0);
        viewport.set_width(2);
        viewport.goto_percent(text, 50.0);
        let before = viewport.progress_chars(text);
        viewport.set_width(8);
        let after = viewport.progress_chars(text);
        assert_eq!(before, after);
    }

    #[test]
    fn end_stops_at_the_start_of_the_last_page() {
        let text = "one\ntwo\nthree\nfour\nfive\nsix";
        let mut viewport = Viewport::new(text, 0);
        viewport.set_width(20);
        viewport.visible_lines(text, 3);
        viewport.goto_end(text);
        let lines = viewport.visible_lines(text, 3);
        assert_eq!(lines.len(), 3);
        assert_eq!(&text[lines[0].range()], "four");
        assert_eq!(&text[lines[2].range()], "six");

        viewport.scroll_by(text, 100);
        let lines = viewport.visible_lines(text, 3);
        assert_eq!(&text[lines[0].range()], "four");
        assert_eq!(&text[lines[2].range()], "six");
    }

    #[test]
    fn short_document_does_not_move_when_goto_end_is_pressed() {
        let text = "one\ntwo";
        let mut viewport = Viewport::new(text, 0);
        viewport.set_width(20);
        viewport.visible_lines(text, 4);
        viewport.goto_end(text);
        assert_eq!(viewport.anchor(), 0);
    }

    #[test]
    fn resizing_reclamps_an_old_end_anchor() {
        let text = "one\ntwo\nthree\nfour\nfive\nsix";
        let mut viewport = Viewport::new(text, 0);
        viewport.set_width(20);
        viewport.visible_lines(text, 3);
        viewport.goto_end(text);
        assert_eq!(&text[viewport.visible_lines(text, 3)[0].range()], "four");

        let lines = viewport.visible_lines(text, 5);
        assert_eq!(&text[lines[0].range()], "two");
        assert_eq!(&text[lines[4].range()], "six");
    }
}
