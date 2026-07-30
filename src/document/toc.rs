pub(crate) fn label_is_visible_at(text: &str, label: &str, offset: usize) -> bool {
    let visible_line = text
        .get(offset..)
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    let label = semantic_key(label);
    let visible = semantic_key(visible_line);
    !label.is_empty()
        && (label == visible
            || (label.chars().count() >= 2 && visible.contains(&label))
            || (visible.chars().count() >= 2 && label.contains(&visible)))
}

pub(crate) fn labels_compatible(left: &str, right: &str) -> bool {
    let left = semantic_key(left);
    let right = semantic_key(right);
    left == right
        || (left.chars().count() >= 2 && right.starts_with(&left))
        || (right.chars().count() >= 2 && left.starts_with(&right))
}

pub(crate) fn label_is_landmark(label: &str) -> bool {
    let key = semantic_key(label);
    matches!(
        key.as_str(),
        "目录" | "目錄" | "封面" | "扉页" | "扉頁" | "书名页" | "書名頁" | "copyright"
    ) || key.starts_with("版权")
        || key.starts_with("版權")
}

fn semantic_key(label: &str) -> String {
    label
        .chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compatibility_ignores_punctuation_case_and_optional_suffixes() {
        assert!(labels_compatible("Chapter 12: Arrival", "chapter-12"));
        assert!(labels_compatible("第一章 风起", "第一章"));
        assert!(!labels_compatible("第一章 风起", "第二章 云涌"));
    }

    #[test]
    fn visible_labels_and_landmarks_use_the_same_semantic_key() {
        let text = "\n  第一章：风起\n正文";
        assert!(label_is_visible_at(text, "第一章 风起", 0));
        assert!(label_is_landmark("版 权 页"));
        assert!(!label_is_landmark("第一章"));
    }
}
