use std::ops::Range;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SearchDirection {
    Forward,
    Backward,
}

/// Finds one literal match and wraps at the document edge.
pub fn find_literal(
    text: &str,
    query: &str,
    from: usize,
    direction: SearchDirection,
) -> Option<Range<usize>> {
    if query.is_empty() || text.is_empty() {
        return None;
    }
    let from = floor_char_boundary(text, from.min(text.len()));
    let start = match direction {
        SearchDirection::Forward => text[from..]
            .find(query)
            .map(|offset| from + offset)
            .or_else(|| text[..from].find(query)),
        SearchDirection::Backward => text[..from]
            .rfind(query)
            .or_else(|| text[from..].rfind(query).map(|offset| from + offset)),
    }?;
    Some(start..start + query.len())
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
    fn finds_unicode_literal_and_wraps_forward() {
        let text = "第一章\n第二章";
        let second = find_literal(text, "章", 4, SearchDirection::Forward).unwrap();
        assert_eq!(&text[second.clone()], "章");
        let wrapped = find_literal(text, "第一", text.len(), SearchDirection::Forward).unwrap();
        assert_eq!(wrapped.start, 0);
    }

    #[test]
    fn wraps_backward() {
        let text = "one two one";
        let found = find_literal(text, "one", 0, SearchDirection::Backward).unwrap();
        assert_eq!(found, 8..11);
    }

    #[test]
    fn rejects_empty_query() {
        assert!(find_literal("text", "", 0, SearchDirection::Forward).is_none());
    }
}
