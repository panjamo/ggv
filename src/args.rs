use clap::Parser;

use crate::theme::Theme;

#[derive(Parser)]
#[command(name = "ggv")]
#[command(version = concat!("0.1.", env!("GGV_COMMIT_COUNT")))]
#[command(about = concat!("Git Graph Visualizer v0.1.", env!("GGV_COMMIT_COUNT"), " - Generate Graphviz DOT files from Git repositories"))]
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
        help = "Color theme: dark or light [default: dark]",
        default_value_t = Theme::Dark,
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
        short = 's',
        long = "svg-only",
        help = "Generate a standalone SVG for sharing without starting the web server",
        action = clap::ArgAction::SetFalse,
        default_value_t = true
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
        short = 'p',
        long,
        help = "Custom prompt passed to gia via -c (overrides the built-in default prompt)"
    )]
    pub gia_prompt: Option<String>,

    #[arg(
        short = 'l',
        long,
        help = "Language locale for AI output (e.g. de-DE, en-US, fr-FR)",
        default_value = "de-DE"
    )]
    pub lang: String,

    #[arg(
        short = 'N',
        long,
        help = "Deactivate microphone audio recording in gia",
        action = clap::ArgAction::SetFalse,
        default_value_t = true
    )]
    pub gia_audio: bool,
}
