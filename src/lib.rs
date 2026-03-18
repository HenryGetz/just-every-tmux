use std::collections::{HashMap, HashSet};
use std::env;
use std::fs;
use std::io::{self, BufRead, BufReader, Stdout};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use chrono::{DateTime, Local};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use once_cell::sync::Lazy;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph};
use ratatui::{Frame, Terminal};
use regex::Regex;
use serde_json::Value;
use strfmt::strfmt;
use which::which;

pub mod exporter;

use crate::exporter::ExportMode;

static VALID_NAME_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^[A-Za-z0-9._:-]+$").expect("valid"));
static INVALID_CHARS_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"[^A-Za-z0-9._:-]+").expect("valid"));
static DUP_UNDERSCORE_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"_+").expect("valid"));
static TMUX_PATH: Lazy<Option<String>> =
    Lazy::new(|| which("tmux").ok().map(|path| path.to_string_lossy().to_string()));
static SESSION_ID_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}")
        .expect("valid")
});

const USAGE: &str = r#"
br — tmux session helper with fuzzy TUI

New in this version:
  • TUI shows each session's "Last" time (most recent of: last-attached, activity, created)
  • Sessions auto-sort by most recent first; fuzzy results rank by match, then recency
  • `br --list` prints sessions sorted by recency (names only)

Modes:
  br <name>  => worktree mode (real Git worktree under ~/.br/w-<name>, branch w/<name>)
  b  <name>  => CWD mode     (no worktree; just open tmux in current dir)

Usage:
  br <session-name>     # create/attach worktree+branch, tmux there, run coder
  b  <session-name>     # create/attach tmux in CWD, run coder (no Git)
  br --list             # list tmux sessions (no TUI)
  br -h | --help        # help
  br                    # TUI: fuzzy-filter; Enter/Space to open (uses this tool's mode)
  b                     # same TUI, but new sessions use CWD mode

Env (all optional):
  BR_RUN_CMD        startup command sent to tmux (default: "coder")
  BR_PREFIX         branch prefix (default: "w/")
  BR_BASE           base ref for new branches (STRICT; default: "origin/main")
  BR_WORKTREES_DIR  directory for worktrees (default: "~/.br")
  BR_REPO           explicit repo root (overrides autodetect)
  BR_VERBOSE        if set, prints diagnostics on stderr
  BR_MODE           override mode: "worktree" | "cwd" (otherwise inferred from argv[0])

Notes:
  • Session names are auto-normalized: invalid characters are replaced with "_".
    Allowed characters are: letters, digits, . _ : -
"#;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Mode {
    Worktree,
    Cwd,
}

#[derive(Clone, Debug)]
struct SessionInfo {
    name: String,
    name_lc: String,
    last_attached: i64,
    activity: i64,
    created: i64,
}

impl SessionInfo {
    fn new(name: String, last_attached: i64, activity: i64, created: i64) -> Self {
        let name_lc = name.to_lowercase();
        Self {
            name,
            name_lc,
            last_attached,
            activity,
            created,
        }
    }

    fn sort_ts(&self) -> i64 {
        self.last_attached.max(self.activity).max(self.created)
    }
}

#[derive(Debug)]
struct ExitError {
    code: i32,
    msg: String,
}

impl ExitError {
    fn new(code: i32, msg: impl Into<String>) -> Self {
        Self {
            code,
            msg: msg.into(),
        }
    }
}

type BrResult<T> = Result<T, ExitError>;

#[derive(Debug)]
struct CmdResult {
    code: i32,
    stdout: String,
    stderr: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InputMode {
    Filter,
    NewSession,
}

struct PendingOpen {
    name: String,
    when: Instant,
}

struct PendingDelete {
    name: String,
    when: Instant,
}

struct PendingPaneDelete {
    session: String,
    pane_id: String,
    when: Instant,
}

struct PendingExport {
    name: String,
    when: Instant,
}

#[derive(Clone, Copy)]
enum ExportDepth {
    Compact,
    Medium,
    Full,
    Json,
}

impl ExportDepth {
    fn export_mode(self) -> ExportMode {
        match self {
            ExportDepth::Compact => ExportMode::Compact,
            ExportDepth::Medium => ExportMode::Medium,
            ExportDepth::Full => ExportMode::Full,
            ExportDepth::Json => ExportMode::Json,
        }
    }
}

struct SessionPreview {
    lines: Vec<String>,
    pane_ids: Vec<String>,
    selected_idx: usize,
}

struct App {
    all_sessions: Vec<SessionInfo>,
    items: Vec<SessionInfo>,
    filter: String,
    selected: usize,
    input_mode: InputMode,
    new_session_name: String,
    status_line: Option<(String, Instant)>,
    pending_open: Option<PendingOpen>,
    pending_delete: Option<PendingDelete>,
    pending_pane_delete: Option<PendingPaneDelete>,
    pending_export: Option<PendingExport>,
    show_help: bool,
    show_preview: bool,
    pending_g_until: Option<Instant>,
    preview_for: Option<String>,
    preview_lines: Vec<String>,
    preview_pane_ids: Vec<String>,
    preview_selected_idx: usize,
    preview_updated_at: Option<Instant>,
}

const COLOR_BG: Color = Color::Rgb(12, 16, 24);
const COLOR_PANEL: Color = Color::Rgb(20, 26, 36);
const COLOR_TEXT: Color = Color::Rgb(227, 233, 242);
const COLOR_MUTED: Color = Color::Rgb(129, 144, 168);
const COLOR_ACCENT: Color = Color::Rgb(110, 186, 255);
const COLOR_ACCENT_2: Color = Color::Rgb(107, 224, 186);
const COLOR_WARN: Color = Color::Rgb(255, 205, 125);
const COLOR_SELECTED_BG: Color = Color::Rgb(58, 108, 175);
const COLOR_SELECTED_FG: Color = Color::Rgb(245, 250, 255);

impl App {
    fn new() -> Self {
        let all_sessions = tmux_sessions_raw().unwrap_or_default();
        let items = filter_and_sort(&all_sessions, "");
        Self {
            all_sessions,
            items,
            filter: String::new(),
            selected: 0,
            input_mode: InputMode::Filter,
            new_session_name: String::new(),
            status_line: None,
            pending_open: None,
            pending_delete: None,
            pending_pane_delete: None,
            pending_export: None,
            show_help: true,
            show_preview: true,
            pending_g_until: None,
            preview_for: None,
            preview_lines: Vec::new(),
            preview_pane_ids: Vec::new(),
            preview_selected_idx: 0,
            preview_updated_at: None,
        }
    }

    fn refresh_items(&mut self) {
        self.items = filter_and_sort(&self.all_sessions, &self.filter);
        if self.items.is_empty() {
            self.selected = 0;
        } else {
            self.selected = self.selected.min(self.items.len() - 1);
        }
    }

    fn refresh_sessions(&mut self) {
        self.all_sessions = tmux_sessions_raw().unwrap_or_default();
        self.selected = 0;
        self.refresh_items();
        self.preview_for = None;
        self.preview_pane_ids.clear();
        self.preview_selected_idx = 0;
        self.preview_updated_at = None;
    }

    fn selected_name(&self) -> Option<&str> {
        self.items.get(self.selected).map(|s| s.name.as_str())
    }

    fn refresh_preview_if_needed(&mut self, force: bool) {
        if !self.show_preview {
            self.preview_for = None;
            self.preview_lines.clear();
            self.preview_pane_ids.clear();
            self.preview_selected_idx = 0;
            self.preview_updated_at = None;
            return;
        }

        let selected = match self.selected_name() {
            Some(name) => name.to_string(),
            None => {
                self.preview_for = None;
                self.preview_lines = vec!["No session selected".to_string()];
                self.preview_pane_ids.clear();
                self.preview_selected_idx = 0;
                self.preview_updated_at = Some(Instant::now());
                return;
            }
        };

        let stale = self
            .preview_updated_at
            .is_none_or(|t| Instant::now().duration_since(t) > Duration::from_secs(2));
        let changed = self.preview_for.as_deref() != Some(selected.as_str());
        if !force && !stale && !changed {
            return;
        }

        let selected_pane = self
            .preview_pane_ids
            .get(self.preview_selected_idx)
            .map(|s| s.as_str());

        let preview = session_preview_data(&selected, selected_pane).unwrap_or_else(|err| SessionPreview {
            lines: vec![
                format!("Preview unavailable: {}", err.msg.trim_end()),
                "(tmux info could not be fetched)".to_string(),
            ],
            pane_ids: Vec::new(),
            selected_idx: 0,
        });

        self.preview_lines = preview.lines;
        self.preview_pane_ids = preview.pane_ids;
        self.preview_selected_idx = preview
            .selected_idx
            .min(self.preview_pane_ids.len().saturating_sub(1));
        self.preview_for = Some(selected);
        self.preview_updated_at = Some(Instant::now());
    }

    fn step_preview_pane(&mut self, delta: isize) {
        if self.preview_pane_ids.is_empty() {
            self.set_status_for("No pane selected", Duration::from_millis(900));
            return;
        }

        let len = self.preview_pane_ids.len() as isize;
        let cur = self.preview_selected_idx as isize;
        let next = (cur + delta).rem_euclid(len) as usize;
        self.preview_selected_idx = next;
        self.refresh_preview_if_needed(true);
    }

    fn set_status_for(&mut self, msg: impl Into<String>, duration: Duration) {
        self.status_line = Some((msg.into(), Instant::now() + duration));
    }

