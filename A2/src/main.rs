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

const DEFAULT_COL_WIDTH: usize = 58;
const MIN_COL_WIDTH: usize = 30;
const COLUMN_SEPARATOR: &str = " | ";

/// Queries the current terminal width in columns, if stdout is a terminal
/// or `COLUMNS` is set in the environment.
fn detect_terminal_width() -> Option<usize> {
    if std::io::stdout().is_terminal()
        && let Some(cols) = platform_terminal_width()
    {
        return Some(cols);
    }

    env::var("COLUMNS")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|&c| c > 0)
}

#[cfg(windows)]
fn platform_terminal_width() -> Option<usize> {
    #[repr(C)]
    struct Coord {
        x: i16,
        y: i16,
    }

    #[repr(C)]
    struct SmallRect {
        left: i16,
        top: i16,
        right: i16,
        bottom: i16,
    }

    #[repr(C)]
    struct ConsoleScreenBufferInfo {
        size: Coord,
        cursor_position: Coord,
        attributes: u16,
        window: SmallRect,
        maximum_window_size: Coord,
    }

    unsafe extern "system" {
        fn GetStdHandle(n_std_handle: u32) -> isize;
        fn GetConsoleScreenBufferInfo(
            h_console_output: isize,
            lp_console_screen_buffer_info: *mut ConsoleScreenBufferInfo,
        ) -> i32;
    }

    const STD_OUTPUT_HANDLE: u32 = (-11i32) as u32;

    unsafe {
        let handle = GetStdHandle(STD_OUTPUT_HANDLE);
        if handle == 0 || handle == -1 {
            return None;
        }
        let mut info = std::mem::zeroed::<ConsoleScreenBufferInfo>();
        if GetConsoleScreenBufferInfo(handle, &mut info) == 0 {
            return None;
        }
        let width = info.window.right - info.window.left + 1;
        if width > 0 {
            Some(width as usize)
        } else {
            None
        }
    }
}

#[cfg(unix)]
fn platform_terminal_width() -> Option<usize> {
    use std::process::Stdio;

    let tty = fs::File::open("/dev/tty").ok()?;
    let output = Command::new("stty")
        .arg("size")
        .stdin(Stdio::from(tty))
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let cols = text.split_whitespace().nth(1)?.parse::<usize>().ok()?;
    (cols > 0).then_some(cols)
}

#[cfg(not(any(windows, unix)))]
fn platform_terminal_width() -> Option<usize> {
    None
}

/// Computes the width of each side-by-side column from an optional terminal width.
fn column_width_for(term_width: Option<usize>) -> usize {
    match term_width {
        Some(w) => w
            .saturating_sub(COLUMN_SEPARATOR.len())
            .checked_div(2)
            .unwrap_or(DEFAULT_COL_WIDTH)
            .max(MIN_COL_WIDTH),
        None => DEFAULT_COL_WIDTH,
    }
}

/// Locates the `original_repo` submodule directory.
fn find_original_repo() -> Option<PathBuf> {
    let candidates = [
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("original_repo"),
        PathBuf::from("original_repo"),
        PathBuf::from("A2").join("original_repo"),
    ];
    candidates.into_iter().find(|p| p.join("Cargo.toml").is_file())
}

/// Attempts to initialize the `A2/original_repo` git submodule when running inside
/// a git checkout that has not fetched submodules yet.
fn try_init_original_repo_submodule() -> anyhow::Result<PathBuf> {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let Some(repo_root) = manifest_dir.parent() else {
        anyhow::bail!(
            "Could not find `original_repo` submodule. Run `git submodule update --init`."
        );
    };

    if !repo_root.join(".git").exists() || !repo_root.join(".gitmodules").is_file() {
        anyhow::bail!(
            "Could not find `original_repo` submodule. Run `git submodule update --init`."
        );
    }

    eprintln!("`A2/original_repo` is not initialized; running `git submodule update --init`...");
    let status = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["submodule", "update", "--init", "--", "A2/original_repo"])
        .status()
        .map_err(|e| {
            anyhow::anyhow!(
                "Failed to invoke `git` to initialize `A2/original_repo`: {e}. Run `git submodule update --init` manually."
            )
        })?;

    if !status.success() {
        anyhow::bail!(
            "`git submodule update --init -- A2/original_repo` exited with {status}. Run `git submodule update --init` manually."
        );
    }

    find_original_repo().ok_or_else(|| {
        anyhow::anyhow!(
            "Submodule initialized, but `original_repo/Cargo.toml` is still missing."
        )
    })
}

/// Builds the original `rwetac` binary once and returns its path.
fn build_before_binary() -> anyhow::Result<PathBuf> {
    let repo_dir = match find_original_repo() {
        Some(dir) => dir,
        None => try_init_original_repo_submodule()?,
    };
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

/// Prints `left` and `right` outputs side by side in two equal-width columns
/// sized to `col_width`.
fn print_side_by_side(title: &str, left: &str, right: &str, col_width: usize) {
    let sep = COLUMN_SEPARATOR;
    let total_width = col_width * 2 + sep.len();
    println!("{}", "=".repeat(total_width));
    println!("{title}");
    println!("{}", "=".repeat(total_width));
    println!(
        "{:<w$}{sep}{:<w$}",
        "BEFORE (class compiler)",
        "AFTER (Assignment 2)",
        w = col_width
    );
    println!(
        "{}{sep}{}",
        "-".repeat(col_width),
        "-".repeat(col_width)
    );

    let left_lines = wrap_lines(left, col_width);
    let right_lines = wrap_lines(right, col_width);
    let max_lines = left_lines.len().max(right_lines.len());

    for i in 0..max_lines {
        let l = left_lines.get(i).map(String::as_str).unwrap_or("");
        let r = right_lines.get(i).map(String::as_str).unwrap_or("");
        let l_pad = " ".repeat(col_width.saturating_sub(visible_len(l)));
        let r_pad = " ".repeat(col_width.saturating_sub(visible_len(r)));
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
    let col_width = column_width_for(detect_terminal_width());
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
                print_side_by_side(&title, &before_out, &after_out, col_width);
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
    fn column_width_adapts_to_terminal_width_with_floor_and_fallback() {
        assert_eq!(column_width_for(None), DEFAULT_COL_WIDTH);
        assert_eq!(column_width_for(Some(160)), 78);
        assert_eq!(column_width_for(Some(80)), 38);
        assert_eq!(column_width_for(Some(40)), MIN_COL_WIDTH);
    }

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
