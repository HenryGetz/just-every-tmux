use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

use chrono::Utc;
use serde_json::Value;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExportMode {
    Compact,
    Medium,
    Full,
    Json,
}

#[derive(Clone, Debug)]
struct MediumToolCall {
    name: String,
    call_markdown: String,
    timestamp: String,
}

impl ExportMode {
    fn as_str(self) -> &'static str {
        match self {
            ExportMode::Compact => "compact",
            ExportMode::Medium => "medium",
            ExportMode::Full => "full",
            ExportMode::Json => "json",
        }
    }
}

pub fn export_session_markdown(
    session_id: &str,
    out_path: &Path,
    mode: ExportMode,
    code_dir: &Path,
) -> Result<PathBuf, String> {
    let rollout = rollout_path_for_session(session_id, code_dir)?;
    if !rollout.exists() {
        return Err(format!("Rollout file not found: {}", rollout.display()));
    }

    let out_file = resolve_output_path(session_id, out_path)?;
    let session_meta = first_session_meta(&rollout)?;

    let mut out = BufWriter::new(
        File::create(&out_file).map_err(|e| format!("failed to create {}: {}", out_file.display(), e))?,
    );

    writeln!(out, "# Chat Transcript\n").map_err(|e| e.to_string())?;
    writeln!(out, "Session ID: {}\n", session_id).map_err(|e| e.to_string())?;
    writeln!(out, "Export Mode: {}", mode.as_str()).map_err(|e| e.to_string())?;
    writeln!(out, "Exported At (UTC): {}\n", Utc::now().to_rfc3339()).map_err(|e| e.to_string())?;
    if let Some(meta) = session_meta {
        writeln!(out, "Session Meta:").map_err(|e| e.to_string())?;
        for key in ["timestamp", "cwd", "originator", "cli_version"] {
            if let Some(val) = meta.get(key) {
                writeln!(out, "- {}: {}", key, value_as_string(val)).map_err(|e| e.to_string())?;
            }
        }
        writeln!(out).map_err(|e| e.to_string())?;
    }
    writeln!(out, "---\n").map_err(|e| e.to_string())?;

    let file = File::open(&rollout).map_err(|e| format!("failed to open {}: {}", rollout.display(), e))?;
    let reader = BufReader::new(file);
    let mut image_idx = 0usize;
    let mut last_written_block: Option<String> = None;
    let mut pending_medium_calls: HashMap<String, MediumToolCall> = HashMap::new();
    let mut medium_output_sigs: HashMap<String, String> = HashMap::new();
    let mut calls_with_output: HashSet<String> = HashSet::new();
    let mut last_medium_reasoning_key: Option<String> = None;

    for line in reader.lines() {
        let line = line.map_err(|e| e.to_string())?;
        let obj: Value = serde_json::from_str(&line).map_err(|e| format!("invalid jsonl in {}: {}", rollout.display(), e))?;
        let ts = obj.get("timestamp").and_then(Value::as_str).unwrap_or("");
        let otype = obj.get("type").and_then(Value::as_str).unwrap_or("");

        match mode {
            ExportMode::Compact => {
                if otype != "response_item" {
                    continue;
                }
                if let Some(rendered) = render_compact_response_item(ts, obj.get("payload"), &mut image_idx) {
                    if last_written_block.as_deref() == Some(rendered.as_str()) {
                        continue;
                    }
                    out.write_all(rendered.as_bytes()).map_err(|e| e.to_string())?;
                    last_written_block = Some(rendered);
                }
            }
            export_mode @ (ExportMode::Medium | ExportMode::Full) => {
                let detailed_calls = matches!(export_mode, ExportMode::Full);
                if otype == "response_item" {
                    let payload = obj.get("payload");
                    let ptype = payload
                        .and_then(|p| p.get("type"))
                        .and_then(Value::as_str)
                        .unwrap_or("");

                    if ptype == "function_call" {
                        if let Some((call_id, call)) =
                            capture_medium_tool_call(payload, 140, detailed_calls, ts)
                        {
                            pending_medium_calls.insert(call_id, call);
                        } else if let Some(rendered) =
                            render_unpaired_tool_call(ts, payload, 140, detailed_calls)
                        {
                            if last_written_block.as_deref() == Some(rendered.as_str()) {
                                continue;
                            }
                            out.write_all(rendered.as_bytes()).map_err(|e| e.to_string())?;
                            last_written_block = Some(rendered);
                            last_medium_reasoning_key = None;
                        }
                        continue;
                    }

                    if ptype == "function_call_output" {
                        let call_id = payload
                            .and_then(|p| p.get("call_id"))
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string();
                        let paired_call = pending_medium_calls.get(&call_id);
                        if !call_id.is_empty() {
                            let sig = medium_tool_output_signature(payload);
                            if medium_output_sigs.get(&call_id).is_some_and(|prev| prev == &sig) {
                                continue;
                            }
                            medium_output_sigs.insert(call_id.clone(), sig);
                            calls_with_output.insert(call_id.clone());
                        }
                        if let Some(rendered) = render_medium_tool_output(ts, payload, paired_call, 180) {
                            if last_written_block.as_deref() == Some(rendered.as_str()) {
                                continue;
                            }
                            out.write_all(rendered.as_bytes()).map_err(|e| e.to_string())?;
                            last_written_block = Some(rendered);
                            last_medium_reasoning_key = None;
                        }
                        continue;
                    }

                    let reasoning_key = medium_reasoning_key(payload);
                    if reasoning_key.is_some()
                        && last_medium_reasoning_key.as_deref() == reasoning_key.as_deref()
                    {
                        continue;
                    }

                    if let Some(rendered) = render_medium_response_item(ts, payload, &mut image_idx, 180) {
                        if last_written_block.as_deref() == Some(rendered.as_str()) {
                            continue;
                        }
                        out.write_all(rendered.as_bytes()).map_err(|e| e.to_string())?;
                        last_written_block = Some(rendered);
                        last_medium_reasoning_key = reasoning_key;
                    }
                } else if otype == "event" {
                    let payload = obj.get("payload").cloned().unwrap_or(Value::Null);
                    let label = event_label(&payload);
                    if !is_medium_event_worthy(&label) {
                        continue;
                    }
                    let rendered = {
                        let title = if label.is_empty() {
                            "EVENT".to_string()
                        } else {
                            format!("EVENT `{}`", inline_code(&label))
                        };
                        let body = if payload.is_null() {
                            "(no details)".to_string()
                        } else {
                            format!("- details: {}", format_medium_event(&payload, 240))
                        };
                        markdown_section(ts, &title, &body)
                    };
                    if last_written_block.as_deref() == Some(rendered.as_str()) {
                        continue;
                    }
                    out.write_all(rendered.as_bytes()).map_err(|e| e.to_string())?;
                    last_written_block = Some(rendered);
                    last_medium_reasoning_key = None;
                }
            }
            ExportMode::Json => {
                if let Some(rendered) = render_full_item(ts, otype, obj.get("payload"), &mut image_idx) {
                    if last_written_block.as_deref() == Some(rendered.as_str()) {
                        continue;
                    }
                    out.write_all(rendered.as_bytes()).map_err(|e| e.to_string())?;
                    last_written_block = Some(rendered);
                }
            }
        }
    }

    if matches!(mode, ExportMode::Medium | ExportMode::Full) {
        let mut pending_only: Vec<&MediumToolCall> = pending_medium_calls
            .iter()
            .filter_map(|(call_id, call)| {
                if calls_with_output.contains(call_id) {
                    None
                } else {
                    Some(call)
                }
            })
            .collect();
        pending_only.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));

        for call in pending_only {
            let rendered = render_pending_tool_call(call);
            if last_written_block.as_deref() == Some(rendered.as_str()) {
                continue;
            }
            out.write_all(rendered.as_bytes()).map_err(|e| e.to_string())?;
            last_written_block = Some(rendered);
        }
    }

    out.flush().map_err(|e| e.to_string())?;
    Ok(out_file)
}

