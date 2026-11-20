//! Diagnostics representation and rustc-style rendering.
//!
//! Emits structured diagnostic messages with primary/secondary spans,
//! inline carets, source snippets, line numbers, and terminal ANSI colors.

use std::cmp::max;
use std::collections::BTreeMap;
use std::fmt;
use std::io::IsTerminal;
use logos::Span;

use crate::source_map::SourceMap;

/// Severity of a diagnostic message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

impl Severity {
    pub fn name(&self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
        }
    }
}

/// A labeled span pointing to a location in the source code with an optional message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Label {
    pub span: Span,
    pub message: String,
}

impl Label {
    pub fn new(span: Span, message: impl Into<String>) -> Self {
        Self {
            span,
            message: message.into(),
        }
    }
}

/// A complete diagnostic report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub severity: Severity,
    pub message: String,
    pub primary: Label,
    pub secondary: Vec<Label>,
    pub suggestion: Option<String>,
}

impl Diagnostic {
    /// Creates an error diagnostic where the primary label repeats the top-level message.
    pub fn error(span: Span, message: impl Into<String>) -> Self {
        let msg = message.into();
        Self {
            severity: Severity::Error,
            message: msg.clone(),
            primary: Label::new(span, msg),
            secondary: Vec::new(),
            suggestion: None,
        }
    }