    fn prune_timers(&mut self) {
        if let Some((_, until)) = &self.status_line {
            if Instant::now() >= *until {
                self.status_line = None;
            }
        }
        if let Some(pd) = &self.pending_delete {
            if Instant::now() >= pd.when {
                self.pending_delete = None;
            }
        }
        if let Some(ppd) = &self.pending_pane_delete {
            if Instant::now() >= ppd.when {
                self.pending_pane_delete = None;
            }
        }
        if let Some(pe) = &self.pending_export {
            if Instant::now() >= pe.when {
                self.pending_export = None;
            }
        }
        if let Some(until) = self.pending_g_until {
            if Instant::now() >= until {
                self.pending_g_until = None;
            }
        }
    }
}

pub fn run() -> i32 {
    match run_inner() {
        Ok(code) => code,
        Err(err) => {
            if !err.msg.is_empty() {
                eprint!("{}", err.msg);
            }
            err.code
        }
    }
}

fn run_inner() -> BrResult<i32> {
    let args: Vec<String> = env::args().collect();
    let mode = detect_mode(args.first());

    if args.len() > 1 {
        let arg = &args[1];
        if arg == "-h" || arg == "--help" {
            println!("{}", USAGE.trim());
            return Ok(0);
        }
        if arg == "--list" {
            for name in list_sessions()? {
                println!("{}", name);
            }
            return Ok(0);
        }

        let name = normalize_or_exit(arg)?;
        create_session_if_needed(&name, mode)?;
        return Ok(exec_attach_or_switch(&name));
    }

    match tui(mode)? {
        None => Ok(0),
        Some(name) => {
            create_session_if_needed(&name, mode)?;
            Ok(exec_attach_or_switch(&name))
        }
    }
}

fn verbose_enabled() -> bool {
    env::var_os("BR_VERBOSE").is_some()
}

fn detect_mode(argv0: Option<&String>) -> Mode {
    match env::var("BR_MODE").ok().as_deref() {
        Some("worktree") => return Mode::Worktree,
        Some("cwd") => return Mode::Cwd,
        _ => {}
    }

    let program_name = argv0
        .and_then(|s| Path::new(s).file_name())
        .and_then(|s| s.to_str())
        .unwrap_or("br");

    if program_name == "br" {
        Mode::Worktree
    } else {
        Mode::Cwd
    }
}

fn tmux_path() -> BrResult<&'static str> {
    TMUX_PATH
        .as_deref()
        .ok_or_else(|| ExitError::new(127, "Error: tmux not found in PATH.\n"))
}

fn inside_tmux() -> bool {
    env::var_os("TMUX").is_some()
}

fn run_capture(program: &str, args: &[String], cwd: Option<&Path>) -> BrResult<CmdResult> {
    if verbose_enabled() {
        let cwd_txt = cwd
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| env::current_dir().unwrap_or_else(|_| PathBuf::from(".")).display().to_string());
        eprintln!("[br] run: {} {} (cwd={})", program, args.join(" "), cwd_txt);
    }

    let mut cmd = Command::new(program);
    cmd.args(args);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    cmd.stdin(Stdio::null());

    let out = cmd.output().map_err(|err| {
        ExitError::new(
            1,
            format!("[br] failed to run {}: {}\n", program, err),
        )
    })?;

    Ok(CmdResult {
        code: out.status.code().unwrap_or(1),
        stdout: String::from_utf8_lossy(&out.stdout).to_string(),
        stderr: String::from_utf8_lossy(&out.stderr).to_string(),
    })
}

fn run_status(program: &str, args: &[String], cwd: Option<&Path>) -> BrResult<i32> {
    if verbose_enabled() {
        let cwd_txt = cwd
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| env::current_dir().unwrap_or_else(|_| PathBuf::from(".")).display().to_string());
        eprintln!("[br] run: {} {} (cwd={})", program, args.join(" "), cwd_txt);
    }

    let mut cmd = Command::new(program);
    cmd.args(args);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    cmd.stdin(Stdio::null());

    let status = cmd.status().map_err(|err| {
        ExitError::new(
            1,
            format!("[br] failed to run {}: {}\n", program, err),
        )
    })?;

    Ok(status.code().unwrap_or(1))
}

fn parse_i64(s: &str) -> i64 {
    s.trim().parse::<i64>().unwrap_or(0)
}

fn parse_tmux_session_line(line: &str) -> Option<SessionInfo> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }

    if trimmed.contains('\t') {
        let parts: Vec<&str> = trimmed.split('\t').collect();
        let name = parts.first().copied().unwrap_or("").trim();
        if name.is_empty() {
            return None;
        }
        return Some(SessionInfo::new(
            name.to_string(),
            parse_i64(parts.get(1).copied().unwrap_or("")),
            parse_i64(parts.get(2).copied().unwrap_or("")),
            parse_i64(parts.get(3).copied().unwrap_or("")),
        ));
    }

    // Fallback for tmux-compatible tools (like psmux) that may ignore -F and print:
    // "<name>: 1 windows (created ...)"
    let name = if let Some(idx) = trimmed.find(": ") {
        let (candidate, rest_with_sep) = trimmed.split_at(idx);
        let rest = &rest_with_sep[2..];
        let starts_with_count = rest
            .chars()
            .next()
            .map(|c| c.is_ascii_digit())
            .unwrap_or(false);
        if !candidate.trim().is_empty() && starts_with_count && rest.contains(" window") {
            candidate.trim().to_string()
        } else {
            trimmed.to_string()
        }
    } else {
        trimmed.to_string()
    };

    Some(SessionInfo::new(name, 0, 0, 0))
}

fn tmux_sessions_raw() -> BrResult<Vec<SessionInfo>> {
    let tmux = tmux_path()?;
    let fmt = "#{session_name}\t#{session_last_attached}\t#{session_activity}\t#{session_created}";
    let args = vec!["list-sessions".to_string(), "-F".to_string(), fmt.to_string()];
    let out = run_capture(tmux, &args, None)?;
    if out.code != 0 || out.stdout.is_empty() {
        return Ok(Vec::new());
    }

    let mut sessions = Vec::new();
    for line in out.stdout.lines() {
        if let Some(parsed) = parse_tmux_session_line(line) {
            sessions.push(parsed);
        }
    }

    Ok(sessions)
}

fn list_sessions() -> BrResult<Vec<String>> {
    let mut sessions = tmux_sessions_raw()?;
    sessions.sort_by(|a, b| {
        b.sort_ts()
            .cmp(&a.sort_ts())
            .then_with(|| a.name_lc.cmp(&b.name_lc))
    });
    Ok(sessions.into_iter().map(|s| s.name).collect())
}

fn session_exists(name: &str) -> BrResult<bool> {
    let tmux = tmux_path()?;
    let args = vec!["has-session".to_string(), "-t".to_string(), name.to_string()];
    let code = run_status(tmux, &args, None)?;
    Ok(code == 0)
}

fn tmux_capture(args: Vec<String>) -> BrResult<CmdResult> {
    let tmux = tmux_path()?;
    run_capture(tmux, &args, None)
}

fn extract_session_id_from_path(path: &str) -> Option<String> {
    SESSION_ID_RE.find(path).map(|m| m.as_str().to_string())
}

fn session_id_from_pid_fds(pid: i32) -> Option<String> {
    let fd_dir = PathBuf::from(format!("/proc/{}/fd", pid));
    let entries = fs::read_dir(fd_dir).ok()?;

    for entry in entries.flatten() {
        let Ok(target) = fs::read_link(entry.path()) else {
            continue;
        };
        let target_text = target.to_string_lossy();
        if !(target_text.contains("/.code/sessions/") && target_text.contains("rollout-")) {
            continue;
        }
        if let Some(id) = extract_session_id_from_path(&target_text) {
            return Some(id);
        }
    }

    None
}

fn normalize_path_for_compare(path: &str) -> String {
    let mut normalized = path.replace('\\', "/");
    while normalized.ends_with('/') && normalized.len() > 1 {
        normalized.pop();
    }
    if cfg!(windows) {
        normalized.to_ascii_lowercase()
    } else {
        normalized
    }
}

fn paths_match(a: &str, b: &str) -> bool {
    normalize_path_for_compare(a) == normalize_path_for_compare(b)
}

fn session_meta_cwd_from_rollout(path: &Path) -> Option<String> {
    let file = fs::File::open(path).ok()?;
    let reader = BufReader::new(file);
    for line in reader.lines() {
        let Ok(line) = line else {
            continue;
        };
        let Ok(obj) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if obj.get("type").and_then(Value::as_str) != Some("session_meta") {
            continue;
        }
        return obj
            .get("payload")
            .and_then(|p| p.get("cwd"))
            .and_then(Value::as_str)
            .map(ToString::to_string);
    }
    None
}

fn session_ids_from_cwd_in(cwd: &str, code_dir: &Path) -> Vec<String> {
    let catalog_path = code_dir.join("sessions/index/catalog.jsonl");
    let Ok(content) = fs::read_to_string(catalog_path) else {
        return session_ids_from_cwd_scan_rollouts(cwd, code_dir);
    };
    let mut out = Vec::new();
    let mut seen = HashSet::new();

    for line in content.lines().rev() {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(obj) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if obj.get("deleted").and_then(Value::as_bool).unwrap_or(false) {
            continue;
        }
        let Some(session_id) = obj.get("session_id").and_then(Value::as_str) else {
            continue;
        };
        let Some(rollout_rel) = obj.get("rollout_path").and_then(Value::as_str) else {
            continue;
        };
        let rollout = code_dir.join(rollout_rel);
        let Some(meta_cwd) = session_meta_cwd_from_rollout(&rollout) else {
            continue;
        };
        if paths_match(&meta_cwd, cwd) && seen.insert(session_id.to_string()) {
            out.push(session_id.to_string());
        }
    }

    if out.is_empty() {
        return session_ids_from_cwd_scan_rollouts(cwd, code_dir);
    }

    out
}

