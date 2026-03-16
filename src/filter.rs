use std::collections::HashSet;

#[derive(Debug, Clone)]
pub struct RefFilter {
    pub branches: bool,
    pub remotes: bool,
    pub tags: bool,
    pub head: bool,
    pub stashes: bool,
}

impl RefFilter {
    pub fn from_string(filter_str: &str) -> Self {
        let filter_chars: HashSet<char> = filter_str.chars().collect();
        Self {
            branches: filter_chars.contains(&'b'),
            remotes: filter_chars.contains(&'r'),
            tags: filter_chars.contains(&'t'),
            head: filter_chars.contains(&'h'),
            stashes: filter_chars.contains(&'s'),
        }
    }

    pub fn should_include_branches(&self) -> bool {
        self.branches
    }

    pub fn should_include_remotes(&self) -> bool {
        self.remotes
    }

    pub fn should_include_tags(&self) -> bool {
        self.tags
    }

    pub fn should_include_head(&self) -> bool {
        self.head
    }

    pub fn should_include_stashes(&self) -> bool {
        self.stashes
    }
}