fn resolve_output_path(session_id: &str, out_path: &Path) -> Result<PathBuf, String> {
    if out_path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
    {
        if let Some(parent) = out_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("failed to create {}: {}", parent.display(), e))?;
        }
        return Ok(out_path.to_path_buf());
    }

    fs::create_dir_all(out_path)
        .map_err(|e| format!("failed to create {}: {}", out_path.display(), e))?;
    Ok(out_path.join(format!("{}.md", session_id)))
}

fn rollout_path_for_session(session_id: &str, code_dir: &Path) -> Result<PathBuf, String> {
    if let Some(path) = rollout_path_from_catalog(session_id, code_dir)? {
        return Ok(path);
    }

    if let Some(path) = rollout_path_from_scan(session_id, code_dir) {
        return Ok(path);
    }

    Err(format!(
        "Session not found: {} (searched catalog and rollout files under {})",
        session_id,
        code_dir.display()
    ))
}

fn rollout_path_from_catalog(session_id: &str, code_dir: &Path) -> Result<Option<PathBuf>, String> {
    let catalog_candidates = [
        code_dir.join("sessions/index/catalog.jsonl"),
        code_dir.join("index/catalog.jsonl"),
        code_dir.join("catalog.jsonl"),
    ];

    for catalog in catalog_candidates {
        let Ok(file) = File::open(&catalog) else {
            continue;
        };
        let reader = BufReader::new(file);

        for line in reader.lines() {
            let line = match line {
                Ok(v) if !v.trim().is_empty() => v,
                _ => continue,
            };
            let obj: Value = match serde_json::from_str(&line) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let sid = obj.get("session_id").and_then(Value::as_str).unwrap_or("");
            if sid != session_id {
                continue;
            }
            let deleted = obj.get("deleted").and_then(Value::as_bool).unwrap_or(false);
            if deleted {
                return Err(format!("Session {} is marked deleted in catalog", session_id));
            }
            let rel = obj
                .get("rollout_path")
                .and_then(Value::as_str)
                .ok_or_else(|| format!("Session {} missing rollout_path in catalog", session_id))?;

            for rel_candidate in rollout_rel_candidates(rel) {
                if let Some(path) = resolve_rollout_path(code_dir, &rel_candidate) {
                    return Ok(Some(path));
                }
            }

            continue;
        }
    }

    Ok(None)
}

fn rollout_rel_candidates(rel: &str) -> Vec<String> {
    let mut out = Vec::new();
    out.push(rel.to_string());

    if let Some(without_snapshot) = rel.strip_suffix(".snapshot.json") {
        out.push(format!("{}.jsonl", without_snapshot));
    }

    out
}

fn resolve_rollout_path(code_dir: &Path, rel: &str) -> Option<PathBuf> {
    let rel_path = Path::new(rel);
    if rel_path.is_absolute() {
        return Some(rel_path.to_path_buf());
    }

    let candidates = [
        code_dir.join(rel_path),
        code_dir
            .parent()
            .map(|p| p.join(rel_path))
            .unwrap_or_else(|| code_dir.join(rel_path)),
        code_dir.join("sessions").join(rel_path),
    ];

    candidates.into_iter().find(|p| p.exists())
}

fn rollout_path_from_scan(session_id: &str, code_dir: &Path) -> Option<PathBuf> {
    let sid = session_id.to_ascii_lowercase();
    let mut matches = Vec::new();

    for root in rollout_search_roots(code_dir) {
        collect_rollout_matches(&root, &sid, &mut matches);
    }

    matches.sort_by(|a, b| rollout_sort_key(b).cmp(&rollout_sort_key(a)));
    matches.into_iter().next()
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
        let key = root.to_string_lossy().to_ascii_lowercase();
        if seen.insert(key) {
            deduped.push(root);
        }
    }
    deduped
}

fn collect_rollout_matches(dir: &Path, session_id_lc: &str, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(ft) = entry.file_type() else {
            continue;
        };

        if ft.is_dir() {
            collect_rollout_matches(&path, session_id_lc, out);
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

        let name_lc = name.to_ascii_lowercase();
        if !name_lc.contains(session_id_lc) {
            continue;
        }

        out.push(path);
    }
}

fn rollout_sort_key(path: &Path) -> (u128, String) {
    let modified = path
        .metadata()
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .map(|age| u128::MAX.saturating_sub(age.as_nanos()))
        .unwrap_or(0);

    (modified, path.to_string_lossy().to_string())
}

fn first_session_meta(rollout: &Path) -> Result<Option<Value>, String> {
    let file = File::open(rollout).map_err(|e| format!("failed to open {}: {}", rollout.display(), e))?;
    let reader = BufReader::new(file);
    for line in reader.lines() {
        let line = line.map_err(|e| e.to_string())?;
        let obj: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if obj.get("type").and_then(Value::as_str) == Some("session_meta") {
            return Ok(obj.get("payload").cloned());
        }
    }
    Ok(None)
}

fn markdown_section(ts: &str, title: &str, body: &str) -> String {
    let mut out = String::new();
    out.push_str(&format!("### [{}] {}\n\n", ts, title));
    out.push_str(body.trim_end());
    out.push_str("\n\n");
    out
}

fn markdown_fence(lang: &str, body: &str) -> String {
    let safe = body.replace("```", "``\\`");
    format!("```{}\n{}\n```\n", lang, safe.trim_end())
}

fn markdown_blockquote(text: &str) -> String {
    let lines: Vec<String> = text.lines().map(|line| format!("> {}", line)).collect();
    lines.join("\n")
}

fn inline_code(text: &str) -> String {
    text.replace('`', "'")
}

fn render_compact_response_item(ts: &str, payload: Option<&Value>, image_idx: &mut usize) -> Option<String> {
    let payload = payload?;
    if payload.get("type").and_then(Value::as_str) != Some("message") {
        return None;
    }

    let role_raw = payload.get("role").and_then(Value::as_str).unwrap_or("unknown");
    if role_raw != "user" && role_raw != "assistant" {
        return None;
    }

    let role = role_raw.to_uppercase();
    let parts = message_parts(payload.get("content"), image_idx, true);
    if parts.is_empty() {
        return None;
    }
    if role_raw == "user" && is_auto_system_status_message(&parts) {
        return None;
    }

    Some(markdown_section(ts, &role, &parts.join("\n")))
}

