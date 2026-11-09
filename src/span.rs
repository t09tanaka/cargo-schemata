// Copyright 2021-2023 Martin Pool
// Ported from cargo-mutants 27.1.0 (MIT); see NOTICE.

//! Locations (line/column) and spans between them in source code, plus the
//! conversion from line/column to byte offsets that the rewriting stage needs.

use std::fmt;
use std::ops::Range;

use serde::Serialize;

/// A (line, column) position in a source file.
#[derive(Clone, Copy, Eq, PartialEq, Serialize)]
pub struct LineColumn {
    /// 1-based line number.
    pub line: usize,
    /// 1-based column, measured in chars.
    pub column: usize,
}

impl From<proc_macro2::LineColumn> for LineColumn {
    fn from(l: proc_macro2::LineColumn) -> Self {
        LineColumn {
            line: l.line,
            column: l.column + 1,
        }
    }
}

impl fmt::Debug for LineColumn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "LineColumn({}, {})", self.line, self.column)
    }
}

/// A contiguous text span in a file.
#[derive(Clone, Copy, Eq, PartialEq, Serialize)]
pub struct Span {
    /// The *inclusive* position where the span starts.
    pub start: LineColumn,
    /// The *exclusive* position where the span ends.
    pub end: LineColumn,
}

impl Span {
    /// Return the region of a multi-line string that this span covers.
    pub fn extract(&self, s: &str) -> String {
        let mut r = String::new();
        let mut line_no = 1;
        let mut col_no = 1;
        let start = self.start;
        let end = self.end;
        for c in s.chars() {
            if ((line_no == start.line && col_no >= start.column) || line_no > start.line)
                && (line_no < end.line || (line_no == end.line && col_no < end.column))
            {
                r.push(c);
            }
            if c == '\n' {
                line_no += 1;
                if line_no > end.line {
                    break;
                }
                col_no = 1;
            } else if c == '\r' {
                // counts as part of the last column, not a separate column
            } else {
                col_no += 1;
            }
            if line_no == end.line && col_no >= end.column {
                break;
            }
        }
        r
    }

    /// Replace a subregion of text.
    ///
    /// Returns a copy of `s` with the region identified by this span replaced by
    /// `replacement`.
    pub fn replace(&self, s: &str, replacement: &str) -> String {
        let mut r = String::with_capacity(s.len() + replacement.len());
        let mut line_no = 1;
        let mut col_no = 1;
        let start = self.start;
        let end = self.end;
        for c in s.chars() {
            if line_no == start.line && col_no == start.column {
                r.push_str(replacement);
            }
            if line_no < start.line
                || line_no > end.line
                || (line_no == start.line && col_no < start.column)
                || (line_no == end.line && col_no >= end.column)
            {
                r.push(c);
            }
            if c == '\n' {
                line_no += 1;
                col_no = 1;
            } else if c == '\r' {
                // counts as part of the last column, not a separate column
            } else {
                col_no += 1;
            }
        }
        if line_no == start.line && col_no == start.column {
            r.push_str(replacement);
        }
        r
    }
}

impl From<proc_macro2::Span> for Span {
    fn from(s: proc_macro2::Span) -> Self {
        Span {
            start: s.start().into(),
            end: s.end().into(),
        }
    }
}

impl fmt::Debug for Span {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Span({}, {}, {}, {})",
            self.start.line, self.start.column, self.end.line, self.end.column
        )
    }
}

/// Maps (line, column) positions to byte offsets in a given text.
///
/// Built from the *original* file text (which may contain CRLF), so that the
/// byte ranges handed to the rewriting stage index the real file contents.
pub struct LineIndex<'a> {
    text: &'a str,
    line_starts: Vec<usize>,
}

impl<'a> LineIndex<'a> {
    pub fn new(text: &'a str) -> Self {
        let mut line_starts = vec![0];
        line_starts.extend(text.match_indices('\n').map(|(i, _)| i + 1));
        LineIndex { text, line_starts }
    }

    /// Byte offset of a position. Columns count chars, 1-based.
    pub fn offset(&self, pos: LineColumn) -> usize {
        let line_start = self.line_starts[pos.line - 1];
        let rest = &self.text[line_start..];
        rest.char_indices()
            .nth(pos.column - 1)
            .map_or(self.text.len(), |(i, _)| line_start + i)
    }

    pub fn range(&self, span: Span) -> Range<usize> {
        self.offset(span.start)..self.offset(span.end)
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn line_index_counts_chars_and_handles_crlf() {
        let text = "aé\r\nb+c\n";
        let ix = LineIndex::new(text);
        let lc = |line, column| LineColumn { line, column };
        assert_eq!(ix.offset(lc(1, 3)), 3);
        assert_eq!(ix.offset(lc(2, 2)), 6);
        let span = Span {
            start: lc(2, 2),
            end: lc(2, 3),
        };
        assert_eq!(&text[ix.range(span)], "+");
    }

    #[test]
    fn extract_and_replace() {
        let span = Span {
            start: LineColumn { line: 2, column: 2 },
            end: LineColumn { line: 2, column: 4 },
        };
        assert_eq!(span.extract("abc\ndefg\n"), "ef");
        assert_eq!(span.replace("abc\ndefg\n", "X"), "abc\ndXg\n");
    }
}
