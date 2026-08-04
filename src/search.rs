use std::ops::{ControlFlow, Range};

use regex::{Regex, RegexBuilder};
use thiserror::Error;

use crate::loose_match::LooseMatcher;

const MAX_PATTERN_CHARS: usize = 4_096;
const REGEX_SIZE_LIMIT: usize = 10 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SearchDirection {
    Forward,
    Backward,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SearchKind {
    LooseLiteral,
    ExactLiteral,
    Regex,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchQuery {
    pub kind: SearchKind,
    pub pattern: String,
}

impl SearchQuery {
    pub fn new(kind: SearchKind, pattern: impl Into<String>) -> Self {
        Self { kind, pattern: pattern.into() }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchHit {
    pub range: Range<usize>,
    pub ordinal: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchAnalysis {
    pub current: Option<SearchHit>,
    pub total: usize,
    pub previews: Vec<SearchHit>,
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum SearchError {
    #[error("search text cannot be empty")]
    Empty,
    #[error("search pattern cannot exceed {MAX_PATTERN_CHARS} characters")]
    PatternTooLong,
    #[error("invalid regular expression: {0}")]
    InvalidRegex(String),
}

/// Finds one match and wraps at the document edge.
pub fn find_next(
    text: &str,
    query: &SearchQuery,
    from: usize,
    direction: SearchDirection,
) -> Result<Option<Range<usize>>, SearchError> {
    let matcher = Matcher::compile(query)?;
    if text.is_empty() {
        return Ok(None);
    }
    let from = floor_char_boundary(text, from.min(text.len()));
    let mut first = None;
    let mut last = None;
    let mut directional = None;
    matcher.for_each(text, |range| {
        first.get_or_insert_with(|| range.clone());
        last = Some(range.clone());
        match direction {
            SearchDirection::Forward if range.start >= from => {
                directional = Some(range);
                ControlFlow::Break(())
            }
            SearchDirection::Backward if range.end <= from => {
                directional = Some(range);
                ControlFlow::Continue(())
            }
            _ => ControlFlow::Continue(()),
        }
    });
    Ok(match direction {
        SearchDirection::Forward => directional.or(first),
        SearchDirection::Backward => directional.or(last),
    })
}

/// Counts all matches and retains a bounded preview window around the selected
/// hit.
pub fn analyze(
    text: &str,
    query: &SearchQuery,
    from: usize,
    direction: SearchDirection,
    preview_limit: usize,
) -> Result<SearchAnalysis, SearchError> {
    let matcher = Matcher::compile(query)?;
    if text.is_empty() {
        return Ok(SearchAnalysis { current: None, total: 0, previews: Vec::new() });
    }
    let from = floor_char_boundary(text, from.min(text.len()));
    let mut total = 0;
    let mut first = None::<SearchHit>;
    let mut last = None::<SearchHit>;
    let mut directional = None::<SearchHit>;
    matcher.for_each(text, |range| {
        let hit = SearchHit { range, ordinal: total };
        first.get_or_insert_with(|| hit.clone());
        match direction {
            SearchDirection::Forward if directional.is_none() && hit.range.start >= from => {
                directional = Some(hit.clone());
            }
            SearchDirection::Backward if hit.range.end <= from => {
                directional = Some(hit.clone());
            }
            _ => {}
        }
        last = Some(hit);
        total += 1;
        ControlFlow::Continue(())
    });
    let current = match direction {
        SearchDirection::Forward => directional.or_else(|| first.clone()),
        SearchDirection::Backward => directional.or(last),
    };
    let Some(current_hit) = current.as_ref() else {
        return Ok(SearchAnalysis { current: None, total: 0, previews: Vec::new() });
    };
    if preview_limit == 0 {
        return Ok(SearchAnalysis { current, total, previews: Vec::new() });
    }
    let before = preview_limit / 2;
    let mut start = current_hit.ordinal.saturating_sub(before);
    start = start.min(total.saturating_sub(preview_limit));
    let end = start.saturating_add(preview_limit).min(total);
    let mut previews = Vec::with_capacity(end.saturating_sub(start));
    let mut ordinal = 0;
    matcher.for_each(text, |range| {
        if ordinal >= end {
            return ControlFlow::Break(());
        }
        if ordinal >= start {
            previews.push(SearchHit { range, ordinal });
        }
        ordinal += 1;
        ControlFlow::Continue(())
    });
    Ok(SearchAnalysis { current, total, previews })
}

enum Matcher {
    Exact(String),
    Loose(LooseMatcher),
    Regex(Regex),
}

impl Matcher {
    fn compile(query: &SearchQuery) -> Result<Self, SearchError> {
        if query.pattern.is_empty() {
            return Err(SearchError::Empty);
        }
        if query.pattern.chars().count() > MAX_PATTERN_CHARS {
            return Err(SearchError::PatternTooLong);
        }
        match query.kind {
            SearchKind::ExactLiteral => Ok(Self::Exact(query.pattern.clone())),
            SearchKind::LooseLiteral => {
                let loose = LooseMatcher::new(&query.pattern);
                if loose.is_empty() {
                    Ok(Self::Exact(query.pattern.clone()))
                } else {
                    Ok(Self::Loose(loose))
                }
            }
            SearchKind::Regex => RegexBuilder::new(&query.pattern)
                .size_limit(REGEX_SIZE_LIMIT)
                .build()
                .map(Self::Regex)
                .map_err(|error| SearchError::InvalidRegex(error.to_string())),
        }
    }

    fn for_each(&self, text: &str, mut visit: impl FnMut(Range<usize>) -> ControlFlow<()>) {
        match self {
            Self::Exact(query) => {
                for (start, _) in text.match_indices(query) {
                    if visit(start..start + query.len()).is_break() {
                        break;
                    }
                }
            }
            Self::Regex(regex) => {
                for found in regex.find_iter(text).filter(|found| !found.is_empty()) {
                    if visit(found.range()).is_break() {
                        break;
                    }
                }
            }
            Self::Loose(pattern) => pattern.for_each(text, visit),
        }
    }
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

    fn query(kind: SearchKind, pattern: &str) -> SearchQuery { SearchQuery::new(kind, pattern) }

    #[test]
    fn exact_search_finds_unicode_and_wraps_in_both_directions() {
        let text = "第一章\n第二章";
        let exact = query(SearchKind::ExactLiteral, "章");
        let second = find_next(text, &exact, 4, SearchDirection::Forward).unwrap().unwrap();
        assert_eq!(&text[second.clone()], "章");
        let wrapped = find_next(
            text,
            &query(SearchKind::ExactLiteral, "第一"),
            text.len(),
            SearchDirection::Forward,
        )
        .unwrap()
        .unwrap();
        assert_eq!(wrapped.start, 0);
        let backward = find_next(text, &exact, 0, SearchDirection::Backward).unwrap().unwrap();
        assert_eq!(&text[backward], "章");
    }

    #[test]
    fn loose_search_ignores_layout_but_not_prose() {
        let text = "前言\n第 一 章——風 起\n第一章 风雨将起";
        let loose = query(SearchKind::LooseLiteral, "第一章風起");
        let found = find_next(text, &loose, 0, SearchDirection::Forward).unwrap().unwrap();
        assert_eq!(&text[found], "第 一 章——風 起");

        let absent = query(SearchKind::LooseLiteral, "第一章风起");
        assert_eq!(
            find_next(text, &absent, text.find("风雨").unwrap(), SearchDirection::Forward).unwrap(),
            None
        );
    }

    #[test]
    fn loose_search_folds_case_and_full_width_ascii() {
        let text = "ＣＨＡＰＴＥＲ：１２";
        let found = find_next(
            text,
            &query(SearchKind::LooseLiteral, "chapter 12"),
            0,
            SearchDirection::Forward,
        )
        .unwrap()
        .unwrap();
        assert_eq!(&text[found], text);
    }

    #[test]
    fn loose_search_does_not_cross_hard_lines() {
        assert_eq!(
            find_next(
                "第一章\n风起",
                &query(SearchKind::LooseLiteral, "第一章风起"),
                0,
                SearchDirection::Forward,
            )
            .unwrap(),
            None
        );
    }

    #[test]
    fn punctuation_only_loose_query_falls_back_to_exact() {
        let text = "正文……继续";
        let found =
            find_next(text, &query(SearchKind::LooseLiteral, "……"), 0, SearchDirection::Forward)
                .unwrap()
                .unwrap();
        assert_eq!(&text[found], "……");
    }

    #[test]
    fn regex_reports_errors_skips_empty_and_wraps() {
        assert!(matches!(
            find_next("text", &query(SearchKind::Regex, "["), 0, SearchDirection::Forward,),
            Err(SearchError::InvalidRegex(_))
        ));
        assert_eq!(
            find_next("abc", &query(SearchKind::Regex, "^|b"), 0, SearchDirection::Forward,)
                .unwrap(),
            Some(1..2)
        );
        assert_eq!(
            find_next(
                "one 12 two 34",
                &query(SearchKind::Regex, r"\d+"),
                0,
                SearchDirection::Backward,
            )
            .unwrap(),
            Some(11..13)
        );
    }

    #[test]
    fn analysis_counts_matches_and_centres_preview_window() {
        let analysis = analyze(
            "a a a a a",
            &query(SearchKind::ExactLiteral, "a"),
            4,
            SearchDirection::Forward,
            3,
        )
        .unwrap();
        assert_eq!(analysis.total, 5);
        assert_eq!(analysis.current.as_ref().unwrap().ordinal, 2);
        assert_eq!(analysis.previews.iter().map(|hit| hit.ordinal).collect::<Vec<_>>(), [1, 2, 3]);
    }

    #[test]
    fn rejects_empty_and_excessively_long_queries() {
        assert_eq!(
            find_next("text", &query(SearchKind::ExactLiteral, ""), 0, SearchDirection::Forward,),
            Err(SearchError::Empty)
        );
        assert_eq!(
            find_next(
                "text",
                &query(SearchKind::ExactLiteral, &"x".repeat(MAX_PATTERN_CHARS + 1)),
                0,
                SearchDirection::Forward,
            ),
            Err(SearchError::PatternTooLong)
        );
    }
}
