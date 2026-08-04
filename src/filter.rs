use crate::loose_match::LooseMatcher;

#[derive(Clone, Debug)]
pub(crate) struct FilteredList {
    query: String,
    visible_indices: Vec<usize>,
}

impl FilteredList {
    pub(crate) fn new(total: usize) -> Self {
        Self { query: String::new(), visible_indices: (0..total).collect() }
    }

    pub(crate) fn query(&self) -> &str { &self.query }

    pub(crate) fn is_active(&self) -> bool { !self.query.is_empty() }

    pub(crate) fn len(&self) -> usize { self.visible_indices.len() }

    pub(crate) fn original_index(&self, visible_index: usize) -> Option<usize> {
        self.visible_indices.get(visible_index).copied()
    }

    pub(crate) fn update(
        &mut self,
        query: String,
        labels: &[String],
        selected_visible: usize,
    ) -> usize {
        let preferred = self.original_index(selected_visible);
        self.query = query;
        self.rebuild(labels);
        preferred
            .and_then(|original| self.visible_indices.iter().position(|index| *index == original))
            .unwrap_or(0)
    }

    pub(crate) fn refresh(&mut self, labels: &[String]) { self.rebuild(labels); }

    pub(crate) fn items(&self, labels: &[String]) -> Vec<String> {
        self.visible_indices.iter().filter_map(|index| labels.get(*index).cloned()).collect()
    }

    fn rebuild(&mut self, labels: &[String]) {
        if self.query.is_empty() {
            self.visible_indices = (0..labels.len()).collect();
            return;
        }
        let matcher = LooseMatcher::new(&self.query);
        self.visible_indices = labels
            .iter()
            .enumerate()
            .filter_map(|(index, label)| {
                let matches = if matcher.is_empty() {
                    label.contains(&self.query)
                } else {
                    matcher.is_match(label)
                };
                matches.then_some(index)
            })
            .collect();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels() -> Vec<String> {
        vec![
            "第一章 龙回故乡".to_owned(),
            "第二章 风起".to_owned(),
            "CHAPTER １２：Return".to_owned(),
        ]
    }

    #[test]
    fn filters_with_the_same_layout_tolerance_as_search() {
        let labels = labels();
        let mut list = FilteredList::new(labels.len());
        let selected = list.update("第一章龙回".to_owned(), &labels, 0);
        assert_eq!(selected, 0);
        assert_eq!(list.items(&labels), ["第一章 龙回故乡"]);

        list.update("chapter 12 return".to_owned(), &labels, 0);
        assert_eq!(list.original_index(0), Some(2));
    }

    #[test]
    fn preserves_the_original_selection_when_it_still_matches() {
        let labels = labels();
        let mut list = FilteredList::new(labels.len());
        let selected = list.update("章".to_owned(), &labels, 1);
        assert_eq!(selected, 1);
        assert_eq!(list.original_index(selected), Some(1));
    }

    #[test]
    fn empty_and_unmatched_queries_have_safe_mappings() {
        let labels = labels();
        let mut list = FilteredList::new(labels.len());
        assert_eq!(list.update("没有".to_owned(), &labels, 2), 0);
        assert_eq!(list.len(), 0);
        assert_eq!(list.original_index(0), None);

        assert_eq!(list.update(String::new(), &labels, 0), 0);
        assert_eq!(list.len(), labels.len());
    }
}
