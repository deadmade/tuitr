# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

`tuitr` is a terminal UI file viewer/annotator built with Rust and ratatui. It opens a file from the command line, displays it in a split pane with a file tree on the left, and lets you attach per-line comments that are stored in memory for the session.

Usage: `tuitr <file>`

## Commands

```sh
cargo build          # build debug
cargo build --release
cargo run -- <file>  # run with a file
cargo clippy         # lint
cargo fmt            # format
cargo test           # tests (currently none)
```

The Nix flake provides a dev shell with `cargo`, `rustc`, `rustfmt`, `clippy`, `jj`, `git`, and `ripgrep-all`. Enter it with `nix develop` (or automatically via `.envrc` / direnv).

## Architecture

There are three modules:

- **`src/app.rs`** — `App` struct: owns all state (loaded file lines, per-line comments, cursor/scroll positions, mode, focus) and handles all input logic. Comments are stored in `HashMap<usize, String>` keyed by line index. `all_comments` preserves comments for previously-visited files so switching files doesn't lose them. Two enums drive behavior: `Mode` (Normal / EditComment) and `Focus` (Tree / File).

- **`src/tree.rs`** — `FileTree`: a flat `Vec<TreeEntry>` rebuilt on every expand/collapse. The `expanded: HashSet<PathBuf>` is the source of truth; `rebuild()` does a depth-first walk to regenerate `entries`. Hidden files (dot-prefixed) are filtered out. Directories sort before files.

- **`src/ui.rs`** — Pure rendering via ratatui. Layout is vertical: top split (25% tree / 75% file), optional comment editor, status bar. Comment boxes are rendered inline below their source lines using box-drawing characters. `comment_box_height` and `comment_editor_height` are called from `app.rs` to drive scroll accounting — the file scroll logic must account for comment boxes taking extra rows.

Key scroll invariant: `App::scroll_to_cursor` walks from `scroll` to `cursor` summing display rows (1 per code line + comment box height if present) and advances `scroll` until the cursor fits within `view_height`.
