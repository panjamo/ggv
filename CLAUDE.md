# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

GGV (Git Graph Visualizer) is a Rust CLI tool that generates Graphviz DOT files from Git repositories and renders them to SVG in-browser via WASM. The tool analyzes Git commit history, branches, and tags to create a condensed visual representation showing only referenced commits (branch tips, tags, root, merge junctions).

## Development Commands

### Build and Run
- `cargo build` - Build the project
- `cargo run` - Run with default options (generates SVG, opens it, deletes intermediate DOT file)
- `cargo run -- --help` - Show CLI help
- `cargo run -- --repo-path /path/to/repo --output custom.dot --no-show` - Run with custom options

### Code Quality
- `cargo clippy --fix --allow-dirty` - Run linting with automatic fixes (required for uncommitted changes)
- `cargo fmt` - Format code according to Rust standards
- `cargo check` - Fast compilation check without producing executable

### Development Workflow
Always run the complete quality check sequence:
1. `cargo clippy --fix --allow-dirty`
2. `cargo fmt`
3. `cargo build` - Verify no compilation errors

## Architecture

### Core Components

**Args Struct (`args.rs`)**
- CLI argument parsing using clap with derive macros
- Key options: `repo_path`, `output` (DOT file, default `ggv-<repo-name>.dot`), `no_show`, `filter`, `gitlab_url`
- `web_server: bool` — defaults to `true`; set to `false` via `-s` / `--svg-only` to skip the web server and generate a standalone SVG
- `max_diff_files: usize` — diff file limit for the web UI (default 100); if exceeded the diff view is suppressed and only commit cards are shown; `0` disables the limit
- `gia_audio: bool` — defaults to `false`; set to `true` via `-N` / `--gia-audio` to enable microphone recording in gia
- `limit: usize` — restricts the graph to the N most recent commits by timestamp (default 0 = no limit); set via `-L` / `--limit`
- `age_fade: bool` — defaults to `true`; pass `-a` / `--age-fade` to disable; fades nodes and edges linearly by age: oldest commit = 20% opacity, newest = 100%

**RefFilter (`main.rs`)**
- Parses the `--filter` string (`b`=branches, `r`=remotes, `t`=tags, `h`=HEAD, `s`=stashes); default is `"brts"`
- Controls which Git refs are included in the graph

**CommitNode (`main.rs`)**
- Represents a Git commit with metadata (ID, message, timestamp, parents, tags, refs)
- Handles DOT node formatting: shape, color, label, URL, and tooltip attributes
- Color coding: yellow=current checkout, light blue=local branch, light green=remote, pink=tag, orange=other refs

**GitGraphviz (`graph.rs`)**
- Main orchestration struct holding the Repository, RefFilter, optional GitLab base URL, and commit limit
- `generate_dot()` — collects referenced commits, applies limit filter (if set), builds condensed graph, writes DOT file
- Limit filter: after collecting all referenced commits, sorts by timestamp (newest first) and retains only the top N commits
- `build_condensed_graph()` / `find_connection_path()` — adds merge-junction commits needed to maintain graph connectivity
- `find_condensed_connections()` — traces the nearest condensed ancestor for each commit edge
- `collect_path_commits()` — gathers commits between two nodes for hover tooltips
- `add_tagged_commits()`, `add_root_commits()`, `add_branch_readmes()` — enrich commit nodes
- `detect_gitlab_url()` / `parse_gitlab_remote_url()` — auto-detects GitLab base URL from the remote
- `build_edge_attrs()` — constructs DOT edge attribute string; edge color is a heatmap (grey→orange→red, logarithmic, normalized across all edges) encoding changed-file count; edge thickness (`penwidth`) scales logarithmically with total changed lines (insertions + deletions, range 0.5–8.0); edge label (commit count) uses fixed fontsize=8
- `edge_penwidth()` — maps changed-line count to penwidth using `0.5 + log10(lines+1) * 1.2`, clamped to 0.5–8.0
- `edge_heatmap_color()` — maps file count to a grey→orange→red color using a logarithmic ratio normalized to the max file count across all edges
- `apply_alpha()` (`commit_node.rs`) — appends an alpha byte to `#RRGGBB` hex colors for age-fade; opacity computed linearly: `0.2 + 0.8 * (ts - min_ts) / (max_ts - min_ts)`; edge opacity is the average of its two endpoint commit opacities

