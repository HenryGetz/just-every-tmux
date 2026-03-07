# `just-every-tmux`

Because manually juggling tmux sessions by hand is a great way to pretend you love pain.

This is a small Rust CLI/TUI for opening, creating, filtering, and cleaning up tmux sessions with less ceremony and fewer typos.

For broader context, this project lives in the same ecosystem as [`just-every/code`](https://github.com/just-every/code).

## What you get

- `br` mode: opens sessions backed by real Git worktrees under `~/.br/w-<name>` with branch prefix `w/`.
- `b` mode: opens sessions in your current directory (no worktree setup).
- Fuzzy TUI that sorts by recency so the thing you *actually* used recently shows up first.
- Session preview pane, quick kill actions, and markdown export for session logs.

## Install

```bash
git clone https://github.com/HenryGetz/just-every-tmux.git
cd just-every-tmux
cargo build --release
mkdir -p ~/.local/bin
ln -sf "$(pwd)/target/release/br" ~/.local/bin/br
ln -sf "$(pwd)/target/release/b" ~/.local/bin/b
ln -sf "$(pwd)/target/release/cx" ~/.local/bin/cx
```

Then make sure `~/.local/bin` is in your `PATH`.

## Quick use

```bash
# Worktree mode (branch + worktree + tmux)
br feature-login

# Current-dir mode (just tmux)
b notes

# List sessions (recency sorted)
br --list

# Open interactive picker
br
```

## Key ideas

- `Enter` / `Space`: open selected session
- `n` or `F2`: create new session
- `F3`: toggle preview pane
- `F8` / `Ctrl+X`: kill selected session (with confirmation)
- `F9`: force-kill selected session
- `Ctrl+S`: export session transcript markdown
- `q`: quit

Direct exporter CLI (self-contained in this repo):

```bash
cx <session-id> --medium
cx <session-id> --full --out ~/coder-md
cx <session-id> --json --out ~/coder-md
```

Export modes:

- `compact`: user + assistant messages only
- `medium`: readable transcript with abbreviated tool calls (planning stays checklist-style)
- `full`: readable transcript with detailed tool call bodies
- `json`: JSON-heavy dump for maximal fidelity

Yes, you can still use raw `tmux` commands manually if you miss suffering.

## Environment knobs

- `BR_RUN_CMD`: startup command sent to tmux (default: `coder`)
- `BR_PREFIX`: branch prefix (default: `w/`)
- `BR_BASE`: base ref for new branches (default: `origin/main`)
- `BR_WORKTREES_DIR`: worktree directory (default: `~/.br`)
- `BR_REPO`: explicit repo root (overrides autodetect)
- `BR_VERBOSE`: print diagnostics
- `BR_MODE`: force mode (`worktree` or `cwd`)
- `BR_EXPORT_OUT`: export directory for `Ctrl+S` (default: `~/coder-md`)
- `BR_CODE_DIR`: code data dir for exports (default: `~/.code`)

## Requirements

- Linux/macOS
- `tmux`
- Rust toolchain (`cargo`)

That is all. No YAML labyrinth required.
