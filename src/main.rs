mod args;
mod commit_node;
mod filter;
mod graph;
mod graphviz;
mod theme;
mod utils;

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
        eprintln!("Warning: 'git fetch --tags --prune' exited with status {}", status);
    }
    Ok(())
}

fn main() -> Result<()> {
    let args = Args::parse();

    if !args.no_fetch {
        fetch_tags(&args.repo_path)?;
    }

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
    let git_viz = GitGraphviz::new(
        &args.repo_path,
        filter,
        args.gitlab_url,
        args.from,
        args.theme,
    )?;
    git_viz.generate_dot(&output)?;

    if !args.no_show {
        let svg_path = generate_svg(&output)?;
        if !args.keep_dot {
            std::fs::remove_file(&output)
                .with_context(|| format!("Failed to delete DOT file: {}", output))?;
        }
        open_file(&svg_path)?;
    }

    Ok(())
}
