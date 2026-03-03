mod args;
mod commit_node;
mod filter;
mod graph;
mod graphviz;
mod utils;

use anyhow::Result;
use args::Args;
use clap::Parser;
use filter::RefFilter;
use graph::GitGraphviz;
use graphviz::{generate_svg, open_file};
use utils::repo_name_from_path;

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
        open_file(&svg_path)?;
    }

    Ok(())
}