**Web Server (`web_server.rs`)**
- Started automatically unless `-s` / `--svg-only` is passed
- Handles diff and AI summary requests from the SVG context menu
- `load_diff_prompt()` — reads `~/.ggv/prompt/default_prompt.md`; creates the file with the built-in default if absent; prompt is re-read on every request so edits take effect without restart
- `/delete-tag` route: deletes a tag locally (`git tag -d`) then from all remotes (`git push <remote> --delete`); triggers SVG regeneration
- `diff2html_section()` — builds a self-contained HTML fragment with commit history cards and a side-by-side diff (diff2html embedded inline); appended after the AI summary in `build_html()`; returns `Err` (omits diff section) when changed-file count exceeds `max_diff_files`
- `run_diff2html()` — serves the standalone `/diff2html` (non-AI) and `/diff2html-single` endpoints; when `max_diff_files` is exceeded the diff is suppressed and a warning notice is shown in place of the file view; commit cards are always rendered
- `render_commit_cards()` — parses `git log --pretty=format:...` output into styled HTML cards with per-ref badge coloring and a per-commit file-count badge
- `batch_file_counts()` — fetches changed-file counts for all commits in a range with a single `git log --name-only` call; result is a `HashMap<hash, count>` consumed by `render_commit_cards()`
- `/diff2html?ai=1` — AI summary + diff2html inline; `?nolog=1` skips commit log metadata (diff-only); both served by the `/diff2html` route handler
- `build_html()` — assembles the full AI summary page; accepts an optional `diff_section` rendered below the Markdown summary card
- diff2html CSS and JS are embedded as Rust string constants (no CDN dependency)

**Utility Functions (`graphviz.rs` and `utils.rs`)**
- `enhance_svg()` — injects interactive JavaScript into SVG for drag-to-compare, context menus, and clipboard operations
- `open_file()` — cross-platform file/URL opening (Windows: `cmd /C start`, macOS: `open`, Linux: `xdg-open`)
- `time_ago()` — human-readable relative timestamps for tooltips
- `repo_name_from_path()` — derives the default output filename from the repository folder name

### Data Flow
1. Parse CLI arguments and open Git repository using git2
2. Collect referenced commits for each enabled ref type (branches, remotes, tags, HEAD)
3. Add root commits and tag associations; mark branch tips and current checkout
4. Apply commit limit filter if `--limit N` is set: sort all commits by timestamp (newest first) and keep only top N
5. Build condensed graph: keep only referenced commits plus necessary merge-junction commits
6. Pre-compute condensed parent edges and forge (GitLab/GitHub) compare URLs
7. Compute per-commit opacity for age-fade (linear, oldest=0.2, newest=1.0) if `--age-fade` is active (default on)
8. Write DOT file with styled nodes and edges; apply alpha to node fill/border/font colors and edge heatmap color
8. Start web server that renders DOT → SVG in-browser via @hpcc-js/wasm-graphviz (WASM)
9. Unless `--no-show`: open browser to view the rendered SVG

### Dependencies
- `git2` — Git repository access and commit traversal
- `clap` with derive features — CLI argument parsing
- `anyhow` — Error handling with context
- `chrono` with serde — Date/time types
- `dirs` — Resolves the user home directory for config file paths

### External Requirements
- **@hpcc-js/wasm-graphviz** (WASM): Loaded automatically from CDN in the browser - no local installation required
- **gia** (optional): Required for AI diff summaries. Must be available in `PATH`. See [github.com/panjamo/gia](https://github.com/panjamo/gia)
