//! The Lumen scanner: the core of stage 1.
//!
//! [`Scanner`] walks the source string one Unicode character at a time and
//! groups characters into [`Token`]s. It keeps two cursors in lock-step:
//! * a byte `offset`, so each token's [`Span`] can slice the original source;
//! * a 1-based `line`/`col`, so errors report a human-readable location.
//!
//! Design points worth being able to explain:
//! * We index the source by byte offset but always step whole `char`s, so
//!   multi-byte UTF-8 (Chinese identifiers, string contents) just works.
//! * A number stops before `..`, so `1..5` lexes as `1`, `..`, `5` instead of
//!   mistaking `1.` for a float.
//! * A string becomes a plain `Str`, or — if it contains `{expr}` — a `Template`
//!   built from alternating literal and expression parts.

use lumen_common::{Pos, Span};

use crate::token::{StrPart, Token, TokenKind};

/// An error raised while scanning. Each variant carries a [`Span`] so the caller
/// can underline the exact offending location in the source.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum LexError {
    #[error("unexpected character '{ch}' at line {}, column {}", .span.line, .span.col)]
    UnexpectedChar { ch: char, span: Span },

    #[error("unterminated string starting at line {}, column {}", .span.line, .span.col)]
    UnterminatedString { span: Span },

    #[error("unterminated interpolation: missing '}}' at line {}, column {}", .span.line, .span.col)]
    UnterminatedInterpolation { span: Span },

    #[error("invalid escape sequence '\\{ch}' at line {}, column {}", .span.line, .span.col)]
    InvalidEscape { ch: char, span: Span },

    #[error("number '{lexeme}' is out of range at line {}, column {}", .span.line, .span.col)]
    NumberOutOfRange { lexeme: String, span: Span },
}

/// Scans one `&str` of Lumen source into tokens.
pub struct Scanner<'src> {
    source: &'src str,
    /// Byte offset of the next unread character.
    offset: usize,
    /// 1-based line of the next unread character.
    line: u32,
    /// 1-based column (in chars) of the next unread character.
    col: u32,
}

impl<'src> Scanner<'src> {
    pub fn new(source: &'src str) -> Self {
        Scanner {
            source,
            offset: 0,
            line: 1,
            col: 1,
        }
    }

    /// Scan the whole input into tokens, always ending with an `Eof` token.
    pub fn scan_all(mut self) -> Result<Vec<Token>, LexError> {
        let mut tokens = Vec::new();
        loop {
            self.skip_trivia();
            if self.is_at_end() {
                tokens.push(Token {
                    kind: TokenKind::Eof,
                    span: self.point_span(),
                    lexeme: String::new(),
                });
                return Ok(tokens);
            }
            tokens.push(self.scan_token()?);
        }
    }

    // ----------------------------------------------------------------- cursor

    fn is_at_end(&self) -> bool {
        self.offset >= self.source.len()
    }

    /// The current character without consuming it.
    fn peek(&self) -> Option<char> {
        self.source[self.offset..].chars().next()
    }

    /// The character after the current one, without consuming anything.
    fn peek_next(&self) -> Option<char> {
        let mut chars = self.source[self.offset..].chars();
        chars.next();
        chars.next()
    }

    /// Consume and return the current character, advancing offset/line/col.
    /// `offset` always lands on a UTF-8 boundary because we add `len_utf8()`.
    fn advance(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.offset += c.len_utf8();
        if c == '\n' {
            self.line += 1;
            self.col = 1;
        } else {
            self.col += 1;
        }
        Some(c)
    }

    /// Consume the current character only if it equals `expected`.
    fn eat(&mut self, expected: char) -> bool {
        if self.peek() == Some(expected) {
            self.advance();
            true
        } else {
            false
        }
    }

    /// The current cursor position.
    fn here(&self) -> Pos {
        Pos::new(self.line, self.col)
    }

    /// A zero-width span at the current position (used for `Eof`).
    fn point_span(&self) -> Span {
        Span::new(self.offset, self.offset, self.line, self.col)
    }

    /// Build a span from `start`/`start_pos` to the current offset.
    fn span_from(&self, start: usize, start_pos: Pos) -> Span {
        Span::new(start, self.offset, start_pos.line, start_pos.col)
    }

    // ----------------------------------------------------------------- trivia

