use anyhow::{Context, Result};
use clap::Parser;
use git2::{BranchType, Oid, Repository};
use std::collections::{BTreeSet, HashMap};
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
        help = "Output DOT file path",
        default_value = "git-graph.dot"
    )]
    output: String,

    #[arg(long, help = "Skip SVG generation and opening", action = clap::ArgAction::SetTrue)]
    no_show: bool,

    #[arg(
        short,
        long,
        help = "Filter git refs by type (b=branches, r=remotes, t=tags, h=head)",
        default_value = "brt"
    )]
    filter: String,
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

    fn get_dot_node(&self) -> String {
        let mut label_parts = Vec::new();
        let mut color = "white"; // Default color

        // Only show commit hash if no refs or tags are present
        if self.refs.is_empty() && self.tags.is_empty() {
            label_parts.push(self._short_id.clone());
        }

        // Determine color priority: local branches > remote branches > other refs > tags
        let mut has_local_branch = false;
        let mut has_remote_branch = false;
        let mut has_other_refs = false;

        // Add all reference names (branches, remotes, HEAD) with color coding
        if !self.refs.is_empty() {
            let mut local_branches = Vec::new();
            let mut remote_branches = Vec::new();
            let mut other_refs = Vec::new();

            for r in &self.refs {
                if r.starts_with("refs/heads/") {
                    local_branches.push(format!("🌿 {}", r.trim_start_matches("refs/heads/")));
                    has_local_branch = true;
                } else if r.starts_with("refs/remotes/") {
                    remote_branches.push(format!("🌐 {}", r.trim_start_matches("refs/remotes/")));
                    has_remote_branch = true;
                } else {
                    other_refs.push(format!("📍 {}", r.trim_start_matches("refs/")));
                    has_other_refs = true;
                }
            }

            let mut ref_parts = Vec::new();
            if !local_branches.is_empty() {
                ref_parts.push(local_branches.join(", "));
            }
            if !remote_branches.is_empty() {
                ref_parts.push(remote_branches.join(", "));
            }
            if !other_refs.is_empty() {
                ref_parts.push(other_refs.join(", "));
            }

            if !ref_parts.is_empty() {
                label_parts.push(ref_parts.join(", "));
            }
        }

        // Add tags separately
        if !self.tags.is_empty() {
            let tags_str = self
                .tags
                .iter()
                .map(|t| format!("🏷️ {}", t.trim_start_matches("refs/tags/")))
                .collect::<Vec<_>>()
                .join(", ");
            label_parts.push(tags_str);
        }

        // Set color based on priority: local > other refs > remote > tags
        if has_local_branch {
            color = "\"#e3f2fd\""; // Local branches - light blue gradient
        } else if has_other_refs {
            color = "\"#fff3e0\""; // Other refs (HEAD, ROOT) - light orange
        } else if has_remote_branch {
            color = "\"#e8f5e8\""; // Remote branches - light green
        } else if !self.tags.is_empty() {
            color = "\"#fce4ec\""; // Tags - light pink
        }

        let mut label = label_parts.join(" ");

        if self.is_tip {
            label = format!("{} ⭐", label);
        }

        // Add branch readme if available
        if let Some(readme) = &self.branch_readme {
            label = format!("{}\\n📄 {}", label, readme);
        }

        // Choose shape based on reference type
        let shape = if has_local_branch {
            "ellipse"
        } else if has_remote_branch {
            "diamond"
        } else if has_other_refs {
            "house"
        } else if !self.tags.is_empty() {
            "octagon"
        } else {
            "circle"
        };

        format!(
            "\"{}\" [label=\"{}\", shape={}, style=\"filled,bold\", color=\"#2c3e50\", fillcolor={}, fontname=\"Arial\", fontsize=8, fontcolor=\"#2c3e50\", penwidth=1, width=0.8, height=0.5]",
            self.id, label, shape, color
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
}

impl GitGraphviz {
    fn new(repo_path: &str, filter: RefFilter) -> Result<Self> {
        let repo = Repository::open(repo_path)
            .with_context(|| format!("Failed to open repository at: {}", repo_path))?;

        Ok(Self { repo, filter })
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

        if self.filter.should_include_head() {
            if let Ok(head) = self.repo.head() {
                if let Some(oid) = head.target() {
                    let commit_id = self.add_ref_commit(&mut referenced_commits, oid)?;
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

        // Add root commits (commits with no parents) as if they were referenced
        self.add_root_commits(&mut referenced_commits)?;

        // Add branch readme information
        self.add_branch_readmes(&mut referenced_commits, &branch_tips)?;

        // Mark tips
        for tip_id in branch_tips.values() {
            if let Some(commit) = referenced_commits.get_mut(tip_id) {
                commit.set_tip(true);
            }
        }

        // Find junction commits and trace connections between referenced commits
        let condensed_graph = self.build_condensed_graph(&referenced_commits)?;

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

        // Write all commit nodes (now condensed)
        for commit in condensed_graph.values() {
            writeln!(writer, "  {}", commit.get_dot_node())?;
        }

        // Write connections between commits in the condensed graph
        for commit in condensed_graph.values() {
            let connections =
                self.find_condensed_connections(&commit.id, &condensed_graph, &referenced_commits)?;

            for connection_id in connections {
                if condensed_graph.contains_key(&connection_id) {
                    writeln!(writer, "  \"{}\" -> \"{}\"", connection_id, commit.id)?;
                }
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

fn generate_svg(dot_path: &str) -> Result<String> {
    let dot_file = Path::new(dot_path);
    let svg_path = dot_file.with_extension("svg");

    let output = Command::new("dot")
        .arg("-Tsvg")
        .arg(dot_path)
        .arg("-o")
        .arg(&svg_path)
        .output()
        .with_context(|| {
            "Failed to execute dot.exe. Make sure Graphviz is installed and dot.exe is in PATH"
        })?;

    if !output.status.success() {
        let error_msg = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow::anyhow!("dot.exe failed: {}", error_msg));
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

fn main() -> Result<()> {
    let args = Args::parse();

    let filter = RefFilter::from_string(&args.filter);
    let git_viz = GitGraphviz::new(&args.repo_path, filter)?;
    git_viz.generate_dot(&args.output)?;

    if !args.no_show {
        let svg_path = generate_svg(&args.output)?;
        open_file(&svg_path)?;
    }

    Ok(())
}
