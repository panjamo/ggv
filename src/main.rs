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
    is_tip: bool,
    _parents: Vec<String>,
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
            is_tip: false,
            _parents,
        }
    }

    fn add_tag(&mut self, tag: String) {
        self.tags.insert(tag);
    }

    fn set_tip(&mut self, is_tip: bool) {
        self.is_tip = is_tip;
    }

    fn get_dot_node(&self) -> String {
        let mut label = self._short_id.clone();

        if !self.tags.is_empty() {
            let tags_str = self
                .tags
                .iter()
                .map(|t| t.trim_start_matches("refs/tags/"))
                .collect::<Vec<_>>()
                .join(", ");
            label = format!("[{}]", tags_str);
        }

        if self.is_tip {
            label = format!("{} {{TIP}}", label);
        }

        format!(
            "\"{}\" [label=\"{}\", shape=box, style=filled, color=black, fillcolor=white]",
            self.id, label
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
                    branch_tips.insert(ref_name, commit_id);
                }
            }
        }

        if self.filter.should_include_head() {
            if let Ok(head) = self.repo.head() {
                if let Some(oid) = head.target() {
                    let commit_id = self.add_ref_commit(&mut referenced_commits, oid)?;
                    branch_tips.insert("HEAD".to_string(), commit_id);
                }
            }
        }

        if self.filter.should_include_tags() {
            self.add_tagged_commits(&mut referenced_commits)?;
        }

        // Mark tips
        for tip_id in branch_tips.values() {
            if let Some(commit) = referenced_commits.get_mut(tip_id) {
                commit.set_tip(true);
            }
        }

        // Find junction commits and trace connections between referenced commits
        let condensed_graph = self.build_condensed_graph(&referenced_commits)?;

        // Write DOT file
        writeln!(writer, "digraph git {{")?;
        writeln!(writer, "  rankdir=TB;")?;
        writeln!(writer, "  node [fontsize=10];")?;

        // Write all commit nodes (now condensed)
        for commit in condensed_graph.values() {
            writeln!(writer, "  {}", commit.get_dot_node())?;
        }

        // Write direct connections between referenced commits
        for commit in condensed_graph.values() {
            let connections = self.find_connections_to_referenced_commits(
                &commit.id,
                &condensed_graph,
                &referenced_commits,
            )?;

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

    fn build_condensed_graph(
        &self,
        referenced_commits: &HashMap<String, CommitNode>,
    ) -> Result<HashMap<String, CommitNode>> {
        let mut condensed_graph = referenced_commits.clone();

        // Find junction commits - commits that are merge points connecting referenced commits
        // but are not themselves referenced
        let mut junctions = HashMap::new();

        for commit_id in referenced_commits.keys() {
            self.find_junction_commits(commit_id, referenced_commits, &mut junctions)?;
        }

        // Add junction commits to the condensed graph
        for (junction_id, junction_commit) in junctions {
            condensed_graph.insert(junction_id, junction_commit);
        }

        Ok(condensed_graph)
    }

    fn find_junction_commits(
        &self,
        start_commit_id: &str,
        referenced_commits: &HashMap<String, CommitNode>,
        junctions: &mut HashMap<String, CommitNode>,
    ) -> Result<()> {
        let mut visited = std::collections::HashSet::new();
        let mut to_visit = Vec::new();
        let max_depth = 50; // Limit depth to prevent very long searches

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
                // If this commit has multiple parents (merge commit) and connects
                // to referenced commits through different paths, it's a junction
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
                        junctions.insert(current_id.clone(), junction_commit);
                    }
                }

                // Continue traversing parents
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

    fn find_connections_to_referenced_commits(
        &self,
        commit_id: &str,
        condensed_graph: &HashMap<String, CommitNode>,
        referenced_commits: &HashMap<String, CommitNode>,
    ) -> Result<Vec<String>> {
        let mut connections = Vec::new();
        let mut visited = std::collections::HashSet::new();

        if let Ok(commit_oid) = commit_id.parse::<Oid>() {
            if let Ok(commit) = self.repo.find_commit(commit_oid) {
                for parent_id in commit.parent_ids() {
                    let connection = self.find_next_referenced_commit(
                        &parent_id.to_string(),
                        condensed_graph,
                        referenced_commits,
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

    fn find_next_referenced_commit(
        &self,
        start_commit_id: &str,
        condensed_graph: &HashMap<String, CommitNode>,
        _referenced_commits: &HashMap<String, CommitNode>,
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

            // Otherwise, traverse parents to find the next referenced commit
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
