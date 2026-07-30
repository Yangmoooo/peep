use std::collections::HashSet;

use super::TocEntry;

pub(crate) fn detect_chapter_headings(text: &str) -> Vec<TocEntry> {
    let mut entries = Vec::new();
    let mut seen_volumes = HashSet::<String>::new();
    let mut inside_volume = false;
    let mut byte_offset = 0;

    for line in text.split('\n') {
        let trimmed = line.trim_start();
        let line_offset = byte_offset + line.len().saturating_sub(trimmed.len());
        let heading = trimmed.trim_end();

        if let Some(marker) = marker_at_start(heading) {
            match marker.kind {
                MarkerKind::Volume if volume_has_boundary(heading, marker.end) => {
                    let chapter = find_compound_chapter(heading, marker.end);
                    let volume_end = chapter.map_or(heading.len(), |chapter| chapter.start);
                    let volume_label = heading[..volume_end].trim_end();
                    if seen_volumes.insert(volume_label.to_owned()) {
                        entries.push(TocEntry {
                            label: volume_label.to_owned(),
                            offset: line_offset,
                            depth: 0,
                        });
                    }
                    inside_volume = true;
                    if let Some(chapter) = chapter {
                        entries.push(TocEntry {
                            label: heading[chapter.start..].trim_end().to_owned(),
                            offset: line_offset,
                            depth: 1,
                        });
                    }
                }
                MarkerKind::Chapter if chapter_marker_is_valid(heading, marker) => {
                    entries.push(TocEntry {
                        label: heading.to_owned(),
                        offset: line_offset,
                        depth: u8::from(inside_volume),
                    });
                }
                _ => {}
            }
        } else if let Some(label) = match_standalone_heading(heading) {
            entries.push(TocEntry { label: label.to_owned(), offset: line_offset, depth: 0 });
        }

        byte_offset += line.len() + 1;
    }
    entries
}

#[derive(Clone, Copy)]
struct Marker {
    start: usize,
    end: usize,
    unit: char,
    kind: MarkerKind,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum MarkerKind {
    Volume,
    Chapter,
}

fn marker_at_start(line: &str) -> Option<Marker> { parse_marker(line, 0) }

fn parse_marker(line: &str, start: usize) -> Option<Marker> {
    let rest = line.get(start..)?.strip_prefix('第')?;
    let number_end = number_end(rest)?;
    let after_number = &rest[number_end..];
    let unit = after_number.chars().next()?;
    let kind = match unit {
        '卷' | '部' | '集' => MarkerKind::Volume,
        '章' | '回' => MarkerKind::Chapter,
        _ => return None,
    };
    let end = start + '第'.len_utf8() + number_end + unit.len_utf8();
    Some(Marker { start, end, unit, kind })
}

fn volume_has_boundary(line: &str, marker_end: usize) -> bool {
    line[marker_end..].chars().next().is_none_or(is_heading_boundary)
}

fn chapter_marker_is_valid(line: &str, marker: Marker) -> bool {
    marker.unit != '回' || !line[marker.end..].starts_with('合')
}

fn find_compound_chapter(line: &str, after_volume: usize) -> Option<Marker> {
    line[after_volume..].char_indices().find_map(|(relative, character)| {
        if character != '第' {
            return None;
        }
        let start = after_volume + relative;
        let separated = line[..start].chars().next_back().is_some_and(is_heading_boundary);
        if !separated {
            return None;
        }
        let marker = parse_marker(line, start)?;
        (marker.kind == MarkerKind::Chapter && chapter_marker_is_valid(line, marker))
            .then_some(marker)
    })
}

fn is_heading_boundary(character: char) -> bool {
    character.is_whitespace()
        || matches!(character, ':' | '：' | '-' | '—' | '–' | '·' | '、' | '.' | '．')
}

fn match_standalone_heading(line: &str) -> Option<&str> {
    for special in ["序章", "终章", "尾声", "楔子", "引子", "番外", "代序", "跋", "尾聲"]
    {
        if line.starts_with(special) {
            return Some(special);
        }
    }
    if let Some(rest) = line.strip_prefix("Chapter ").or_else(|| line.strip_prefix("CHAPTER ")) {
        let digits_end = rest.chars().take_while(char::is_ascii_digit).count();
        if digits_end > 0 {
            return Some(&line[.."Chapter ".len() + digits_end]);
        }
    }
    None
}

fn number_end(value: &str) -> Option<usize> {
    let mut characters = value.char_indices();
    let (_, first) = characters.next()?;
    if !is_number_character(first) {
        return None;
    }
    let mut end = first.len_utf8();
    for (offset, character) in characters {
        if !is_number_character(character) {
            break;
        }
        end = offset + character.len_utf8();
    }
    Some(end)
}

fn is_number_character(character: char) -> bool {
    character.is_ascii_digit()
        || matches!(
            character,
            '零' | '〇'
                | '一'
                | '二'
                | '三'
                | '四'
                | '五'
                | '六'
                | '七'
                | '八'
                | '九'
                | '十'
                | '百'
                | '千'
                | '万'
                | '萬'
                | '壹'
                | '贰'
                | '貳'
                | '叁'
                | '參'
                | '肆'
                | '伍'
                | '陆'
                | '陸'
                | '柒'
                | '捌'
                | '玖'
                | '拾'
                | '佰'
                | '仟'
        )
}
