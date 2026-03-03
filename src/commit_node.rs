use std::collections::{BTreeSet, HashMap, HashSet};

pub struct PredecessorInfo {
    pub parent_id: String,
    pub url: Option<String>,
    pub tooltip: Option<String>,
}

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
}

fn branch_style(branch_name: &str) -> NodeColors {
    if branch_name == "main" || branch_name == "master" {
        NodeColors {
            fill: "#059669",
            border: "#34D399",
            font: "#F0FDF4",
            dashed: false,
        }
    } else if branch_name == "develop" {
        NodeColors {
            fill: "#7C3AED",
            border: "#A78BFA",
            font: "#F5F3FF",
            dashed: false,
        }
    } else if branch_name.starts_with("feature/") {
        NodeColors {
            fill: "#2563EB",
            border: "#60A5FA",
            font: "#EFF6FF",
            dashed: false,
        }
    } else if branch_name.starts_with("release/") {
        NodeColors {
            fill: "#D97706",
            border: "#FBBF24",
            font: "#FFFBEB",
            dashed: false,
        }
    } else if branch_name.starts_with("hotfix/") {
        NodeColors {
            fill: "#DC2626",
            border: "#F87171",
            font: "#FEF2F2",
            dashed: false,
        }
    } else {
        NodeColors {
            fill: "#334155",
            border: "#60A5FA",
            font: "#E2E8F0",
            dashed: false,
        }
    }
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

    pub fn get_dot_node(&self, predecessors: &[PredecessorInfo]) -> String {
        let (label_parts, colors, has_local_branch, has_remote_branch, has_other_refs) =
            self.build_label_parts();

        let is_plain = self.refs.is_empty() && self.tags.is_empty();

        if predecessors.len() > 1 || is_plain {
            self.get_dot_node_html(
                predecessors,
                &label_parts,
                &colors,
                has_local_branch,
                has_remote_branch,
            )
        } else {
            let pred = predecessors.first();
            self.get_dot_node_standard(
                pred.and_then(|p| p.url.as_deref()),
                pred.and_then(|p| p.tooltip.as_deref()),
                &label_parts,
                &colors,
                has_local_branch,
                has_remote_branch,
                has_other_refs,
            )
        }
    }

    /// Compute label text and node colors from refs/tags.
    fn build_label_parts(&self) -> (Vec<String>, NodeColors, bool, bool, bool) {
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
                    fill: "#334155",
                    border: "#F8FAFC",
                    font: "#F8FAFC",
                    dashed: false,
                }
            } else {
                NodeColors {
                    fill: "#1E293B",
                    border: "#475569",
                    font: "#94A3B8",
                    dashed: false,
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
            branch_style(primary_branch.as_deref().unwrap_or(""))
        } else if !self.tags.is_empty() {
            NodeColors {
                fill: "#0F172A",
                border: "#94A3B8",
                font: "#CBD5E1",
                dashed: true,
            }
        } else if has_other_refs {
            NodeColors {
                fill: "#334155",
                border: "#64748B",
                font: "#CBD5E1",
                dashed: false,
            }
        } else {
            NodeColors {
                fill: "#1E293B",
                border: "#475569",
                font: "#94A3B8",
                dashed: false,
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

    /// Single-predecessor rendering.
    #[allow(clippy::too_many_arguments)]
    fn get_dot_node_standard(
        &self,
        url: Option<&str>,
        tooltip: Option<&str>,
        label_parts: &[String],
        colors: &NodeColors,
        has_local_branch: bool,
        has_remote_branch: bool,
        _has_other_refs: bool,
    ) -> String {
        let mut label = label_parts.join("\\n");

        if self.is_current_checkout {
            label = format!("CURRENT\\n{}", label);
        }
        if let Some(readme) = &self.branch_readme {
            label = format!("{}\\n{}", label, readme);
        }

        let style = if colors.dashed {
            "dashed,filled"
        } else if has_local_branch || has_remote_branch {
            "rounded,filled"
        } else {
            "filled"
        };

        let penwidth = if self.is_current_checkout { 2 } else { 1 };
        let font_size: u8 = if colors.dashed { 8 } else { 9 };

        let url_attr = url.map_or(String::new(), |u| {
            format!(", URL=\"{}\", target=\"_blank\"", u)
        });
        let tooltip_attr = tooltip.map_or(String::new(), |t| {
            let escaped = t
                .replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace('\n', "\\n");
            format!(", tooltip=\"{}\"", escaped)
        });

        format!(
            "\"{}\" [label=\"{}\", shape=box, style=\"{}\", color=\"{}\", fillcolor=\"{}\", fontname=\"Arial\", fontsize={}, fontcolor=\"{}\", penwidth={}, width=0.9, height=0.4{}{}]",
            self.id, label, style, colors.border, colors.fill, font_size, colors.font, penwidth, url_attr, tooltip_attr
        )
    }

    /// Multi-predecessor rendering using an HTML-label table.
    fn get_dot_node_html(
        &self,
        predecessors: &[PredecessorInfo],
        label_parts: &[String],
        colors: &NodeColors,
        has_local_branch: bool,
        has_remote_branch: bool,
    ) -> String {
        let penwidth = if self.is_current_checkout { 2 } else { 1 };
        let col_count = predecessors.len();

        let mut identity = label_parts.join("\n");
        if self.is_current_checkout {
            identity = format!("CURRENT\n{}", identity);
        }
        if let Some(readme) = &self.branch_readme {
            identity = format!("{}\n{}", identity, readme);
        }
        let identity_html = html_escape(&identity).replace('\n', "<BR/>");

        let header_style = if has_local_branch || has_remote_branch {
            format!(
                "BORDER=\"{}\" COLOR=\"{}\" BGCOLOR=\"{}\" STYLE=\"ROUNDED\"",
                penwidth, colors.border, colors.fill
            )
        } else if colors.dashed {
            format!(
                "BORDER=\"{}\" COLOR=\"{}\" BGCOLOR=\"{}\" STYLE=\"DASHED\"",
                penwidth, colors.border, colors.fill
            )
        } else {
            format!(
                "BORDER=\"{}\" COLOR=\"{}\" BGCOLOR=\"{}\"",
                penwidth, colors.border, colors.fill
            )
        };

        let mut html = format!(
            "<<TABLE BORDER=\"0\" CELLBORDER=\"0\" CELLSPACING=\"2\" CELLPADDING=\"4\">\
             <TR><TD COLSPAN=\"{}\" {} ALIGN=\"CENTER\"><FONT FACE=\"Arial Bold\" POINT-SIZE=\"9\" COLOR=\"{}\"><B>{}</B></FONT></TD></TR>\
             <TR>",
            col_count, header_style, colors.font, identity_html
        );

        for (i, pred) in predecessors.iter().enumerate() {
            let short = &pred.parent_id[..pred.parent_id.len().min(7)];
            let href_attr = pred.url.as_deref().map_or(String::new(), |u| {
                format!(" HREF=\"{}\" TARGET=\"_blank\"", html_escape(u))
            });
            let tooltip_attr = pred.tooltip.as_deref().map_or(String::new(), |t| {
                format!(" TOOLTIP=\"{}\"", html_escape(t).replace('\n', "&#10;"))
            });
            html.push_str(&format!(
                "<TD PORT=\"p{}\" BORDER=\"1\" COLOR=\"#334155\" BGCOLOR=\"#1E293B\" ALIGN=\"CENTER\"{}{}>\
                 <FONT FACE=\"Arial\" POINT-SIZE=\"7\" COLOR=\"#94A3B8\">← {}</FONT></TD>",
                i, href_attr, tooltip_attr, short
            ));
        }

        html.push_str("</TR></TABLE>>");

        format!(
            "\"{}\" [label={}, shape=plaintext, fontname=\"Arial\", fontsize=9]",
            self.id, html
        )
    }
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

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
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
