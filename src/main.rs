use anyhow::{Context, Result};
use clap::Parser;
use git2::{BranchType, Oid, Repository};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::process::Command;

#[derive(Parser)]
#[command(name = "ggv")]
#[command(about = "Git Graph Visualizer - Generate Graphviz DOT files from Git repositories")]
struct Args {
    #[arg(short, long, help = "Path to Git repository", default_value = ".")]
    repo_path: String,

    #[arg(
        short,
        long,
        help = "Output DOT file path (default: ggv-<repo-name>.dot)"
    )]
    output: Option<String>,

    #[arg(long, help = "Skip SVG generation and opening", action = clap::ArgAction::SetTrue)]
    no_show: bool,

    #[arg(
        short,
        long,
        help = "Filter git refs by type (b=branches, r=remotes, t=tags, h=head)",
        default_value = "brt"
    )]
    filter: String,

    #[arg(
        long,
        help = "GitLab base URL for clickable commit links (e.g. https://gitlab.com/namespace/project). Auto-detected from remote if not specified."
    )]
    gitlab_url: Option<String>,

    #[arg(
        long,
        help = "Limit graph to this commit and its descendants (accepts commit hash, branch, or tag)"
    )]
    from: Option<String>,
}

#[derive(Debug, Clone)]
struct RefFilter {
    branches: bool,
    remotes: bool,
    tags: bool,
    head: bool,
}

impl RefFilter {
    fn from_string(filter_str: &str) -> Self {
        let filter_chars: std::collections::HashSet<char> = filter_str.chars().collect();
        Self {
            branches: filter_chars.contains(&'b'),
            remotes: filter_chars.contains(&'r'),
            tags: filter_chars.contains(&'t'),
            head: filter_chars.contains(&'h'),
        }
    }

    fn should_include_branches(&self) -> bool {
        self.branches
    }

    fn should_include_remotes(&self) -> bool {
        self.remotes
    }

    fn should_include_tags(&self) -> bool {
        self.tags
    }

    fn should_include_head(&self) -> bool {
        self.head
    }
}

