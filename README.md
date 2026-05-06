# tuitr

A terminal file viewer and annotator with syntax highlighting. Open any file or directory, browse the tree, annotate lines with comments, and copy your notes as an AI prompt.

## Features

- **Syntax highlighting** via [syntect](https://github.com/trishume/syntect) (LaTeX, Rust, Python, Markdown, and more)
- **File tree** with nested gitignore-aware filtering and expand/collapse
- **Per-line comments** rendered inline with box-drawing characters, persisted to `~/.config/tuitr/comments.db`
- **Search** within the current file (`/`, `n`, `N`)
- **Soft-wrapping** of long lines
- **Clipboard** — yank a line, yank current file's comments, or export all comments across all files as an AI prompt
- **File watching** — automatically reloads when the open file changes on disk
- **Binary/large file guard** — shows a message instead of garbled content
- **Resizable tree panel** — `<`/`>` to adjust the split
- **Mouse scroll** — scroll wheel works in both panels

## Install

```sh
cargo install tuitr
```

With Nix:

```sh
nix run github:deadmade/tuitr -- <path>
```

## Usage

```sh
tuitr <file|directory>
```

Comments are saved automatically to `.tuitr.json` in the root directory and restored on next open.

## Keybindings

### File panel

| Key | Action |
|-----|--------|
| `j` / `↓` / scroll | Move down |
| `k` / `↑` / scroll | Move up |
| `g` / `G` | Jump to top / bottom |
| `/` | Start search |
| `n` / `N` | Next / previous match |
| `c` | Add or edit comment on current line |
| `d` | Delete comment on current line |
| `D` | Delete all comments in file |
| `y` | Yank current line (+ comment) to clipboard |
| `Y` | Yank current file's comments as an AI prompt |
| `E` | Export ALL annotations (all files) as an AI prompt |
| `<` / `>` | Shrink / grow the tree panel |
| `Tab` | Switch focus to file |
| `q` | Quit |

### Tree panel

| Key | Action |
|-----|--------|
| `j` / `↓` / scroll | Move down |
| `k` / `↑` / scroll | Move up |
| `Enter` / `l` | Open file / expand directory |
| `Space` / `h` | Toggle expand |
| `E` | Export ALL annotations (all files) as an AI prompt |
| `<` / `>` | Shrink / grow the tree panel |
| `Tab` | Switch focus to file |
| `q` | Quit |

### Search mode

| Key | Action |
|-----|--------|
| `Enter` | Confirm query and jump to first match |
| `Esc` | Cancel |

### Comment edit mode

| Key | Action |
|-----|--------|
| `Enter` | Save comment |
| `Esc` | Cancel |

## AI prompt format

Pressing `Y` copies a prompt like:

```
I found the following issues in /path/to/file.rs. Please create a plan to fix them:

Line 12 (`fn process_data() {`):
This function does too many things

Line 34 (`let x = 0;`):
Variable name is not descriptive
```