fn rollout_search_roots(code_dir: &Path) -> Vec<PathBuf> {
    let mut roots = Vec::new();

    let sessions_child = code_dir.join("sessions");
    if sessions_child.is_dir() {
        roots.push(sessions_child);
    }

    if code_dir
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.eq_ignore_ascii_case("sessions"))
    {
        roots.push(code_dir.to_path_buf());
    }

    if code_dir
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.eq_ignore_ascii_case("index"))
    {
        if let Some(parent) = code_dir.parent() {
            roots.push(parent.to_path_buf());
        }
    }

    if roots.is_empty() {
        roots.push(code_dir.to_path_buf());
    }

    let mut deduped = Vec::new();
    let mut seen = HashSet::new();
    for root in roots {
        let key = normalize_path_for_compare(&root.to_string_lossy());
        if seen.insert(key) {
            deduped.push(root);
        }
    }
    deduped
}

fn collect_rollout_paths(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(ft) = entry.file_type() else {
            continue;
        };

        if ft.is_dir() {
            collect_rollout_paths(&path, out);
            continue;
        }
        if !ft.is_file() {
            continue;
        }

        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.starts_with("rollout-") || !name.ends_with(".jsonl") {
            continue;
        }

        out.push(path);
    }
}

fn session_ids_from_cwd_scan_rollouts(cwd: &str, code_dir: &Path) -> Vec<String> {
    let mut rollouts = Vec::new();
    for root in rollout_search_roots(code_dir) {
        collect_rollout_paths(&root, &mut rollouts);
    }

    rollouts.sort_by(|a, b| b.cmp(a));

    let mut out = Vec::new();
    let mut seen = HashSet::new();

    for rollout in rollouts {
        let Some(meta_cwd) = session_meta_cwd_from_rollout(&rollout) else {
            continue;
        };
        if !paths_match(&meta_cwd, cwd) {
            continue;
        }

        let Some(path_str) = rollout.to_str() else {
            continue;
        };
        let Some(session_id) = extract_session_id_from_path(path_str) else {
            continue;
        };
        if seen.insert(session_id.clone()) {
            out.push(session_id);
        }
    }

    out
}

fn session_id_from_pane_paths(name: &str) -> Option<String> {
    let out = tmux_capture(vec![
        "list-panes".to_string(),
        "-t".to_string(),
        name.to_string(),
        "-F".to_string(),
        "#{pane_current_path}\t#{pane_active}".to_string(),
    ])
    .ok()?;

    if out.code != 0 {
        return None;
    }

    let code_dir = export_code_dir();

    let mut active = Vec::new();
    let mut other = Vec::new();
    let mut seen = HashSet::new();

    for row in out.stdout.lines() {
        let mut parts = row.split('\t');
        let cwd = parts.next().unwrap_or("").trim().to_string();
        let is_active = parts.next().unwrap_or("0") == "1";
        if cwd.is_empty() || !seen.insert(normalize_path_for_compare(&cwd)) {
            continue;
        }
        if is_active {
            active.push(cwd);
        } else {
            other.push(cwd);
        }
    }

    for cwd in active.into_iter().chain(other.into_iter()) {
        let matches = session_ids_from_cwd_in(&cwd, &code_dir);
        if matches.len() == 1 {
            return matches.first().cloned();
        }
    }

    None
}

fn session_id_from_tty(tty: &str) -> Option<String> {
    let tty_short = tty.strip_prefix("/dev/").unwrap_or(tty);
    let args = vec![
        "-t".to_string(),
        tty_short.to_string(),
        "-o".to_string(),
        "pid=".to_string(),
    ];
    let out = run_capture("ps", &args, None).ok()?;
    if out.code != 0 {
        return None;
    }

    for line in out.stdout.lines() {
        let Ok(pid) = line.trim().parse::<i32>() else {
            continue;
        };
        if let Some(id) = session_id_from_pid_fds(pid) {
            return Some(id);
        }
    }

    None
}

fn coder_session_id_for_tmux_session(name: &str) -> Option<String> {
    let out = tmux_capture(vec![
        "list-panes".to_string(),
        "-t".to_string(),
        name.to_string(),
        "-F".to_string(),
        "#{pane_tty}\t#{pane_active}".to_string(),
    ])
    .ok()?;

    if out.code != 0 {
        return None;
    }

    let mut active_ttys = Vec::new();
    let mut other_ttys = Vec::new();
    let mut seen = HashSet::new();

    for row in out.stdout.lines() {
        let mut parts = row.split('\t');
        let tty = parts.next().unwrap_or("").trim().to_string();
        let active = parts.next().unwrap_or("0") == "1";
        if tty.is_empty() || !seen.insert(tty.clone()) {
            continue;
        }

        if active {
            active_ttys.push(tty);
        } else {
            other_ttys.push(tty);
        }
    }

    for tty in active_ttys.into_iter().chain(other_ttys.into_iter()) {
        if let Some(id) = session_id_from_tty(&tty) {
            return Some(id);
        }
    }

    session_id_from_pane_paths(name)
}

fn export_out_path() -> PathBuf {
    match env::var("BR_EXPORT_OUT") {
        Ok(v) if !v.trim().is_empty() => expand_path(&v),
        _ => expand_path("~/coder-md"),
    }
}

fn export_code_dir() -> PathBuf {
    if let Ok(v) = env::var("BR_CODE_DIR") {
        if !v.trim().is_empty() {
            return expand_path(&v);
        }
    }
    if let Ok(v) = env::var("CX_CODE_DIR") {
        if !v.trim().is_empty() {
            return expand_path(&v);
        }
    }

    let code = expand_path("~/.code");
    if code.exists() {
        return code;
    }

    let codex = expand_path("~/.codex");
    if codex.exists() {
        return codex;
    }

    code
}

fn start_export_prompt(app: &mut App) {
    let Some(name) = app.selected_name().map(|s| s.to_string()) else {
        app.set_status_for("No session selected", Duration::from_millis(1000));
        return;
    };

    app.pending_export = Some(PendingExport {
        name: name.clone(),
        when: Instant::now() + Duration::from_secs(8),
    });
    app.set_status_for(
        format!(
            "Export '{}' as [1]compact [2]medium [3]full [4]json (Enter=medium, Esc cancel)",
            name
        ),
        Duration::from_secs(8),
    );
}

fn export_session_markdown(app: &mut App, name: &str, depth: ExportDepth) {
    app.pending_export = None;

    let Some(session_id) = coder_session_id_for_tmux_session(name) else {
        app.set_status_for(
            format!("No coder session id found for '{}'", name),
            Duration::from_millis(1800),
        );
        return;
    };

    let out_path = export_out_path();
    let code_dir = export_code_dir();

    match exporter::export_session_markdown(&session_id, &out_path, depth.export_mode(), &code_dir) {
        Ok(path) => {
            app.set_status_for(format!("Wrote: {}", path.display()), Duration::from_secs(3));
        }
        Err(err) => {
            app.set_status_for(format!("Export failed: {}", err), Duration::from_secs(3));
        }
    }
}

fn kill_session(name: &str) -> BrResult<bool> {
    let out = tmux_capture(vec![
        "kill-session".to_string(),
        "-t".to_string(),
        name.to_string(),
    ])?;
    Ok(out.code == 0)
}

fn kill_pane(session: &str, pane_id: &str) -> BrResult<bool> {
    let out = tmux_capture(vec![
        "kill-pane".to_string(),
        "-t".to_string(),
        format!("{}:{}", session, pane_id),
    ])?;
    Ok(out.code == 0)
}

fn session_preview_data(name: &str, selected_pane: Option<&str>) -> BrResult<SessionPreview> {
    let panes = tmux_capture(vec![
        "list-panes".to_string(),
        "-t".to_string(),
        name.to_string(),
        "-F".to_string(),
        "#{window_index}.#{pane_index}\t#{pane_active}".to_string(),
    ])?;

    if panes.code != 0 {
        return Ok(SessionPreview {
            lines: vec!["(pane preview unavailable)".to_string()],
            pane_ids: Vec::new(),
            selected_idx: 0,
        });
    }

    let mut pane_ids = Vec::new();
    let mut active_idx = 0usize;

    for row in panes.stdout.lines().take(24) {
        let mut parts = row.split('\t');
        let pane = parts.next().unwrap_or("?").trim().to_string();
        let active = parts.next().unwrap_or("0") == "1";
        if pane.is_empty() {
            continue;
        }
        if active {
            active_idx = pane_ids.len();
        }
        pane_ids.push(pane);
    }

    if pane_ids.is_empty() {
        return Ok(SessionPreview {
            lines: vec!["(no panes)".to_string()],
            pane_ids,
            selected_idx: 0,
        });
    }

    let mut selected_idx = selected_pane
        .and_then(|sel| pane_ids.iter().position(|pane| pane == sel))
        .unwrap_or(active_idx.min(pane_ids.len().saturating_sub(1)));

    if selected_idx >= pane_ids.len() {
        selected_idx = 0;
    }

    let pane = &pane_ids[selected_idx];
    let out = tmux_capture(vec![
        "capture-pane".to_string(),
        "-p".to_string(),
        "-t".to_string(),
        format!("{}:{}", name, pane),
        "-S".to_string(),
        "-60".to_string(),
    ])?;

    let lines = if out.code != 0 {
        vec!["(pane preview unavailable)".to_string()]
    } else {
        let captured: Vec<&str> = out.stdout.lines().collect();
        if captured.is_empty() {
            vec!["(pane is empty)".to_string()]
        } else {
            let tail_start = captured.len().saturating_sub(24);
            captured[tail_start..]
                .iter()
                .map(|line| (*line).to_string())
                .collect()
        }
    };

    Ok(SessionPreview {
        lines,
        pane_ids,
        selected_idx,
    })
}

