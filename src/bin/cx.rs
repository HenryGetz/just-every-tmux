use std::env;
use std::path::PathBuf;

use b_revamp::exporter::{self, ExportMode};

fn expand_path(path: &str) -> PathBuf {
    let expanded = shellexpand::full(path)
        .map(|s| s.to_string())
        .unwrap_or_else(|_| path.to_string());
    let p = PathBuf::from(expanded);
    if p.is_absolute() {
        p
    } else {
        env::current_dir().unwrap_or_else(|_| PathBuf::from(".")).join(p)
    }
}

fn usage() {
    eprintln!(
        "cx (rust)\n\nUsage:\n  cx <session-id> [--out <dir|file.md>] [--code-dir <dir>] [--compact|--medium|--full|--json]\n\nDefaults:\n  --out ~/coder-md\n  --code-dir ~/.code\n  --compact\n"
    );
}

fn main() {
    let mut args = env::args().skip(1);

    let mut session_id: Option<String> = None;
    let mut out_path = expand_path("~/coder-md");
    let mut code_dir = expand_path("~/.code");
    let mut mode = ExportMode::Compact;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                usage();
                std::process::exit(0);
            }
            "-o" | "--out" => {
                let Some(v) = args.next() else {
                    eprintln!("Missing value for --out");
                    std::process::exit(2);
                };
                out_path = expand_path(&v);
            }
            "-c" | "--code-dir" => {
                let Some(v) = args.next() else {
                    eprintln!("Missing value for --code-dir");
                    std::process::exit(2);
                };
                code_dir = expand_path(&v);
            }
            "--compact" => mode = ExportMode::Compact,
            "--medium" => mode = ExportMode::Medium,
            "--full" => mode = ExportMode::Full,
            "--json" => mode = ExportMode::Json,
            value if value.starts_with('-') => {
                eprintln!("Unknown flag: {}", value);
                std::process::exit(2);
            }
            value => {
                if session_id.is_some() {
                    eprintln!("Unexpected argument: {}", value);
                    std::process::exit(2);
                }
                session_id = Some(value.to_string());
            }
        }
    }

    let Some(session_id) = session_id else {
        usage();
        std::process::exit(2);
    };

    match exporter::export_session_markdown(&session_id, &out_path, mode, &code_dir) {
        Ok(path) => {
            println!("Wrote: {}", path.display());
            std::process::exit(0);
        }
        Err(err) => {
            eprintln!("Export failed: {}", err);
            std::process::exit(1);
        }
    }
}
