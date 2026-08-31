# DeTree

DeTree is a small native command-line tool that turns an indented text list or
JSON tree into directories and Markdown files. It is the Rust successor to the
original Python implementation.

## Why DeTree

- Single native executable (about 200 KB on Windows)
- No runtime dependencies
- Works with text lists and JSON
- Generates Markdown front matter and body content
- Builds for Windows, Linux, macOS, and Linux ARM64

## Install

Download the binary for your platform from
[GitHub Releases](https://github.com/morganross/detree3/releases), or install
the current source with Cargo:

```bash
cargo install --git https://github.com/morganross/detree3 --bin detree
```

## Usage

```text
detree <input_file> <output_dir> [OPTIONS]

Options:
  --format <auto|text|json>   Input format (default: auto)
  --remove-digits             Remove leading digits from generated names
  --allow-empty-folders       Create folders for leaf nodes
```

### Text input

```text
Documentation
  Getting Started
    Installation
    Quick Start
  Reference
    Command Line
```

```bash
detree structure.txt ./docs
```

Nodes with children become directories containing `index.md`. Leaf nodes
become Markdown files. A line containing `**` is written as body content for
the current node instead of becoming another file.

### JSON input

```json
[
  {
    "name": "Documentation",
    "type": "directory",
    "children": [
      {"name": "Getting Started", "type": "file", "body": "Welcome."}
    ]
  }
]
```

```bash
detree structure.json ./docs --format json
```

## Development

DeTree requires Rust 1.81 or newer.

```bash
cargo fmt --check
cargo test --locked
cargo clippy --all-targets --all-features -- -D warnings
cargo build --release --locked
```

Generated Cargo output belongs in `target/` and is not committed.

## Security and data safety

DeTree processes local files and does not use the network. Generated names are
sanitized before files are created. Use a new or backed-up output directory if
you need to preserve existing files with colliding names.

Security reports should follow [SECURITY.md](SECURITY.md).

## License

MIT. See [LICENSE](LICENSE).

