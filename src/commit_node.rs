use std::collections::{BTreeSet, HashMap, HashSet};

#[derive(Debug, Clone)]
pub struct CommitNode {
    pub id: String,
    _short_id: String,
    _message: String,
    timestamp: i64,
    tags: BTreeSet<String>,
    refs: BTreeSet<String>,
    is_tip: bool,
    _parents: Vec<String>,
    branch_readme: Option<String>,
    is_current_checkout: bool,
}

impl CommitNode {
    pub fn new(commit: &git2::Commit) -> Self {
        let id = commit.id().to_string();
        let _short_id = format!("{:.7}", id);
        let _message = commit.message().unwrap_or("").to_string();
        let timestamp = commit.time().seconds();
        let _parents: Vec<String> = commit.parent_ids().map(|oid| oid.to_string()).collect();

        Self {
            id,
            _short_id,
            _message,
            timestamp,
            tags: BTreeSet::new(),
            refs: BTreeSet::new(),
            is_tip: false,
            _parents,
            branch_readme: None,
            is_current_checkout: false,
        }
    }

    pub fn add_tag(&mut self, tag: String) {
        self.tags.insert(tag);
    }

    pub fn add_ref(&mut self, ref_name: String) {
        self.refs.insert(ref_name);
    }

    pub fn set_branch_readme(&mut self, readme: String) {
        self.branch_readme = Some(readme);
    }

    pub fn set_tip(&mut self, is_tip: bool) {
        self.is_tip = is_tip;
    }

    pub fn set_current_checkout(&mut self, is_current: bool) {
        self.is_current_checkout = is_current;
    }

    pub fn get_dot_node(&self, url: Option<&str>, tooltip: Option<&str>) -> String {
        let mut label_parts = Vec::new();
        let mut color = "white";

        if self.refs.is_empty() && self.tags.is_empty() {
            label_parts.push(self._short_id.clone());
        }

        let mut has_local_branch = false;
        let mut has_remote_branch = false;
        let mut has_other_refs = false;

        if !self.refs.is_empty() {
            let mut local_branches = HashSet::new();
            let mut remote_branches = HashMap::new();
            let mut other_refs = Vec::new();

            for r in &self.refs {
                if r.starts_with("refs/heads/") {
                    let branch_name = r.trim_start_matches("refs/heads/");
                    local_branches.insert(branch_name.to_string());
                    has_local_branch = true;
                } else if r.starts_with("refs/remotes/") {
                    let remote_ref = r.trim_start_matches("refs/remotes/");
                    if let Some((remote, branch)) = remote_ref.split_once('/') {
                        remote_branches.insert(branch.to_string(), remote.to_string());
                    }
                    has_remote_branch = true;
                } else {
                    other_refs.push(format!("📍 {}", r.trim_start_matches("refs/")));
                    has_other_refs = true;
                }
            }

            let mut ref_parts = Vec::new();
            let mut processed_branches = HashSet::new();

            for local_branch in &local_branches {
                if let Some(remote_name) = remote_branches.get(local_branch) {
                    ref_parts.push(format!("🌿🌐 {} ({})", local_branch, remote_name));
                    processed_branches.insert(local_branch.clone());
                }
            }

            for local_branch in &local_branches {
                if !processed_branches.contains(local_branch) {
                    ref_parts.push(format!("🌿 {}", local_branch));
                }
            }

            for (branch, remote) in &remote_branches {
                if !local_branches.contains(branch) {
                    ref_parts.push(format!("🌐 {}/{}", remote, branch));
                }
            }

            if !other_refs.is_empty() {
                ref_parts.extend(other_refs);
            }

            if !ref_parts.is_empty() {
                label_parts.push(ref_parts.join("\\n"));
            }
        }

        if !self.tags.is_empty() {
            let tags_str = self
                .tags
                .iter()
                .map(|t| format!("🏷️ {}", t.trim_start_matches("refs/tags/")))
                .collect::<Vec<_>>()
                .join("\\n");
            label_parts.push(tags_str);
        }

        if self.is_current_checkout {
            color = "\"#fff9c4\"";
        } else if has_local_branch {
            color = "\"#e3f2fd\"";
        } else if has_other_refs {
            color = "\"#fff3e0\"";
        } else if has_remote_branch {
            color = "\"#e8f5e8\"";
        } else if !self.tags.is_empty() {
            color = "\"#fce4ec\"";
        }

        let mut label = label_parts.join("\\n");

        if self.is_current_checkout {
            label = format!("➤ {}", label);
        }

        if self.is_tip {
            label = format!("{} ⭐", label);
        }

        if let Some(readme) = &self.branch_readme {
            label = format!("{}\\n📄 {}", label, readme);
        }

        let shape = if has_local_branch || has_remote_branch {
            "box"
        } else if has_other_refs {
            "house"
        } else if !self.tags.is_empty() {
            "octagon"
        } else {
            "egg"
        };

        let penwidth = if self.is_current_checkout { 3 } else { 0 };
        let border_color = if self.is_current_checkout {
            "\"#f57f17\""
        } else {
            "\"#2c3e50\""
        };

        let url_attr = if let Some(u) = url {
            format!(", URL=\"{}\", target=\"_blank\"", u)
        } else {
            String::new()
        };

        let tooltip_attr = if let Some(t) = tooltip {
            let escaped = t
                .replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace('\n', "\\n");
            format!(", tooltip=\"{}\"", escaped)
        } else {
            String::new()
        };

        format!(
            "\"{}\" [label=\"{}\", shape={}, style=\"rounded,filled,bold\", color={}, fillcolor={}, fontname=\"Arial\", fontsize=8, fontcolor=\"#2c3e50\", penwidth={}, width=0.8, height=0.5{}{}]",
            self.id, label, shape, border_color, color, penwidth, url_attr, tooltip_attr
        )
    }
}

impl PartialEq for CommitNode {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Eq for CommitNode {}

impl PartialOrd for CommitNode {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for CommitNode {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.timestamp.cmp(&other.timestamp)
    }
}