    /// Skip whitespace and `// line comments` between tokens.
    fn skip_trivia(&mut self) {
        loop {
            match self.peek() {
                Some(c) if c.is_whitespace() => {
                    self.advance();
                }
                Some('/') if self.peek_next() == Some('/') => {
                    // Consume to end of line; the '\n' is left for the
                    // whitespace branch so the line counter still ticks.
                    while let Some(c) = self.peek() {
                        if c == '\n' {
                            break;
                        }
                        self.advance();
                    }
                }
                _ => return,
            }
        }
    }

    // ------------------------------------------------------------- dispatch

    /// Scan exactly one token. The caller guarantees we are not at end of input
    /// and that leading trivia has already been skipped.
    fn scan_token(&mut self) -> Result<Token, LexError> {
        let start = self.offset;
        let start_pos = self.here();
        let c = self.advance().expect("scan_token called at end of input");

        let kind = match c {
            '(' => TokenKind::LParen,
            ')' => TokenKind::RParen,
            '{' => TokenKind::LBrace,
            '}' => TokenKind::RBrace,
            '[' => TokenKind::LBracket,
            ']' => TokenKind::RBracket,
            ',' => TokenKind::Comma,
            ':' => TokenKind::Colon,
            ';' => TokenKind::Semicolon,
            '+' => TokenKind::Plus,
            '-' => TokenKind::Minus,
            '*' => TokenKind::Star,
            '/' => TokenKind::Slash, // `//` comments are handled in skip_trivia
            '%' => TokenKind::Percent,
            '=' => {
                if self.eat('=') {
                    TokenKind::EqEq
                } else {
                    TokenKind::Eq
                }
            }
            '!' => {
                if self.eat('=') {
                    TokenKind::BangEq
                } else {
                    TokenKind::Bang
                }
            }
            '<' => {
                if self.eat('=') {
                    TokenKind::LtEq
                } else {
                    TokenKind::Lt
                }
            }
            '>' => {
                if self.eat('=') {
                    TokenKind::GtEq
                } else {
                    TokenKind::Gt
                }
            }
            '&' => {
                if self.eat('&') {
                    TokenKind::AmpAmp
                } else {
                    return Err(self.char_error('&', start, start_pos));
                }
            }
            '|' => {
                if self.eat('|') {
                    TokenKind::PipePipe
                } else {
                    return Err(self.char_error('|', start, start_pos));
                }
            }
            '.' => {
                if self.eat('.') {
                    if self.eat('=') {
                        TokenKind::DotDotEq
                    } else {
                        TokenKind::DotDot
                    }
                } else {
                    return Err(self.char_error('.', start, start_pos));
                }
            }
            '"' => self.string(start, start_pos)?,
            c if c.is_ascii_digit() => self.number(start, start_pos)?,
            c if is_ident_start(c) => self.identifier(start),
            other => return Err(self.char_error(other, start, start_pos)),
        };

        Ok(Token {
            kind,
            span: self.span_from(start, start_pos),
            lexeme: self.source[start..self.offset].to_string(),
        })
    }

    fn char_error(&self, ch: char, start: usize, start_pos: Pos) -> LexError {
        LexError::UnexpectedChar {
            ch,
            span: self.span_from(start, start_pos),
        }
    }

    // -------------------------------------------------------------- literals

    /// Scan a number. The first digit has already been consumed. A `.` is a
    /// decimal point only when a digit follows it, so range syntax like `1..5`
    /// is not mistaken for a float.
    fn number(&mut self, start: usize, start_pos: Pos) -> Result<TokenKind, LexError> {
        while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
            self.advance();
        }