fn render_medium_response_item(
    ts: &str,
    payload: Option<&Value>,
    image_idx: &mut usize,
    max_len: usize,
) -> Option<String> {
    let payload = payload?;
    let ptype = payload.get("type").and_then(Value::as_str).unwrap_or("unknown");

    match ptype {
        "message" => render_compact_response_item(ts, Some(payload), image_idx),
        "function_call" => {
            let name = payload.get("name").and_then(Value::as_str).unwrap_or("unknown");
            let args = parse_json_maybe(payload.get("arguments"));
            Some(markdown_section(
                ts,
                &format!("TOOL_CALL `{}`", inline_code(name)),
                &format!("- arguments: {}", one_line_value(&args, max_len, 0)),
            ))
        }
        "function_call_output" => {
            let call_id = payload.get("call_id").and_then(Value::as_str).unwrap_or("");
            let output = parse_json_maybe(payload.get("output"));
            let output_summary = summarize_tool_output(&output, max_len);
            Some(markdown_section(
                ts,
                &format!("TOOL_OUTPUT `{}`", inline_code(call_id)),
                &output_summary,
            ))
        }
        "reasoning" => {
            let summaries = extract_reasoning_summaries(payload);
            if summaries.is_empty() {
                return None;
            }
            let body = summaries
                .iter()
                .map(|s| markdown_blockquote(s))
                .collect::<Vec<_>>()
                .join("\n\n");
            Some(markdown_section(ts, "REASONING", &body))
        }
        _ => Some(markdown_section(
            ts,
            &format!("RESPONSE_ITEM {}", ptype),
            &format!("- payload: {}", one_line_value(payload, max_len, 0)),
        )),
    }
}

fn medium_reasoning_key(payload: Option<&Value>) -> Option<String> {
    let payload = payload?;
    if payload.get("type").and_then(Value::as_str) != Some("reasoning") {
        return None;
    }
    let summaries = extract_reasoning_summaries(payload);
    if summaries.is_empty() {
        return None;
    }
    Some(summaries.join("\n\n"))
}

fn capture_medium_tool_call(
    payload: Option<&Value>,
    max_len: usize,
    detailed_calls: bool,
    ts: &str,
) -> Option<(String, MediumToolCall)> {
    let payload = payload?;
    if payload.get("type").and_then(Value::as_str) != Some("function_call") {
        return None;
    }

    let call_id = payload.get("call_id").and_then(Value::as_str)?.to_string();
    let name = payload
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    let args = parse_json_maybe(payload.get("arguments"));
    let call_markdown = summarize_tool_call(&name, &args, max_len, detailed_calls);

    Some((
        call_id,
        MediumToolCall {
            name,
            call_markdown,
            timestamp: ts.to_string(),
        },
    ))
}

fn render_pending_tool_call(call: &MediumToolCall) -> String {
    markdown_section(
        &call.timestamp,
        &format!("TOOL `{}`", inline_code(&call.name)),
        &call.call_markdown,
    )
}

fn render_unpaired_tool_call(
    ts: &str,
    payload: Option<&Value>,
    max_len: usize,
    detailed_calls: bool,
) -> Option<String> {
    let payload = payload?;
    if payload.get("type").and_then(Value::as_str) != Some("function_call") {
        return None;
    }

    let name = payload.get("name").and_then(Value::as_str).unwrap_or("unknown");
    let args = parse_json_maybe(payload.get("arguments"));
    let call_markdown = summarize_tool_call(name, &args, max_len, detailed_calls);

    Some(markdown_section(
        ts,
        &format!("TOOL `{}`", inline_code(name)),
        &call_markdown,
    ))
}

fn summarize_tool_call(name: &str, args: &Value, max_len: usize, detailed_calls: bool) -> String {
    if name == "update_plan" {
        return format_update_plan_call(args);
    }

    if name == "shell" {
        let command = args
            .get("command")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(Value::as_str)
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let command_text = if command.len() >= 3
            && (command[0] == "bash" || command[0] == "sh")
            && command[1] == "-lc"
        {
            command[2].clone()
        } else {
            command.join(" ")
        };
        let collapsed = collapse_ws(&command_text);
        if collapsed.contains("apply_patch") {
            return "- call: `apply_patch <patch>`".to_string();
        }

        if !detailed_calls {
            let summary = truncate_with_indicator(&collapsed, max_len.max(40));
            return format!("- call: `{}`", inline_code(&summary));
        }

        let mut lines = Vec::new();
        if let Some(workdir) = args.get("workdir").and_then(Value::as_str) {
            if !workdir.is_empty() {
                lines.push(format!("- cwd: `{}`", inline_code(workdir)));
            }
        }
        lines.push("- call:".to_string());
        lines.push(markdown_fence("bash", &command_text).trim_end().to_string());
        return lines.join("\n");
    }

    let compact_args = serde_json::to_string(args).unwrap_or_else(|_| safe_json(args));
    let compact_args = collapse_ws(&compact_args);
    format!("- call: {} {}", name, compact_args)
}

fn format_update_plan_call(args: &Value) -> String {
    let mut lines = Vec::new();

    if let Some(name) = args.get("name").and_then(Value::as_str) {
        if !name.is_empty() {
            lines.push(format!("- plan: {}", name));
        }
    }

    if let Some(plan) = args.get("plan").and_then(Value::as_array) {
        for item in plan {
            let status = item.get("status").and_then(Value::as_str).unwrap_or("pending");
            let step = item.get("step").and_then(Value::as_str).unwrap_or("(unnamed step)");
            let mut suffix = String::new();
            if status == "in_progress" {
                suffix.push_str(" (in progress)");
            } else if status != "completed" && status != "pending" {
                suffix.push_str(&format!(" (status: {})", status));
            }

            let mark = if status == "completed" { "X" } else { " " };
            lines.push(format!("- [{}] {}{}", mark, step, suffix));
        }
    }

    if lines.is_empty() {
        let compact_args = serde_json::to_string(args).unwrap_or_else(|_| safe_json(args));
        return format!("- call: update_plan {}", collapse_ws(&compact_args));
    }

    lines.join("\n")
}

fn truncate_with_indicator(text: &str, max_len: usize) -> String {
    if text.chars().count() <= max_len {
        return text.to_string();
    }

    let kept: String = text.chars().take(max_len).collect();
    let remaining = text.chars().count().saturating_sub(max_len);
    format!("{} (+{} chars)", kept.trim_end(), remaining)
}

fn render_medium_tool_output(
    ts: &str,
    payload: Option<&Value>,
    call: Option<&MediumToolCall>,
    max_len: usize,
) -> Option<String> {
    let payload = payload?;
    if payload.get("type").and_then(Value::as_str) != Some("function_call_output") {
        return None;
    }

    let output = parse_json_maybe(payload.get("output"));
    let output_obj = output.as_object();
    let mut lines = Vec::new();

    if let Some(call) = call {
        lines.push(call.call_markdown.clone());
    }

    let mut exit_code: Option<i64> = None;
    if let Some(meta) = output_obj
        .and_then(|obj| obj.get("metadata"))
        .and_then(Value::as_object)
    {
        exit_code = meta.get("exit_code").and_then(Value::as_i64);
        if let Some(code) = exit_code {
            lines.push(format!("- exit_code: {}", code));
        }
        if let Some(dur) = meta.get("duration_seconds") {
            lines.push(format!("- duration_s: {}", one_line_value(dur, 40, 0)));
        }
    }

    let output_text = output_obj
        .and_then(|obj| obj.get("output"))
        .map(|v| one_line_value(v, max_len, 0))
        .unwrap_or_else(|| one_line_value(&output, max_len, 0));

    if should_include_medium_output(exit_code, &output_text) {
        lines.push(format!("- output: {}", output_text));
    }

    if lines.is_empty() {
        lines.push("- output: (no data)".to_string());
    }

    let title = call
        .map(|c| format!("TOOL `{}`", inline_code(&c.name)))
        .unwrap_or_else(|| "TOOL".to_string());
    Some(markdown_section(ts, &title, &lines.join("\n")))
}

