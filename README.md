# GGV - Git Graph Visualizer

A Rust CLI tool that generates visual representations of Git repository structure using Graphviz DOT format and SVG output.

<img src="doc/icon.png" alt="GGV Icon" width="128" height="128">

## Features

- **Comprehensive Visualization**: Displays commits, branches, remote branches, tags, and HEAD
- **Condensed Graph**: Only referenced commits (branch tips, tags, root, merge junctions) are shown — intermediate commits are skipped for clarity
- **SVG Output**: Generates high-quality SVG images opened automatically in your default viewer
- **Ref Filtering**: Choose which ref types to include (local branches, remotes, tags, HEAD)
- **Subtree View**: Limit the graph to a specific commit and all its descendants (`--from`)
- **GitLab Integration**: Clickable commit nodes linking to GitLab compare views, auto-detected from the remote URL
- **Cross-Platform**: Works on Windows, macOS, and Linux

## Prerequisites

- Rust toolchain (1.70+)
- **Graphviz**: Must be installed — `dot.exe` is called directly

  | Platform | Command |
  |----------|---------|
  | Windows (winget) | `winget install --id Graphviz.Graphviz` |
  | Windows (Chocolatey) | `choco install graphviz` |
  | Windows (manual) | [graphviz.org/download](https://graphviz.org/download/) |
  | macOS | `brew install graphviz` |
  | Linux (Debian/Ubuntu) | `sudo apt install graphviz` |

  On Windows, GGV searches for `dot.exe` automatically in the standard installation directories.
  If it is installed in a non-standard location, set the `GRAPHVIZ_DOT` environment variable:

  ```bat
  set GRAPHVIZ_DOT=C:\MyTools\Graphviz\bin\dot.exe
  ```

![Sample Git Graph](doc/sample.png)

## Installation

```bash
cargo build --release
```

The binary will be available at `target/release/ggv`.

## Usage

### Basic Usage

Generate a graph of the current repository and open it:

```bash
cargo run
```

Or using the compiled binary:

```bash
ggv
```

### Command-Line Options

```
ggv [OPTIONS]

Options:
  -r, --repo-path <PATH>       Path to Git repository [default: .]
  -o, --output <FILE>          Output DOT file path [default: ggv-<repo-name>.dot]
      --no-show                Skip SVG generation and opening
  -f, --filter <CHARS>         Ref types to include: b=branches, r=remotes, t=tags, h=head [default: brt]
      --gitlab-url <URL>       GitLab base URL for clickable commit links
                               (auto-detected from remote if not specified)
      --from <COMMIT>          Limit graph to this commit and its descendants
                               (accepts commit hash, branch name, or tag)
  -h, --help                   Print help
  -V, --version                Print version
```

### Examples

Generate graph for the current repository:

```bash
ggv
```

Generate graph for a specific repository:

```bash
ggv --repo-path /path/to/repo
```

Generate DOT file only, no SVG:

```bash
ggv --no-show
```

Show only local branches (no remotes, no tags):

```bash
ggv --filter b
```

Show branches and tags but not remotes:

```bash
ggv --filter bt
```

Override the GitLab URL for clickable links:

```bash
ggv --gitlab-url https://gitlab.com/mygroup/myproject
```

Show only the history from a specific commit onwards (new root):

```bash
ggv --from abc1234
```

Use a branch name or tag as the new root:

```bash
ggv --from feature/my-branch
ggv --from v2.0.0
```

Combine with filtering — show only local branches descending from a tag:

```bash
ggv --from v1.0.0 --filter b
```

## Output

1. **SVG file** (`ggv-<repo-name>.svg`): Visual graph opened automatically in your default viewer.
   The intermediate DOT file is deleted after SVG generation.
2. With `--no-show`: only the **DOT file** is written (`ggv-<repo-name>.dot`).

### Graph Elements

| Element | Shape | Color |
|---------|-------|-------|
| Current checkout | box | Yellow |
| Local branch tip | box | Light blue |
| Remote-only ref | box | Light green |
| Tag | octagon | Light pink |
| ROOT / HEAD | house | Light orange |
| Plain commit | egg | White |

Nodes with both a local branch and a matching remote branch are shown combined (`🌿🌐 branch (remote)`).
Hover tooltips show the commits condensed into each graph edge.
Clicking a node (in a browser) opens the GitLab compare view for that range.

## Development

### Build Commands

```bash
cargo build           # Development build
cargo build --release # Release build
cargo run             # Run with default options
cargo run -- --repo-path /path/to/repo --filter b
```

### Code Quality

```bash
cargo clippy --fix --allow-dirty
cargo fmt
cargo check
```

### Development Workflow

1. Make changes
2. `cargo clippy --fix --allow-dirty`
3. `cargo fmt`
4. `cargo build`
5. `cargo run`
6. Commit

## Dependencies

- **git2** — Git repository operations
- **clap** — CLI argument parsing
- **anyhow** — Error handling
- **chrono** — Date/time formatting

## Architecture

See `CLAUDE.md` for detailed architecture documentation.

## License

[Specify your license here]

## Contributing

Contributions welcome. Please ensure code passes `cargo clippy` and `cargo fmt` before submitting.