        let mut is_float = false;
        if self.peek() == Some('.') && matches!(self.peek_next(), Some(c) if c.is_ascii_digit()) {
            is_float = true;
            self.advance(); // the '.'
            while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                self.advance();
            }
        }

        let text = &self.source[start..self.offset];
        let span = self.span_from(start, start_pos);
        if is_float {
            let value: f64 = text.parse().map_err(|_| LexError::NumberOutOfRange {
                lexeme: text.to_string(),
                span,
            })?;
            Ok(TokenKind::Float(value))
        } else {
            let value: i64 = text.parse().map_err(|_| LexError::NumberOutOfRange {
                lexeme: text.to_string(),
                span,
            })?;
            Ok(TokenKind::Int(value))
        }
    }

    /// Scan an identifier (the first character is already consumed), then check
    /// whether the whole word is a reserved keyword.
    fn identifier(&mut self, start: usize) -> TokenKind {
        while matches!(self.peek(), Some(c) if is_ident_continue(c)) {
            self.advance();
        }
        let word = &self.source[start..self.offset];
        TokenKind::keyword(word).unwrap_or_else(|| TokenKind::Ident(word.to_string()))
    }

    /// Scan a string literal. The opening `"` is already consumed. Produces a
    /// plain `Str` unless the string contains `{expr}` interpolation, in which
    /// case it produces a `Template` of literal and expression parts.
    fn string(&mut self, start: usize, start_pos: Pos) -> Result<TokenKind, LexError> {
        let mut parts: Vec<StrPart> = Vec::new();
        let mut literal = String::new();
        let mut interpolated = false;

        loop {
            let Some(c) = self.advance() else {
                return Err(LexError::UnterminatedString {
                    span: self.span_from(start, start_pos),
                });
            };
            match c {
                '"' => break,
                '\\' => self.escape(&mut literal, start, start_pos)?,
                '{' => {
                    interpolated = true;
                    if !literal.is_empty() {
                        parts.push(StrPart::Literal(std::mem::take(&mut literal)));
                    }
                    parts.push(self.interpolation(start, start_pos)?);
                }
                other => literal.push(other),
            }
        }

        if interpolated {
            if !literal.is_empty() {
                parts.push(StrPart::Literal(literal));
            }
            Ok(TokenKind::Template(parts))
        } else {
            Ok(TokenKind::Str(literal))
        }
    }

    /// Handle the character(s) after a `\` inside a string, pushing the decoded
    /// character onto `literal`. `str_start`/`str_pos` describe the enclosing
    /// string so an unterminated string can be reported.
    fn escape(
        &mut self,
        literal: &mut String,
        str_start: usize,
        str_pos: Pos,
    ) -> Result<(), LexError> {
        let esc_start = self.offset;
        let esc_pos = self.here();
        let Some(e) = self.advance() else {
            return Err(LexError::UnterminatedString {
                span: self.span_from(str_start, str_pos),
            });
        };
        match e {
            'n' => literal.push('\n'),
            't' => literal.push('\t'),
            'r' => literal.push('\r'),
            '0' => literal.push('\0'),
            '"' => literal.push('"'),
            '\\' => literal.push('\\'),
            // `\{` / `\}` let scripts write literal braces inside interpolated
            // strings without starting an interpolation.
            '{' => literal.push('{'),
            '}' => literal.push('}'),
            other => {
                return Err(LexError::InvalidEscape {
                    ch: other,
                    span: self.span_from(esc_start, esc_pos),
                })
            }
        }
        Ok(())
    }

    /// Capture the raw source of a `{expr}` interpolation. The opening `{` is
    /// already consumed. Brace depth is tracked so nested `{}` (e.g. a dict
    /// literal inside the expression) is balanced correctly.
    fn interpolation(&mut self, str_start: usize, str_pos: Pos) -> Result<StrPart, LexError> {
        let expr_start = self.offset;
        let expr_pos = self.here();
        let mut depth = 1usize;
        loop {
            let Some(c) = self.peek() else {
                return Err(LexError::UnterminatedInterpolation {
                    span: self.span_from(str_start, str_pos),
                });
            };
            match c {
                '{' => {
                    depth += 1;
                    self.advance();
                }
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        let source = self.source[expr_start..self.offset].to_string();
                        let span = Span::new(expr_start, self.offset, expr_pos.line, expr_pos.col);
                        self.advance(); // consume the closing '}'
                        return Ok(StrPart::Expr { source, span });
                    }
                    self.advance();
                }
                _ => {
                    self.advance();
                }
            }
        }
    }
}

/// A character that may start an identifier: a letter (including non-ASCII such
/// as Chinese) or an underscore.
fn is_ident_start(c: char) -> bool {
    c == '_' || c.is_alphabetic()
}

/// A character that may continue an identifier: an identifier-start char or a
/// digit.
fn is_ident_continue(c: char) -> bool {
    c == '_' || c.is_alphanumeric()
}
