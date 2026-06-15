//! lumen-common — types shared by every crate in the Lumen pipeline.
//!
//! The central export is [`Span`], the source location attached to each token
//! and (from stage 2 on) to every AST node. Recording locations from the very
//! first stage is what later makes friendly, source-highlighting error messages
//! possible: if the lexer did not remember *where* a token came from, no amount
//! of work downstream could put a caret under the offending character.

/// A single point in the source text, as a 1-based line and column.
///
/// Columns count Unicode scalar values (`char`s), not bytes, so a caret drawn
/// under column `N` lines up visually even when the line contains multi-byte
/// UTF-8 such as Chinese identifiers or string contents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pos {
    pub line: u32,
    pub col: u32,
}

impl Pos {
    pub fn new(line: u32, col: u32) -> Self {
        Pos { line, col }
    }
}

/// The region of source text a token or AST node came from.
///
/// `start` and `end` are byte offsets into the original source string and form
/// a half-open range `[start, end)`; slicing `source[start..end]` yields the
/// exact text. `line`/`col` record where `start` sits so an error can be located
/// without re-scanning the file from the top.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub line: u32,
    pub col: u32,
}

impl Span {
    pub fn new(start: usize, end: usize, line: u32, col: u32) -> Self {
        Span {
            start,
            end,
            line,
            col,
        }
    }

    /// The byte length of the spanned text.
    pub fn len(&self) -> usize {
        self.end - self.start
    }

    /// True when the span covers no characters (e.g. the synthetic EOF span).
    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }

    /// The start position of this span as a [`Pos`].
    pub fn pos(&self) -> Pos {
        Pos::new(self.line, self.col)
    }

    /// Combine two spans into one covering both. Used in later stages so that,
    /// for example, a binary expression spans from its left operand's start
    /// through its right operand's end. The result keeps the earlier start's
    /// line/col.
    pub fn merge(self, other: Span) -> Span {
        let (first, last) = if self.start <= other.start {
            (self, other)
        } else {
            (other, self)
        };
        Span {
            start: first.start,
            end: first.end.max(last.end),
            line: first.line,
            col: first.col,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn len_and_emptiness() {
        let s = Span::new(3, 7, 1, 4);
        assert_eq!(s.len(), 4);
        assert!(!s.is_empty());
        assert!(Span::new(5, 5, 1, 1).is_empty());
    }

    #[test]
    fn merge_keeps_outer_bounds_and_earlier_start() {
        let a = Span::new(0, 2, 1, 1);
        let b = Span::new(5, 9, 1, 6);
        let merged = a.merge(b);
        assert_eq!((merged.start, merged.end), (0, 9));
        assert_eq!((merged.line, merged.col), (1, 1));
        // merging is order-independent in coverage
        assert_eq!(b.merge(a).start, 0);
        assert_eq!(b.merge(a).end, 9);
    }
}