fn render_start_cmd(name: &str) -> String {
    let template = env::var("BR_RUN_CMD").unwrap_or_else(|_| "coder".to_string());
    let mut vars = HashMap::new();
    vars.insert("name".to_string(), name.to_string());
    strfmt(&template, &vars).unwrap_or(template)
}

fn send_start_cmd(name: &str) -> BrResult<()> {
    let tmux = tmux_path()?;
    let cmd = render_start_cmd(name);
    let args = vec![
        "send-keys".to_string(),
        "-t".to_string(),
        name.to_string(),
        cmd,
        "C-m".to_string(),
    ];
    let _ = run_status(tmux, &args, None)?;
    Ok(())
}

fn exec_attach_or_switch(name: &str) -> i32 {
    let mut cmd = Command::new("tmux");
    if inside_tmux() {
        cmd.args(["switch-client", "-t", name]);
    } else {
        cmd.args(["attach", "-t", name]);
    }

    match cmd.status() {
        Ok(status) => status.code().unwrap_or(1),
        Err(err) => {
            eprintln!("[br] failed to run tmux: {}", err);
            127
        }
    }
}

fn normalize_session_name(raw: &str) -> String {
    let mut name = raw.trim().to_string();
    if name.is_empty() {
        return String::new();
    }

    name = INVALID_CHARS_RE.replace_all(&name, "_").to_string();
    name = DUP_UNDERSCORE_RE.replace_all(&name, "_").to_string();
    name = name.trim_matches('_').to_string();

    if name.is_empty() {
        return String::new();
    }

    if !VALID_NAME_RE.is_match(&name) {
        return String::new();
    }

    name
}

fn normalize_or_exit(raw: &str) -> BrResult<String> {
    let name = normalize_session_name(raw);
    if name.is_empty() {
        return Err(ExitError::new(
            2,
            format!(
                "Invalid session name: {}\nAllowed: letters, digits, . _ : -\n",
                raw
            ),
        ));
    }
    if name != raw {
        eprintln!("[br] Normalized session name: {} -> {}", raw, name);
    }
    Ok(name)
}

fn expand_path(path: &str) -> PathBuf {
    let mut input = path.to_string();
    if let Some(home) = home_dir_string() {
        if path == "~" {
            input = home;
        } else if let Some(rest) = path.strip_prefix("~/").or_else(|| path.strip_prefix("~\\")) {
            input = format!("{}/{}", home, rest);
        }
    }

    let expanded = shellexpand::full(&input)
        .map(|s| s.to_string())
        .unwrap_or(input);
    let p = PathBuf::from(expanded);
    if p.is_absolute() {
        p
    } else {
        env::current_dir().unwrap_or_else(|_| PathBuf::from(".")).join(p)
    }
}

fn home_dir_string() -> Option<String> {
    if let Ok(home) = env::var("HOME") {
        if !home.trim().is_empty() {
            return Some(home);
        }
    }

    if let Ok(profile) = env::var("USERPROFILE") {
        if !profile.trim().is_empty() {
            return Some(profile);
        }
    }

    let drive = env::var("HOMEDRIVE").ok();
    let path = env::var("HOMEPATH").ok();
    match (drive, path) {
        (Some(d), Some(p)) if !d.is_empty() && !p.is_empty() => Some(format!("{}{}", d, p)),
        _ => None,
    }
}

fn git_ok(repo: &Path) -> bool {
    let args = vec!["rev-parse".to_string(), "--is-inside-work-tree".to_string()];
    run_capture("git", &args, Some(repo))
        .map(|out| out.code == 0 && out.stdout.trim() == "true")
        .unwrap_or(false)
}

fn repo_root() -> Option<PathBuf> {
    if let Ok(env_repo) = env::var("BR_REPO") {
        let root = expand_path(&env_repo);
        if git_ok(&root) {
            return Some(root);
        }
        eprintln!("[br] BR_REPO={:?} is not a git repo; ignoring.", root);
    }

    let args = vec!["rev-parse".to_string(), "--show-toplevel".to_string()];
    match run_capture("git", &args, None) {
        Ok(out) if out.code == 0 => Some(PathBuf::from(out.stdout.trim())),
        _ => None,
    }
}

fn strict_base_ref(repo: &Path) -> BrResult<String> {
    let base = env::var("BR_BASE").unwrap_or_else(|_| "origin/main".to_string());

    let fetch_args = vec![
        "fetch".to_string(),
        "--prune".to_string(),
        "--tags".to_string(),
        "origin".to_string(),
    ];
    let fetch = run_capture("git", &fetch_args, Some(repo))?;
    if fetch.code != 0 {
        return Err(ExitError::new(
            2,
            "[br] git fetch origin failed; cannot ensure fresh main.\n",
        ));
    }

    let verify_args = vec![
        "rev-parse".to_string(),
        "--verify".to_string(),
        "--quiet".to_string(),
        base.clone(),
    ];
    let verify = run_status("git", &verify_args, Some(repo))?;
    if verify != 0 {
        return Err(ExitError::new(
            2,
            format!(
                "[br] base ref {:?} not found after fetch; set BR_BASE or fix remotes.\n",
                base
            ),
        ));
    }

    Ok(base)
}

fn branch_exists(repo: &Path, branch: &str) -> BrResult<bool> {
    let args = vec![
        "show-ref".to_string(),
        "--verify".to_string(),
        "--quiet".to_string(),
        format!("refs/heads/{}", branch),
    ];
    let code = run_status("git", &args, Some(repo))?;
    Ok(code == 0)
}

fn worktrees_base_dir(repo: &Path) -> PathBuf {
    match env::var("BR_WORKTREES_DIR") {
        Ok(v) if v.is_empty() => repo.parent().unwrap_or(repo).to_path_buf(),
        Ok(v) => expand_path(&v),
        Err(_) => expand_path("~/.br"),
    }
}

fn worktree_path(repo: &Path, name: &str) -> PathBuf {
    worktrees_base_dir(repo).join(format!("w-{}", name))
}

fn path_is_git(path: &Path) -> bool {
    path.join(".git").is_dir()
}

fn ensure_worktree_for_session(name: &str) -> BrResult<Option<PathBuf>> {
    let repo = match repo_root() {
        Some(repo) => repo,
        None => {
            if verbose_enabled() {
                eprintln!("[br] No git repo found; cannot create worktree.");
            }
            return Ok(None);
        }
    };

    let base_dir = worktrees_base_dir(&repo);
    fs::create_dir_all(&base_dir).map_err(|err| {
        ExitError::new(
            1,
            format!("[br] failed to create worktree dir {}: {}\n", base_dir.display(), err),
        )
    })?;

    let prefix = env::var("BR_PREFIX").unwrap_or_else(|_| "w/".to_string());
    let branch = format!("{}{}", prefix, name);
    let path = worktree_path(&repo, name);

    if path_is_git(&path) {
        if verbose_enabled() {
            eprintln!("[br] Reusing existing worktree at {}", path.display());
        }
        return Ok(Some(path));
    }

    if path.exists() {
        let is_empty_dir = path
            .is_dir()
            && fs::read_dir(&path)
                .map(|mut it| it.next().is_none())
                .unwrap_or(false);
        if !is_empty_dir {
            return Err(ExitError::new(
                2,
                format!(
                    "[br] Worktree target exists and is not empty: {}\n",
                    path.display()
                ),
            ));
        }
    }

    let base_ref = strict_base_ref(&repo)?;

    let add_args = if !branch_exists(&repo, &branch)? {
        vec![
            "worktree".to_string(),
            "add".to_string(),
            "-b".to_string(),
            branch.clone(),
            path.display().to_string(),
            base_ref,
        ]
    } else {
        vec![
            "worktree".to_string(),
            "add".to_string(),
            path.display().to_string(),
            branch,
        ]
    };

    let add = run_capture("git", &add_args, Some(&repo))?;
    if add.code != 0 {
        return Err(ExitError::new(
            2,
            format!("[br] git worktree add failed:\n{}\n", add.stderr.trim()),
        ));
    }

    Ok(Some(path))
}

fn create_session_if_needed(name: &str, mode: Mode) -> BrResult<()> {
    if session_exists(name)? {
        return Ok(());
    }

    let target_dir = match mode {
        Mode::Worktree => {
            let wt = ensure_worktree_for_session(name)?;
            wt.ok_or_else(|| {
                ExitError::new(
                    2,
                    "[br] Worktree mode requested but unavailable (not a repo or failed). Aborting.\n",
                )
            })?
        }
        Mode::Cwd => env::current_dir().map_err(|err| {
            ExitError::new(1, format!("[br] failed to get current dir: {}\n", err))
        })?,
    };

    let tmux = tmux_path()?;
    let args = vec![
        "new-session".to_string(),
        "-d".to_string(),
        "-s".to_string(),
        name.to_string(),
        "-c".to_string(),
        target_dir.display().to_string(),
    ];
    let code = run_status(tmux, &args, None)?;
    if code != 0 {
        return Err(ExitError::new(
            code,
            format!("[br] tmux new-session failed with code {}\n", code),
        ));
    }

    send_start_cmd(name)?;
    Ok(())
}

fn is_subsequence(query: &str, text: &str) -> (bool, i64) {
    let mut start = 0usize;
    let mut last: Option<usize> = None;
    let mut gap = 0i64;

    for ch in query.chars() {
        if let Some(pos_rel) = text[start..].find(ch) {
            let pos = start + pos_rel;
            if let Some(prev) = last {
                gap += (pos as i64 - prev as i64 - 1).max(0);
            }
            last = Some(pos);
            start = pos + ch.len_utf8();
        } else {
            return (false, 1_000_000_000);
        }
    }

    (true, gap)
}

