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
    // Volume units collide with ordinary prose such as “第二部长篇小说”, so
    // they require an explicit separator before any title.
    line[marker_end..].chars().next().is_none_or(is_heading_boundary)
}

fn chapter_marker_is_valid(line: &str, marker: Marker) -> bool {
    // Chapter titles commonly omit a separator (“第一章风起”), so keep this
    // deliberately looser than volume markers and only reject the known
    // lexical collision “回合”.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn detected(text: &str) -> Vec<(String, u8, usize)> {
        detect_chapter_headings(text)
            .into_iter()
            .map(|entry| (entry.label().to_owned(), entry.depth(), entry.offset()))
            .collect()
    }

    #[test]
    fn recognises_supported_line_start_forms() {
        let cases: &[(&str, &[(&str, u8)])] = &[
            ("第一章 标题", &[("第一章 标题", 0)]),
            ("第一章没有空格", &[("第一章没有空格", 0)]),
            ("第壹佰貳拾叁章 标题", &[("第壹佰貳拾叁章 标题", 0)]),
            ("CHAPTER 12 Arrival", &[("CHAPTER 12", 0)]),
            ("番外 春日", &[("番外", 0)]),
        ];

        for (text, expected) in cases {
            let actual = detected(text)
                .into_iter()
                .map(|(label, depth, _)| (label, depth))
                .collect::<Vec<_>>();
            let expected = expected
                .iter()
                .map(|(label, depth)| ((*label).to_owned(), *depth))
                .collect::<Vec<_>>();
            assert_eq!(actual, expected, "input: {text:?}");
        }
    }

    #[test]
    fn rejects_prose_collisions_and_embedded_markers() {
        for text in [
            "第二回合开始后，众人沉默了。",
            "第二部长篇小说并不是卷名。",
            "他说到第二回往事时停了下来。",
            "这里没有章节标题。",
        ] {
            assert!(detected(text).is_empty(), "input: {text:?}");
        }
    }

    #[test]
    fn preserves_indentation_offsets_and_volume_hierarchy() {
        let text = concat!(
            "　第一卷　最后一战　第一章　心事一灯知\n",
            "正文。\n",
            "第一卷　最后一战　第二章　痛作无家别\n",
        );

        assert_eq!(
            detected(text),
            vec![
                ("第一卷　最后一战".to_owned(), 0, '　'.len_utf8()),
                ("第一章　心事一灯知".to_owned(), 1, '　'.len_utf8()),
                (
                    "第二章　痛作无家别".to_owned(),
                    1,
                    "　第一卷　最后一战　第一章　心事一灯知\n正文。\n".len(),
                ),
            ]
        );
    }

    #[test]
    fn keeps_duplicate_chapter_numbers_with_distinct_titles() {
        let entries = detected("第1357章 昨日\n正文。\n第1357章 明日");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].0, "第1357章 昨日");
        assert_eq!(entries[1].0, "第1357章 明日");
        assert!(entries[0].2 < entries[1].2);
    }
}
