# just-every-tmux

`just-every-tmux` is a fast Rust TUI/CLI for managing tmux coding sessions and exporting high-quality session transcripts.

It gives you three binaries:

- `br`: worktree mode (branch + worktree + tmux)
- `b`: current-directory mode (tmux only)
- `cx`: transcript exporter for coder sessions

## Platform Support

- Linux: fully supported
- macOS: fully supported
- Windows: fully supported natively in PowerShell (no WSL required)

For step-by-step setup instructions, use `INSTALL.md`.

## Quick Install (No `cargo run`)

This installs real binaries (`br`, `b`, `cx`) into your user bin directory so you can run them directly.

### Linux/macOS

```bash
git clone https://github.com/HenryGetz/just-every-tmux.git
cd just-every-tmux
./install
```

Optional guided installer (simple interactive menu):

```bash
./scripts/install-tui.sh
```

Then restart your shell, or run:

```bash
export PATH="$HOME/.local/bin:$PATH"
```

### Windows (Native PowerShell)

```powershell
git clone https://github.com/HenryGetz/just-every-tmux.git
cd just-every-tmux
powershell -ExecutionPolicy Bypass -File .\scripts\install-windows.ps1
```

This does a native Windows install and also auto-installs `psmux` (tmux-compatible) when needed.
It uses the current Windows user profile paths by default (including `%USERPROFILE%\.code` for coder sessions).
If Python is missing, the installer first tries `winget` and then automatically falls back to an embedded Python runtime in `%USERPROFILE%\.portable-msvc\python-embed`.

By default, the installer uses a lightweight portable MSVC toolchain via `portable-msvc.py`.
No Visual Studio Installer is required for the default path.

If you explicitly want full Visual Studio Build Tools instead:

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\install-windows.ps1 -UseMsvcBuildTools
```

If you want to skip `psmux` auto-install:

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\install-windows.ps1 -SkipPsmux
```

## Quick Start

```bash
# Worktree mode
br feature-login

# Current-directory mode
b notes

# Interactive picker
br
```

`b <name>` connects the new tmux pane to one shared opencode server (`opencode serve`, auto-started detached on first use) instead of launching a fresh opencode/coder TUI per session. Every `b <name>` pane attaches to the same server, each in its own directory, so you get per-folder sessions without one server per session. Check the server with `b --server-status`; its log lives at `${XDG_STATE_HOME:-~/.local/state}/b/serve.log`. Worktree mode (`br`) keeps `coder` as the startup command.

## TUI Keys

- `Enter` / `Space`: open selected session
- `n` or `F2`: create new session
- `F3`: toggle preview pane
- `F4`: rename selected session
- `F8` / `Ctrl+X`: kill selected session (with confirmation)
- `F9`: force-kill selected session
- `Ctrl+S`: open export mode chooser
- `Ctrl+P` / `F10`: copy last assistant output from selected chat to clipboard
- `Ctrl+'` / `Ctrl+"`: open copy picker for the last 4 assistant outputs
  - choose `1..4` (`Enter` defaults to `1`) to copy last/2nd/3rd/4th-to-last
  - after copy completes, opens a fullscreen preview modal of the copied markdown
  - `Esc` / `Enter` closes preview, arrows or `j/k` scroll
- `q`: quit

Text input shortcuts (filter/new/rename fields):

- `Alt+Backspace` / `Ctrl+W`: delete previous word

## Exporter (`cx`)

```bash
cx <session-id> --compact
cx <session-id> --medium
cx <session-id> --full
cx <session-id> --json
```

Export modes:

- `compact`: user + assistant messages only
- `medium`: concise markdown; abbreviated tool calls; planning rendered as checklist
- `full`: detailed markdown; full shell/tool call bodies
- `json`: JSON-heavy dump for highest fidelity/debugging

Default output directory for exports is `~/coder-md`.

## Environment Variables

- `BR_RUN_CMD`: startup command sent to tmux (default: `coder` for worktree mode; `b` sessions default to `opencode attach <url> --dir <dir>`; supports `{name}`, `{dir}`, `{url}` substitution). In `b` (CWD) mode a bare `opencode` value is treated as the attach default; set another command (e.g. `coder`) to run something else. To run standalone (non-attached) opencode in a `b` session, set `BR_RUN_CMD` to anything other than exactly `opencode` (e.g. `BR_RUN_CMD='opencode --print-logs'`).
- `BR_SERVER_URL`: opencode server URL used by `b` sessions (default: `http://127.0.0.1:4096`). If `OPENCODE_SERVER_PASSWORD` is set in your environment, both the spawned server and the attach commands inherit it consistently. `b` starts a server only when nothing is listening on the configured port; if a process already holds it (a loaded opencode, or an unrelated one) `b` re-probes briefly, warns, and proceeds to attach rather than starting a doomed second server, so startup failures are reported by `opencode attach` itself. When `b` does start a server and it dies, the error includes the tail of `~/.local/state/b/serve.log`. If another process is squatting the port, pick a different one via `BR_SERVER_URL`.
- `BR_PREFIX`: branch prefix (default: `w/`)
- `BR_BASE`: base ref for new branches (default: `origin/main`)
- `BR_WORKTREES_DIR`: worktree directory (default: `~/.br`)
- `BR_REPO`: explicit repo root (overrides autodetect)
- `BR_VERBOSE`: print diagnostics
- `BR_MODE`: force mode (`worktree` or `cwd`)
- `BR_EXPORT_OUT`: export directory for `Ctrl+S` (default: `~/coder-md`)
- `BR_CODE_DIR`: code data dir for exports (default: `~/.code`)

## Notes

- Copy (`Ctrl+P` / picker) and export (`Ctrl+S` / `cx`) resolve sessions from coder's `~/.code` session files, so they apply to coder/worktree sessions. Sessions created via `b <name>` attach to a shared opencode server and have no coder session files; use worktree mode (`br`) for sessions you want to copy/export from.

## Development

```bash
cargo test
cargo run --bin br
```