fn should_include_medium_output(exit_code: Option<i64>, output: &str) -> bool {
    if exit_code.unwrap_or(0) != 0 {
        return true;
    }
    let lower = output.to_ascii_lowercase();
    if lower.contains("error") || lower.contains("fail") || lower.contains("panic") || lower.contains("warn") {
        return true;
    }
    false
}

fn medium_tool_output_signature(payload: Option<&Value>) -> String {
    let payload = payload.unwrap_or(&Value::Null);
    let output = parse_json_maybe(payload.get("output"));
    let output_obj = output.as_object();

    let code = output_obj
        .and_then(|obj| obj.get("metadata"))
        .and_then(Value::as_object)
        .and_then(|meta| meta.get("exit_code"))
        .map(|v| one_line_value(v, 20, 0))
        .unwrap_or_else(|| "none".to_string());
    let duration = output_obj
        .and_then(|obj| obj.get("metadata"))
        .and_then(Value::as_object)
        .and_then(|meta| meta.get("duration_seconds"))
        .map(|v| one_line_value(v, 20, 0))
        .unwrap_or_else(|| "none".to_string());
    let body = output_obj
        .and_then(|obj| obj.get("output"))
        .map(|v| one_line_value(v, 200, 0))
        .unwrap_or_else(|| one_line_value(&output, 200, 0));

    format!("{}|{}|{}", code, duration, body)
}

fn render_full_item(ts: &str, otype: &str, payload: Option<&Value>, image_idx: &mut usize) -> Option<String> {
    match otype {
        "session_meta" => Some(markdown_section(
            ts,
            "SESSION_META",
            &markdown_fence("json", &safe_json(payload.unwrap_or(&Value::Null))),
        )),
        "event" => Some(markdown_section(
            ts,
            "EVENT",
            &markdown_fence("json", &safe_json(payload.unwrap_or(&Value::Null))),
        )),
        "response_item" => {
            let payload = payload?;
            let ptype = payload.get("type").and_then(Value::as_str).unwrap_or("unknown");
            match ptype {
                "message" => {
                    let role = payload
                        .get("role")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown")
                        .to_uppercase();
                    let parts = message_parts(payload.get("content"), image_idx, false);
                    let body = if parts.is_empty() {
                        "(empty message)".to_string()
                    } else {
                        parts.join("\n")
                    };
                    Some(markdown_section(ts, &role, &body))
                }
                "function_call" => {
                    let name = payload.get("name").and_then(Value::as_str).unwrap_or("unknown");
                    let args = parse_json_maybe(payload.get("arguments"));
                    Some(markdown_section(
                        ts,
                        &format!("TOOL_CALL `{}`", inline_code(name)),
                        &format!("{}", markdown_fence("json", &safe_json(&args))),
                    ))
                }
                "function_call_output" => {
                    let call_id = payload.get("call_id").and_then(Value::as_str).unwrap_or("");
                    let output = parse_json_maybe(payload.get("output"));
                    Some(markdown_section(
                        ts,
                        &format!("TOOL_OUTPUT `{}`", inline_code(call_id)),
                        &format!("{}", markdown_fence("json", &safe_json(&output))),
                    ))
                }
                _ => Some(markdown_section(
                    ts,
                    &format!("RESPONSE_ITEM {}", ptype),
                    &markdown_fence("json", &safe_json(payload)),
                )),
            }
        }
        "" => None,
        other => Some(markdown_section(
            ts,
            &other.to_uppercase(),
            &markdown_fence("json", &safe_json(payload.unwrap_or(&Value::Null))),
        )),
    }
}

fn message_parts(content: Option<&Value>, image_idx: &mut usize, scrub_noise: bool) -> Vec<String> {
    let mut parts = Vec::new();
    let Some(items) = content.and_then(Value::as_array) else {
        return parts;
    };

    for item in items {
        let Some(itype) = item.get("type").and_then(Value::as_str) else {
            continue;
        };
        match itype {
            "input_text" | "output_text" => {
                if let Some(text) = item.get("text").and_then(Value::as_str) {
                    let cleaned = if scrub_noise {
                        scrub_noise_lines(text)
                    } else {
                        text.to_string()
                    };
                    if !cleaned.trim().is_empty() {
                        parts.push(escape_heading_lines(&cleaned));
                    }
                }
            }
            "input_image" => {
                *image_idx += 1;
                parts.push(image_name(item, *image_idx));
            }
            _ => parts.push(safe_json(item)),
        }
    }

    parts
}

