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
use graphviz::{generate_svg, open_file};
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

    let svg_path = std::path::Path::new(&output)
        .with_extension("svg")
        .to_string_lossy()
        .to_string();

    let (server_handle, web_server_url) = if args.web_server {
        let regen = web_server::RegenerateConfig {
            repo_path: args.repo_path.clone(),
            dot_path: output.clone(),
            filter: args.filter.clone(),
            gitlab_url: args.gitlab_url.clone(),
            from_commit: args.from.clone(),
            theme: args.theme,
            current_branch_only: args.current_branch,
            no_fetch: args.no_fetch,
            keep_dot: args.keep_dot,
            web_server_url: String::new(), // filled in by start()
        };
        let (handle, port) = web_server::start(
            args.web_port,
            args.repo_path.clone(),
            svg_path.clone(),
            args.gia_prompt,
            args.lang,
            args.gia_audio,
            args.theme,
            Some(regen),
            args.max_diff_files,
        )
        .context("Failed to start diff web server")?;
        (Some(handle), Some(web_server::base_url(port)))
    } else {
        (None, None)
    };

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

    if !args.no_show {
        let generated_svg = generate_svg(
            &output,
            git_viz.forge_url(),
            web_server_url.as_deref(),
            &repo_name,
        )?;
        if !args.keep_dot {
            std::fs::remove_file(&output)
                .with_context(|| format!("Failed to delete DOT file: {}", output))?;
        }
        if let Some(ref ws_url) = web_server_url {
            open_file(&format!("{}/view", ws_url))?;
        } else {
            open_file(&generated_svg)?;
        }
    }

    if let Some(handle) = server_handle {
        handle.join().ok();
    }

    Ok(())
}
