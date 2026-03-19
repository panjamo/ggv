use anyhow::{Context, Result};
use git2::{BranchType, Oid, Repository};
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufWriter, Write};

use crate::commit_node::CommitNode;
use crate::filter::RefFilter;
use crate::theme::Theme;
use crate::utils::{repo_name_from_path, time_ago};

type EdgeAttrs = HashMap<
    (String, String),
    (
        Option<String>,
        Option<String>,
        usize,
        Option<String>,
        usize,
        usize,
    ),
>;

pub struct GitGraphviz {
    repo: Repository,
    filter: RefFilter,
    gitlab_base_url: Option<String>,
    ancestor_oid: Option<Oid>,
    theme: Theme,
    current_branch_only: bool,
    limit: usize,
}

impl GitGraphviz {
    pub fn new(
        repo_path: &str,
        filter: RefFilter,
        gitlab_url: Option<String>,
        from_commit: Option<String>,
        theme: Theme,
        current_branch_only: bool,
        limit: usize,
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
            theme,
            current_branch_only,
            limit,
        })
    }

    pub fn forge_url(&self) -> Option<&str> {
        self.gitlab_base_url.as_deref()
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

    /// Single revwalk that collects the first `max` commits AND counts the total.
    fn collect_path_commits_with_count(
        &self,
        from_id: &str,
        stop_id: Option<&str>,
        max: usize,
    ) -> (Vec<(String, String, String, String)>, usize) {
        let mut sample = Vec::new();
        let mut total: usize = 0;

        let Ok(oid) = from_id.parse::<Oid>() else {
            return (sample, total);
        };
        let Ok(mut revwalk) = self.repo.revwalk() else {
            return (sample, total);
        };
        if revwalk.push(oid).is_err() {
            return (sample, total);
        }
        if let Some(stop) = stop_id {
            if let Ok(stop_oid) = stop.parse::<Oid>() {
                let _ = revwalk.hide(stop_oid);
            }
        }

        // Cap the walk: once we have our samples we only need a rough count,
        // not an exact one for every commit in a potentially huge range.
        const MAX_WALK: usize = 2_000;
        for oid_result in revwalk {
            let Ok(oid) = oid_result else { break };
            total += 1;
            if total > MAX_WALK {
                break;
            }
            if sample.len() < max {
                let Ok(commit) = self.repo.find_commit(oid) else {
                    continue;
                };
                let id_str = oid.to_string();
                let short_id = format!("{:.7}", id_str);
                let message = commit.summary().unwrap_or("").to_string();
                let author = String::from_utf8_lossy(commit.author().name_bytes()).to_string();
                let when = time_ago(commit.time().seconds());
                sample.push((short_id, message, author, when));
            }
        }

        if total > max {
            let label = if total > MAX_WALK {
                format!("({}+ commits, truncated)", MAX_WALK)
            } else {
                "(truncated)".to_string()
            };
            sample.push(("...".to_string(), label, String::new(), String::new()));
        }

        (sample, total)
    }

    pub fn generate_dot(&self, output_path: &str, splines_arg: &str) -> Result<()> {
        let mut referenced_commits: HashMap<String, CommitNode> = HashMap::new();
        let mut branch_tips: HashMap<String, String> = HashMap::new();

        // Resolve HEAD OID once for --current-branch filtering
        let head_oid: Option<Oid> = self.repo.head().ok().and_then(|h| h.target());

        // Returns true if oid is an ancestor of HEAD (or IS HEAD), used when --current-branch is set
        let is_on_current_branch = |oid: Oid| -> bool {
            match head_oid {
                None => true,
                Some(head) => {
                    oid == head || self.repo.graph_descendant_of(head, oid).unwrap_or(false)
                }
            }
        };

        let mut current_checkout_id: Option<String> = None;

        if self.limit > 0 {
            // FAST PATH: one time-sorted revwalk to build the commit universe, then label
            // only the refs whose tip falls within it. Skips root/merge-base walks entirely.
            let mut revwalk = self.repo.revwalk()?;
            revwalk.set_sorting(git2::Sort::TIME)?;

            if self.filter.should_include_branches() {
                let _ = revwalk.push_glob("refs/heads/*");
            }
            if self.filter.should_include_remotes() {
                let _ = revwalk.push_glob("refs/remotes/*");
            }
            // Always seed from HEAD so detached-HEAD repos are covered
            if let Some(oid) = head_oid {
                let _ = revwalk.push(oid);
            }
            if self.filter.should_include_stashes() {
                if let Ok(reflog) = self.repo.reflog("refs/stash") {
                    for entry in reflog.iter() {
                        let _ = revwalk.push(entry.id_new());
                    }
                }
            }

            let mut universe: HashSet<String> = HashSet::new();
            for oid_result in &mut revwalk {
                if universe.len() >= self.limit {
                    break;
                }
                let Ok(oid) = oid_result else { continue };
                universe.insert(oid.to_string());
            }

            // Label commits in the universe with their branch refs
            if self.filter.should_include_branches() {
                for branch_result in self.repo.branches(Some(BranchType::Local))? {
                    let (branch, _) = branch_result?;
                    let branch_name = match branch.name() {
                        Ok(Some(n)) => n.to_string(),
                        Ok(None) => "unknown".to_string(),
                        Err(_) => continue,
                    };
                    let ref_name = format!("refs/heads/{}", branch_name);
                    if let Some(oid) = branch.get().target() {
                        if self.current_branch_only && !is_on_current_branch(oid) {
                            continue;
                        }
                        if universe.contains(&oid.to_string()) {
                            let commit_id = self.add_ref_commit(&mut referenced_commits, oid)?;
                            if let Some(n) = referenced_commits.get_mut(&commit_id) {
                                n.add_ref(ref_name.clone());
                            }
                            branch_tips.insert(ref_name, commit_id);
                        }
                    }
                }
            }

            if self.filter.should_include_remotes() {
                for branch_result in self.repo.branches(Some(BranchType::Remote))? {
                    let (branch, _) = branch_result?;
                    let branch_name = match branch.name() {
                        Ok(Some(n)) => n.to_string(),
                        Ok(None) => "unknown".to_string(),
                        Err(_) => continue,
                    };
                    let ref_name = format!("refs/remotes/{}", branch_name);
                    if let Some(oid) = branch.get().target() {
                        if self.current_branch_only && !is_on_current_branch(oid) {
                            continue;
                        }
                        if universe.contains(&oid.to_string()) {
                            let commit_id = self.add_ref_commit(&mut referenced_commits, oid)?;
                            if let Some(n) = referenced_commits.get_mut(&commit_id) {
                                n.add_ref(ref_name.clone());
                            }
                            branch_tips.insert(ref_name, commit_id);
                        }
                    }
                }
            }

            if let Some(oid) = head_oid {
                if universe.contains(&oid.to_string()) {
                    let commit_id = self.add_ref_commit(&mut referenced_commits, oid)?;
                    current_checkout_id = Some(commit_id.clone());
                    if self.filter.should_include_head() {
                        if let Some(n) = referenced_commits.get_mut(&commit_id) {
                            n.add_ref("HEAD".to_string());
                        }
                        branch_tips.insert("HEAD".to_string(), commit_id);
                    }
                }
            }

            if self.filter.should_include_stashes() {
                if let Ok(reflog) = self.repo.reflog("refs/stash") {
                    for (index, entry) in reflog.iter().enumerate() {
                        let oid = entry.id_new();
                        if universe.contains(&oid.to_string()) {
                            let commit_id = self.add_ref_commit(&mut referenced_commits, oid)?;
                            let ref_name = format!("stash@{{{}}}", index);
                            if let Some(n) = referenced_commits.get_mut(&commit_id) {
                                n.add_ref(ref_name.clone());
                                n.is_stash = true;
                            }
                            branch_tips.insert(ref_name, commit_id);
                        }
                    }
                }
            }

            if self.filter.should_include_tags() {
                let mut tag_commits: Vec<(Oid, String)> = Vec::new();
                self.repo.tag_foreach(|oid, name| {
                    let tag_name = String::from_utf8_lossy(name).to_string();
                    if let Ok(obj) = self.repo.find_object(oid, None) {
                        if let Ok(commit_obj) = obj.peel(git2::ObjectType::Commit) {
                            if universe.contains(&commit_obj.id().to_string()) {
                                tag_commits.push((commit_obj.id(), tag_name));
                            }
                        }
                    }
                    true
                })?;
                for (commit_oid, tag_name) in tag_commits {
                    let commit_id = self.add_ref_commit(&mut referenced_commits, commit_oid)?;
                    if let Some(n) = referenced_commits.get_mut(&commit_id) {
                        n.add_tag(tag_name);
                    }
                }
            }

            eprintln!("Applied limit: showing {} most recent commits", self.limit);
        } else {
            // NORMAL PATH: enumerate all refs, then add roots and merge bases

            if self.filter.should_include_branches() {
                let branches = self.repo.branches(Some(BranchType::Local))?;
                for branch_result in branches {
                    let (branch, _) = branch_result?;
                    let branch_name = match branch.name() {
                        Ok(Some(name)) => name.to_string(),
                        Ok(None) => "unknown".to_string(),
                        Err(_) => continue,
                    };
                    let ref_name = format!("refs/heads/{}", branch_name);
                    if let Some(oid) = branch.get().target() {
                        if self.current_branch_only && !is_on_current_branch(oid) {
                            continue;
                        }
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
                    let branch_name = match branch.name() {
                        Ok(Some(name)) => name.to_string(),
                        Ok(None) => "unknown".to_string(),
                        Err(_) => continue,
                    };
                    let ref_name = format!("refs/remotes/{}", branch_name);
                    if let Some(oid) = branch.get().target() {
                        if self.current_branch_only && !is_on_current_branch(oid) {
                            continue;
                        }
                        let commit_id = self.add_ref_commit(&mut referenced_commits, oid)?;
                        if let Some(commit_node) = referenced_commits.get_mut(&commit_id) {
                            commit_node.add_ref(ref_name.clone());
                        }
                        branch_tips.insert(ref_name, commit_id);
                    }
                }
            }

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

            if self.filter.should_include_stashes() {
                if let Ok(reflog) = self.repo.reflog("refs/stash") {
                    for (index, entry) in reflog.iter().enumerate() {
                        let oid = entry.id_new();
                        let commit_id = self.add_ref_commit(&mut referenced_commits, oid)?;
                        let ref_name = format!("stash@{{{}}}", index);
                        if let Some(commit_node) = referenced_commits.get_mut(&commit_id) {
                            commit_node.add_ref(ref_name.clone());
                            commit_node.is_stash = true;
                        }
                        branch_tips.insert(ref_name, commit_id);
                    }
                }
            }

            if self.filter.should_include_tags() {
                self.add_tagged_commits(&mut referenced_commits, head_oid)?;
            }

            if let Some(ancestor_oid) = self.ancestor_oid {
                self.add_merge_base_commits(&mut referenced_commits, &branch_tips)?;
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
                self.add_merge_base_commits(&mut referenced_commits, &branch_tips)?;
                self.add_root_commits(&mut referenced_commits)?;
            }
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
            let connections = self.find_condensed_connections(&commit.id, &condensed_graph)?;
            let mut seen = HashSet::new();
            let valid: Vec<String> = connections
                .into_iter()
                .filter(|id| condensed_graph.contains_key(id) && seen.insert(id.clone()))
                .collect();
            commit_parents.insert(commit.id.clone(), valid);
        }

        // Calculate graph statistics
        let node_count = condensed_graph.len();
        let edge_count: usize = commit_parents.values().map(|v| v.len()).sum();

        eprintln!("Graph stats: {} nodes, {} edges", node_count, edge_count);

        // Determine splines mode: auto (based on edge count) or explicit value
        let splines_mode = if splines_arg == "auto" {
            if edge_count > 1200 {
                eprintln!("Auto mode: using 'polyline' splines (edge count > 1200)");
                "polyline"
            } else {
                eprintln!("Auto mode: using 'ortho' splines (edge count <= 1200)");
                "ortho"
            }
        } else {
            eprintln!("Using explicit splines mode: '{}'", splines_arg);
            splines_arg
        };

        let file = File::create(output_path)
            .with_context(|| format!("Failed to create output file: {}", output_path))?;
        let mut writer = BufWriter::new(file);

        let tc = self.theme.colors();
        writeln!(writer, "digraph git {{")?;
        writeln!(writer, "  rankdir=BT;")?;
        writeln!(writer, "  bgcolor=\"{}\";", tc.bg)?;
        writeln!(
            writer,
            "  node [fontname=\"Arial\", fontsize=9, fontcolor=\"{}\", fillcolor=\"{}\", color=\"{}\", style=filled];",
            tc.node_default_font, tc.node_default_fill, tc.node_default_border
        )?;
        writeln!(
            writer,
            "  edge [color=\"{}\", arrowhead=none, dir=none, fontsize=8, fontname=\"Arial\", fontcolor=\"#94A3B8\"];",
            tc.edge_color
        )?;
        let graph_tooltip = build_graph_tooltip(&self.repo)
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n");
        writeln!(
            writer,
            "  graph [splines={}, nodesep=0.4, ranksep=0.5, pad=\"0.5,0.5\", mclimit=5.0, tooltip=\"{}\"];",
            splines_mode, graph_tooltip
        )?;

        // Write all nodes
        for commit in condensed_graph.values() {
            writeln!(writer, "  {}", commit.get_dot_node(self.theme))?;
        }

        // Collect all (parent_id, child_id) pairs that need edge attributes
        let mut pairs_ordered: Vec<(String, String)> = Vec::new();
        for commit in condensed_graph.values() {
            let is_ancestor_root = self
                .ancestor_oid
                .is_some_and(|a| a.to_string() == commit.id);
            if is_ancestor_root {
                continue;
            }
            let parents = commit_parents.get(&commit.id).cloned().unwrap_or_default();
            for pid in parents {
                pairs_ordered.push((pid, commit.id.clone()));
            }
        }

        // Compute file lists and changed-line counts for all edges via git2
        let batch_files = batch_diff_file_lists(&self.repo, &pairs_ordered);

        // Compute max file_count across all edges for heatmap normalization
        let max_file_count: usize = batch_files
            .values()
            .map(|(v, _)| v.len())
            .max()
            .unwrap_or(1)
            .max(1);

        // Build edge attributes: (parent_id, child_id) -> (url, tooltip, count)
        let mut edge_attrs: EdgeAttrs = HashMap::new();
        for (pid, child_id) in &pairs_ordered {
            let commit = match condensed_graph.get(child_id) {
                Some(c) => c,
                None => continue,
            };
            let url = self.gitlab_base_url.as_deref().map(|base| {
                let is_github = base.contains("github.com");
                let from_ref = if is_github {
                    condensed_graph
                        .get(pid)
                        .map(|c| c.id.as_str())
                        .unwrap_or(pid.as_str())
                } else {
                    condensed_graph
                        .get(pid)
                        .map(|c| c.best_ref_for_url())
                        .unwrap_or(pid.as_str())
                };
                let to_ref = if is_github {
                    commit.id.as_str()
                } else {
                    commit.best_ref_for_url()
                };
                let compare_segment = if is_github {
                    "/compare/"
                } else {
                    "/-/compare/"
                };
                format!(
                    "{}{}{}...{}",
                    base,
                    compare_segment,
                    url_encode_ref(from_ref),
                    url_encode_ref(to_ref)
                )
            });
            let (path_commits, count) =
                self.collect_path_commits_with_count(child_id, Some(pid.as_str()), 20);
            let tooltip = build_tooltip(&path_commits);
            let (files, lines, file_count) = batch_files
                .get(&(pid.clone(), child_id.clone()))
                .map(|(v, total_lines)| {
                    let total = v.len();
                    const MAX: usize = 30;
                    let list = if total == 0 {
                        None
                    } else if total > MAX {
                        let mut truncated = v[..MAX].join("|");
                        truncated.push_str(&format!("|... +{} more", total - MAX));
                        Some(truncated)
                    } else {
                        Some(v.join("|"))
                    };
                    (list, *total_lines, v.len())
                })
                .unwrap_or((None, 0, 0));
            edge_attrs.insert(
                (pid.clone(), child_id.clone()),
                (url, tooltip, count, files, lines, file_count),
            );
        }

        for (child_id, parents) in &commit_parents {
            for parent_id in parents {
                let (url, tooltip, count, files, lines, file_count) = edge_attrs
                    .get(&(parent_id.clone(), child_id.clone()))
                    .map(|(u, t, c, f, l, fc)| {
                        (u.as_deref(), t.as_deref(), *c, f.as_deref(), *l, *fc)
                    })
                    .unwrap_or((None, None, 0, None, 0, 0));
                let attrs = build_edge_attrs(
                    url,
                    tooltip,
                    count,
                    files,
                    lines,
                    file_count,
                    max_file_count,
                );
                writeln!(writer, "  \"{}\" -> \"{}\"{}", parent_id, child_id, attrs)?;
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

    fn add_tagged_commits(
        &self,
        all_commits: &mut HashMap<String, CommitNode>,
        head_oid: Option<Oid>,
    ) -> Result<()> {
        let mut tag_commits = Vec::new();

        self.repo.tag_foreach(|oid, name| {
            let tag_name = String::from_utf8_lossy(name).to_string();

            if let Ok(tag_target) = self.repo.find_object(oid, None) {
                if let Ok(commit_obj) = tag_target.peel(git2::ObjectType::Commit) {
                    tag_commits.push((commit_obj.id(), tag_name));
                }
            }
            true
        })?;

        for (commit_oid, tag_name) in tag_commits {
            if self.current_branch_only {
                let on_branch = match head_oid {
                    None => true,
                    Some(head) => {
                        commit_oid == head
                            || self
                                .repo
                                .graph_descendant_of(head, commit_oid)
                                .unwrap_or(false)
                    }
                };
                if !on_branch {
                    continue;
                }
            }
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
                                        let description: String =
                                            content.lines().take(10).collect::<Vec<_>>().join("\n");
                                        let trimmed = description.trim().to_string();
                                        if !trimmed.is_empty() {
                                            if let Some(commit_node) =
                                                all_commits.get_mut(commit_id)
                                            {
                                                commit_node.set_branch_readme(trimmed);
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

    fn add_merge_base_commits(
        &self,
        all_commits: &mut HashMap<String, CommitNode>,
        branch_tips: &HashMap<String, String>,
    ) -> Result<()> {
        let tip_oids: Vec<Oid> = {
            let mut seen = HashSet::new();
            branch_tips
                .values()
                .filter_map(|id| id.parse::<Oid>().ok())
                .filter(|oid| seen.insert(*oid))
                .collect()
        };

        for i in 0..tip_oids.len() {
            for j in (i + 1)..tip_oids.len() {
                if let Ok(base_oid) = self.repo.merge_base(tip_oids[i], tip_oids[j]) {
                    self.add_ref_commit(all_commits, base_oid)?;
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

                let is_stash = referenced_commits
                    .get(&current_id)
                    .map(|n| n.is_stash)
                    .unwrap_or(false);
                for (i, parent_id) in commit.parent_ids().enumerate() {
                    // Skip internal git-stash parents (index, untracked) — only follow parent 0
                    if is_stash && i > 0 {
                        continue;
                    }
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
    ) -> Result<Vec<String>> {
        let mut connections = Vec::new();

        if let Ok(commit_oid) = commit_id.parse::<Oid>() {
            if let Ok(commit) = self.repo.find_commit(commit_oid) {
                let is_stash = condensed_graph
                    .get(commit_id)
                    .map(|n| n.is_stash)
                    .unwrap_or(false);
                for (i, parent_id) in commit.parent_ids().enumerate() {
                    // Skip internal git-stash parents (index, untracked) — only follow parent 0
                    if is_stash && i > 0 {
                        continue;
                    }
                    let mut visited = HashSet::new();
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

fn url_encode_ref(r: &str) -> String {
    let mut out = String::with_capacity(r.len());
    for b in r.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => {
                out.push('%');
                out.push_str(&format!("{:02X}", b));
            }
        }
    }
    out
}

fn build_graph_tooltip(repo: &Repository) -> String {
    let repo_path = repo
        .workdir()
        .and_then(|p| p.to_str())
        .unwrap_or("")
        .trim_end_matches(['/', '\\']);
    let repo_name = repo_name_from_path(repo_path);

    let mut lines = vec![format!("Repository: {}", repo_name)];

    if let Ok(head) = repo.head() {
        let branch = if head.is_branch() {
            head.shorthand().unwrap_or("unknown").to_string()
        } else {
            "detached HEAD".to_string()
        };
        lines.push(format!("Branch: {}", branch));

        if let Some(oid) = head.target() {
            if let Ok(commit) = repo.find_commit(oid) {
                let short_id = oid.to_string()[..7].to_string();
                let msg = commit.summary().unwrap_or("").trim().to_string();
                let when = time_ago(commit.time().seconds());
                lines.push(format!("Commit:  {} {}", short_id, msg));
                // Skip author display to avoid potential UTF-8 panics from git2
                lines.push(format!("Date:    {}", when));
            }
        }
    }

    lines.join("\n")
}

fn build_tooltip(path_commits: &[(String, String, String, String)]) -> Option<String> {
    if path_commits.is_empty() {
        return None;
    }
    Some(
        path_commits
            .iter()
            .map(|(hash, msg, author, when)| format!("{}: {} ({}, {})", hash, msg, author, when))
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

/// Compute file list and changed-line count between two commits using git2.
/// Returns a map from (from_sha, to_sha) → (Vec<file_path>, total_changed_lines).
fn batch_diff_file_lists(
    repo: &Repository,
    pairs: &[(String, String)],
) -> HashMap<(String, String), (Vec<String>, usize)> {
    pairs
        .iter()
        .map(|(from_sha, to_sha)| {
            let stats = diff_tree_stats(repo, from_sha, to_sha).unwrap_or((Vec::new(), 0));
            ((from_sha.clone(), to_sha.clone()), stats)
        })
        .collect()
}

fn diff_tree_stats(
    repo: &Repository,
    from_sha: &str,
    to_sha: &str,
) -> Option<(Vec<String>, usize)> {
    let from_tree = repo
        .find_commit(Oid::from_str(from_sha).ok()?)
        .ok()?
        .tree()
        .ok()?;
    let to_tree = repo
        .find_commit(Oid::from_str(to_sha).ok()?)
        .ok()?
        .tree()
        .ok()?;
    let diff = repo
        .diff_tree_to_tree(Some(&from_tree), Some(&to_tree), None)
        .ok()?;
    let stats = diff.stats().ok()?;
    let total_lines = stats.insertions() + stats.deletions();
    let files: Vec<String> = diff
        .deltas()
        .filter_map(|d| {
            d.new_file()
                .path_bytes()
                .or_else(|| d.old_file().path_bytes())
                .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
        })
        .collect();
    Some((files, total_lines))
}

fn edge_penwidth(line_count: usize) -> f32 {
    if line_count == 0 {
        return 1.0;
    }
    let pw = 0.5 + (line_count as f64 + 1.0).log10() as f32 * 1.2;
    pw.clamp(0.5, 8.0)
}

fn edge_heatmap_color(file_count: usize, max_file_count: usize) -> String {
    if max_file_count == 0 || file_count == 0 {
        return "#B0B0B0".to_string(); // light grey
    }
    // Logarithmic ratio: small counts quickly move away from grey
    let ratio = ((file_count as f64 + 1.0).log10() / (max_file_count as f64 + 1.0).log10())
        .clamp(0.0, 1.0);
    // Interpolate: grey (#B0B0B0) -> orange (#FF8C00) -> red (#CC0000)
    let (r, g, b) = if ratio < 0.5 {
        let t = ratio * 2.0;
        let r = (0xB0 as f64 + t * (0xFF - 0xB0) as f64).round() as u8;
        let g = (0xB0 as f64 + t * (0x8C as f64 - 0xB0 as f64)).round() as u8;
        let b = (0xB0 as f64 + t * (0x00 as f64 - 0xB0 as f64)).round() as u8;
        (r, g, b)
    } else {
        let t = (ratio - 0.5) * 2.0;
        let r = (0xFF as f64 + t * (0xCC as f64 - 0xFF as f64)).round() as u8;
        let g = (0x8C as f64 + t * (0x00 as f64 - 0x8C as f64)).round() as u8;
        let b = 0u8;
        (r, g, b)
    };
    format!("#{:02X}{:02X}{:02X}", r, g, b)
}

fn build_edge_attrs(
    url: Option<&str>,
    tooltip: Option<&str>,
    count: usize,
    files: Option<&str>,
    lines: usize,
    file_count: usize,
    max_file_count: usize,
) -> String {
    let url_part = url.map_or(String::new(), |u| {
        format!("URL=\"{}\", target=\"_blank\"", u)
    });
    let tooltip_part = tooltip.map_or(String::new(), |t| {
        let escaped = t
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n");
        format!("tooltip=\"{}\"", escaped)
    });
    let label_part = if count > 0 {
        format!("xlabel=\"{}\"", count)
    } else {
        String::new()
    };
    let id_part = files.map_or(String::new(), |f| {
        let escaped = f.replace('\\', "\\\\").replace('"', "\\\"");
        format!("id=\"files:{}\"", escaped)
    });
    let penwidth_part = format!("penwidth={:.1}", edge_penwidth(lines));
    let color = edge_heatmap_color(file_count, max_file_count);
    let color_part = format!("color=\"{}\"", color);
    let parts: Vec<&str> = [
        &url_part,
        &tooltip_part,
        &label_part,
        &id_part,
        &penwidth_part,
        &color_part,
    ]
    .iter()
    .filter(|s| !s.is_empty())
    .map(|s| s.as_str())
    .collect();
    if parts.is_empty() {
        String::new()
    } else {
        format!(" [{}]", parts.join(", "))
    }
}
