use clap::Parser;

use crate::theme::Theme;

#[derive(Parser)]
#[command(name = "ggv")]
#[command(about = "Git Graph Visualizer - Generate Graphviz DOT files from Git repositories")]
pub struct Args {
    #[arg(short, long, help = "Path to Git repository", default_value = ".")]
    pub repo_path: String,

    #[arg(
        short,
        long,
        help = "Output DOT file path (default: ggv-<repo-name>.dot)"
    )]
    pub output: Option<String>,

    #[arg(long, help = "Skip SVG generation and opening", action = clap::ArgAction::SetTrue)]
    pub no_show: bool,

    #[arg(
        short,
        long,
        help = "Filter git refs by type (b=branches, r=remotes, t=tags, h=head)",
        default_value = "brt"
    )]
    pub filter: String,

    #[arg(
        long,
        help = "GitLab base URL for clickable commit links (e.g. https://gitlab.com/namespace/project). Auto-detected from remote if not specified."
    )]
    pub gitlab_url: Option<String>,

    #[arg(
        long,
        help = "Limit graph to this commit and its descendants (accepts commit hash, branch, or tag)"
    )]
    pub from: Option<String>,

    #[arg(
        long,
        help = "Skip automatic 'git fetch --tags' before generating the graph",
        action = clap::ArgAction::SetTrue
    )]
    pub no_fetch: bool,

    #[arg(
        long,
        help = "Keep the intermediate DOT file after SVG generation",
        action = clap::ArgAction::SetTrue
    )]
    pub keep_dot: bool,

    #[arg(
        long,
        help = "Color theme: dark (default) or light",
        default_value_t = Theme::Dark,
        value_enum
    )]
    pub theme: Theme,
}
