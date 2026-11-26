use std::env;
use std::fs;
use std::io::IsTerminal;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use rwetac::diagnostic::Diagnostic;
use rwetac::lexer::Lexer;
use rwetac::parser::Parser;
use rwetac::resolver::Resolver;
use rwetac::source_map::SourceMap;
use rwetac::typecheck::Typechecker;

const COL_WIDTH: usize = 58;

/// Locates the `original_repo` submodule directory.
fn find_original_repo() -> Option<PathBuf> {
    let candidates = [
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("original_repo"),
        PathBuf::from("original_repo"),
        PathBuf::from("A2").join("original_repo"),
    ];
    candidates.into_iter().find(|p| p.join("Cargo.toml").is_file())
}

/// Builds the original `rwetac` binary once and returns its path.
fn build_before_binary() -> anyhow::Result<PathBuf> {
    let repo_dir = find_original_repo().ok_or_else(|| {
        anyhow::anyhow!(
            "Could not find `original_repo` submodule. Run `git submodule update --init`."
        )
    })?;
    let manifest = repo_dir.join("Cargo.toml");
    let status = Command::new("cargo")
        .arg("build")
        .arg("--quiet")
        .arg("--manifest-path")
        .arg(&manifest)
        .arg("-p")
        .arg("rwetac")
        .status()?;
    if !status.success() {
        anyhow::bail!("Failed to build original `rwetac` from {}", manifest.display());
    }
    let bin_name = if cfg!(windows) { "rwetac.exe" } else { "rwetac" };
    Ok(repo_dir.join("target").join("debug").join(bin_name))
}

/// Runs the original class compiler binary up to `--validate` and returns its
/// single-error output (or a success message).
fn run_before(before_bin: &Path, path: &Path) -> String {
    let output = match Command::new(before_bin).arg("--validate").arg(path).output() {
        Ok(o) => o,
        Err(e) => return format!("[ERROR] Failed to run original compiler: {e}"),
    };

    if output.status.success() {
        return "[OK] No errors reported.".to_string();
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let combined = if !stderr.trim().is_empty() {
        stderr.trim().to_string()
    } else if !stdout.trim().is_empty() {
        stdout.trim().to_string()
    } else {
        format!("Exited with status {}", output.status)
    };
    combined
}

/// Runs the Assignment 2 front-end (`rwetac`) and returns all accumulated
/// rendered diagnostics (plus total count).
fn run_after(path: &Path, display_name: &str, use_color: bool) -> anyhow::Result<(String, usize)> {
    let input = fs::read_to_string(path)?;
    let source_map = SourceMap::new(input.clone());

    let lexer = Lexer::new(&input);
    let mut parser = Parser::new(lexer);
    let (mut ast, mut diagnostics): (rwetac::ast::Program, Vec<Diagnostic>) = parser.parse();

    let mut resolver = Resolver::new();
    diagnostics.extend(resolver.resolve(&mut ast));

    let typecheck_res = catch_unwind(AssertUnwindSafe(|| {
        let mut typechecker = Typechecker::new();
        typechecker.typecheck(&ast)
    }));

    let mut panic_note = None;
    match typecheck_res {
        Ok(Ok(())) => {}
        Ok(Err(type_diags)) => diagnostics.extend(type_diags),
        Err(_) => {
            panic_note = Some("[NOTE] Typechecker panicked on recovered AST.".to_string());
        }
    }

    if diagnostics.is_empty() && panic_note.is_none() {
        return Ok(("[OK] No errors reported.".to_string(), 0));
    }

    let count = diagnostics.len();
    let mut out = diagnostics
        .iter()
        .map(|d| d.render(&source_map, Some(display_name), use_color))
        .collect::<Vec<_>>()
        .join("\n");

    if let Some(note) = panic_note {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&note);
    }

    Ok((out, count))
}

/// Returns the number of visible characters in `s`, ignoring ANSI CSI (`\x1b[...m`) sequences.
fn visible_len(s: &str) -> usize {
    let mut len = 0usize;
    let mut chars = s.chars();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' && chars.as_str().starts_with('[') {
            chars.next(); // consume '['
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            len += 1;
        }
    }
    len
}

