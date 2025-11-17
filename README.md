# GGV - Git Graph Visualizer

A Rust CLI tool that generates visual representations of Git repository structure using Graphviz DOT format and SVG output.

<img src="doc/icon.png" alt="GGV Icon" width="128" height="128">
 
## Features

- **Comprehensive Visualization**: Displays commits, branches, tags, and their relationships
- **SVG Output**: Generates high-quality SVG images for easy viewing and sharing
- **Configurable**: Flexible CLI options for custom workflows
- **Cross-Platform**: Works on Windows, macOS, and Linux

## Prerequisites

- Rust toolchain (1.70+)
- **Graphviz**: Must be installed on your system
  - **Windows**: Download from [graphviz.org](https://graphviz.org/download/) or use `choco install graphviz`
  - **macOS**: `brew install graphviz`
  - **Linux**: `sudo apt install graphviz` (Debian/Ubuntu) or equivalent

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

```bash
ggv [OPTIONS]

Options:
  -r, --repo-path <PATH>    Path to Git repository [default: current directory]
  -o, --output <FILE>       Output DOT file path [default: git-graph.dot]
      --no-show             Skip SVG generation and file opening
  -h, --help                Print help
  -V, --version             Print version
```

### Examples

Generate graph for a specific repository:

```bash
ggv --repo-path /path/to/repo
```

Generate DOT file only (no SVG):

```bash
ggv --output custom.dot --no-show
```

Custom output location without opening:

```bash
ggv --repo-path ~/projects/myrepo --output ~/graphs/repo.dot --no-show
```

## Output

The tool generates:

1. **DOT file** (`git-graph.dot` by default): Graphviz format representation
2. **SVG file** (`git-graph.svg` by default): Visual graph automatically opened in your default viewer

### Graph Elements

- **Nodes**: Commits with short hash and message
- **Edges**: Parent-child relationships between commits
- **Colors**: Branch-specific coloring for visual distinction
- **Tags**: Displayed with commit information
- **Branch Tips**: Specially marked to identify branch heads

## Development

### Build Commands

```bash
# Development build
cargo build

# Release build
cargo build --release

# Run with default options
cargo run

# Run with custom options
cargo run -- --repo-path /path/to/repo --output custom.dot
```

### Code Quality

```bash
# Run linter with fixes
cargo clippy --fix --allow-dirty

# Format code
cargo fmt

# Check compilation
cargo check
```

### Development Workflow

1. Make changes
2. Run `cargo clippy --fix --allow-dirty`
3. Run `cargo fmt`
4. Run `cargo build` to verify compilation
5. Test with `cargo run`
6. Commit changes

## Dependencies

- **git2**: Git repository operations
- **clap**: CLI argument parsing
- **anyhow**: Error handling
- **graphviz-rust**: Pure Rust Graphviz implementation for SVG generation

## Architecture

The tool follows a simple pipeline:

1. Parse CLI arguments
2. Open Git repository
3. Walk all local branches to collect commits
4. Associate tags with commits
5. Generate DOT format output
6. Convert to SVG (if enabled)
7. Open SVG file (if enabled)

See `CLAUDE.md` for detailed architecture documentation.

## License

[Specify your license here]

## Contributing

Contributions welcome. Please ensure code passes `cargo clippy` and `cargo fmt` before submitting.
