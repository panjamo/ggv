mod args;
mod commit_node;
mod filter;
mod graph;
mod graphviz;
mod theme;
mod utils;
mod web_server;

use anyhow::{Context, Result};
use args::Args;
use clap::Parser;
use filter::RefFilter;
use graph::GitGraphviz;
use graphviz::open_file;
use utils::repo_name_from_path;

fn fetch_tags(repo_path: &str) -> Result<()> {
    let status = std::process::Command::new("git")
        .args(["-C", repo_path, "fetch", "--tags", "--prune"])
        .status()
        .context("Failed to run 'git fetch --tags --prune'")?;
    if !status.success() {
        eprintln!(
            "Warning: 'git fetch --tags --prune' exited with status {}",
            status
        );
    }
    Ok(())
}

fn main() -> Result<()> {
    let args = Args::parse();

    if !args.no_fetch {
        fetch_tags(&args.repo_path)?;
    }

    let repo_name = repo_name_from_path(&args.repo_path);
    let output = args.output.unwrap_or_else(|| match &args.from {
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
            format!("ggv-{}-from-{}.dot", &repo_name, safe_from)
        }
        None => format!("ggv-{}.dot", &repo_name),
    });

    // Create git_viz first so that auto-detected gitlab_url is available for the web server.
    let from_clone = args.from.clone();
    let filter = RefFilter::from_string(&args.filter);
    let git_viz = GitGraphviz::new(
        &args.repo_path,
        filter,
        args.gitlab_url,
        args.from,
        args.theme,
        args.current_branch,
    )?;

    git_viz.generate_dot(&output)?;

    let regen = Some(web_server::RegenerateConfig {
        repo_path: args.repo_path.clone(),
        dot_path: output.clone(),
        filter: args.filter.clone(),
        gitlab_url: git_viz.forge_url().map(String::from),
        from_commit: from_clone,
        theme: args.theme,
        current_branch_only: args.current_branch,
        no_fetch: args.no_fetch,
        web_server_url: String::new(), // filled in by start()
    });
    let (handle, port) = web_server::start(
        args.web_port,
        args.repo_path.clone(),
        output.clone(),
        args.gia_prompt,
        args.lang,
        args.gia_audio,
        args.theme,
        regen,
        args.max_diff_files,
    )
    .context("Failed to start web server")?;
    let ws_url = web_server::base_url(port);
    let target = if args.no_show {
        format!("{}/autosave", ws_url)
    } else {
        format!("{}/view", ws_url)
    };
    open_file(&target)?;
    handle.join().ok();
    Ok(())
}