fn escape_heading_lines(text: &str) -> String {
    text.lines()
        .map(|line| {
            let trimmed = line.trim_start();
            if trimmed.starts_with('#') {
                let indent = line.len().saturating_sub(trimmed.len());
                let mut out = String::new();
                out.push_str(&line[..indent]);
                out.push('\\');
                out.push_str(trimmed);
                out
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn image_name(item: &Value, idx: usize) -> String {
    for key in ["name", "filename", "file_name", "path"] {
        if let Some(v) = item.get(key).and_then(Value::as_str) {
            if !v.is_empty() {
                return v.to_string();
            }
        }
    }

    let url_text = item
        .get("image_url")
        .and_then(|v| match v {
            Value::String(s) => Some(s.as_str()),
            Value::Object(_) => v.get("url").and_then(Value::as_str),
            _ => None,
        })
        .unwrap_or("");
    if !url_text.is_empty() && !url_text.starts_with("data:") {
        let without_query = url_text.split('?').next().unwrap_or(url_text);
        let base = without_query.rsplit('/').next().unwrap_or(without_query);
        if !base.is_empty() {
            return base.to_string();
        }
    }

    format!("image_{:03}", idx)
}

fn parse_json_maybe(value: Option<&Value>) -> Value {
    let Some(v) = value else {
        return Value::Null;
    };

    match v {
        Value::String(s) => serde_json::from_str::<Value>(s).unwrap_or_else(|_| Value::String(s.clone())),
        _ => v.clone(),
    }
}

fn safe_json(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

fn value_as_string(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => one_line_value(other, 200, 0),
    }
}

fn event_label(payload: &Value) -> String {
    let Some(map) = payload.as_object() else {
        return String::new();
    };

    if let Some(msg) = map.get("msg").and_then(Value::as_object) {
        for key in ["type", "name", "event"] {
            if let Some(v) = msg.get(key).and_then(Value::as_str) {
                if !v.is_empty() {
                    return v.to_string();
                }
            }
        }
    }

    for key in ["type", "name", "event"] {
        if let Some(v) = map.get(key).and_then(Value::as_str) {
            if !v.is_empty() {
                return v.to_string();
            }
        }
    }
    String::new()
}

fn is_medium_event_worthy(label: &str) -> bool {
    if label.is_empty() {
        return false;
    }
    let l = label.to_ascii_lowercase();
    l.contains("error") || l.contains("fail") || l.contains("panic")
}

fn is_auto_system_status_message(parts: &[String]) -> bool {
    let text = parts.join("\n");
    text.contains("== System Status ==") && text.contains("[automatic message added by system]")
}

fn duration_seconds(duration: Option<&Value>) -> Option<f64> {
    let obj = duration?.as_object()?;
    let secs = obj.get("secs").and_then(Value::as_f64).unwrap_or(0.0);
    let nanos = obj.get("nanos").and_then(Value::as_f64).unwrap_or(0.0);
    Some(secs + nanos / 1_000_000_000.0)
}

fn format_medium_event(payload: &Value, max_len: usize) -> String {
    let Some(map) = payload.as_object() else {
        return one_line_value(payload, max_len, 0);
    };

    let msg = map.get("msg");
    if let Some(msg_obj) = msg.and_then(Value::as_object) {
        let mtype = msg_obj.get("type").and_then(Value::as_str).unwrap_or("");
        if mtype == "mcp_tool_call_begin" || mtype == "mcp_tool_call_end" {
            let mut parts = Vec::new();
            let invocation = msg_obj.get("invocation").and_then(Value::as_object);

            let mut header = Vec::new();
            if let Some(v) = msg_obj.get("call_id").and_then(Value::as_str) {
                if !v.is_empty() {
                    header.push(format!("call_id={}", v));
                }
            }
            if let Some(inv) = invocation {
                if let Some(v) = inv.get("server").and_then(Value::as_str) {
                    if !v.is_empty() {
                        header.push(format!("server={}", v));
                    }
                }
                if let Some(v) = inv.get("tool").and_then(Value::as_str) {
                    if !v.is_empty() {
                        header.push(format!("tool={}", v));
                    }
                }

                if let Some(args) = inv.get("arguments") {
                    if !args.is_null() {
                        parts.push(format!("arguments={}", one_line_value(args, max_len, 0)));
                    }
                }
            }

            if !header.is_empty() {
                parts.insert(0, header.join(" "));
            }

            if mtype == "mcp_tool_call_end" {
                if let Some(dur) = duration_seconds(msg_obj.get("duration")) {
                    parts.push(format!("duration_s={:.3}", dur));
                }

                if let Some(result) = msg_obj.get("result") {
                    let compact_result = result
                        .get("Ok")
                        .and_then(Value::as_object)
                        .and_then(|ok| ok.get("structuredContent"))
                        .unwrap_or(result);
                    if !compact_result.is_null() {
                        parts.push(format!("result={}", one_line_value(compact_result, max_len, 0)));
                    }
                }
            }

            if !parts.is_empty() {
                return parts.join("\n");
            }
        }

        return one_line_value(&Value::Object(msg_obj.clone()), max_len, 0);
    }

    one_line_value(payload, max_len, 0)
}

fn extract_reasoning_summaries(payload: &Value) -> Vec<String> {
    let mut out = Vec::new();

    let summary = payload.get("summary");
    let entries: Vec<&Value> = match summary {
        Some(Value::Array(arr)) => arr.iter().collect(),
        Some(v) => vec![v],
        None => vec![],
    };

    for item in entries {
        if let Some(text) = item.get("text").and_then(Value::as_str) {
            let cleaned = text.trim();
            if !cleaned.is_empty() {
                out.push(cleaned.to_string());
            }
        }
    }

    out
}

fn one_line_value(value: &Value, max_len: usize, depth: usize) -> String {
    if depth > 3 {
        return "<depth-limit>".to_string();
    }

    match value {
        Value::Null => "{}".to_string(),
        Value::Bool(v) => v.to_string(),
        Value::Number(v) => v.to_string(),
        Value::String(s) => shorten(&collapse_ws(&scrub_noise_lines(s)), max_len),
        Value::Array(arr) => {
            let mut parts = Vec::new();
            for (i, item) in arr.iter().enumerate() {
                if i >= 6 {
                    break;
                }
                parts.push(one_line_value(item, 40, depth + 1));
            }
            shorten(&format!("[{}]", parts.join(", ")), max_len)
        }
        Value::Object(map) => {
            let mut parts = Vec::new();
            for (i, (k, v)) in map.iter().enumerate() {
                if i >= 8 {
                    break;
                }
                parts.push(format!("{}={}", k, one_line_value(v, 60, depth + 1)));
            }
            shorten(&parts.join(" "), max_len)
        }
    }
}

fn collapse_ws(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn scrub_noise_lines(text: &str) -> String {
    text.lines()
        .filter(|line| !is_noise_line(line))
        .collect::<Vec<_>>()
        .join("\n")
}

fn is_noise_line(line: &str) -> bool {
    let trimmed = line.trim();

    let normalized_quotes = trimmed.replace("\\\"", "\"");
    if let Some(idx) = normalized_quotes.find("\"encrypted_content\"") {
        let after = &normalized_quotes[idx + "\"encrypted_content\"".len()..];
        if after.trim_start().starts_with(':') {
            return true;
        }
    }

    let Some((key, value)) = trimmed.split_once('=') else {
        return false;
    };

    match key.trim() {
        "encrypted_content" => true,
        "content" => value.trim() == "{}",
        _ => false,
    }
}

fn summarize_tool_output(value: &Value, max_len: usize) -> String {
    let mut lines: Vec<String> = Vec::new();

    if let Some(obj) = value.as_object() {
        if let Some(meta) = obj.get("metadata").and_then(Value::as_object) {
            if let Some(code) = meta.get("exit_code") {
                lines.push(format!("- exit_code: {}", one_line_value(code, 40, 0)));
            }
            if let Some(dur) = meta.get("duration_seconds") {
                lines.push(format!("- duration_s: {}", one_line_value(dur, 40, 0)));
            }
        }
        if let Some(output) = obj.get("output") {
            lines.push(format!("- output: {}", one_line_value(output, max_len, 0)));
        }
    }

    if lines.is_empty() {
        lines.push(format!("- output: {}", one_line_value(value, max_len, 0)));
    }

    lines.join("\n")
}

fn shorten(text: &str, max_len: usize) -> String {
    if text.chars().count() <= max_len {
        return text.to_string();
    }
    if max_len == 0 {
        return String::new();
    }
    text.chars().take(max_len).collect::<String>()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn reasoning_medium_uses_full_summary_and_skips_encrypted() {
        let payload: Value = serde_json::json!({
            "type": "reasoning",
            "summary": [{"type": "summary_text", "text": "**Inspecting repo and test commands**"}],
            "content": null,
            "encrypted_content": "super-secret"
        });

        let mut img = 0usize;
        let rendered = render_medium_response_item("2026-03-06T15:17:06.776Z", Some(&payload), &mut img, 240)
            .expect("rendered");

        assert!(rendered.contains("### [2026-03-06T15:17:06.776Z] REASONING"));
        assert!(rendered.contains("> **Inspecting repo and test commands**"));
        assert!(!rendered.contains("encrypted_content"));
        assert!(!rendered.contains("content={}"));
    }

    #[test]
    fn reasoning_medium_skips_empty_summary() {
        let payload: Value = serde_json::json!({
            "type": "reasoning",
            "summary": [],
            "content": null,
            "encrypted_content": "super-secret"
        });

        let mut img = 0usize;
        let rendered = render_medium_response_item("2026-03-06T15:17:06.776Z", Some(&payload), &mut img, 240);
        assert!(rendered.is_none());
    }

    #[test]
    fn export_medium_end_to_end_writes_reasoning_summary() {
        let uniq = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("b-revamp-export-test-{}", uniq));
        let code_dir = root.join(".code");
        let catalog_dir = code_dir.join("sessions/index");
        let rollout_dir = code_dir.join("sessions/2026/03/06");
        fs::create_dir_all(&catalog_dir).expect("catalog dir");
        fs::create_dir_all(&rollout_dir).expect("rollout dir");

        let session_id = "79fe0ea5-f15d-4b40-88fb-15e9b4dd2991";
        let rollout_rel = format!(
            "sessions/2026/03/06/rollout-2026-03-06T15-16-48-{}.jsonl",
            session_id
        );
        let rollout_path = code_dir.join(&rollout_rel);

        let catalog_line = serde_json::json!({
            "session_id": session_id,
            "rollout_path": rollout_rel,
            "deleted": false
        })
        .to_string();
        fs::write(catalog_dir.join("catalog.jsonl"), format!("{}\n", catalog_line)).expect("catalog write");

        let session_meta = serde_json::json!({
            "timestamp": "2026-03-06T15:16:48.748Z",
            "type": "session_meta",
            "payload": {
                "timestamp": "2026-03-06T15:16:48.677Z",
                "cwd": "/home/wavy/ai/b-revamp",
                "originator": "code_cli_rs",
                "cli_version": "0.0.0"
            }
        });
        let reasoning = serde_json::json!({
            "timestamp": "2026-03-06T15:17:06.776Z",
            "type": "response_item",
            "payload": {
                "type": "reasoning",
                "summary": [{"type": "summary_text", "text": "**Inspecting repo and test commands**"}],
                "content": null,
                "encrypted_content": "abc"
            }
        });
        let message = serde_json::json!({
            "timestamp": "2026-03-06T15:17:06.800Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "ok"}]
            }
        });

        let jsonl = format!("{}\n{}\n{}\n", session_meta, reasoning, message);
        fs::write(&rollout_path, jsonl).expect("rollout write");

        let out_file = export_session_markdown(session_id, &root.join("out"), ExportMode::Medium, &code_dir)
            .expect("exported");
        let body = fs::read_to_string(&out_file).expect("read output");

        assert!(body.contains("Export Mode: medium"));
        assert!(body.contains("### [2026-03-06T15:17:06.776Z] REASONING"));
        assert!(body.contains("> **Inspecting repo and test commands**"));
        assert!(!body.contains("encrypted_content"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn medium_escapes_markdown_headings_inside_messages() {
        let payload: Value = serde_json::json!({
            "type": "message",
            "role": "user",
            "content": [{"type": "input_text", "text": "### heading\nnormal line"}]
        });

        let mut img = 0usize;
        let rendered = render_medium_response_item("2026-03-07T15:45:37.004Z", Some(&payload), &mut img, 180)
            .expect("rendered");

        assert!(rendered.contains("\\### heading"));
        assert!(!rendered.contains("\n### heading\n"));
        assert!(rendered.contains("normal line"));
    }

    #[test]
    fn medium_coalesces_tool_call_and_output() {
        let uniq = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("b-revamp-export-test-tool-{}", uniq));
        let code_dir = root.join(".code");
        let catalog_dir = code_dir.join("sessions/index");
        let rollout_dir = code_dir.join("sessions/2026/03/07");
        fs::create_dir_all(&catalog_dir).expect("catalog dir");
        fs::create_dir_all(&rollout_dir).expect("rollout dir");

        let session_id = "11111111-1111-1111-1111-111111111111";
        let rollout_rel = format!(
            "sessions/2026/03/07/rollout-2026-03-07T15-00-00-{}.jsonl",
            session_id
        );
        let rollout_path = code_dir.join(&rollout_rel);

        let catalog_line = serde_json::json!({
            "session_id": session_id,
            "rollout_path": rollout_rel,
            "deleted": false
        })
        .to_string();
        fs::write(catalog_dir.join("catalog.jsonl"), format!("{}\n", catalog_line)).expect("catalog write");

        let function_call = serde_json::json!({
            "timestamp": "2026-03-07T15:00:01.000Z",
            "type": "response_item",
            "payload": {
                "type": "function_call",
                "name": "shell",
                "call_id": "call_abc",
                "arguments": "{\"command\":[\"bash\",\"-lc\",\"cargo test\"],\"workdir\":\"/home/wavy/ai/b-revamp\"}"
            }
        });
        let function_output = serde_json::json!({
            "timestamp": "2026-03-07T15:00:02.000Z",
            "type": "response_item",
            "payload": {
                "type": "function_call_output",
                "call_id": "call_abc",
                "output": "{\"output\":\"this output is intentionally long to be omitted in medium mode because successful commands should stay concise and focused\",\"metadata\":{\"exit_code\":0,\"duration_seconds\":1.2}}"
            }
        });

        let jsonl = format!("{}\n{}\n", function_call, function_output);
        fs::write(&rollout_path, jsonl).expect("rollout write");

        let out_file = export_session_markdown(session_id, &root.join("out"), ExportMode::Medium, &code_dir)
            .expect("exported");
        let body = fs::read_to_string(&out_file).expect("read output");

        assert!(body.contains("TOOL `shell`"));
        assert!(body.contains("- call: `cargo test`"));
        assert!(body.contains("- exit_code: 0"));
        assert!(!body.contains("- output:"));
        assert!(!body.contains("TOOL_CALL `shell`"));
        assert!(!body.contains("```bash"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn full_keeps_detailed_shell_call_and_output() {
        let uniq = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("b-revamp-export-test-full-tool-{}", uniq));
        let code_dir = root.join(".code");
        let catalog_dir = code_dir.join("sessions/index");
        let rollout_dir = code_dir.join("sessions/2026/03/07");
        fs::create_dir_all(&catalog_dir).expect("catalog dir");
        fs::create_dir_all(&rollout_dir).expect("rollout dir");

        let session_id = "11111111-1111-1111-1111-111111111112";
        let rollout_rel = format!(
            "sessions/2026/03/07/rollout-2026-03-07T15-00-00-{}.jsonl",
            session_id
        );
        let rollout_path = code_dir.join(&rollout_rel);

        let catalog_line = serde_json::json!({
            "session_id": session_id,
            "rollout_path": rollout_rel,
            "deleted": false
        })
        .to_string();
        fs::write(catalog_dir.join("catalog.jsonl"), format!("{}\n", catalog_line)).expect("catalog write");

        let function_call = serde_json::json!({
            "timestamp": "2026-03-07T15:00:01.000Z",
            "type": "response_item",
            "payload": {
                "type": "function_call",
                "name": "shell",
                "call_id": "call_abc",
                "arguments": "{\"command\":[\"bash\",\"-lc\",\"cargo test\"],\"workdir\":\"/home/wavy/ai/b-revamp\"}"
            }
        });
        let function_output = serde_json::json!({
            "timestamp": "2026-03-07T15:00:02.000Z",
            "type": "response_item",
            "payload": {
                "type": "function_call_output",
                "call_id": "call_abc",
                "output": "{\"output\":\"ok\",\"metadata\":{\"exit_code\":0,\"duration_seconds\":1.2}}"
            }
        });

        let jsonl = format!("{}\n{}\n", function_call, function_output);
        fs::write(&rollout_path, jsonl).expect("rollout write");

        let out_file = export_session_markdown(session_id, &root.join("out"), ExportMode::Full, &code_dir)
            .expect("exported");
        let body = fs::read_to_string(&out_file).expect("read output");

        assert!(body.contains("TOOL `shell`"));
        assert!(body.contains("- cwd: `/home/wavy/ai/b-revamp`"));
        assert!(body.contains("```bash\ncargo test\n```"));
        assert!(body.contains("- exit_code: 0"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn medium_preserves_tool_call_when_output_missing() {
        let uniq = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("b-revamp-export-test-missing-output-{}", uniq));
        let code_dir = root.join(".code");
        let catalog_dir = code_dir.join("sessions/index");
        let rollout_dir = code_dir.join("sessions/2026/03/07");
        fs::create_dir_all(&catalog_dir).expect("catalog dir");
        fs::create_dir_all(&rollout_dir).expect("rollout dir");

        let session_id = "11111111-1111-1111-1111-111111111113";
        let rollout_rel = format!(
            "sessions/2026/03/07/rollout-2026-03-07T15-00-00-{}.jsonl",
            session_id
        );
        let rollout_path = code_dir.join(&rollout_rel);

        let catalog_line = serde_json::json!({
            "session_id": session_id,
            "rollout_path": rollout_rel,
            "deleted": false
        })
        .to_string();
        fs::write(catalog_dir.join("catalog.jsonl"), format!("{}\n", catalog_line)).expect("catalog write");

        let function_call = serde_json::json!({
            "timestamp": "2026-03-07T15:00:01.000Z",
            "type": "response_item",
            "payload": {
                "type": "function_call",
                "name": "shell",
                "call_id": "call_missing",
                "arguments": "{\"command\":[\"bash\",\"-lc\",\"echo hello\"],\"workdir\":\"/home/wavy/ai/b-revamp\"}"
            }
        });

        fs::write(&rollout_path, format!("{}\n", function_call)).expect("rollout write");

        let out_file = export_session_markdown(session_id, &root.join("out"), ExportMode::Medium, &code_dir)
            .expect("exported");
        let body = fs::read_to_string(&out_file).expect("read output");

        assert!(body.contains("TOOL `shell`"));
        assert!(body.contains("echo hello"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn medium_preserves_tool_call_without_call_id() {
        let uniq = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("b-revamp-export-test-no-call-id-{}", uniq));
        let code_dir = root.join(".code");
        let catalog_dir = code_dir.join("sessions/index");
        let rollout_dir = code_dir.join("sessions/2026/03/07");
        fs::create_dir_all(&catalog_dir).expect("catalog dir");
        fs::create_dir_all(&rollout_dir).expect("rollout dir");

        let session_id = "11111111-1111-1111-1111-111111111114";
        let rollout_rel = format!(
            "sessions/2026/03/07/rollout-2026-03-07T15-00-00-{}.jsonl",
            session_id
        );
        let rollout_path = code_dir.join(&rollout_rel);

        let catalog_line = serde_json::json!({
            "session_id": session_id,
            "rollout_path": rollout_rel,
            "deleted": false
        })
        .to_string();
        fs::write(catalog_dir.join("catalog.jsonl"), format!("{}\n", catalog_line)).expect("catalog write");

        let function_call = serde_json::json!({
            "timestamp": "2026-03-07T15:00:01.000Z",
            "type": "response_item",
            "payload": {
                "type": "function_call",
                "name": "shell",
                "arguments": "{\"command\":[\"bash\",\"-lc\",\"echo missing-call-id\"]}"
            }
        });

        fs::write(&rollout_path, format!("{}\n", function_call)).expect("rollout write");

        let out_file = export_session_markdown(session_id, &root.join("out"), ExportMode::Medium, &code_dir)
            .expect("exported");
        let body = fs::read_to_string(&out_file).expect("read output");

        assert!(body.contains("TOOL `shell`"));
        assert!(body.contains("missing-call-id"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn medium_dedupes_consecutive_reasoning_with_different_metadata() {
        let uniq = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("b-revamp-export-test-reasoning-{}", uniq));
        let code_dir = root.join(".code");
        let catalog_dir = code_dir.join("sessions/index");
        let rollout_dir = code_dir.join("sessions/2026/03/07");
        fs::create_dir_all(&catalog_dir).expect("catalog dir");
        fs::create_dir_all(&rollout_dir).expect("rollout dir");

        let session_id = "22222222-2222-2222-2222-222222222222";
        let rollout_rel = format!(
            "sessions/2026/03/07/rollout-2026-03-07T16-00-00-{}.jsonl",
            session_id
        );
        let rollout_path = code_dir.join(&rollout_rel);

        let catalog_line = serde_json::json!({
            "session_id": session_id,
            "rollout_path": rollout_rel,
            "deleted": false
        })
        .to_string();
        fs::write(catalog_dir.join("catalog.jsonl"), format!("{}\n", catalog_line)).expect("catalog write");

        let reasoning_a = serde_json::json!({
            "timestamp": "2026-03-07T16:00:01.000Z",
            "type": "response_item",
            "payload": {
                "type": "reasoning",
                "summary": [{"type": "summary_text", "text": "**Duplicate summary**"}],
                "encrypted_content": "cipher-a"
            }
        });
        let reasoning_b = serde_json::json!({
            "timestamp": "2026-03-07T16:00:01.200Z",
            "type": "response_item",
            "payload": {
                "type": "reasoning",
                "summary": [{"type": "summary_text", "text": "**Duplicate summary**"}],
                "encrypted_content": "cipher-b"
            }
        });

        let jsonl = format!("{}\n{}\n", reasoning_a, reasoning_b);
        fs::write(&rollout_path, jsonl).expect("rollout write");

        let out_file = export_session_markdown(session_id, &root.join("out"), ExportMode::Medium, &code_dir)
            .expect("exported");
        let body = fs::read_to_string(&out_file).expect("read output");

        assert_eq!(body.matches("### [2026-03-07T16:00:01.").count(), 1);
        assert_eq!(body.matches("**Duplicate summary**").count(), 1);
        assert!(!body.contains("cipher-"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn full_shell_call_summary_keeps_full_command() {
        let args: Value = serde_json::json!({
            "command": [
                "bash",
                "-lc",
                "set -euo pipefail base=\"evidence/2026-03-03/revision-matrix\" mkdir -p \"$base\" echo SENTINEL_END"
            ],
            "workdir": "/tmp"
        });

        let summary = summarize_tool_call("shell", &args, 40, true);
        assert!(summary.contains("SENTINEL_END"));
        assert!(summary.contains("```bash"));
    }

    #[test]
    fn medium_shell_call_summary_is_abbreviated() {
        let args: Value = serde_json::json!({
            "command": [
                "bash",
                "-lc",
                "set -euo pipefail base=\"evidence/2026-03-03/revision-matrix\" mkdir -p \"$base\" echo SENTINEL_END"
            ],
            "workdir": "/tmp"
        });

        let summary = summarize_tool_call("shell", &args, 40, false);
        assert!(summary.contains("- call: `set -euo pipefail"));
        assert!(summary.contains("(+"));
        assert!(!summary.contains("```bash"));
    }

    #[test]
    fn medium_update_plan_call_summary_has_no_ellipsis() {
        let args: Value = serde_json::json!({
            "name": "Version Recon + Docs",
            "plan": [
                {"step": "Run torsocks-only version probes", "status": "in_progress"},
                {"step": "Extract verifiable version indicators", "status": "pending"}
            ]
        });

        let summary = summarize_tool_call("update_plan", &args, 40, false);
        assert!(summary.contains("- plan: Version Recon + Docs"));
        assert!(summary.contains("- [ ] Run torsocks-only version probes (in progress)"));
        assert!(summary.contains("torsocks-only version probes"));
        assert!(!summary.contains("..."));
    }

    #[test]
    fn scrub_noise_lines_preserves_mentions() {
        let text = "Mention encrypted_content in docs\nshow \"encrypted_content\" example";
        assert_eq!(scrub_noise_lines(text), text);
    }

    #[test]
    fn scrub_noise_lines_removes_only_noise_assignments() {
        let text = "keep this line\nencrypted_content=abc\ncontent={}\nkeep final";
        assert_eq!(scrub_noise_lines(text), "keep this line\nkeep final");
    }

    #[test]
    fn scrub_noise_lines_removes_json_style_encrypted_content() {
        let text = "{\"type\":\"reasoning\",\"encrypted_content\":\"abc\"}";
        assert_eq!(scrub_noise_lines(text), "");
    }

    #[test]
    fn scrub_noise_lines_removes_json_style_encrypted_content_with_spacing() {
        let text = "{\"type\":\"reasoning\", \"encrypted_content\" : \"abc\"}";
        assert_eq!(scrub_noise_lines(text), "");
    }

    #[test]
    fn export_falls_back_to_rollout_scan_when_catalog_missing() {
        let uniq = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("b-revamp-export-test-scan-{}", uniq));
        let code_dir = root.join(".code");
        let rollout_dir = code_dir.join("sessions/2026/03/18");
        fs::create_dir_all(&rollout_dir).expect("rollout dir");

        let session_id = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
        let rollout_path = rollout_dir.join(format!(
            "rollout-2026-03-18T12-00-00-{}.jsonl",
            session_id
        ));

        let session_meta = serde_json::json!({
            "timestamp": "2026-03-18T12:00:00.000Z",
            "type": "session_meta",
            "payload": {"cwd": "C:/Users/bmthub"}
        });
        let assistant = serde_json::json!({
            "timestamp": "2026-03-18T12:00:01.000Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "hello from scan fallback"}]
            }
        });
        fs::write(&rollout_path, format!("{}\n{}\n", session_meta, assistant)).expect("rollout write");

        let out_file = export_session_markdown(session_id, &root.join("out"), ExportMode::Medium, &code_dir)
            .expect("exported");
        let body = fs::read_to_string(&out_file).expect("read output");
        assert!(body.contains("hello from scan fallback"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn export_accepts_sessions_dir_as_code_dir() {
        let uniq = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("b-revamp-export-test-sessions-root-{}", uniq));
        let code_dir = root.join(".code");
        let sessions_dir = code_dir.join("sessions");
        let rollout_dir = sessions_dir.join("2026/03/18");
        fs::create_dir_all(&rollout_dir).expect("rollout dir");

        let session_id = "bbbbbbbb-cccc-dddd-eeee-ffffffffffff";
        let rollout_path = rollout_dir.join(format!(
            "rollout-2026-03-18T12-00-00-{}.jsonl",
            session_id
        ));

        let session_meta = serde_json::json!({
            "timestamp": "2026-03-18T12:00:00.000Z",
            "type": "session_meta",
            "payload": {"cwd": "C:/Users/bmthub"}
        });
        let assistant = serde_json::json!({
            "timestamp": "2026-03-18T12:00:01.000Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "hello from sessions dir"}]
            }
        });
        fs::write(&rollout_path, format!("{}\n{}\n", session_meta, assistant)).expect("rollout write");

        let out_file = export_session_markdown(
            session_id,
            &root.join("out"),
            ExportMode::Medium,
            &sessions_dir,
        )
        .expect("exported");
        let body = fs::read_to_string(&out_file).expect("read output");
        assert!(body.contains("hello from sessions dir"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn export_uses_jsonl_when_catalog_points_to_snapshot_file() {
        let uniq = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("b-revamp-export-test-snapshot-catalog-{}", uniq));
        let code_dir = root.join(".code");
        let catalog_dir = code_dir.join("sessions/index");
        let rollout_dir = code_dir.join("sessions/2026/03/18");
        fs::create_dir_all(&catalog_dir).expect("catalog dir");
        fs::create_dir_all(&rollout_dir).expect("rollout dir");

        let session_id = "cccccccc-dddd-eeee-ffff-000000000000";
        let base_name = format!("rollout-2026-03-18T12-00-00-{}", session_id);
        let rollout_rel_snapshot = format!("sessions/2026/03/18/{}.snapshot.json", base_name);
        let rollout_jsonl = rollout_dir.join(format!("{}.jsonl", base_name));

        let catalog_line = serde_json::json!({
            "session_id": session_id,
            "rollout_path": rollout_rel_snapshot,
            "deleted": false
        })
        .to_string();
        fs::write(catalog_dir.join("catalog.jsonl"), format!("{}\n", catalog_line)).expect("catalog write");

        let session_meta = serde_json::json!({
            "timestamp": "2026-03-18T12:00:00.000Z",
            "type": "session_meta",
            "payload": {"cwd": "C:/Users/bmthub"}
        });
        let assistant = serde_json::json!({
            "timestamp": "2026-03-18T12:00:01.000Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "hello from snapshot-catalog fallback"}]
            }
        });
        fs::write(&rollout_jsonl, format!("{}\n{}\n", session_meta, assistant)).expect("rollout write");

        let out_file = export_session_markdown(session_id, &root.join("out"), ExportMode::Medium, &code_dir)
            .expect("exported");
        let body = fs::read_to_string(&out_file).expect("read output");
        assert!(body.contains("hello from snapshot-catalog fallback"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn export_falls_back_to_scan_when_catalog_rollout_path_is_stale() {
        let uniq = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("b-revamp-export-test-stale-catalog-{}", uniq));
        let code_dir = root.join(".code");
        let catalog_dir = code_dir.join("sessions/index");
        let rollout_dir = code_dir.join("sessions/2026/03/18");
        fs::create_dir_all(&catalog_dir).expect("catalog dir");
        fs::create_dir_all(&rollout_dir).expect("rollout dir");

        let session_id = "dddddddd-eeee-ffff-0000-111111111111";
        let missing_rel = "sessions/2026/03/18/rollout-2026-03-18T00-00-00-missing.jsonl";
        let real_rel = format!("sessions/2026/03/18/rollout-2026-03-18T12-30-00-{}.jsonl", session_id);
        let real_path = code_dir.join(&real_rel);

        let catalog_line = serde_json::json!({
            "session_id": session_id,
            "rollout_path": missing_rel,
            "deleted": false
        })
        .to_string();
        fs::write(catalog_dir.join("catalog.jsonl"), format!("{}\n", catalog_line)).expect("catalog write");

        let session_meta = serde_json::json!({
            "timestamp": "2026-03-18T12:00:00.000Z",
            "type": "session_meta",
            "payload": {"cwd": "C:/Users/bmthub"}
        });
        let assistant = serde_json::json!({
            "timestamp": "2026-03-18T12:00:01.000Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "hello from stale-catalog fallback"}]
            }
        });
        fs::write(&real_path, format!("{}\n{}\n", session_meta, assistant)).expect("rollout write");

        let out_file = export_session_markdown(session_id, &root.join("out"), ExportMode::Medium, &code_dir)
            .expect("exported");
        let body = fs::read_to_string(&out_file).expect("read output");
        assert!(body.contains("hello from stale-catalog fallback"));

        let _ = fs::remove_dir_all(&root);
    }
}