fn fuzzy_score(query: &str, text: &str) -> i64 {
    if query.is_empty() {
        return 0;
    }
    if text == query {
        return 1_000_000;
    }
    if text.starts_with(query) {
        return 500_000 - text.len() as i64;
    }
    if let Some(idx) = text.find(query) {
        return 400_000 - (idx as i64) * 100 - text.len() as i64;
    }
    let (ok, gap) = is_subsequence(query, text);
    if ok {
        return 300_000 - gap * 10 - text.len() as i64;
    }
    -1_000_000_000
}

fn format_ago(ts: i64) -> String {
    if ts <= 0 {
        return "—".to_string();
    }

    let now = now_epoch_secs() as i64;
    let delta = (now - ts).max(0);
    if delta < 60 {
        return format!("{}s", delta);
    }
    let m = delta / 60;
    if m < 60 {
        return format!("{}m", m);
    }
    let h = m / 60;
    if h < 48 {
        return format!("{}h", h);
    }
    let d = h / 24;
    if d < 14 {
        return format!("{}d", d);
    }
    let w = d / 7;
    if w < 8 {
        return format!("{}w", w);
    }

    if let Some(dt_utc) = DateTime::from_timestamp(ts, 0) {
        let dt_local = dt_utc.with_timezone(&Local);
        return dt_local.format("%Y-%m-%d").to_string();
    }
    "old".to_string()
}

fn now_epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn age_color(ts: i64) -> Color {
    if ts <= 0 {
        return COLOR_MUTED;
    }
    let delta = (now_epoch_secs() as i64 - ts).max(0);
    if delta < 60 * 60 {
        COLOR_ACCENT_2
    } else if delta < 24 * 60 * 60 {
        COLOR_ACCENT
    } else if delta < 7 * 24 * 60 * 60 {
        COLOR_WARN
    } else {
        COLOR_MUTED
    }
}

fn filter_and_sort(sessions: &[SessionInfo], query: &str) -> Vec<SessionInfo> {
    if query.is_empty() {
        let mut out = sessions.to_vec();
        out.sort_by(|a, b| {
            b.sort_ts()
                .cmp(&a.sort_ts())
                .then_with(|| a.name_lc.cmp(&b.name_lc))
        });
        return out;
    }

    let q = query.to_lowercase();
    let mut scored: Vec<(i64, SessionInfo)> = sessions
        .iter()
        .filter_map(|s| {
            let score = fuzzy_score(&q, &s.name_lc);
            if score > -1_000_000_000 {
                Some((score, s.clone()))
            } else {
                None
            }
        })
        .collect();

    scored.sort_by(|(sa, a), (sb, b)| {
        sb.cmp(sa)
            .then_with(|| b.sort_ts().cmp(&a.sort_ts()))
            .then_with(|| a.name_lc.cmp(&b.name_lc))
    });

    scored.into_iter().map(|(_, s)| s).collect()
}

fn move_selection_up(app: &mut App, amount: usize) {
    if !app.items.is_empty() {
        app.selected = app.selected.saturating_sub(amount);
    }
}

fn move_selection_down(app: &mut App, amount: usize) {
    if !app.items.is_empty() {
        app.selected = (app.selected + amount).min(app.items.len().saturating_sub(1));
    }
}

fn delete_last_word(text: &mut String) {
    let mut chars: Vec<char> = text.chars().collect();
    while chars.last().is_some_and(|c| c.is_whitespace()) {
        chars.pop();
    }
    while chars.last().is_some_and(|c| !c.is_whitespace()) {
        chars.pop();
    }
    *text = chars.into_iter().collect();
}

fn clear_filter(app: &mut App) {
    if !app.filter.is_empty() {
        app.filter.clear();
        app.selected = 0;
        app.refresh_items();
    }
}

fn handle_gg(app: &mut App) {
    let now = Instant::now();
    if app.pending_g_until.is_some_and(|until| now < until) {
        app.selected = 0;
        app.pending_g_until = None;
        return;
    }
    app.pending_g_until = Some(now + Duration::from_millis(700));
}

fn request_open_from_raw(app: &mut App, raw: &str) -> Option<Option<String>> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }

    let normalized = normalize_session_name(raw);
    if normalized.is_empty() {
        app.set_status_for(
            "Invalid name. Allowed: letters, digits, . _ : -",
            Duration::from_millis(1100),
        );
        return None;
    }

    if normalized != raw {
        app.set_status_for(
            format!("Using session name: {}", normalized),
            Duration::from_millis(900),
        );
        app.pending_open = Some(PendingOpen {
            name: normalized,
            when: Instant::now() + Duration::from_millis(900),
        });
        return None;
    }

    Some(Some(normalized))
}

fn prompt_delete_selected_session(app: &mut App) {
    let Some(name) = app.selected_name().map(|s| s.to_string()) else {
        app.set_status_for("No session selected", Duration::from_millis(1000));
        return;
    };

    app.pending_delete = Some(PendingDelete {
        name: name.clone(),
        when: Instant::now() + Duration::from_secs(3),
    });
    app.set_status_for(
        format!("Delete session '{}' ? [y/N]", name),
        Duration::from_secs(3),
    );
}

fn kill_session_by_name(app: &mut App, name: String) {
    app.pending_delete = None;
    match kill_session(&name) {
        Ok(true) => {
            app.refresh_sessions();
            app.set_status_for(
                format!("Killed session '{}'.", name),
                Duration::from_millis(1200),
            );
        }
        Ok(false) => {
            app.set_status_for(
                format!("Failed to kill session '{}'.", name),
                Duration::from_millis(1400),
            );
        }
        Err(err) => {
            app.set_status_for(
                format!("Kill failed: {}", err.msg.trim_end()),
                Duration::from_millis(1600),
            );
        }
    }
}

fn maybe_kill_selected_session(app: &mut App, immediate: bool) {
    let Some(name) = app.selected_name().map(|s| s.to_string()) else {
        app.set_status_for("No session selected", Duration::from_millis(1000));
        return;
    };

    if !immediate {
        let now = Instant::now();
        if !app
            .pending_delete
            .as_ref()
            .is_some_and(|p| p.name == name && now < p.when)
        {
            app.pending_delete = Some(PendingDelete {
                name: name.clone(),
                when: now + Duration::from_secs(2),
            });
            app.set_status_for(
                format!("Press Ctrl+X again to kill session '{}'.", name),
                Duration::from_secs(2),
            );
            return;
        }
    }

    kill_session_by_name(app, name);
}

fn maybe_kill_selected_pane(app: &mut App, immediate: bool) {
    let Some(session) = app.selected_name().map(|s| s.to_string()) else {
        app.set_status_for("No session selected", Duration::from_millis(1000));
        return;
    };

    let Some(pane_id) = app.preview_pane_ids.get(app.preview_selected_idx).cloned() else {
        app.set_status_for(
            "No pane selected (open preview with F3).",
            Duration::from_millis(1200),
        );
        return;
    };

    if !immediate {
        let now = Instant::now();
        if !app.pending_pane_delete.as_ref().is_some_and(|p| {
            p.session == session && p.pane_id == pane_id && now < p.when
        }) {
            app.pending_pane_delete = Some(PendingPaneDelete {
                session: session.clone(),
                pane_id: pane_id.clone(),
                when: now + Duration::from_secs(2),
            });
            app.set_status_for(
                format!("Press Ctrl+Y again to kill pane '{}:{}'", session, pane_id),
                Duration::from_secs(2),
            );
            return;
        }
    }

    app.pending_pane_delete = None;
    match kill_pane(&session, &pane_id) {
        Ok(true) => {
            app.set_status_for(
                format!("Killed pane '{}:{}'", session, pane_id),
                Duration::from_millis(1200),
            );
            app.refresh_preview_if_needed(true);
        }
        Ok(false) => {
            app.set_status_for(
                format!("Failed to kill pane '{}:{}'", session, pane_id),
                Duration::from_millis(1500),
            );
        }
        Err(err) => {
            app.set_status_for(
                format!("Pane kill failed: {}", err.msg.trim_end()),
                Duration::from_millis(1700),
            );
        }
    }
}

