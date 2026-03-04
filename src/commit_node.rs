use std::collections::{BTreeSet, HashMap, HashSet};

use crate::theme::Theme;

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

struct NodeColors {
    fill: &'static str,
    border: &'static str,
    font: &'static str,
    dashed: bool,
    base_penwidth: f32,
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

    /// Returns the best human-readable name for use in GitLab URLs.
    /// Priority: tag > local branch > remote branch > SHA hash.
    pub fn best_ref_for_url(&self) -> &str {
        if let Some(tag) = self.tags.iter().next() {
            return tag.trim_start_matches("refs/tags/");
        }
        for r in &self.refs {
            if let Some(name) = r.strip_prefix("refs/heads/") {
                return name;
            }
        }
        for r in &self.refs {
            if let Some(rest) = r.strip_prefix("refs/remotes/") {
                if let Some((_, branch)) = rest.split_once('/') {
                    return branch;
                }
            }
        }
        &self.id
    }

    pub fn get_dot_node(&self, theme: Theme) -> String {
        let (label_parts, colors, has_local_branch, has_remote_branch, has_other_refs) =
            self.build_label_parts(theme);
        self.get_dot_node_standard(
            &label_parts,
            &colors,
            has_local_branch,
            has_remote_branch,
            has_other_refs,
        )
    }

    /// Compute label text and node colors from refs/tags.
    fn build_label_parts(&self, theme: Theme) -> (Vec<String>, NodeColors, bool, bool, bool) {
        let tc = theme.colors();
        let mut label_parts = Vec::new();
        let mut has_local_branch = false;
        let mut has_remote_branch = false;
        let mut has_other_refs = false;
        let mut primary_branch: Option<String> = None;

        // Plain commit (junction node or unlabeled)
        if self.refs.is_empty() && self.tags.is_empty() {
            label_parts.push(self._short_id.clone());
            if let Some(summary) = self._message.lines().next() {
                let summary = summary.trim();
                if !summary.is_empty() {
                    label_parts.push(strip_merge_remote(summary));
                }
            }
            let colors = if self.is_current_checkout {
                NodeColors {
                    fill: tc.plain_current_fill,
                    border: tc.plain_current_border,
                    font: tc.plain_current_font,
                    dashed: false,
                    base_penwidth: 1.0,
                }
            } else {
                NodeColors {
                    fill: tc.plain_fill,
                    border: tc.plain_border,
                    font: tc.plain_font,
                    dashed: false,
                    base_penwidth: 1.0,
                }
            };
            return (label_parts, colors, false, false, false);
        }

        if !self.refs.is_empty() {
            let mut local_branches = BTreeSet::new();
            let mut remote_branches: HashMap<String, String> = HashMap::new();
            let mut other_refs = Vec::new();

            for r in &self.refs {
                if r.starts_with("refs/heads/") {
                    let branch_name = r.trim_start_matches("refs/heads/").to_string();
                    if primary_branch.is_none() {
                        primary_branch = Some(branch_name.clone());
                    }
                    local_branches.insert(branch_name);
                    has_local_branch = true;
                } else if r.starts_with("refs/remotes/") {
                    let remote_ref = r.trim_start_matches("refs/remotes/");
                    if let Some((remote, branch)) = remote_ref.split_once('/') {
                        if primary_branch.is_none() {
                            primary_branch = Some(branch.to_string());
                        }
                        remote_branches.insert(branch.to_string(), remote.to_string());
                    }
                    has_remote_branch = true;
                } else {
                    other_refs.push(r.trim_start_matches("refs/").to_string());
                    has_other_refs = true;
                }
            }

            let mut ref_parts = Vec::new();
            let mut processed = HashSet::new();

            for lb in &local_branches {
                if let Some(rn) = remote_branches.get(lb) {
                    ref_parts.push(format!("{} [{}]", lb, rn));
                    processed.insert(lb.clone());
                }
            }
            for lb in &local_branches {
                if !processed.contains(lb) {
                    ref_parts.push(lb.clone());
                }
            }
            for (branch, remote) in &remote_branches {
                if !local_branches.contains(branch) {
                    ref_parts.push(format!("{}/{}", remote, branch));
                }
            }
            ref_parts.extend(other_refs);

            if !ref_parts.is_empty() {
                label_parts.push(ref_parts.join("\\n"));
            }
        }

        if !self.tags.is_empty() {
            let tags_str = self
                .tags
                .iter()
                .map(|t| t.trim_start_matches("refs/tags/").to_string())
                .collect::<Vec<_>>()
                .join("\\n");
            label_parts.push(tags_str);
        }

        let colors = if has_local_branch || has_remote_branch {
            let (fill, border, font) = theme.branch_colors(primary_branch.as_deref().unwrap_or(""));
            NodeColors {
                fill,
                border,
                font,
                dashed: false,
                base_penwidth: 1.0,
            }
        } else if !self.tags.is_empty() {
            NodeColors {
                fill: tc.tag_fill,
                border: tc.tag_border,
                font: tc.tag_font,
                dashed: true,
                base_penwidth: tc.tag_penwidth,
            }
        } else if has_other_refs {
            NodeColors {
                fill: tc.other_fill,
                border: tc.other_border,
                font: tc.other_font,
                dashed: false,
                base_penwidth: 1.0,
            }
        } else {
            NodeColors {
                fill: tc.plain_fill,
                border: tc.plain_border,
                font: tc.plain_font,
                dashed: false,
                base_penwidth: 1.0,
            }
        };

        (
            label_parts,
            colors,
            has_local_branch,
            has_remote_branch,
            has_other_refs,
        )
    }

    fn get_dot_node_standard(
        &self,
        label_parts: &[String],
        colors: &NodeColors,
        has_local_branch: bool,
        has_remote_branch: bool,
        _has_other_refs: bool,
    ) -> String {
        let mut label = label_parts
            .iter()
            .map(|p| dot_escape(p))
            .collect::<Vec<_>>()
            .join("\\n");

        if self.is_current_checkout {
            label = format!("CURRENT\\n{}", label);
        }
        if let Some(readme) = &self.branch_readme {
            label = format!("{}\\n{}", label, dot_escape(readme));
        }

        let style = if colors.dashed {
            "dashed,filled"
        } else if has_local_branch || has_remote_branch {
            "rounded,filled"
        } else {
            "filled"
        };

        let penwidth = if self.is_current_checkout {
            2.0_f32
        } else {
            colors.base_penwidth
        };
        let font_size: u8 = if colors.dashed { 8 } else { 9 };

        format!(
            "\"{}\" [label=\"{}\", shape=box, style=\"{}\", color=\"{}\", fillcolor=\"{}\", fontname=\"Arial\", fontsize={}, fontcolor=\"{}\", penwidth={}, width=0.9, height=0.4]",
            self.id, label, style, colors.border, colors.fill, font_size, colors.font, penwidth
        )
    }
}

/// Escapes characters that break DOT double-quoted strings.
fn dot_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Strips the remote URL from git merge messages.
fn strip_merge_remote(msg: &str) -> String {
    if let Some(of_pos) = msg.find(" of ") {
        if let Some(into_offset) = msg[of_pos..].find(" into ") {
            return format!("{}{}", &msg[..of_pos], &msg[of_pos + into_offset..]);
        }
    }
    msg.to_string()
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
