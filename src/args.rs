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

    #[arg(short = 'n', long, help = "Skip SVG generation and opening", action = clap::ArgAction::SetTrue)]
    pub no_show: bool,

    #[arg(
        short,
        long,
        help = "Filter git refs by type (b=branches, r=remotes, t=tags, h=head)",
        default_value = "brt"
    )]
    pub filter: String,

    #[arg(
        short = 'g',
        long,
        help = "GitLab base URL for clickable commit links (e.g. https://gitlab.com/namespace/project). Auto-detected from remote if not specified."
    )]
    pub gitlab_url: Option<String>,

    #[arg(
        short = 'F',
        long,
        help = "Limit graph to this commit and its descendants (accepts commit hash, branch, or tag)"
    )]
    pub from: Option<String>,

    #[arg(
        short = 'X',
        long,
        help = "Skip automatic 'git fetch --tags' before generating the graph",
        action = clap::ArgAction::SetTrue
    )]
    pub no_fetch: bool,

    #[arg(
        short = 'k',
        long,
        help = "Keep the intermediate DOT file after SVG generation",
        action = clap::ArgAction::SetTrue
    )]
    pub keep_dot: bool,

    #[arg(
        short = 't',
        long,
        help = "Color theme: dark or light",
        default_value_t = Theme::Light,
        value_enum
    )]
    pub theme: Theme,

    #[arg(
        short = 'c',
        long,
        help = "Show only refs that are on the current branch (ancestors of HEAD)",
        action = clap::ArgAction::SetTrue
    )]
    pub current_branch: bool,

    #[arg(
        short = 'w',
        long,
        help = "Start a local web server that runs 'git diff | gia' and shows a summary page when clicking edges",
        action = clap::ArgAction::SetTrue
    )]
    pub web_server: bool,

    #[arg(
        short = 'P',
        long,
        help = "Port for the local diff web server (0 = OS-assigned free port)",
        default_value_t = 0
    )]
    pub web_port: u16,

    #[arg(
        short = 'b',
        long,
        help = "Pass -b to gia so it opens its own browser window instead of returning text",
        action = clap::ArgAction::SetTrue
    )]
    pub gia_browser: bool,

    #[arg(
        short = 'p',
        long,
        help = "Custom prompt passed to gia via -c (overrides the built-in default prompt)"
    )]
    pub gia_prompt: Option<String>,
}
