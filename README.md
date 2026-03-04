# GGV - Git Graph Visualizer

A Rust CLI tool that generates visual representations of Git repository structure using Graphviz DOT format and SVG output.

<img src="doc/icon.png" alt="GGV Icon" width="128" height="128">

## Features

- **Comprehensive Visualization**: Displays commits, branches, remote branches, tags, and HEAD
- **Condensed Graph**: Only referenced commits (branch tips, tags, root, merge junctions) are shown — intermediate commits are skipped for clarity
- **Dual Theme**: Dark and light theme — branch nodes are color-coded by type (main, develop, feature/\*, release/\*, hotfix/\*); switch with `-t light`
- **Auto Fetch**: Runs `git fetch --tags --prune` before generating the graph to ensure tags are current and stale remote-tracking refs are removed
- **SVG Output**: Generates high-quality SVG images opened automatically in your default viewer
- **Ref Filtering**: Choose which ref types to include (local branches, remotes, tags, HEAD)
- **Current-Branch View**: `-c` hides all refs not on the ancestry path of HEAD — shows only what is reachable from the current checkout
- **Subtree View**: Limit the graph to a specific commit and all its descendants (`-F`)
- **Forge Integration**: Clickable graph edges linking to GitLab or GitHub compare views, with hover tooltips showing the condensed commits — auto-detected from the remote URL
- **Drag-to-Compare**: Drag any commit node onto another to open a GitLab or GitHub compare view for that arbitrary range — order is corrected automatically (always `older...newer`)
- **SHA Copy**: Click any commit node to copy its full 40-character SHA to the clipboard (amber border flash confirms)
- **Graph Tooltip**: Hover the SVG background to see the repository name, current branch, HEAD commit, author, and date
- **AI Diff Server**: Start a local web server (`-w`) that opens `git difftool` (default) or runs `git diff | gia` for an AI-generated summary (`-a`) when you click an edge count label
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
  If installed in a non-standard location, set the `GRAPHVIZ_DOT` environment variable:

  ```bat
  set GRAPHVIZ_DOT=C:\MyTools\Graphviz\bin\dot.exe
  ```

