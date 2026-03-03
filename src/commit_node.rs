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
        let (label_parts, color, has_local_branch, has_remote_branch, has_other_refs) =
            self.build_label_parts();

        if predecessors.len() > 1 {
            self.get_dot_node_html(
                predecessors,
                &label_parts,
                color,
                has_local_branch,
                has_remote_branch,
            )
        } else {
            let pred = predecessors.first();
            self.get_dot_node_standard(
                pred.and_then(|p| p.url.as_deref()),
                pred.and_then(|p| p.tooltip.as_deref()),
                &label_parts,
                color,
                has_local_branch,
                has_remote_branch,
                has_other_refs,
            )
        }
    }

    /// Shared logic: compute label text, color, and ref-type flags.
    fn build_label_parts(&self) -> (Vec<String>, &'static str, bool, bool, bool) {
        let mut label_parts = Vec::new();
        let mut color = "white";
        let mut has_local_branch = false;
        let mut has_remote_branch = false;
        let mut has_other_refs = false;

        if self.refs.is_empty() && self.tags.is_empty() {
            label_parts.push(self._short_id.clone());
            if let Some(summary) = self._message.lines().next() {
                let summary = summary.trim();
                if !summary.is_empty() {
                    label_parts.push(strip_merge_remote(summary));
                }
            }
        }

        if !self.refs.is_empty() {
            let mut local_branches = HashSet::new();
            let mut remote_branches = HashMap::new();
            let mut other_refs = Vec::new();

            for r in &self.refs {
                if r.starts_with("refs/heads/") {
                    local_branches.insert(r.trim_start_matches("refs/heads/").to_string());
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
            let mut processed = HashSet::new();

            for lb in &local_branches {
                if let Some(rn) = remote_branches.get(lb) {
                    ref_parts.push(format!("🌿🌐 {} ({})", lb, rn));
                    processed.insert(lb.clone());
                }
            }
            for lb in &local_branches {
                if !processed.contains(lb) {
                    ref_parts.push(format!("🌿 {}", lb));
                }
            }
            for (branch, remote) in &remote_branches {
                if !local_branches.contains(branch) {
                    ref_parts.push(format!("🌐 {}/{}", remote, branch));
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

        (
            label_parts,
            color,
            has_local_branch,
            has_remote_branch,
            has_other_refs,
        )
    }

    /// Standard (0–1 predecessor) rendering — existing appearance.
    #[allow(clippy::too_many_arguments)]
    fn get_dot_node_standard(
        &self,
        url: Option<&str>,
        tooltip: Option<&str>,
        label_parts: &[String],
        color: &str,
        has_local_branch: bool,
        has_remote_branch: bool,
        has_other_refs: bool,
    ) -> String {
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
            "\"{}\" [label=\"{}\", shape={}, style=\"rounded,filled,bold\", color={}, fillcolor={}, fontname=\"Arial\", fontsize=8, fontcolor=\"#2c3e50\", penwidth={}, width=0.8, height=0.5{}{}]",
            self.id, label, shape, border_color, color, penwidth, url_attr, tooltip_attr
        )
    }

    /// Multi-predecessor rendering using an HTML-label table.
    /// Top row: node identity (refs/tags). Bottom row: one cell per predecessor.
    fn get_dot_node_html(
        &self,
        predecessors: &[PredecessorInfo],
        label_parts: &[String],
        color: &str,
        has_local_branch: bool,
        has_remote_branch: bool,
    ) -> String {
        // Strip surrounding quotes from color for use as HTML attribute value.
        let bgcolor = color.trim_matches('"');
        let penwidth = if self.is_current_checkout { 3 } else { 1 };
        let border_color = if self.is_current_checkout {
            "#f57f17"
        } else {
            "#2c3e50"
        };

        let col_count = predecessors.len();

        // Build top-row identity text (HTML-escape, newlines → <BR/>).
        let mut identity = label_parts.join("\n");
        if self.is_current_checkout {
            identity = format!("➤ {}", identity);
        }
        if self.is_tip {
            identity = format!("{} ⭐", identity);
        }
        if let Some(readme) = &self.branch_readme {
            identity = format!("{}\n📄 {}", identity, readme);
        }
        let identity_html = html_escape(&identity).replace('\n', "<BR/>");

        // Top-row cell style.
        let header_style = if has_local_branch || has_remote_branch {
            format!(
                "BORDER=\"{}\" COLOR=\"{}\" BGCOLOR=\"{}\" STYLE=\"ROUNDED\"",
                penwidth, border_color, bgcolor
            )
        } else {
            format!(
                "BORDER=\"{}\" COLOR=\"{}\" BGCOLOR=\"{}\"",
                penwidth, border_color, bgcolor
            )
        };

        let mut html = format!(
            "<<TABLE BORDER=\"0\" CELLBORDER=\"0\" CELLSPACING=\"2\" CELLPADDING=\"4\">\
             <TR><TD COLSPAN=\"{}\" {} ALIGN=\"CENTER\"><FONT FACE=\"Arial Bold\" POINT-SIZE=\"8\"><B>{}</B></FONT></TD></TR>\
             <TR>",
            col_count, header_style, identity_html
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
                "<TD PORT=\"p{}\" BORDER=\"1\" COLOR=\"{}\" BGCOLOR=\"#dfe6ed\" ALIGN=\"CENTER\"{}{}>\
                 <FONT FACE=\"Arial\" POINT-SIZE=\"7\">← {}</FONT></TD>",
                i, border_color, href_attr, tooltip_attr, short
            ));
        }

        html.push_str("</TR></TABLE>>");

        format!(
            "\"{}\" [label={}, shape=plaintext, fontname=\"Arial\", fontsize=8]",
            self.id, html
        )
    }
}

/// Strips the remote URL from git merge messages.
/// "Merge branch 'X' of <url> into Y" → "Merge branch 'X' into Y"
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
