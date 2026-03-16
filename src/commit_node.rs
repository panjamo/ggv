use std::collections::BTreeSet;

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
    branch_readme: Option<String>,
    is_current_checkout: bool,
    pub is_stash: bool,
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

        Self {
            id,
            _short_id,
            _message,
            timestamp,
            tags: BTreeSet::new(),
            refs: BTreeSet::new(),
            is_tip: false,
            branch_readme: None,
            is_current_checkout: false,
            is_stash: false,
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

    pub fn timestamp(&self) -> i64 {
        self.timestamp
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

        // Build structured node id: sha~L~local1,local2~R~remote1,remote2~T~tag1,tag2
        // ~ is forbidden in git ref names, making it a safe separator.
        // Used by the web UI context menu to reliably identify branch/tag names.
        let local_branches: Vec<&str> = self
            .refs
            .iter()
            .filter_map(|r| r.strip_prefix("refs/heads/"))
            .collect();
        let remote_branches: Vec<&str> = self
            .refs
            .iter()
            .filter_map(|r| r.strip_prefix("refs/remotes/"))
            .collect();
        let tag_names: Vec<&str> = self
            .tags
            .iter()
            .map(|t| t.trim_start_matches("refs/tags/"))
            .collect();
        let mut node_id = self.id.clone();
        if !local_branches.is_empty() {
            node_id.push_str("~L~");
            node_id.push_str(&local_branches.join(","));
        }
        if !remote_branches.is_empty() {
            node_id.push_str("~R~");
            node_id.push_str(&remote_branches.join(","));
        }
        if !tag_names.is_empty() {
            node_id.push_str("~T~");
            node_id.push_str(&tag_names.join(","));
        }

        self.get_dot_node_standard(
            &label_parts,
            &colors,
            has_local_branch,
            has_remote_branch,
            has_other_refs,
            &node_id,
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
            let mut remote_branches: BTreeSet<(String, String)> = BTreeSet::new();
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
                        remote_branches.insert((remote.to_string(), branch.to_string()));
                    }
                    has_remote_branch = true;
                } else {
                    other_refs.push(r.trim_start_matches("refs/").to_string());
                    has_other_refs = true;
                }
            }

            let mut ref_parts = Vec::new();

            for lb in &local_branches {
                ref_parts.push(lb.clone());
            }
            for (remote, branch) in &remote_branches {
                ref_parts.push(format!("{}/{}", remote, branch));
            }
            if self.is_stash && !has_local_branch && !has_remote_branch {
                // Stash node: only the stash reference label; message goes to tooltip only
                label_parts.extend(other_refs);
            } else {
                ref_parts.extend(other_refs);
                label_parts.extend(ref_parts);
            }
        }

        if !self.tags.is_empty() {
            for t in &self.tags {
                label_parts.push(t.trim_start_matches("refs/tags/").to_string());
            }
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
        node_id: &str,
    ) -> String {
        let mut label = label_parts
            .iter()
            .map(|p| dot_escape(p))
            .collect::<Vec<_>>()
            .join("\\n");

        if self.is_current_checkout {
            label = format!("CURRENT\\n{}", label);
        }
        if self.branch_readme.is_some() {
            label.push_str("\\n📝");
        }

        let tooltip_attr = if let Some(readme) = &self.branch_readme {
            let escaped = readme
                .replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace('\n', "\\n");
            format!(", tooltip=\"{}\"", escaped)
        } else {
            let msg = self
                ._message
                .trim()
                .replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace('\n', "\\n");
            if msg.is_empty() {
                String::new()
            } else {
                format!(", tooltip=\"{}\"", msg)
            }
        };

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
            "\"{}\" [id=\"{}\", label=\"{}\", shape=box, style=\"{}\", color=\"{}\", fillcolor=\"{}\", fontname=\"Arial\", fontsize={}, fontcolor=\"{}\", penwidth={}, width=0.9, height=0.4{}]",
            self.id, node_id, label, style, colors.border, colors.fill, font_size, colors.font, penwidth, tooltip_attr
        )
    }
}

/// Escapes characters that break DOT double-quoted strings.
fn dot_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
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