    /// Creates an error diagnostic with a distinct title message and inline primary label.
    pub fn error_with_label(span: Span, message: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            message: message.into(),
            primary: Label::new(span, label),
            secondary: Vec::new(),
            suggestion: None,
        }
    }

    /// Creates a warning diagnostic.
    pub fn warning(span: Span, message: impl Into<String>) -> Self {
        let msg = message.into();
        Self {
            severity: Severity::Warning,
            message: msg.clone(),
            primary: Label::new(span, msg),
            secondary: Vec::new(),
            suggestion: None,
        }
    }

    /// Attaches a secondary label (context label).
    pub fn with_secondary(mut self, span: Span, message: impl Into<String>) -> Self {
        self.secondary.push(Label::new(span, message));
        self
    }

    /// Attaches an optional suggestion or help note.
    pub fn with_suggestion(mut self, suggestion: impl Into<String>) -> Self {
        self.suggestion = Some(suggestion.into());
        self
    }

    /// Renders this diagnostic to a rustc-style formatted string.
    ///
    /// If `color` is true, ANSI color escape sequences are included.
    pub fn render(&self, sm: &SourceMap, file_name: Option<&str>, color: bool) -> String {
        let mut out = String::new();

        let red_bold = if color { "\x1b[1;31m" } else { "" };
        let yellow_bold = if color { "\x1b[1;33m" } else { "" };
        let blue_bold = if color { "\x1b[1;34m" } else { "" };
        let cyan_bold = if color { "\x1b[1;36m" } else { "" };
        let bold = if color { "\x1b[1m" } else { "" };
        let reset = if color { "\x1b[0m" } else { "" };

        let sev_color = match self.severity {
            Severity::Error => red_bold,
            Severity::Warning => yellow_bold,
        };

        // Header: e.g. "error: mismatched types in variable initialization"
        out.push_str(&format!(
            "{}{}: {}{}{}\n",
            sev_color,
            self.severity.name(),
            bold,
            self.message,
            reset
        ));

        // Primary location for arrow
        let (prim_start_line, prim_start_col, _, _) = sm.span_to_snippet_location(&self.primary.span);
        let file_disp = file_name.unwrap_or("<input>");

        // Collect all snippet locations to determine max line number for gutter width
        struct LocatedLabel<'a> {
            is_primary: bool,
            start_line: usize,
            start_col: usize,
            end_line: usize,
            end_col: usize,
            label: &'a Label,
        }

        let mut located: Vec<LocatedLabel> = Vec::new();

        let (psl, psc, pel, pec) = sm.span_to_snippet_location(&self.primary.span);
        located.push(LocatedLabel {
            is_primary: true,
            start_line: psl,
            start_col: psc,
            end_line: pel,
            end_col: pec,
            label: &self.primary,
        });

        for sec in &self.secondary {
            let (sl, sc, el, ec) = sm.span_to_snippet_location(&sec.span);
            located.push(LocatedLabel {
                is_primary: false,
                start_line: sl,
                start_col: sc,
                end_line: el,
                end_col: ec,
                label: sec,
            });
        }

        let mut max_line = prim_start_line;
        for loc in &located {
            max_line = max(max_line, max(loc.start_line, loc.end_line));
        }

        let gutter_width = max_line.to_string().len().max(1);
        let gutter_space = " ".repeat(gutter_width);

        // Arrow line: " --> file:line:col"
        out.push_str(&format!(
            " {}{}-->{} {}:{}:{}\n",
            blue_bold,
            gutter_space,
            reset,
            file_disp,
            prim_start_line,
            prim_start_col
        ));

        // Group labels by line
        let mut lines_map: BTreeMap<usize, Vec<&LocatedLabel>> = BTreeMap::new();
        for loc in &located {
            // For now, if single-line or multi-line, associate with start_line
            lines_map.entry(loc.start_line).or_default().push(loc);
        }

        // Empty gutter line before snippets
        out.push_str(&format!(" {}{}|{}\n", blue_bold, gutter_space, reset));

        let mut prev_line: Option<usize> = None;
        for (&line_num, labels_on_line) in &lines_map {
            if let Some(prev) = prev_line {
                if line_num > prev + 1 {
                    out.push_str(&format!(" {}...\n", blue_bold));
                }
            }
            prev_line = Some(line_num);

            let line_text = sm.get_line_text(line_num);
            out.push_str(&format!(
                "{}{:width$} |{} {}\n",
                blue_bold,
                line_num,
                reset,
                line_text,
                width = gutter_width
            ));

            // Render carets for labels starting on this line
            // Sort labels on line by start_col
            let mut sorted_labels = labels_on_line.clone();
            sorted_labels.sort_by_key(|l| l.start_col);

            let mut caret_line = String::new();
            caret_line.push_str(&format!(" {}{}|{} ", blue_bold, gutter_space, reset));

            let mut current_col = 1;
            let mut label_messages: Vec<String> = Vec::new();

            for loc in sorted_labels {
                if loc.start_col > current_col {
                    caret_line.push_str(&" ".repeat(loc.start_col - current_col));
                    current_col = loc.start_col;
                }

                let span_len = if loc.end_col >= loc.start_col {
                    loc.end_col - loc.start_col + 1
                } else {
                    1
                };

                let caret_char = if loc.is_primary { '^' } else { '-' };
                let caret_color = if loc.is_primary { red_bold } else { cyan_bold };

                caret_line.push_str(&format!(
                    "{}{}{}",
                    caret_color,
                    caret_char.to_string().repeat(span_len),
                    reset
                ));
                current_col += span_len;

                if !loc.label.message.is_empty() {
                    label_messages.push(loc.label.message.clone());
                }
            }

            if !label_messages.is_empty() {
                caret_line.push(' ');
                caret_line.push_str(&label_messages.join(", "));
            }

            out.push_str(&caret_line);
            out.push('\n');
        }

        // Empty gutter line after snippets
        out.push_str(&format!(" {}{}|{}\n", blue_bold, gutter_space, reset));

        // Suggestion / help note if present
        if let Some(suggestion) = &self.suggestion {
            out.push_str(&format!(
                " {}{}={} {}{}:{} {}\n",
                blue_bold,
                gutter_space,
                reset,
                bold,
                "help",
                reset,
                suggestion
            ));
        }

        out
    }

    /// Renders the diagnostic to stderr or stdout, automatically choosing ANSI color
    /// depending on whether stderr is connected to a terminal.
    pub fn render_to_terminal(&self, sm: &SourceMap, file_name: Option<&str>) -> String {
        let is_term = std::io::stderr().is_terminal();
        self.render(sm, file_name, is_term)
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.severity.name(), self.message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_diagnostic_rendering_plain() {
        let src = "fn test() {\n    let x: int = 5;\n    let y: bool = x + 1;\n}";
        let sm = SourceMap::new(src.to_string());

        // x + 1 is at bytes 50..55 in line 3
        let diag = Diagnostic::error_with_label(
            50..55,
            "mismatched types in variable initialization",
            "expected `bool`, found `int`",
        )
        .with_secondary(23..26, "`x` was declared with type `int` here")
        .with_suggestion("compare the integer expression to produce a `bool`");

        let rendered = diag.render(&sm, Some("test.eta"), false);

        assert!(rendered.contains("error: mismatched types in variable initialization"));
        assert!(rendered.contains("--> test.eta:3:19"));
        assert!(rendered.contains("let x: int = 5;"));
        assert!(rendered.contains("--- `x` was declared with type `int` here"));
        assert!(rendered.contains("let y: bool = x + 1;"));
        assert!(rendered.contains("^^^^^ expected `bool`, found `int`"));
        assert!(rendered.contains("= help: compare the integer expression to produce a `bool`"));
    }
}
