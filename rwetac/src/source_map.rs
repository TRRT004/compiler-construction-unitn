//! Maps byte offsets to source locations.
//!
//! The [`SourceMap`] converts byte indices (from [`Span`](logos::Span)s on AST nodes)
//! back to human-readable `(line, column)` positions. It is used by every compilation
//! phase to produce error messages with source locations.

// Simple implementation. In this case only line starts for later look up
// An extended version could be used to print the source at that location and add little arrows to it
// Similar to Rust compiler

/// Maps byte offsets in source code to `(line, column)` positions.
///
/// Precomputes line start offsets at construction time and uses binary search
/// for lookups, so repeated queries are efficient.
pub struct SourceMap {
    source: String,
    line_starts: Vec<usize>,
}

impl SourceMap {
    pub fn new(source: String) -> Self {
        let mut line_starts = vec![0]; // The first line always starts at index 0.
        // Find the start of each subsequent line.
        line_starts.extend(source.match_indices('\n').map(|(i, _)| i + 1));

        SourceMap {
            source,
            line_starts,
        }
    }

    /// Converts a byte index into a `(line, column)` tuple.
    ///
    /// Both line and column numbers are 1-based.
    pub fn get_line_column(&self, byte_idx: usize) -> (usize, usize) {
        // Use binary search to find the line the byte_idx is on.
        let line_num = match self.line_starts.binary_search(&byte_idx) {
            Ok(line_idx) => line_idx + 1, // Exact match, byte is start of this line.
            Err(next_line_idx) => next_line_idx, // Not an exact match, belongs to the previous line.
        };

        // The column is the offset from the start of that line.
        let line_start_idx = self.line_starts[line_num - 1];
        let col_num = byte_idx - line_start_idx + 1;

        (line_num, col_num)
    }

    /// A convenience function to convert an entire Span.
    pub fn get_span_location(&self, span: &logos::Span) -> (usize, usize) {
        self.get_line_column(span.start)
    }

    /// Returns the raw text of the given 1-based line number without trailing newlines.
    pub fn get_line_text(&self, line_num: usize) -> &str {
        if line_num == 0 || line_num > self.line_starts.len() {
            return "";
        }
        let start = self.line_starts[line_num - 1];
        let end = if line_num < self.line_starts.len() {
            self.line_starts[line_num]
        } else {
            self.source.len()
        };
        let text = &self.source[start..end];
        text.trim_end_matches(['\r', '\n'])
    }

    /// Clamps a zero-width span (e.g. at EOF) so that it spans at least 1 character.
    pub fn clamp_span(&self, span: &logos::Span) -> logos::Span {
        if span.start >= span.end {
            span.start..(span.start + 1)
        } else {
            span.clone()
        }
    }

    /// Returns (`start_line`, `start_col`, `end_line`, `end_col`) for the given span,
    /// clamping zero-width spans to width 1.
    pub fn span_to_snippet_location(&self, span: &logos::Span) -> (usize, usize, usize, usize) {
        let clamped = self.clamp_span(span);
        let (start_line, start_col) = self.get_line_column(clamped.start);
        let end_idx = if clamped.end > clamped.start {
            clamped.end - 1
        } else {
            clamped.start
        };
        let (end_line, end_col) = self.get_line_column(end_idx);
        (start_line, start_col, end_line, end_col)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_line_text_and_snippet() {
        let src = "line 1\nline 2\r\nline 3";
        let sm = SourceMap::new(src.to_string());
        assert_eq!(sm.get_line_text(1), "line 1");
        assert_eq!(sm.get_line_text(2), "line 2");
        assert_eq!(sm.get_line_text(3), "line 3");
        assert_eq!(sm.get_line_text(4), "");

        // Span within line 2: "ne 2"
        let span = 9..13;
        let (sl, sc, el, ec) = sm.span_to_snippet_location(&span);
        assert_eq!((sl, sc), (2, 3));
        assert_eq!((el, ec), (2, 6));

        // Zero-width span at EOF
        let eof_span = src.len()..src.len();
        let clamped = sm.clamp_span(&eof_span);
        assert_eq!(clamped.end - clamped.start, 1);
    }
}
