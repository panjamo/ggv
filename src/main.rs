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
    message: String,
    timestamp: i64,
    tags: BTreeSet<String>,
    is_tip: bool,
    parents: Vec<String>,
}

impl CommitNode {
    fn new(commit: &git2::Commit) -> Self {
        let id = commit.id().to_string();
        let _short_id = format!("{:.7}", id);
        let message = commit.message().unwrap_or("").to_string();
        let timestamp = commit.time().seconds();

        let parents: Vec<String> = commit.parent_ids().map(|oid| oid.to_string()).collect();

        Self {
            id,
            _short_id,
            message,
            timestamp,
            tags: BTreeSet::new(),
            is_tip: false,
            parents,
        }
    }

    fn add_tag(&mut self, tag: String) {
        self.tags.insert(tag);
    }

    fn set_tip(&mut self, is_tip: bool) {
        self.is_tip = is_tip;
    }

    fn get_dot_node(&self) -> String {
        let mut label = self.format_commit_message();

        if !self.tags.is_empty() {
            let tags_str = self
                .tags
                .iter()
                .map(|t| t.trim_start_matches("refs/tags/"))
                .collect::<Vec<_>>()
                .join(", ");
            label = format!("{} [{}]", label, tags_str);
        }

        if self.is_tip {
            label = format!("{} {{TIP}}", label);
        }

        format!(
            "\"{}\" [label=\"{}\", shape=box, style=filled, color=black, fillcolor=white]",
            self.id, label
        )
    }

    fn format_commit_message(&self) -> String {
        self.message
            .lines()
            .next()
            .unwrap_or(&self.message)
            .replace('"', "'")
            .replace('\\', "/")
            .trim()
            .to_string()
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
    special_branches: Vec<String>,
    filter: RefFilter,
}

impl GitGraphviz {
    fn new(repo_path: &str, filter: RefFilter) -> Result<Self> {
        let repo = Repository::open(repo_path)
            .with_context(|| format!("Failed to open repository at: {}", repo_path))?;

        let special_branches = vec![
            "refs/heads/master".to_string(),
            "refs/heads/main".to_string(),
            "refs/heads/integration".to_string(),
        ];

        Ok(Self {
            repo,
            special_branches,
            filter,
        })
    }

    fn generate_dot(&self, output_path: &str) -> Result<()> {
        let file = File::create(output_path)
            .with_context(|| format!("Failed to create output file: {}", output_path))?;
        let mut writer = BufWriter::new(file);

        let mut all_commits: HashMap<String, CommitNode> = HashMap::new();
        let mut branch_tips: HashMap<String, String> = HashMap::new();

        // Walk local branches if enabled
        if self.filter.should_include_branches() {
            let branches = self.repo.branches(Some(BranchType::Local))?;
            for branch_result in branches {
                let (branch, _) = branch_result?;
                let branch_name = branch.name()?.unwrap_or("unknown").to_string();
                let ref_name = format!("refs/heads/{}", branch_name);

                if let Some(oid) = branch.get().target() {
                    // Walk commit history from this branch tip
                    self.add_commit_history(&mut all_commits, oid, 10)?;
                    let commit_id = oid.to_string();
                    branch_tips.insert(ref_name, commit_id);
                }
            }
        }

        // Walk remote branches if enabled
        if self.filter.should_include_remotes() {
            let remote_branches = self.repo.branches(Some(BranchType::Remote))?;
            for branch_result in remote_branches {
                let (branch, _) = branch_result?;
                let branch_name = branch.name()?.unwrap_or("unknown").to_string();
                let ref_name = format!("refs/remotes/{}", branch_name);

                if let Some(oid) = branch.get().target() {
                    // Walk commit history from this branch tip
                    self.add_commit_history(&mut all_commits, oid, 10)?;
                    let commit_id = oid.to_string();
                    branch_tips.insert(ref_name, commit_id);
                }
            }
        }

        // Add HEAD if enabled
        if self.filter.should_include_head() {
            if let Ok(head) = self.repo.head() {
                if let Some(oid) = head.target() {
                    // Walk commit history from HEAD
                    self.add_commit_history(&mut all_commits, oid, 10)?;
                    let commit_id = oid.to_string();
                    branch_tips.insert("HEAD".to_string(), commit_id);
                }
            }
        }

        // Add tagged commits if enabled
        if self.filter.should_include_tags() {
            self.add_tagged_commits(&mut all_commits)?;
        }

        // Mark tips
        for tip_id in branch_tips.values() {
            if let Some(commit) = all_commits.get_mut(tip_id) {
                commit.set_tip(true);
            }
        }

        // Write DOT file
        writeln!(writer, "digraph git {{")?;
        writeln!(writer, "  rankdir=TB;")?;
        writeln!(writer, "  node [fontsize=10];")?;

        // Handle special branches first
        for special_branch in &self.special_branches {
            if let Some(tip_id) = branch_tips.get(special_branch) {
                self.write_subgraph(&mut writer, special_branch, tip_id, &all_commits)?;
            }
        }

        // Handle remaining branches
        for (branch_name, tip_id) in &branch_tips {
            if !self.special_branches.contains(branch_name) {
                self.write_subgraph(&mut writer, branch_name, tip_id, &all_commits)?;
            }
        }

        // Write all commit nodes
        for commit in all_commits.values() {
            writeln!(writer, "  {}", commit.get_dot_node())?;
        }

        // Write edges between commits and their parents (only if both commits are in our set)
        for commit in all_commits.values() {
            for parent_id in &commit.parents {
                if all_commits.contains_key(parent_id) {
                    writeln!(writer, "  \"{}\" -> \"{}\"", parent_id, commit.id)?;
                }
            }
        }

        writeln!(writer, "}}")?;
        writer.flush()?;

        println!("Generated DOT file: {}", output_path);
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

    fn add_commit_history(
        &self,
        all_commits: &mut HashMap<String, CommitNode>,
        start_oid: Oid,
        max_depth: usize,
    ) -> Result<()> {
        let mut to_visit = Vec::new();
        let mut visited = std::collections::HashSet::new();

        to_visit.push((start_oid, 0));

        while let Some((oid, depth)) = to_visit.pop() {
            if depth >= max_depth || visited.contains(&oid) {
                continue;
            }

            visited.insert(oid);

            // Add this commit
            let oid_str = oid.to_string();
            if let std::collections::hash_map::Entry::Vacant(e) = all_commits.entry(oid_str.clone())
            {
                let commit = self.repo.find_commit(oid)?;
                let commit_node = CommitNode::new(&commit);
                e.insert(commit_node);
            }

            // Add parents to visit list
            if let Ok(commit) = self.repo.find_commit(oid) {
                for parent_id in commit.parent_ids() {
                    to_visit.push((parent_id, depth + 1));
                }
            }
        }

        Ok(())
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

    fn write_subgraph(
        &self,
        writer: &mut BufWriter<File>,
        branch_name: &str,
        tip_id: &str,
        _all_commits: &HashMap<String, CommitNode>,
    ) -> Result<()> {
        let cluster_name = branch_name.replace(['/', '.', '-'], "_");
        writeln!(writer, "  subgraph cluster_{} {{", cluster_name)?;
        writeln!(writer, "    label=\"{}\";", branch_name)?;
        writeln!(writer, "    color=blue; style=dotted;")?;

        // This is a simplified version - in a full implementation,
        // we'd need to determine which commits belong to this branch
        writeln!(writer, "    \"{}\"", tip_id)?;

        writeln!(writer, "  }}")?;
        Ok(())
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
