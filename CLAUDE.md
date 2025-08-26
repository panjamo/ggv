# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

GGV (Git Graph Visualizer) is a Rust CLI tool that generates Graphviz DOT files from Git repositories and optionally converts them to SVG images for visualization. The tool analyzes Git commit history, branches, and tags to create visual representations of the repository structure.

## Development Commands

### Build and Run
- `cargo build` - Build the project
- `cargo run` - Run with default options (generates DOT file and SVG, opens SVG)
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

**Args Struct (`main.rs:13-27`)**
- CLI argument parsing using clap with derive macros
- Key options: `repo_path`, `output` (DOT file), `show` (SVG generation, defaults to true)

**CommitNode (`main.rs:29-115`)**
- Represents a Git commit with metadata (ID, message, timestamp, parents, tags)
- Implements ordering by timestamp for topological sorting
- Handles DOT node formatting with special markup for tags and branch tips

**GitGraphviz (`main.rs:117-299`)**
- Main orchestration struct containing Git repository and special branch configuration
- `generate_dot()` - Core method that walks branches, processes commits, and writes DOT format
- `walk_branch()` - Traverses commit history using git2's revwalk
- `add_tags_to_commits()` - Associates Git tags with their target commits
- `write_subgraph()` - Creates Graphviz subgraphs for branch visualization

**Utility Functions (`main.rs:301-350`)**
- `generate_svg()` - Uses `graphviz-rust` to convert DOT files to SVG format
- `open_file()` - Cross-platform file opening (Windows: `cmd /C start`, macOS: `open`, Linux: `xdg-open`)

### Data Flow
1. Parse CLI arguments and open Git repository using git2
2. Walk all local branches to collect commits in HashMap (deduplication)
3. Add Git tag associations to commits
4. Mark branch tip commits for special visualization
5. Generate DOT file with nodes (commits) and edges (parent relationships)
6. If `--show` enabled: convert to SVG via graphviz-rust and open file

### Dependencies
- `git2` - Git repository access and commit traversal
- `clap` with derive features - CLI argument parsing
- `anyhow` - Error handling with context
- `chrono` with serde - Date/time handling (imported but not actively used)
- `graphviz-rust` - Pure Rust Graphviz implementation for DOT parsing and SVG generation

### External Requirements
- Cross-platform file opening utilities (built into OS)
- **No external Graphviz installation required** - uses `graphviz-rust` for pure Rust SVG generation

### Special Branch Handling
The tool prioritizes certain branches in subgraph generation:
- `refs/heads/master`
- `refs/heads/main` 
- `refs/heads/integration`

These are processed first in the DOT output for consistent visualization layout.