fn handle_filter_mode(app: &mut App, key: KeyEvent) -> Option<Option<String>> {
    if !matches!(key.code, KeyCode::Char('g') | KeyCode::Char('G')) {
        app.pending_g_until = None;
    }

    if let Some(pending) = app.pending_export.as_ref() {
        if Instant::now() < pending.when {
            let name = pending.name.clone();
            match key.code {
                KeyCode::Enter => {
                    export_session_markdown(app, &name, ExportDepth::Medium);
                    return None;
                }
                KeyCode::Char('1') | KeyCode::Char('c') | KeyCode::Char('C') => {
                    export_session_markdown(app, &name, ExportDepth::Compact);
                    return None;
                }
                KeyCode::Char('2') | KeyCode::Char('m') | KeyCode::Char('M') => {
                    export_session_markdown(app, &name, ExportDepth::Medium);
                    return None;
                }
                KeyCode::Char('3') | KeyCode::Char('f') | KeyCode::Char('F') => {
                    export_session_markdown(app, &name, ExportDepth::Full);
                    return None;
                }
                KeyCode::Char('4') | KeyCode::Char('j') | KeyCode::Char('J') => {
                    export_session_markdown(app, &name, ExportDepth::Json);
                    return None;
                }
                KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Char('q') | KeyCode::Char('Q') => {
                    app.pending_export = None;
                    app.set_status_for("Export canceled.", Duration::from_millis(900));
                    return None;
                }
                _ => {
                    app.set_status_for(
                        "Choose Enter/1/2/3/4 or c/m/f/j (Esc cancel)",
                        Duration::from_millis(900),
                    );
                    return None;
                }
            }
        }

        app.pending_export = None;
    }

    if let Some(pending) = app.pending_delete.as_ref() {
        if Instant::now() < pending.when {
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    let name = pending.name.clone();
                    kill_session_by_name(app, name);
                    return None;
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                    app.pending_delete = None;
                    app.set_status_for("Delete canceled.", Duration::from_millis(900));
                    return None;
                }
                _ => {
                    app.set_status_for("Press y to delete, n/Esc to cancel.", Duration::from_millis(900));
                    return None;
                }
            }
        }

        app.pending_delete = None;
    }

    if key.modifiers.contains(KeyModifiers::ALT) {
        match key.code {
            KeyCode::Char('j') => {
                move_selection_down(app, 1);
                return None;
            }
            KeyCode::Char('k') => {
                move_selection_up(app, 1);
                return None;
            }
            _ => {}
        }
    }

    if key.modifiers.contains(KeyModifiers::CONTROL) {
        match key.code {
            KeyCode::Char('c') => return Some(None),
            KeyCode::Char('j') => {
                app.step_preview_pane(1);
                return None;
            }
            KeyCode::Char('k') => {
                app.step_preview_pane(-1);
                return None;
            }
            KeyCode::Char('y') => {
                maybe_kill_selected_pane(app, false);
                return None;
            }
            KeyCode::Char('x') => {
                maybe_kill_selected_session(app, false);
                return None;
            }
            KeyCode::Char('o') => {
                if !app.filter.trim().is_empty() {
                    let filter = app.filter.clone();
                    return request_open_from_raw(app, &filter);
                }
                if !app.items.is_empty() {
                    return Some(Some(app.items[app.selected].name.clone()));
                }
                return None;
            }
            KeyCode::Char('u') => {
                clear_filter(app);
                return None;
            }
            KeyCode::Char('w') => {
                if !app.filter.is_empty() {
                    delete_last_word(&mut app.filter);
                    app.selected = 0;
                    app.refresh_items();
                }
                return None;
            }
            KeyCode::Char('d') => {
                move_selection_down(app, 10);
                return None;
            }
            KeyCode::Char('b') => {
                move_selection_up(app, 10);
                return None;
            }
            KeyCode::Char('f') => {
                move_selection_down(app, 10);
                return None;
            }
            KeyCode::Char('n') => {
                move_selection_down(app, 1);
                return None;
            }
            KeyCode::Char('p') => {
                move_selection_up(app, 1);
                return None;
            }
            KeyCode::Char('a') => {
                app.selected = 0;
                return None;
            }
            KeyCode::Char('e') => {
                if !app.items.is_empty() {
                    app.selected = app.items.len() - 1;
                }
                return None;
            }
            KeyCode::Char('l') => {
                app.refresh_sessions();
                return None;
            }
            KeyCode::Char('r') => {
                app.refresh_sessions();
                return None;
            }
            KeyCode::Char('s') => {
                start_export_prompt(app);
                return None;
            }
            _ => {}
        }
    }

    match key.code {
        KeyCode::F(1) | KeyCode::Char('?') => {
            app.show_help = !app.show_help;
            None
        }
        KeyCode::F(2) => {
            app.input_mode = InputMode::NewSession;
            app.new_session_name.clear();
            None
        }
        KeyCode::F(3) => {
            app.show_preview = !app.show_preview;
            app.refresh_preview_if_needed(true);
            None
        }
        KeyCode::F(8) => {
            maybe_kill_selected_session(app, false);
            None
        }
        KeyCode::F(9) => {
            maybe_kill_selected_session(app, true);
            None
        }
        KeyCode::F(6) => {
            app.step_preview_pane(-1);
            None
        }
        KeyCode::F(7) => {
            maybe_kill_selected_pane(app, false);
            None
        }
        KeyCode::Char('q') | KeyCode::Char('Q') => Some(None),
        KeyCode::Char('r') | KeyCode::Char('R') | KeyCode::F(5) => {
            app.refresh_sessions();
            None
        }
        KeyCode::Up => {
            move_selection_up(app, 1);
            None
        }
        KeyCode::Down => {
            move_selection_down(app, 1);
            None
        }
        KeyCode::PageUp => {
            move_selection_up(app, 10);
            None
        }
        KeyCode::PageDown => {
            move_selection_down(app, 10);
            None
        }
        KeyCode::Home => {
            app.selected = 0;
            None
        }
        KeyCode::End => {
            if !app.items.is_empty() {
                app.selected = app.items.len() - 1;
            }
            None
        }
        KeyCode::Tab => {
            move_selection_down(app, 1);
            None
        }
        KeyCode::BackTab => {
            move_selection_up(app, 1);
            None
        }
        KeyCode::Char('g') if key.modifiers == KeyModifiers::NONE => {
            handle_gg(app);
            None
        }
        KeyCode::Char('j') if key.modifiers == KeyModifiers::NONE && app.filter.is_empty() => {
            move_selection_down(app, 1);
            None
        }
        KeyCode::Char('k') if key.modifiers == KeyModifiers::NONE && app.filter.is_empty() => {
            move_selection_up(app, 1);
            None
        }
        KeyCode::Char('G') => {
            if !app.items.is_empty() {
                app.selected = app.items.len() - 1;
            }
            None
        }
        KeyCode::Enter | KeyCode::Char(' ') => {
            if app.items.is_empty() {
                None
            } else {
                Some(Some(app.items[app.selected].name.clone()))
            }
        }
        KeyCode::Esc => {
            clear_filter(app);
            None
        }
        KeyCode::Backspace | KeyCode::Delete => {
            if !app.filter.is_empty() {
                app.filter.pop();
                app.selected = 0;
                app.refresh_items();
            } else if key.code == KeyCode::Backspace && key.modifiers == KeyModifiers::NONE {
                prompt_delete_selected_session(app);
            }
            None
        }
        KeyCode::Char('n') | KeyCode::Char('N') => {
            app.input_mode = InputMode::NewSession;
            app.new_session_name.clear();
            None
        }
        KeyCode::Char('/') if key.modifiers == KeyModifiers::NONE => {
            if !app.filter.is_empty() {
                clear_filter(app);
            }
            None
        }
        KeyCode::Char(c)
            if !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER)
                && (c.is_ascii_graphic() || c == ' ') =>
        {
            app.filter.push(c);
            app.selected = 0;
            app.refresh_items();

            if !app.filter.is_empty() {
                let filter_lc = app.filter.to_lowercase();
                if let Some(exact) = app
                    .items
                    .iter()
                    .find(|s| s.name_lc == filter_lc)
                    .map(|s| s.name.clone())
                {
                    return Some(Some(exact));
                }
                if app.items.len() == 1 {
                    return Some(Some(app.items[0].name.clone()));
                }
            }
            None
        }
        _ => None,
    }
}

fn handle_new_session_mode(app: &mut App, key: KeyEvent) -> Option<Option<String>> {
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        match key.code {
            KeyCode::Char('c') => return Some(None),
            KeyCode::Char('u') => {
                app.new_session_name.clear();
                return None;
            }
            KeyCode::Char('w') => {
                delete_last_word(&mut app.new_session_name);
                return None;
            }
            KeyCode::Backspace => {
                delete_last_word(&mut app.new_session_name);
                return None;
            }
            _ => {}
        }
    }

    match key.code {
        KeyCode::Esc => {
            app.input_mode = InputMode::Filter;
            app.new_session_name.clear();
            None
        }
        KeyCode::Backspace | KeyCode::Delete => {
            app.new_session_name.pop();
            None
        }
        KeyCode::Enter => {
            let raw = app.new_session_name.trim().to_string();
            app.input_mode = InputMode::Filter;
            app.new_session_name.clear();

            request_open_from_raw(app, &raw)
        }
        KeyCode::Char(c)
            if !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER)
                && (c.is_ascii_graphic() || c == ' ') =>
        {
            app.new_session_name.push(c);
            None
        }
        _ => None,
    }
}

fn desired_sessions_panel_width(items: &[SessionInfo]) -> u16 {
    let max_name_len = items
        .iter()
        .map(|s| s.name.chars().count())
        .max()
        .unwrap_or(12)
        .clamp(12, 44);

    let ago_col = 10usize;
    let content_width = max_name_len + 2 + ago_col;
    (content_width + 4) as u16
}

fn ellipsize(text: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }

    let char_count = text.chars().count();
    if char_count <= max_chars {
        return text.to_string();
    }

    if max_chars == 1 {
        return "…".to_string();
    }

    let mut out: String = text.chars().take(max_chars - 1).collect();
    out.push('…');
    out
}

fn centered_rect(area: Rect, width: u16, height: u16) -> Rect {
    let w = width.max(12).min(area.width.saturating_sub(2).max(1));
    let h = height.max(5).min(area.height.saturating_sub(2).max(1));
    let x = area.x + area.width.saturating_sub(w) / 2;
    let y = area.y + area.height.saturating_sub(h) / 2;
    Rect {
        x,
        y,
        width: w,
        height: h,
    }
}

fn modal_content(app: &App) -> Option<(String, Vec<String>)> {
    let now = Instant::now();
    if let Some(pending) = app.pending_export.as_ref().filter(|p| now < p.when) {
        return Some((
            "Export Markdown".to_string(),
            vec![
                format!("Session: {}", pending.name),
                "".to_string(),
                "Enter / 2 / m  -> medium".to_string(),
                "1 / c          -> compact".to_string(),
                "3 / f          -> full".to_string(),
                "4 / j          -> json".to_string(),
                "Esc / n / q    -> cancel".to_string(),
            ],
        ));
    }

    if let Some(pending) = app.pending_delete.as_ref().filter(|p| now < p.when) {
        return Some((
            "Confirm Delete".to_string(),
            vec![
                format!("Delete session '{}' ?", pending.name),
                "".to_string(),
                "y -> delete".to_string(),
                "n / Esc -> cancel".to_string(),
            ],
        ));
    }

    None
}