- **gia** (optional): Required for AI diff summaries (`-w -a`). Must be available in `PATH`.
  See [github.com/panjamo/gia](https://github.com/panjamo/gia).

![Sample Git Graph](doc/sample.png)

## Installation

```bash
cargo build --release
```

The binary will be available at `target/release/ggv`.

## Usage

### Basic Usage

```bash
ggv
```

### Command-Line Options

```
ggv [OPTIONS]

Options:
  -r, --repo-path <PATH>    Path to Git repository [default: .]
  -o, --output <FILE>       Output DOT file path [default: ggv-<repo-name>.dot]
  -n, --no-show             Skip SVG generation and opening
  -f, --filter <CHARS>      Ref types: b=branches, r=remotes, t=tags, h=head [default: brt]
  -g, --gitlab-url <URL>    Base URL for compare links — GitLab or GitHub (auto-detected)
  -F, --from <COMMIT>       Limit graph to this commit and its descendants
  -X, --no-fetch            Skip automatic 'git fetch --tags --prune'
  -k, --keep-dot            Keep the intermediate DOT file after SVG generation
  -t, --theme <THEME>       Color theme: dark or light [default: light]
  -c, --current-branch      Show only refs that are ancestors of HEAD
  -w, --web-server          Start the diff web server
  -P, --web-port <PORT>     Port for the diff server (0 = OS-assigned) [default: 0]
  -a, --use-ai              Use AI (gia) to summarize diffs; without this, opens git difftool
  -b, --gia-browser         Pass -b to gia — gia opens its own browser window
  -p, --gia-prompt <TEXT>   Custom prompt passed to gia (overrides built-in default)
  -h, --help                Print help
  -V, --version             Print version
```

### Examples

Generate graph for the current repository:

```bash
ggv
```

Generate graph for a specific repository:

```bash
ggv -r /path/to/repo
```

Generate DOT file only, no SVG:

```bash
ggv -n
```

Skip the automatic tag fetch (faster, offline):

```bash
ggv -X
```

Keep the intermediate DOT file alongside the SVG:

```bash
ggv -k
```

Show only local branches (no remotes, no tags):

```bash
ggv -f b
```

Show branches and tags but not remotes:

```bash
ggv -f bt
```

Override the forge URL for clickable compare links:

```bash
ggv -g https://gitlab.com/mygroup/myproject
ggv -g https://github.com/owner/repo
```

Show only the history from a specific commit onwards:

```bash
ggv -F abc1234
ggv -F feature/my-branch
ggv -F v2.0.0
```

Use the dark theme:

```bash
ggv -t dark
```

Show only the current branch:

```bash
ggv -c
```

Combine subtree with current-branch view:

```bash
ggv -F v1.0.0 -c
```

### Diff Web Server

Start the diff server alongside the graph. Clicking the blue edge count label triggers the configured diff action.

**Default (no `--use-ai`)** — opens `git difftool -d sha1 sha2` in your configured diff tool and returns an empty page:

```bash
ggv -w
```

**With `--use-ai` (`-a`)** — runs `git diff | gia` and shows an AI summary in the browser:

```bash
ggv -w -a
```

Use a fixed port (useful when the SVG will be reopened later):

```bash
ggv -w -P 8080
```

Let gia open its own browser window instead of the built-in summary page:

```bash
ggv -w -a -b
```

Use a custom prompt:

```bash
ggv -w -a -p "list the changed files and explain each change in one sentence"
```

Combined:

```bash
ggv -w -a -b -p "summarize in three bullet points"
```

The process stays alive after the SVG is opened, serving requests until Ctrl+C. Each `ggv -w` instance gets its own OS-assigned port, so multiple instances can run simultaneously.

## Output

1. **SVG file** (`ggv-<repo-name>.svg`): Visual graph opened automatically in your default viewer.
   The intermediate DOT file is deleted after SVG generation unless `-k` is set.
2. With `-n`: only the **DOT file** is written (`ggv-<repo-name>.dot`).

### Graph Elements

Branch nodes are rounded rectangles, color-coded by name. Two built-in themes are available:

#### Dark theme (`-t dark`) — background `#0F172A`

| Branch pattern | Fill | Border | Text |
|----------------|------|--------|------|
| `main` / `master` | `#059669` | `#34D399` | `#F0FDF4` |
| `develop` | `#7C3AED` | `#A78BFA` | `#F5F3FF` |
| `feature/*` | `#2563EB` | `#60A5FA` | `#EFF6FF` |
| `release/*` | `#D97706` | `#FBBF24` | `#FFFBEB` |
| `hotfix/*` | `#DC2626` | `#F87171` | `#FEF2F2` |
| other | `#334155` | `#60A5FA` | `#E2E8F0` |

#### Light theme (`-t light`, default) — background `#F8FAFC`

| Branch pattern | Fill | Border | Text |
|----------------|------|--------|------|
| `main` / `master` | `#ECFDF5` | `#10B981` | `#065F46` |
| `develop` | `#F3E8FF` | `#8B5CF6` | `#5B21B6` |
| `feature/*` | `#EFF6FF` | `#3B82F6` | `#1E40AF` |
| `release/*` | `#FFF7ED` | `#F59E0B` | `#92400E` |
| `hotfix/*` | `#FEF2F2` | `#EF4444` | `#7F1D1D` |
| other | `#F8FAFC` | `#64748B` | `#334155` |

#### Common elements

| Element | Dark | Light |
|---------|------|-------|
| Tag node | Dashed `#94A3B8` border | Dashed `#94A3B8` border, transparent fill |
| Current checkout | 2px border + `CURRENT` label | 2px border + `CURRENT` label |
| Plain commit / junction | Dark slate panel | White panel, `#E2E8F0` border |
| Edges | `#475569` | `#CBD5E1` |

### SVG Interactions

Open the SVG in a browser to use all interactive features:

| Interaction | Result |
|-------------|--------|
| Hover an edge | Tooltip listing commits condensed into that range |
| Click an edge | Opens the GitLab / GitHub compare view for that range |
| Click the blue edge count label | Opens `git difftool` (default) or AI summary page (with `-w -a`) |
| Click a commit node | Copies the full 40-character SHA to the clipboard (amber flash confirms) |
| Drag one commit node onto another | Opens the forge compare view for that range — always `older...newer` |
| Hover the SVG background | Tooltip with repository name, branch, HEAD commit, author, and date |

Drag-to-compare requires a forge URL (auto-detected or set via `-g`). The blue edge count labels are only active when `-w` is used. Add `-a` to get an AI summary instead of opening the difftool.

## Development

### Build Commands

```bash
cargo build           # Development build
cargo build --release # Release build
cargo run             # Run with default options
cargo run -- -r /path/to/repo -f b
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
