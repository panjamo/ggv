use anyhow::{Context, Result};
use git2::{BranchType, Oid, Repository};
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufWriter, Write};

use crate::commit_node::CommitNode;
use crate::filter::RefFilter;
use crate::utils::time_ago;

pub struct GitGraphviz {
    repo: Repository,
    filter: RefFilter,
    gitlab_base_url: Option<String>,
    ancestor_oid: Option<Oid>,
}

impl GitGraphviz {
    pub fn new(
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
        if let Some(rest) = url.strip_prefix("git@") {
            if let Some((host, path)) = rest.split_once(':') {
                let path = path.trim_end_matches(".git");
                return Some(format!("https://{}/{}", host, path));
            }
        }
        if url.starts_with("https://") || url.starts_with("http://") {
            let trimmed = url.trim_end_matches(".git");
            return Some(trimmed.to_string());
        }
        None
    }

    fn detect_gitlab_url(repo: &Repository) -> Option<String> {
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

    pub fn generate_dot(&self, output_path: &str) -> Result<()> {
        let file = File::create(output_path)
            .with_context(|| format!("Failed to create output file: {}", output_path))?;
        let mut writer = BufWriter::new(file);

        let mut referenced_commits: HashMap<String, CommitNode> = HashMap::new();
        let mut branch_tips: HashMap<String, String> = HashMap::new();

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

        let mut current_checkout_id: Option<String> = None;
        if let Ok(head) = self.repo.head() {
            if let Some(oid) = head.target() {
                let commit_id = self.add_ref_commit(&mut referenced_commits, oid)?;
                current_checkout_id = Some(commit_id.clone());

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

        if let Some(ancestor_oid) = self.ancestor_oid {
            let ancestor_id_str = ancestor_oid.to_string();

            let commit_id = self.add_ref_commit(&mut referenced_commits, ancestor_oid)?;
            if let Some(node) = referenced_commits.get_mut(&commit_id) {
                node.add_ref("ROOT".to_string());
            }

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
            self.add_root_commits(&mut referenced_commits)?;
        }

        self.add_branch_readmes(&mut referenced_commits, &branch_tips)?;

        for tip_id in branch_tips.values() {
            if let Some(commit) = referenced_commits.get_mut(tip_id) {
                commit.set_tip(true);
            }
        }

        if let Some(checkout_id) = current_checkout_id {
            if let Some(commit) = referenced_commits.get_mut(&checkout_id) {
                commit.set_current_checkout(true);
            }
        }

        let condensed_graph = self.build_condensed_graph(&referenced_commits)?;

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

        writeln!(writer, "digraph git {{")?;
        writeln!(writer, "  rankdir=BT;")?;
        writeln!(writer, "  bgcolor=\"#f8f9fa\";")?;
        writeln!(writer, "  node [fontname=\"Arial Bold\", fontsize=10];")?;
        writeln!(
            writer,
            "  edge [color=\"#495057\", penwidth=2, arrowsize=0.8, arrowhead=vee];"
        )?;
        writeln!(writer, "  graph [splines=ortho, nodesep=0.3, ranksep=0.4];")?;

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
        let mut tag_commits = Vec::new();

        self.repo.tag_foreach(|oid, name| {
            let tag_name = String::from_utf8_lossy(name).to_string();

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
        let mut additional_commits = HashMap::new();

        for commit_id in referenced_commits.keys() {
            self.find_connection_path(commit_id, referenced_commits, &mut additional_commits)?;
        }

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
        let mut visited = HashSet::new();
        let mut to_visit = Vec::new();
        let max_depth = 100;

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
        let mut visited = HashSet::new();
        let mut to_visit = Vec::new();
        let max_depth = 100;

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

            if referenced_commits.contains_key(&current_id) {
                return Ok(true);
            }

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
        let mut visited = HashSet::new();

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
        visited: &mut HashSet<String>,
    ) -> Result<Option<String>> {
        let mut to_visit = Vec::new();
        to_visit.push(start_commit_id.to_string());

        while let Some(current_id) = to_visit.pop() {
            if visited.contains(&current_id) {
                continue;
            }
            visited.insert(current_id.clone());

            if condensed_graph.contains_key(&current_id) {
                return Ok(Some(current_id));
            }

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