/// Wraps a multi-line string to fit within `width` visible characters per line,
/// preserving ANSI CSI color sequences across wrapped lines.
fn wrap_lines(text: &str, width: usize) -> Vec<String> {
    let mut wrapped = Vec::new();
    for raw_line in text.lines() {
        let line = raw_line.trim_end_matches('\r');
        if line.is_empty() {
            wrapped.push(String::new());
            continue;
        }
        let mut current = String::new();
        let mut current_len = 0usize;
        let mut active_ansi = String::new();
        let mut chars = line.chars();

        while let Some(ch) = chars.next() {
            if ch == '\x1b' && chars.as_str().starts_with('[') {
                let mut seq = String::from("\x1b[");
                chars.next(); // consume '['
                for c in chars.by_ref() {
                    seq.push(c);
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
                if seq == "\x1b[0m" || seq == "\x1b[m" {
                    active_ansi.clear();
                } else {
                    active_ansi.push_str(&seq);
                }
                current.push_str(&seq);
                continue;
            }

            if current_len >= width {
                if !active_ansi.is_empty() {
                    current.push_str("\x1b[0m");
                }
                wrapped.push(current);
                current = active_ansi.clone();
                current_len = 0;
            }
            current.push(ch);
            current_len += 1;
        }
        if !current.is_empty() {
            wrapped.push(current);
        }
    }
    if wrapped.is_empty() {
        wrapped.push(String::new());
    }
    wrapped
}

/// Prints `left` and `right` outputs side by side in two fixed-width columns.
fn print_side_by_side(title: &str, left: &str, right: &str) {
    let sep = " | ";
    let total_width = COL_WIDTH * 2 + sep.len();
    println!("{}", "=".repeat(total_width));
    println!("{title}");
    println!("{}", "=".repeat(total_width));
    println!(
        "{:<COL_WIDTH$}{sep}{:<COL_WIDTH$}",
        "BEFORE (class compiler)", "AFTER (Assignment 2)"
    );
    println!(
        "{}{sep}{}",
        "-".repeat(COL_WIDTH),
        "-".repeat(COL_WIDTH)
    );

    let left_lines = wrap_lines(left, COL_WIDTH);
    let right_lines = wrap_lines(right, COL_WIDTH);
    let max_lines = left_lines.len().max(right_lines.len());

    for i in 0..max_lines {
        let l = left_lines.get(i).map(String::as_str).unwrap_or("");
        let r = right_lines.get(i).map(String::as_str).unwrap_or("");
        let l_pad = " ".repeat(COL_WIDTH.saturating_sub(visible_len(l)));
        let r_pad = " ".repeat(COL_WIDTH.saturating_sub(visible_len(r)));
        println!("{l}{l_pad}{sep}{r}{r_pad}");
    }
    println!();
}

/// Resolves the `examples/` directory relative to the crate manifest or CWD.
fn find_examples_dir() -> Option<PathBuf> {
    let candidates = [
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples"),
        PathBuf::from("examples"),
        PathBuf::from("A2").join("examples"),
    ];
    candidates.into_iter().find(|p| p.is_dir())
}

/// Collects `.eta` files from CLI arguments, or from `examples/` when no args
/// are given.
fn collect_eta_files() -> anyhow::Result<Vec<PathBuf>> {
    let args: Vec<String> = env::args().skip(1).collect();
    let mut files = Vec::new();

    if args.is_empty() {
        let Some(dir) = find_examples_dir() else {
            return Ok(files);
        };
        for entry in fs::read_dir(&dir)? {
            let path = entry?.path();
            if path.is_file() && path.extension().and_then(|e| e.to_str()) == Some("eta") {
                files.push(path);
            }
        }
        files.sort();
    } else {
        for arg in args {
            let path = PathBuf::from(&arg);
            if path.is_dir() {
                let mut dir_files = Vec::new();
                for entry in fs::read_dir(&path)? {
                    let p = entry?.path();
                    if p.is_file() && p.extension().and_then(|e| e.to_str()) == Some("eta") {
                        dir_files.push(p);
                    }
                }
                dir_files.sort();
                files.extend(dir_files);
            } else {
                files.push(path);
            }
        }
    }

    Ok(files)
}

fn main() -> ExitCode {
    let files = match collect_eta_files() {
        Ok(f) => f,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    if files.is_empty() {
        println!(
            "No .eta files found. Place broken programs in `{}/examples/` or pass file/directory paths as arguments.",
            env!("CARGO_MANIFEST_DIR")
        );
        return ExitCode::SUCCESS;
    }

    let before_bin = match build_before_binary() {
        Ok(b) => b,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    let use_color = std::io::stdout().is_terminal();
    let mut total_diagnostics = 0usize;
    let mut files_with_errors = 0usize;

    println!(
        "Comparing BEFORE vs AFTER diagnostics on {} Eta file(s)...\n",
        files.len()
    );

    for path in &files {
        let display_name = path.display().to_string();
        let before_out = run_before(&before_bin, path);
        match run_after(path, &display_name, use_color) {
            Ok((after_out, count)) => {
                if count > 0 {
                    files_with_errors += 1;
                    total_diagnostics += count;
                }
                let title = format!("{display_name} ({count} diagnostic(s) in AFTER)");
                print_side_by_side(&title, &before_out, &after_out);
            }
            Err(e) => {
                eprintln!("=== {} (failed to read: {}) ===\n", display_name, e);
                return ExitCode::FAILURE;
            }
        }
    }

    println!(
        "Summary: {} file(s) checked, {} with diagnostics, {} total diagnostic(s) reported by AFTER.",
        files.len(),
        files_with_errors,
        total_diagnostics
    );

    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn examples_produce_before_and_after_output_without_panicking() {
        let Some(dir) = find_examples_dir() else {
            return;
        };
        let mut eta_files: Vec<PathBuf> = fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok().map(|entry| entry.path()))
            .filter(|p| p.is_file() && p.extension().and_then(|s| s.to_str()) == Some("eta"))
            .collect();
        eta_files.sort();

        let before_bin = build_before_binary().expect("original_repo should build");
        for path in eta_files {
            let before = run_before(&before_bin, &path);
            assert!(!before.is_empty());
            let (after, count) = run_after(&path, &path.display().to_string(), false)
                .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
            assert!(
                count > 0,
                "expected at least one diagnostic for broken example {}",
                path.display()
            );
            assert!(!after.is_empty());
        }
    }
}