fn draw_modal_overlay(frame: &mut Frame<'_>, app: &App) {
    let Some((title, lines)) = modal_content(app) else {
        return;
    };

    let area = frame.area();
    let max_line = lines.iter().map(|l| l.chars().count()).max().unwrap_or(24);
    let width = (max_line + 8).clamp(40, 72) as u16;
    let height = (lines.len() + 4).clamp(7, 16) as u16;
    let popup = centered_rect(area, width, height);

    frame.render_widget(Clear, popup);

    let block = Block::default()
        .title(format!(" {} ", title))
        .title_style(Style::default().fg(COLOR_WARN).add_modifier(Modifier::BOLD))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(COLOR_WARN))
        .style(Style::default().bg(COLOR_PANEL));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let content: Vec<Line<'_>> = lines.into_iter().map(Line::from).collect();
    frame.render_widget(
        Paragraph::new(content).style(Style::default().fg(COLOR_TEXT)),
        inner,
    );
}

fn draw_ui(frame: &mut Frame<'_>, app: &mut App) {
    let area = frame.area();
    frame.render_widget(
        Block::default().style(Style::default().bg(COLOR_BG).fg(COLOR_TEXT)),
        area,
    );

    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .margin(1)
    .split(area);

    let title = Paragraph::new("br — tmux sessions")
        .style(Style::default().fg(COLOR_ACCENT).add_modifier(Modifier::BOLD));
    frame.render_widget(title, chunks[0]);

    let help_1_text = if app.show_help {
        "Enter/Space open  ↑/↓ move  j/k move  gg top  Shift+G bottom  Tab/Shift+Tab move  PgUp/PgDn jump"
    } else {
        "F1/? help  Enter/Space open  n/F2 new  F3 preview  F7 pane-kill  F8/F9 sess-kill  r refresh  q quit"
    };
    let help_1 = Paragraph::new(help_1_text).style(Style::default().fg(COLOR_MUTED));
    frame.render_widget(help_1, chunks[1]);

    let help_2_text = if app.show_help {
        "Type filter  / fresh-search  Backspace + y/n delete  Ctrl+S export-md chooser  Ctrl+W word-del  Ctrl+U clear  Ctrl+O open/create  Ctrl+J/K pane  Ctrl+Y pane-kill  Ctrl+X/F8 kill  F9 force"
    } else {
        ""
    };
    let help_2 = Paragraph::new(help_2_text).style(Style::default().fg(COLOR_MUTED));
    frame.render_widget(help_2, chunks[2]);

    let (filter_text, filter_style) = match app.input_mode {
        InputMode::Filter => {
            if app.filter.is_empty() {
                (
                    "Filter: (none)".to_string(),
                    Style::default().fg(COLOR_ACCENT_2),
                )
            } else {
                (
                    format!("Filter: {}", app.filter),
                    Style::default().fg(COLOR_ACCENT_2),
                )
            }
        }
        InputMode::NewSession => (
            format!(
                "New session name: {}  (Enter create, Esc cancel)",
                app.new_session_name
            ),
            Style::default().fg(COLOR_WARN).add_modifier(Modifier::BOLD),
        ),
    };
    frame.render_widget(Paragraph::new(filter_text).style(filter_style), chunks[3]);

    frame.render_widget(
        Paragraph::new("─".repeat(chunks[4].width as usize)).style(Style::default().fg(COLOR_MUTED)),
        chunks[4],
    );

    let content_area = chunks[5];
    let content_chunks = if app.show_preview {
        let total_w = content_area.width;
        let min_preview_w = 36u16;
        let desired_sessions_w = desired_sessions_panel_width(&app.items);

        let mut sessions_w = desired_sessions_w
            .max(20)
            .min(total_w.saturating_sub(1).max(1));

        if total_w > min_preview_w + 20 {
            sessions_w = sessions_w.min(total_w - min_preview_w);
        }

        Layout::horizontal([Constraint::Length(sessions_w), Constraint::Min(1)]).split(content_area)
    } else {
        Layout::horizontal([Constraint::Percentage(100)]).split(content_area)
    };

    let list_area = content_chunks[0];
    let content_width = list_area.width.saturating_sub(4) as usize;
    let ago_col_w = 10usize.min(content_width.saturating_sub(8));
    let name_col_w = content_width.saturating_sub(2 + ago_col_w).max(8);

    let items: Vec<ListItem<'_>> = if app.items.is_empty() {
        vec![ListItem::new(
            Line::from("(no sessions)").style(Style::default().fg(COLOR_MUTED)),
        )]
    } else {
        app.items
            .iter()
            .map(|s| {
                let name = ellipsize(&s.name, name_col_w);
                let ago = format_ago(s.sort_ts());
                let header = Line::from(vec![
                    Span::styled(
                        format!("{:<name_w$}", name, name_w = name_col_w),
                        Style::default().fg(COLOR_TEXT),
                    ),
                    Span::raw("  "),
                    Span::styled(
                        format!("{:>ago_w$}", ago, ago_w = ago_col_w),
                        Style::default().fg(age_color(s.sort_ts())),
                    ),
                ]);
                ListItem::new(header).style(Style::default().fg(COLOR_TEXT))
            })
            .collect()
    };

    let list = List::new(items)
        .block(
            Block::default()
                .title(" Sessions ")
                .title_style(Style::default().fg(COLOR_ACCENT).add_modifier(Modifier::BOLD))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(COLOR_ACCENT))
                .style(Style::default().bg(COLOR_PANEL)),
        )
        .highlight_style(
            Style::default()
                .bg(COLOR_SELECTED_BG)
                .fg(COLOR_SELECTED_FG)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▸ ");

    let mut list_state = ListState::default();
    if !app.items.is_empty() {
        list_state.select(Some(app.selected.min(app.items.len().saturating_sub(1))));
    }
    frame.render_stateful_widget(list, list_area, &mut list_state);

    if app.show_preview && content_chunks.len() > 1 {
        let preview_items: Vec<ListItem<'_>> = if app.preview_lines.is_empty() {
            vec![ListItem::new(Line::from("(no preview)").style(Style::default().fg(COLOR_MUTED)))]
        } else {
            app.preview_lines
                .iter()
                .map(|line| ListItem::new(Line::from(line.clone())).style(Style::default().fg(COLOR_TEXT)))
                .collect()
        };

        let preview = List::new(preview_items).block(
            Block::default()
                .title(" Preview ")
                .title_style(Style::default().fg(COLOR_ACCENT_2).add_modifier(Modifier::BOLD))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(COLOR_ACCENT_2))
                .style(Style::default().bg(COLOR_PANEL)),
        );
        frame.render_widget(preview, content_chunks[1]);
    }

    let (status, status_style) = if let Some((msg, _)) = &app.status_line {
        (msg.clone(), Style::default().fg(COLOR_WARN))
    } else {
        (
            format!("{} session(s) — sorted by most recent", app.items.len()),
            Style::default().fg(COLOR_MUTED),
        )
    };
    frame.render_widget(Paragraph::new(status).style(status_style), chunks[6]);

    draw_modal_overlay(frame, app);
}

fn setup_terminal() -> BrResult<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode().map_err(|err| ExitError::new(1, format!("[br] failed to enable raw mode: {}\n", err)))?;

    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)
        .map_err(|err| ExitError::new(1, format!("[br] failed to enter alt screen: {}\n", err)))?;

    Terminal::new(CrosstermBackend::new(stdout))
        .map_err(|err| ExitError::new(1, format!("[br] failed to setup terminal: {}\n", err)))
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<Stdout>>) {
    let _ = disable_raw_mode();
    let _ = execute!(terminal.backend_mut(), LeaveAlternateScreen);
    let _ = terminal.show_cursor();
}