#[derive(Debug, Clone)]
struct CommitNode {
    id: String,
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
    fn new(commit: &git2::Commit) -> Self {
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

    fn add_tag(&mut self, tag: String) {
        self.tags.insert(tag);
    }

    fn add_ref(&mut self, ref_name: String) {
        self.refs.insert(ref_name);
    }

    fn set_branch_readme(&mut self, readme: String) {
        self.branch_readme = Some(readme);
    }

    fn set_tip(&mut self, is_tip: bool) {
        self.is_tip = is_tip;
    }

    fn set_current_checkout(&mut self, is_current: bool) {
        self.is_current_checkout = is_current;
    }

    fn get_dot_node(&self, url: Option<&str>, tooltip: Option<&str>) -> String {
        let mut label_parts = Vec::new();
        let mut color = "white"; // Default color

        // Only show commit hash if no refs or tags are present
        if self.refs.is_empty() && self.tags.is_empty() {
            label_parts.push(self._short_id.clone());
        }

        // Determine color priority: current checkout > local branches > remote branches > other refs > tags
        let mut has_local_branch = false;
        let mut has_remote_branch = false;
        let mut has_other_refs = false;

        // Add all reference names (branches, remotes, HEAD) with color coding
        if !self.refs.is_empty() {
            let mut local_branches = HashSet::new();
            let mut remote_branches = HashMap::new();
            let mut other_refs = Vec::new();

            // Collect local branches, remote branches, and other refs
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

            // Process matching local/remote pairs first (abbreviated)
            for local_branch in &local_branches {
                if let Some(remote_name) = remote_branches.get(local_branch) {
                    ref_parts.push(format!("🌿🌐 {} ({})", local_branch, remote_name));
                    processed_branches.insert(local_branch.clone());
                }
            }

            // Add remaining local branches (no remote counterpart)
            for local_branch in &local_branches {
                if !processed_branches.contains(local_branch) {
                    ref_parts.push(format!("🌿 {}", local_branch));
                }
            }

            // Add remaining remote branches (no local counterpart)
            for (branch, remote) in &remote_branches {
                if !local_branches.contains(branch) {
                    ref_parts.push(format!("🌐 {}/{}", remote, branch));
                }
            }

            // Add other refs
            if !other_refs.is_empty() {
                ref_parts.extend(other_refs);
            }

            if !ref_parts.is_empty() {
                label_parts.push(ref_parts.join("\\n"));
            }
        }

        // Add tags separately
        if !self.tags.is_empty() {
            let tags_str = self
                .tags
                .iter()
                .map(|t| format!("🏷️ {}", t.trim_start_matches("refs/tags/")))
                .collect::<Vec<_>>()
                .join("\\n");
            label_parts.push(tags_str);
        }

        // Set color based on priority: current checkout > local > other refs > remote > tags
        if self.is_current_checkout {
            color = "\"#fff9c4\""; // Current checkout - bright yellow
        } else if has_local_branch {
            color = "\"#e3f2fd\""; // Local branches - light blue gradient
        } else if has_other_refs {
            color = "\"#fff3e0\""; // Other refs (HEAD, ROOT) - light orange
        } else if has_remote_branch {
            color = "\"#e8f5e8\""; // Remote branches - light green
        } else if !self.tags.is_empty() {
            color = "\"#fce4ec\""; // Tags - light pink
        }

        let mut label = label_parts.join("\\n");

        if self.is_current_checkout {
            label = format!("➤ {}", label);
        }

        if self.is_tip {
            label = format!("{} ⭐", label);
        }

        // Add branch readme if available
        if let Some(readme) = &self.branch_readme {
            label = format!("{}\\n📄 {}", label, readme);
        }

        // Choose shape based on reference type
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

struct GitGraphviz {
    repo: Repository,
    filter: RefFilter,
    gitlab_base_url: Option<String>,
    ancestor_oid: Option<Oid>,
}

impl GitGraphviz {
    fn new(
        repo_path: &str,
        filter: RefFilter,
        gitlab_url: Option<String>,
        from_commit: Option<String>,
    ) -> Result<Self> {
        let repo = Repository::open(repo_path)
            .with_context(|| format!("Failed to open repository at: {}", repo_path))?;

        let gitlab_base_url = gitlab_url.or_else(|| Self::detect_gitlab_url(&repo));

        let ancestor_oid = if let Some(ref spec) = from_commit {
            let obj = repo
                .revparse_single(spec)
                .with_context(|| format!("Could not resolve --from '{}' to a commit", spec))?;
            let commit = obj
                .peel_to_commit()
                .with_context(|| format!("'{}' does not point to a commit", spec))?;
            Some(commit.id())
        } else {
            None
        };

        Ok(Self {
            repo,
            filter,
            gitlab_base_url,
            ancestor_oid,
        })
    }

    fn parse_gitlab_remote_url(url: &str) -> Option<String> {
        // SSH: git@gitlab.example.com:namespace/project.git
        if let Some(rest) = url.strip_prefix("git@") {
            if let Some((host, path)) = rest.split_once(':') {
                let path = path.trim_end_matches(".git");
                return Some(format!("https://{}/{}", host, path));
            }
        }
        // HTTPS: https://gitlab.example.com/namespace/project.git
        if url.starts_with("https://") || url.starts_with("http://") {
            let trimmed = url.trim_end_matches(".git");
            return Some(trimmed.to_string());
        }
        None
    }

    fn detect_gitlab_url(repo: &Repository) -> Option<String> {
        // Try "origin" first, then any other remote
        if let Ok(remote) = repo.find_remote("origin") {
            if let Some(url) = remote.url() {
                if let Some(base_url) = Self::parse_gitlab_remote_url(url) {
                    return Some(base_url);
                }
            }
        }
        if let Ok(remotes) = repo.remotes() {
            for remote_name in remotes.iter().flatten() {
                if remote_name == "origin" {
                    continue;
                }
                if let Ok(remote) = repo.find_remote(remote_name) {
                    if let Some(url) = remote.url() {
                        if let Some(base_url) = Self::parse_gitlab_remote_url(url) {
                            return Some(base_url);
                        }
                    }
                }
            }
        }
        None
    }

    fn collect_path_commits(
        &self,
        from_id: &str,
        stop_id: Option<&str>,
        max: usize,
    ) -> Vec<(String, String, String, String)> {
        let mut result = Vec::new();
        let mut visited = HashSet::new();
        let mut queue = std::collections::VecDeque::new();

        if let Ok(oid) = from_id.parse::<Oid>() {
            queue.push_back(oid);
        }

        while let Some(oid) = queue.pop_front() {
            let id_str = oid.to_string();
            if visited.contains(&id_str) {
                continue;
            }
            if stop_id.is_some_and(|s| s == id_str) {
                continue;
            }
            visited.insert(id_str.clone());

            if let Ok(commit) = self.repo.find_commit(oid) {
                let short_id = format!("{:.7}", id_str);
                let message = commit.summary().unwrap_or("").to_string();
                let author = commit.author().name().unwrap_or("").to_string();
                let when = time_ago(commit.time().seconds());
                result.push((short_id, message, author, when));
                if result.len() >= max {
                    result.push((
                        "...".to_string(),
                        "(truncated)".to_string(),
                        String::new(),
                        String::new(),
                    ));
                    break;
                }
                for parent_id in commit.parent_ids() {
                    if !visited.contains(&parent_id.to_string()) {
                        queue.push_back(parent_id);
                    }
                }
            }
        }

        result
    }

    fn generate_dot(&self, output_path: &str) -> Result<()> {
        let file = File::create(output_path)
            .with_context(|| format!("Failed to create output file: {}", output_path))?;
        let mut writer = BufWriter::new(file);

        let mut referenced_commits: HashMap<String, CommitNode> = HashMap::new();
        let mut branch_tips: HashMap<String, String> = HashMap::new();

        // Collect all commits pointed to by references (branches, remotes, tags)
        if self.filter.should_include_branches() {
            let branches = self.repo.branches(Some(BranchType::Local))?;
            for branch_result in branches {
                let (branch, _) = branch_result?;
                let branch_name = branch.name()?.unwrap_or("unknown").to_string();
                let ref_name = format!("refs/heads/{}", branch_name);

                if let Some(oid) = branch.get().target() {
                    let commit_id = self.add_ref_commit(&mut referenced_commits, oid)?;
                    if let Some(commit_node) = referenced_commits.get_mut(&commit_id) {
                        commit_node.add_ref(ref_name.clone());
                    }
                    branch_tips.insert(ref_name, commit_id);
                }
            }
        }

        if self.filter.should_include_remotes() {
            let remote_branches = self.repo.branches(Some(BranchType::Remote))?;
            for branch_result in remote_branches {
                let (branch, _) = branch_result?;
                let branch_name = branch.name()?.unwrap_or("unknown").to_string();
                let ref_name = format!("refs/remotes/{}", branch_name);

                if let Some(oid) = branch.get().target() {
                    let commit_id = self.add_ref_commit(&mut referenced_commits, oid)?;
                    if let Some(commit_node) = referenced_commits.get_mut(&commit_id) {
                        commit_node.add_ref(ref_name.clone());
                    }
                    branch_tips.insert(ref_name, commit_id);
                }
            }
        }

        // Always track the current checkout (HEAD) for highlighting purposes
        let mut current_checkout_id: Option<String> = None;
        if let Ok(head) = self.repo.head() {
            if let Some(oid) = head.target() {
                let commit_id = self.add_ref_commit(&mut referenced_commits, oid)?;
                current_checkout_id = Some(commit_id.clone());

                // Only add HEAD as a visible ref if filter includes it
                if self.filter.should_include_head() {
                    if let Some(commit_node) = referenced_commits.get_mut(&commit_id) {
                        commit_node.add_ref("HEAD".to_string());
                    }
                    branch_tips.insert("HEAD".to_string(), commit_id);
                }
            }
        }

        if self.filter.should_include_tags() {
            self.add_tagged_commits(&mut referenced_commits)?;
        }

        // Apply ancestor filter: keep only the specified commit and its descendants
        if let Some(ancestor_oid) = self.ancestor_oid {
            let ancestor_id_str = ancestor_oid.to_string();

            // Ensure the ancestor itself is present and marked as ROOT
            let commit_id = self.add_ref_commit(&mut referenced_commits, ancestor_oid)?;
            if let Some(node) = referenced_commits.get_mut(&commit_id) {
                node.add_ref("ROOT".to_string());
            }

            // Retain only the ancestor and commits that descend from it
            referenced_commits.retain(|id, _| {
                if *id == ancestor_id_str {
                    return true;
                }
                id.parse::<Oid>()
                    .ok()
                    .and_then(|oid| self.repo.graph_descendant_of(oid, ancestor_oid).ok())
                    .unwrap_or(false)
            });
        } else {
            // Add root commits (commits with no parents) as if they were referenced
            self.add_root_commits(&mut referenced_commits)?;
        }

        // Add branch readme information
        self.add_branch_readmes(&mut referenced_commits, &branch_tips)?;

        // Mark tips
        for tip_id in branch_tips.values() {
            if let Some(commit) = referenced_commits.get_mut(tip_id) {
                commit.set_tip(true);
            }
        }

        // Mark current checkout
        if let Some(checkout_id) = current_checkout_id {
            if let Some(commit) = referenced_commits.get_mut(&checkout_id) {
                commit.set_current_checkout(true);
            }
        }

        // Find junction commits and trace connections between referenced commits
        let condensed_graph = self.build_condensed_graph(&referenced_commits)?;

        // Pre-compute all condensed connections (needed for both URLs and edges)
        let mut commit_parents: HashMap<String, Vec<String>> = HashMap::new();
        for commit in condensed_graph.values() {
            let connections =
                self.find_condensed_connections(&commit.id, &condensed_graph, &referenced_commits)?;
            let valid: Vec<String> = connections
                .into_iter()
                .filter(|id| condensed_graph.contains_key(id))
                .collect();
            commit_parents.insert(commit.id.clone(), valid);
        }

        // Write DOT file with enhanced styling
        writeln!(writer, "digraph git {{")?;
        writeln!(writer, "  rankdir=BT;")?; // Bottom to Top (flipped)
        writeln!(writer, "  bgcolor=\"#f8f9fa\";")?;
        writeln!(writer, "  node [fontname=\"Arial Bold\", fontsize=10];")?;
        writeln!(
            writer,
            "  edge [color=\"#495057\", penwidth=2, arrowsize=0.8, arrowhead=vee];"
        )?;
        writeln!(writer, "  graph [splines=ortho, nodesep=0.3, ranksep=0.4];")?;

        // Write all commit nodes with compare URLs (parent..child shows accumulated diff)
        for commit in condensed_graph.values() {
            let parent_id = commit_parents
                .get(&commit.id)
                .and_then(|parents| parents.first())
                .map(|s| s.as_str());

            let is_ancestor_root = self
                .ancestor_oid
                .is_some_and(|a| a.to_string() == commit.id);

            let url = if is_ancestor_root {
                None
            } else {
                self.gitlab_base_url.as_deref().map(|base| match parent_id {
                    Some(pid) => format!("{}/-/compare/{}...{}", base, pid, commit.id),
                    None => format!("{}/-/commit/{}", base, commit.id),
                })
            };

            let tooltip = if is_ancestor_root {
                None
            } else {
                let path_commits = self.collect_path_commits(&commit.id, parent_id, 20);
                if path_commits.is_empty() {
                    None
                } else {
                    Some(
                        path_commits
                            .iter()
                            .map(|(hash, msg, author, when)| {
                                format!("{}: {} ({}, {})", hash, msg, author, when)
                            })
                            .collect::<Vec<_>>()
                            .join("\n"),
                    )
                }
            };

            writeln!(
                writer,
                "  {}",
                commit.get_dot_node(url.as_deref(), tooltip.as_deref())
            )?;
        }

        // Write edges from pre-computed connections
        for (child_id, parents) in &commit_parents {
            for parent_id in parents {
                writeln!(writer, "  \"{}\" -> \"{}\"", parent_id, child_id)?;
            }
        }

        writeln!(writer, "}}")?;
        writer.flush()?;

        println!("Generated condensed DOT file: {}", output_path);
        Ok(())
    }

    fn add_ref_commit(
        &self,
        all_commits: &mut HashMap<String, CommitNode>,
        oid: Oid,
    ) -> Result<String> {
        let oid_str = oid.to_string();

        if let std::collections::hash_map::Entry::Vacant(e) = all_commits.entry(oid_str.clone()) {
            let commit = self.repo.find_commit(oid)?;
            let commit_node = CommitNode::new(&commit);
            e.insert(commit_node);
        }

        Ok(oid_str)
    }

    fn add_tagged_commits(&self, all_commits: &mut HashMap<String, CommitNode>) -> Result<()> {
        // Collect tag info first, then add commits
        let mut tag_commits = Vec::new();

        self.repo.tag_foreach(|oid, name| {
            let tag_name = String::from_utf8_lossy(name).to_string();

            // Try to find the commit this tag points to
            if let Ok(tag_target) = self.repo.find_object(oid, None) {
                let commit_oid = match tag_target.kind() {
                    Some(git2::ObjectType::Commit) => Some(oid),
                    Some(git2::ObjectType::Tag) => tag_target.as_tag().map(|tag| tag.target_id()),
                    _ => None,
                };

                if let Some(commit_oid) = commit_oid {
                    tag_commits.push((commit_oid, tag_name));
                }
            }
            true
        })?;

        // Now add the commits and tags
        for (commit_oid, tag_name) in tag_commits {
            let commit_id = self.add_ref_commit(all_commits, commit_oid)?;
            if let Some(commit_node) = all_commits.get_mut(&commit_id) {
                commit_node.add_tag(tag_name);
            }
        }

        Ok(())
    }

    fn add_root_commits(&self, all_commits: &mut HashMap<String, CommitNode>) -> Result<()> {
        let mut revwalk = self.repo.revwalk()?;
        revwalk.push_head()?;
        revwalk.set_sorting(git2::Sort::TOPOLOGICAL)?;

        for oid_result in revwalk {
            let oid = oid_result?;
            if let Ok(commit) = self.repo.find_commit(oid) {
                // If this commit has no parents, it's a root commit
                if commit.parent_count() == 0 {
                    let commit_id = self.add_ref_commit(all_commits, oid)?;
                    if let Some(commit_node) = all_commits.get_mut(&commit_id) {
                        commit_node.add_ref("ROOT".to_string());
                    }
                }
            }
        }

        Ok(())
    }

    fn add_branch_readmes(
        &self,
        all_commits: &mut HashMap<String, CommitNode>,
        branch_tips: &HashMap<String, String>,
    ) -> Result<()> {
        for (branch_ref, commit_id) in branch_tips {
            if let Some(_branch_name) = branch_ref.strip_prefix("refs/heads/") {
                if let Ok(commit_oid) = commit_id.parse::<Oid>() {
                    if let Ok(commit) = self.repo.find_commit(commit_oid) {
                        if let Ok(tree) = commit.tree() {
                            if let Some(entry) = tree.get_name("BRANCHREADME.md") {
                                if let Ok(blob) = self.repo.find_blob(entry.id()) {
                                    if let Ok(content) = std::str::from_utf8(blob.content()) {
                                        if let Some(first_line) = content.lines().next() {
                                            if !first_line.trim().is_empty() {
                                                if let Some(commit_node) =
                                                    all_commits.get_mut(commit_id)
                                                {
                                                    // Wrap long lines at word boundaries
                                                    let wrapped = if first_line.len() > 30 {
                                                        let words: Vec<&str> =
                                                            first_line.split_whitespace().collect();
                                                        let mut lines = Vec::new();
                                                        let mut current_line = String::new();

                                                        for word in words {
                                                            if current_line.len() + word.len() + 1
                                                                > 30
                                                            {
                                                                if !current_line.is_empty() {
                                                                    lines.push(current_line);
                                                                    current_line = word.to_string();
                                                                } else {
                                                                    lines.push(word.to_string());
                                                                }
                                                            } else {
                                                                if !current_line.is_empty() {
                                                                    current_line.push(' ');
                                                                }
                                                                current_line.push_str(word);
                                                            }
                                                        }
                                                        if !current_line.is_empty() {
                                                            lines.push(current_line);
                                                        }
                                                        lines.join("\\n")
                                                    } else {
                                                        first_line.to_string()
                                                    };
                                                    commit_node.set_branch_readme(wrapped);
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn build_condensed_graph(
        &self,
        referenced_commits: &HashMap<String, CommitNode>,
    ) -> Result<HashMap<String, CommitNode>> {
        let mut condensed_graph = referenced_commits.clone();

        // Find junction commits and necessary intermediate commits to maintain connectivity
        let mut additional_commits = HashMap::new();

        // Find connections between all pairs of referenced commits
        for commit_id in referenced_commits.keys() {
            self.find_connection_path(commit_id, referenced_commits, &mut additional_commits)?;
        }

        // Add only the necessary intermediate commits
        for (commit_id, commit_node) in additional_commits {
            condensed_graph.entry(commit_id).or_insert(commit_node);
        }

        Ok(condensed_graph)
    }

    fn find_connection_path(
        &self,
        start_commit_id: &str,
        referenced_commits: &HashMap<String, CommitNode>,
        additional_commits: &mut HashMap<String, CommitNode>,
    ) -> Result<()> {
        let mut visited = std::collections::HashSet::new();
        let mut to_visit = Vec::new();
        let max_depth = 100; // Limit search depth

        if let Ok(start_oid) = start_commit_id.parse::<Oid>() {
            to_visit.push((start_oid, 0));
        }

        while let Some((current_oid, depth)) = to_visit.pop() {
            if depth > max_depth {
                continue;
            }

            let current_id = current_oid.to_string();

            if visited.contains(&current_id) {
                continue;
            }
            visited.insert(current_id.clone());

            if let Ok(commit) = self.repo.find_commit(current_oid) {
                // If this commit is a merge commit, it might be a necessary junction
                if commit.parent_count() > 1 && !referenced_commits.contains_key(&current_id) {
                    let mut connects_to_refs = 0;
                    for parent_id in commit.parent_ids() {
                        if self.connects_to_referenced_commit(
                            &parent_id.to_string(),
                            referenced_commits,
                        )? {
                            connects_to_refs += 1;
                        }
                    }

                    if connects_to_refs > 1 {
                        let junction_commit = CommitNode::new(&commit);
                        additional_commits.insert(current_id.clone(), junction_commit);
                    }
                }

                // Continue traversing parents, but only if they might connect to other refs
                for parent_id in commit.parent_ids() {
                    let parent_id_str = parent_id.to_string();
                    if !referenced_commits.contains_key(&parent_id_str) {
                        to_visit.push((parent_id, depth + 1));
                    }
                }
            }
        }

        Ok(())
    }

    fn connects_to_referenced_commit(
        &self,
        start_commit_id: &str,
        referenced_commits: &HashMap<String, CommitNode>,
    ) -> Result<bool> {
        let mut visited = std::collections::HashSet::new();
        let mut to_visit = Vec::new();
        let max_depth = 100; // Limit depth to prevent very long searches

        if let Ok(start_oid) = start_commit_id.parse::<Oid>() {
            to_visit.push((start_oid, 0));
        }

        while let Some((current_oid, depth)) = to_visit.pop() {
            if depth > max_depth {
                continue;
            }

            let current_id = current_oid.to_string();

            if visited.contains(&current_id) {
                continue;
            }
            visited.insert(current_id.clone());

            // If we found a referenced commit, return true
            if referenced_commits.contains_key(&current_id) {
                return Ok(true);
            }

            // Continue traversing parents
            if let Ok(commit) = self.repo.find_commit(current_oid) {
                for parent_id in commit.parent_ids() {
                    to_visit.push((parent_id, depth + 1));
                }
            }
        }

        Ok(false)
    }

    fn find_condensed_connections(
        &self,
        commit_id: &str,
        condensed_graph: &HashMap<String, CommitNode>,
        _referenced_commits: &HashMap<String, CommitNode>,
    ) -> Result<Vec<String>> {
        let mut connections = Vec::new();
        let mut visited = std::collections::HashSet::new();

        if let Ok(commit_oid) = commit_id.parse::<Oid>() {
            if let Ok(commit) = self.repo.find_commit(commit_oid) {
                for parent_id in commit.parent_ids() {
                    let connection = self.find_next_condensed_commit(
                        &parent_id.to_string(),
                        condensed_graph,
                        &mut visited,
                    )?;

                    if let Some(conn_id) = connection {
                        connections.push(conn_id);
                    }
                }
            }
        }

        Ok(connections)
    }

    fn find_next_condensed_commit(
        &self,
        start_commit_id: &str,
        condensed_graph: &HashMap<String, CommitNode>,
        visited: &mut std::collections::HashSet<String>,
    ) -> Result<Option<String>> {
        let mut to_visit = Vec::new();
        to_visit.push(start_commit_id.to_string());

        while let Some(current_id) = to_visit.pop() {
            if visited.contains(&current_id) {
                continue;
            }
            visited.insert(current_id.clone());

            // If this commit is in our condensed graph, it's our connection target
            if condensed_graph.contains_key(&current_id) {
                return Ok(Some(current_id));
            }

            // Otherwise, traverse parents to find the next condensed commit
            if let Ok(commit_oid) = current_id.parse::<Oid>() {
                if let Ok(commit) = self.repo.find_commit(commit_oid) {
                    for parent_id in commit.parent_ids() {
                        let parent_id_str = parent_id.to_string();
                        if !visited.contains(&parent_id_str) {
                            to_visit.push(parent_id_str);
                        }
                    }
                }
            }
        }

        Ok(None)
    }
}

fn find_dot_executable() -> Option<std::path::PathBuf> {
    use std::path::PathBuf;

    // Check GRAPHVIZ_DOT environment variable first
    if let Ok(env_path) = std::env::var("GRAPHVIZ_DOT") {
        let path = PathBuf::from(&env_path);
        if path.exists() {
            return Some(path);
        }
    }

    // Windows: search common installation directories
    #[cfg(target_os = "windows")]
    {
        let candidates = [
            r"C:\Program Files\Graphviz\bin\dot.exe",
            r"C:\Program Files (x86)\Graphviz\bin\dot.exe",
            r"C:\Graphviz\bin\dot.exe",
        ];
        for candidate in &candidates {
            let path = PathBuf::from(candidate);
            if path.exists() {
                return Some(path);
            }
        }
        // Fall back to searching PATH via 'where'
        if let Ok(output) = Command::new("where").arg("dot").output() {
            if output.status.success() {
                if let Ok(s) = std::str::from_utf8(&output.stdout) {
                    for line in s.lines() {
                        let path = PathBuf::from(line.trim());
                        if path.exists() {
                            return Some(path);
                        }
                    }
                }
            }
        }
    }

    // macOS / Linux: use 'which'
    #[cfg(not(target_os = "windows"))]
    {
        if let Ok(output) = Command::new("which").arg("dot").output() {
            if output.status.success() {
                if let Ok(s) = std::str::from_utf8(&output.stdout) {
                    if let Some(line) = s.lines().next() {
                        let path = PathBuf::from(line.trim());
                        if path.exists() {
                            return Some(path);
                        }
                    }
                }
            }
        }
    }

    None
}

fn generate_svg(dot_path: &str) -> Result<String> {
    let dot_file = Path::new(dot_path);
    let svg_path = dot_file.with_extension("svg");

    let dot_exe = find_dot_executable().ok_or_else(|| {
        anyhow::anyhow!(
            "Graphviz (dot.exe) was not found.\n\
             \n\
             To install Graphviz on Windows:\n\
             \n\
             Option 1 – winget (Windows Package Manager):\n\
             \n\
             winget install --id Graphviz.Graphviz\n\
             \n\
             Option 2 – Chocolatey:\n\
             \n\
             choco install graphviz\n\
             \n\
             Option 3 – Manual download:\n\
             \n\
             https://graphviz.org/download/\n\
             \n\
             After installation, open a new terminal so the PATH is updated.\n\
             Alternatively, set the GRAPHVIZ_DOT environment variable to the full\n\
             path of dot.exe, e.g.:\n\
             \n\
             set GRAPHVIZ_DOT=C:\\Program Files\\Graphviz\\bin\\dot.exe"
        )
    })?;

    println!("Using Graphviz: {}", dot_exe.display());

    let output = Command::new(&dot_exe)
        .args(["-Tsvg", dot_path, "-o"])
        .arg(&svg_path)
        .output()
        .with_context(|| format!("Failed to execute: {}", dot_exe.display()))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow::anyhow!("Graphviz dot failed: {}", stderr));
    }

    let svg_path_str = svg_path.to_string_lossy().to_string();
    println!("Generated SVG file: {}", svg_path_str);
    Ok(svg_path_str)
}

fn open_file(file_path: &str) -> Result<()> {
    #[cfg(target_os = "windows")]
    {
        Command::new("cmd")
            .args(["/C", "start", "", file_path])
            .output()
            .with_context(|| format!("Failed to open file: {}", file_path))?;
    }

    #[cfg(target_os = "macos")]
    {
        Command::new("open")
            .arg(file_path)
            .output()
            .with_context(|| format!("Failed to open file: {}", file_path))?;
    }

    #[cfg(target_os = "linux")]
    {
        Command::new("xdg-open")
            .arg(file_path)
            .output()
            .with_context(|| format!("Failed to open file: {}", file_path))?;
    }

    println!("Opened file: {}", file_path);
    Ok(())
}

fn time_ago(timestamp: i64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let diff = now - timestamp;
    if diff < 60 {
        format!("{} seconds ago", diff)
    } else if diff < 3600 {
        format!("{} minutes ago", diff / 60)
    } else if diff < 86400 {
        format!("{} hours ago", diff / 3600)
    } else if diff < 86400 * 30 {
        format!("{} days ago", diff / 86400)
    } else if diff < 86400 * 365 {
        format!("{} months ago", diff / (86400 * 30))
    } else {
        format!("{} years ago", diff / (86400 * 365))
    }
}

fn repo_name_from_path(repo_path: &str) -> String {
    let path = std::path::Path::new(repo_path);
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    canonical
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("repo")
        .to_string()
}

fn main() -> Result<()> {
    let args = Args::parse();

    let output = args.output.unwrap_or_else(|| {
        let repo_name = repo_name_from_path(&args.repo_path);
        match &args.from {
            Some(from) => {
                let safe_from: String = from
                    .chars()
                    .map(|c| {
                        if c.is_alphanumeric() || c == '-' || c == '_' || c == '.' {
                            c
                        } else {
                            '-'
                        }
                    })
                    .collect();
                format!("ggv-{}-from-{}.dot", repo_name, safe_from)
            }
            None => format!("ggv-{}.dot", repo_name),
        }
    });

    let filter = RefFilter::from_string(&args.filter);
    let git_viz = GitGraphviz::new(&args.repo_path, filter, args.gitlab_url, args.from)?;
    git_viz.generate_dot(&output)?;

    if !args.no_show {
        let svg_path = generate_svg(&output)?;
        std::fs::remove_file(&output)
            .with_context(|| format!("Failed to delete DOT file: {}", output))?;
        open_file(&svg_path)?;
    }

    Ok(())
}
