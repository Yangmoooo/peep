const MAX_ENTRIES: usize = 100;

#[derive(Clone, Debug)]
pub(crate) struct InputHistory {
    entries: Vec<String>,
    cursor: Option<usize>,
    draft: String,
}

impl InputHistory {
    pub(crate) fn new(entries: Vec<String>) -> Self {
        let mut history = Self { entries: Vec::new(), cursor: None, draft: String::new() };
        for entry in entries {
            history.record(&entry);
        }
        history
    }

    pub(crate) fn entries(&self) -> &[String] { &self.entries }

    pub(crate) fn reset_navigation(&mut self) {
        self.cursor = None;
        self.draft.clear();
    }

    pub(crate) fn previous(&mut self, current: &str) -> Option<String> {
        if self.entries.is_empty() {
            return None;
        }
        let index = match self.cursor {
            Some(index) => index.saturating_sub(1),
            None => {
                self.draft = current.to_owned();
                self.entries.len() - 1
            }
        };
        self.cursor = Some(index);
        self.entries.get(index).cloned()
    }

    pub(crate) fn next(&mut self) -> Option<String> {
        let index = self.cursor?;
        if index + 1 < self.entries.len() {
            self.cursor = Some(index + 1);
            return self.entries.get(index + 1).cloned();
        }
        self.cursor = None;
        Some(self.draft.clone())
    }

    pub(crate) fn record(&mut self, value: &str) -> bool {
        let value = value.trim();
        if value.is_empty() {
            self.reset_navigation();
            return false;
        }
        if self.entries.last().is_some_and(|entry| entry == value) {
            self.reset_navigation();
            return false;
        }
        self.entries.retain(|entry| entry != value);
        self.entries.push(value.to_owned());
        if self.entries.len() > MAX_ENTRIES {
            self.entries.drain(..self.entries.len() - MAX_ENTRIES);
        }
        self.reset_navigation();
        true
    }

    pub(crate) fn clear(&mut self) {
        self.entries.clear();
        self.reset_navigation();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn navigates_oldest_to_draft_without_mutating_entries() {
        let mut history = InputHistory::new(vec!["toc".to_owned(), "recent".to_owned()]);
        assert_eq!(history.previous("draft"), Some("recent".to_owned()));
        assert_eq!(history.previous("ignored"), Some("toc".to_owned()));
        assert_eq!(history.previous("ignored"), Some("toc".to_owned()));
        assert_eq!(history.next(), Some("recent".to_owned()));
        assert_eq!(history.next(), Some("draft".to_owned()));
        assert_eq!(history.next(), None);
        assert_eq!(history.entries(), ["toc", "recent"]);
    }

    #[test]
    fn deduplicates_moves_to_latest_and_enforces_capacity() {
        let mut history = InputHistory::new(Vec::new());
        for index in 0..=MAX_ENTRIES {
            history.record(&format!("command-{index}"));
        }
        assert_eq!(history.entries().len(), MAX_ENTRIES);
        assert_eq!(history.entries()[0], "command-1");

        history.record("command-1");
        assert_eq!(history.entries().last().map(String::as_str), Some("command-1"));
        assert_eq!(history.entries().len(), MAX_ENTRIES);
    }
}