fn tui(_mode: Mode) -> BrResult<Option<String>> {
    let mut terminal = setup_terminal()?;
    let mut app = App::new();
    app.refresh_preview_if_needed(true);

    let result = loop {
        app.prune_timers();
        app.refresh_preview_if_needed(false);

        if let Some(pending) = &app.pending_open {
            if Instant::now() >= pending.when {
                break Ok(Some(pending.name.clone()));
            }
        }

        terminal
            .draw(|frame| draw_ui(frame, &mut app))
            .map_err(|err| ExitError::new(1, format!("[br] failed to draw UI: {}\n", err)))?;

        if event::poll(Duration::from_millis(300))
            .map_err(|err| ExitError::new(1, format!("[br] event poll failed: {}\n", err)))?
        {
            if let Event::Key(key_event) = event::read()
                .map_err(|err| ExitError::new(1, format!("[br] event read failed: {}\n", err)))?
            {
                if key_event.kind != KeyEventKind::Press {
                    continue;
                }

                let action = match app.input_mode {
                    InputMode::Filter => handle_filter_mode(&mut app, key_event),
                    InputMode::NewSession => handle_new_session_mode(&mut app, key_event),
                };

                app.refresh_preview_if_needed(false);

                if let Some(next) = action {
                    break Ok(next);
                }
            }
        }
    };

    restore_terminal(&mut terminal);
    result
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::{
        desired_sessions_panel_width, extract_session_id_from_path, fuzzy_score,
        handle_filter_mode, handle_new_session_mode, normalize_session_name,
        parse_tmux_session_line, paths_match, session_ids_from_cwd_in, App, InputMode,
        SessionInfo,
    };

    fn test_app() -> App {
        let mut app = App {
            all_sessions: vec![
                SessionInfo::new("alpha".to_string(), 0, 0, 0),
                SessionInfo::new("beta".to_string(), 0, 0, 0),
            ],
            items: vec![],
            filter: String::new(),
            selected: 0,
            input_mode: InputMode::Filter,
            new_session_name: String::new(),
            status_line: None,
            pending_open: None,
            pending_delete: None,
            pending_pane_delete: None,
            pending_export: None,
            show_help: true,
            show_preview: false,
            pending_g_until: None,
            preview_for: None,
            preview_lines: Vec::new(),
            preview_pane_ids: Vec::new(),
            preview_selected_idx: 0,
            preview_updated_at: None,
        };
        app.refresh_items();
        app
    }

    #[test]
    fn normalize_name_basic() {
        assert_eq!(normalize_session_name("test"), "test");
        assert_eq!(normalize_session_name(" bad/name "), "bad_name");
        assert_eq!(normalize_session_name("..."), "...");
    }

    #[test]
    fn normalize_name_invalid() {
        assert_eq!(normalize_session_name("   "), "");
        assert_eq!(normalize_session_name("!!!"), "");
    }

    #[test]
    fn fuzzy_ranking_prefers_exact_and_prefix() {
        let exact = fuzzy_score("abc", "abc");
        let prefix = fuzzy_score("abc", "abcdef");
        let contains = fuzzy_score("abc", "xabcx");
        assert!(exact > prefix);
        assert!(prefix > contains);
    }

    #[test]
    fn ctrl_u_clears_filter() {
        let mut app = test_app();
        app.filter = "alpha".to_string();
        app.refresh_items();

        let _ = handle_filter_mode(
            &mut app,
            KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL),
        );

        assert_eq!(app.filter, "");
    }

    #[test]
    fn tab_moves_selection_down() {
        let mut app = test_app();
        assert_eq!(app.selected, 0);

        let _ = handle_filter_mode(&mut app, KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));

        assert_eq!(app.selected, 1);
    }

    #[test]
    fn ctrl_w_deletes_word_in_new_session_mode() {
        let mut app = test_app();
        app.input_mode = InputMode::NewSession;
        app.new_session_name = "feature branch".to_string();

        let _ = handle_new_session_mode(
            &mut app,
            KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL),
        );

        assert_eq!(app.new_session_name, "feature ");
    }

    #[test]
    fn slash_starts_fuzzy_search_without_literal_prefix() {
        let mut app = test_app();
        app.filter = "alpha".to_string();
        app.refresh_items();

        let _ = handle_filter_mode(&mut app, KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
        assert_eq!(app.filter, "");

        let _ = handle_filter_mode(&mut app, KeyEvent::new(KeyCode::Char('b'), KeyModifiers::NONE));
        assert_eq!(app.filter, "b");
        assert_eq!(app.items.len(), 1);
        assert_eq!(app.items[0].name, "beta");
    }

    #[test]
    fn backspace_then_n_cancels_delete_prompt() {
        let mut app = test_app();
        app.filter.clear();

        let _ = handle_filter_mode(
            &mut app,
            KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
        );
        assert!(app.pending_delete.is_some());

        let _ = handle_filter_mode(
            &mut app,
            KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE),
        );
        assert!(app.pending_delete.is_none());
    }

    #[test]
    fn ctrl_s_opens_export_prompt_and_esc_cancels() {
        let mut app = test_app();

        let _ = handle_filter_mode(
            &mut app,
            KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL),
        );
        assert!(app.pending_export.is_some());

        let _ = handle_filter_mode(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.pending_export.is_none());
    }

    #[test]
    fn export_prompt_enter_defaults_to_medium() {
        let mut app = test_app();

        let _ = handle_filter_mode(
            &mut app,
            KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL),
        );
        assert!(app.pending_export.is_some());

        let _ = handle_filter_mode(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.pending_export.is_none());
        assert!(app.status_line.is_some());
    }

    #[test]
    fn desired_session_width_grows_for_longer_names() {
        let short = vec![SessionInfo::new("abc".to_string(), 0, 0, 0)];
        let long = vec![SessionInfo::new("a-very-long-session-name".to_string(), 0, 0, 0)];

        assert!(desired_sessions_panel_width(&long) > desired_sessions_panel_width(&short));
    }

    #[test]
    fn desired_session_width_is_clamped() {
        let extreme = vec![SessionInfo::new("x".repeat(500), 0, 0, 0)];
        assert_eq!(desired_sessions_panel_width(&extreme), 60);
    }

    #[test]
    fn extracts_session_id_from_rollout_path() {
        let path = "/home/wavy/.code/sessions/2026/03/06/rollout-2026-03-06T15-08-26-5567a4cb-9214-4a08-a377-28bb71eb5b44.jsonl";
        assert_eq!(
            extract_session_id_from_path(path).as_deref(),
            Some("5567a4cb-9214-4a08-a377-28bb71eb5b44")
        );
    }

    #[test]
    fn path_compare_ignores_trailing_slash() {
        assert!(paths_match("/home/wavy/project/", "/home/wavy/project"));
    }

    #[test]
    fn path_compare_normalizes_separator() {
        assert!(paths_match("C:/Users/test/.code", "C:\\Users\\test\\.code"));
    }

    #[test]
    fn session_ids_from_cwd_returns_unique_match_only() {
        let uniq = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("b-revamp-cwd-unique-{}", uniq));
        let code_dir = root.join(".code");
        let catalog_dir = code_dir.join("sessions/index");
        let rollout_dir = code_dir.join("sessions/2026/03/07");
        fs::create_dir_all(&catalog_dir).expect("catalog dir");
        fs::create_dir_all(&rollout_dir).expect("rollout dir");

        let sid = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa";
        let rollout_rel = format!("sessions/2026/03/07/rollout-{}.jsonl", sid);
        let rollout_path = code_dir.join(&rollout_rel);
        fs::write(
            &rollout_path,
            "{\"type\":\"session_meta\",\"payload\":{\"cwd\":\"C:/Users/test/project\"}}\n",
        )
        .expect("rollout write");
        fs::write(
            catalog_dir.join("catalog.jsonl"),
            format!(
                "{{\"session_id\":\"{}\",\"rollout_path\":\"{}\",\"deleted\":false}}\n",
                sid, rollout_rel
            ),
        )
        .expect("catalog write");

        let matches = session_ids_from_cwd_in("C:\\Users\\test\\project", &code_dir);
        assert_eq!(matches, vec![sid.to_string()]);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn session_ids_from_cwd_disambiguates_shared_cwd() {
        let uniq = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("b-revamp-cwd-ambiguous-{}", uniq));
        let code_dir = root.join(".code");
        let catalog_dir = code_dir.join("sessions/index");
        let rollout_dir = code_dir.join("sessions/2026/03/07");
        fs::create_dir_all(&catalog_dir).expect("catalog dir");
        fs::create_dir_all(&rollout_dir).expect("rollout dir");

        let sid_a = "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb";
        let sid_b = "cccccccc-cccc-cccc-cccc-cccccccccccc";
        let rel_a = format!("sessions/2026/03/07/rollout-{}.jsonl", sid_a);
        let rel_b = format!("sessions/2026/03/07/rollout-{}.jsonl", sid_b);

        fs::write(
            code_dir.join(&rel_a),
            "{\"type\":\"session_meta\",\"payload\":{\"cwd\":\"/shared/repo\"}}\n",
        )
        .expect("rollout a");
        fs::write(
            code_dir.join(&rel_b),
            "{\"type\":\"session_meta\",\"payload\":{\"cwd\":\"/shared/repo\"}}\n",
        )
        .expect("rollout b");

        let catalog = format!(
            "{{\"session_id\":\"{}\",\"rollout_path\":\"{}\",\"deleted\":false}}\n{{\"session_id\":\"{}\",\"rollout_path\":\"{}\",\"deleted\":false}}\n",
            sid_a, rel_a, sid_b, rel_b
        );
        fs::write(catalog_dir.join("catalog.jsonl"), catalog).expect("catalog write");

        let matches = session_ids_from_cwd_in("/shared/repo", &code_dir);
        assert_eq!(matches.len(), 2);
        assert!(matches.contains(&sid_a.to_string()));
        assert!(matches.contains(&sid_b.to_string()));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn session_ids_from_cwd_scans_rollouts_when_catalog_missing() {
        let uniq = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("b-revamp-cwd-scan-fallback-{}", uniq));
        let code_dir = root.join(".code");
        let rollout_dir = code_dir.join("sessions/2026/03/18");
        fs::create_dir_all(&rollout_dir).expect("rollout dir");

        let sid = "dddddddd-eeee-ffff-1111-222222222222";
        let rollout = rollout_dir.join(format!("rollout-2026-03-18T12-00-00-{}.jsonl", sid));
        fs::write(
            &rollout,
            "{\"type\":\"session_meta\",\"payload\":{\"cwd\":\"C:/Users/test/project\"}}\n",
        )
        .expect("rollout write");

        let matches = session_ids_from_cwd_in("C:\\Users\\test\\project", &code_dir);
        assert_eq!(matches, vec![sid.to_string()]);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn parse_tmux_session_line_prefers_formatted_fields() {
        let line = "alpha\t10\t20\t30";
        let parsed = parse_tmux_session_line(line).expect("parsed");
        assert_eq!(parsed.name, "alpha");
        assert_eq!(parsed.last_attached, 10);
        assert_eq!(parsed.activity, 20);
        assert_eq!(parsed.created, 30);
    }

    #[test]
    fn parse_tmux_session_line_falls_back_for_default_output() {
        let line = "test-thing: 1 windows (created Wed Mar 18 12:51:39 2026)";
        let parsed = parse_tmux_session_line(line).expect("parsed");
        assert_eq!(parsed.name, "test-thing");
        assert_eq!(parsed.last_attached, 0);
        assert_eq!(parsed.activity, 0);
        assert_eq!(parsed.created, 0);
    }
}
