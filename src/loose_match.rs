use std::collections::VecDeque;
use std::ops::{ControlFlow, Range};

pub(crate) struct LooseMatcher {
    characters: Vec<char>,
    prefix: Vec<usize>,
}

impl LooseMatcher {
    pub(crate) fn new(query: &str) -> Self {
        let characters = query
            .chars()
            .filter(|character| *character != '\n' && *character != '\r')
            .filter(|character| !is_loose_separator(*character))
            .flat_map(normalize_character)
            .collect::<Vec<_>>();
        let prefix = prefix_table(&characters);
        Self { characters, prefix }
    }

    pub(crate) fn is_empty(&self) -> bool { self.characters.is_empty() }

    pub(crate) fn is_match(&self, text: &str) -> bool {
        if self.is_empty() {
            return false;
        }
        let mut found = false;
        self.for_each(text, |_| {
            found = true;
            ControlFlow::Break(())
        });
        found
    }

    pub(crate) fn for_each(
        &self,
        text: &str,
        mut visit: impl FnMut(Range<usize>) -> ControlFlow<()>,
    ) {
        debug_assert!(!self.characters.is_empty());
        let pattern_len = self.characters.len();
        let mut matched = 0;
        let mut recent = VecDeque::<Range<usize>>::with_capacity(pattern_len);
        for (start, character) in text.char_indices() {
            if character == '\n' || character == '\r' {
                matched = 0;
                recent.clear();
                continue;
            }
            if is_loose_separator(character) {
                continue;
            }
            let source = start..start + character.len_utf8();
            for normalized in normalize_character(character) {
                while matched > 0 && self.characters[matched] != normalized {
                    matched = self.prefix[matched - 1];
                }
                if self.characters[matched] == normalized {
                    matched += 1;
                }
                recent.push_back(source.clone());
                if recent.len() > pattern_len {
                    recent.pop_front();
                }
                if matched == pattern_len {
                    let range = recent.front().map_or(start, |range| range.start)..source.end;
                    if visit(range).is_break() {
                        return;
                    }
                    matched = 0;
                    recent.clear();
                }
            }
        }
    }
}

fn prefix_table(pattern: &[char]) -> Vec<usize> {
    let mut prefix = vec![0; pattern.len()];
    let mut matched = 0;
    for index in 1..pattern.len() {
        while matched > 0 && pattern[index] != pattern[matched] {
            matched = prefix[matched - 1];
        }
        if pattern[index] == pattern[matched] {
            matched += 1;
        }
        prefix[index] = matched;
    }
    prefix
}

fn normalize_character(character: char) -> impl Iterator<Item = char> {
    fold_width(character).to_lowercase()
}

fn fold_width(character: char) -> char {
    match character {
        '\u{ff01}'..='\u{ff5e}' => char::from_u32(character as u32 - 0xfee0).unwrap_or(character),
        other => other,
    }
}

fn is_loose_separator(character: char) -> bool {
    let folded = fold_width(character);
    (character.is_whitespace() && character != '\n' && character != '\r')
        || folded.is_ascii_punctuation()
        || matches!(
            character,
            '、' | '。'
                | '，'
                | '；'
                | '：'
                | '！'
                | '？'
                | '…'
                | '—'
                | '–'
                | '·'
                | '・'
                | '“'
                | '”'
                | '‘'
                | '’'
                | '「'
                | '」'
                | '『'
                | '』'
                | '《'
                | '》'
                | '〈'
                | '〉'
                | '【'
                | '】'
                | '〔'
                | '〕'
                | '〖'
                | '〗'
                | '（'
                | '）'
                | '［'
                | '］'
                | '｛'
                | '｝'
                | '～'
                | '﹏'
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_case_width_whitespace_and_punctuation_without_skipping_prose() {
        let matcher = LooseMatcher::new("chapter １２：风起");
        assert!(matcher.is_match("CHAPTER 12 风起"));
        assert!(!matcher.is_match("chapter 12 风雨起"));
    }

    #[test]
    fn never_matches_across_a_hard_line() {
        assert!(!LooseMatcher::new("第一章风起").is_match("第一章\n风起"));
    }
}
