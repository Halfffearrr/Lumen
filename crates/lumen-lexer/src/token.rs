//! Token types produced by the Lumen lexer.
//!
//! A [`Token`] couples a classified [`TokenKind`] with the exact source slice it
//! was scanned from (`lexeme`) and its [`Span`]. Everything downstream — parser,
//! resolver, error reporter — works in terms of tokens and never looks at raw
//! characters again.

use lumen_common::Span;

/// The category of a lexed token, carrying the decoded value for literals.
#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    // --- Literals ---
    Int(i64),
    Float(f64),
    /// A string with no interpolation; escape sequences already resolved.
    Str(String),
    /// A string containing `{expr}` interpolation, split into ordered parts.
    Template(Vec<StrPart>),
    Ident(String),

    // --- Keywords ---
    Let,
    Mut,
    Fn,
    If,
    Else,
    While,
    For,
    In,
    Loop,
    Break,
    Return,
    True,
    False,
    Nil,

    // --- Operators ---
    Plus,     // +
    Minus,    // -
    Star,     // *
    Slash,    // /
    Percent,  // %
    Eq,       // =
    EqEq,     // ==
    BangEq,   // !=
    Lt,       // <
    LtEq,     // <=
    Gt,       // >
    GtEq,     // >=
    AmpAmp,   // &&
    PipePipe, // ||
    Bang,     // !
    DotDot,   // ..  (exclusive range)
    DotDotEq, // ..= (inclusive range)

    // --- Delimiters ---
    LParen,    // (
    RParen,    // )
    LBrace,    // {
    RBrace,    // }
    LBracket,  // [
    RBracket,  // ]
    Comma,     // ,
    Colon,     // :
    Semicolon, // ;

    /// End of input. Always the last token so the parser has a sentinel to stop
    /// on without bounds-checking the token vector.
    Eof,
}

impl TokenKind {
    /// Map a fully-scanned word to its keyword `TokenKind`, or `None` if it is a
    /// normal identifier. The lexer scans the whole identifier first and *then*
    /// consults this table, so `lettuce` lexes as one `Ident`, not `let` plus
    /// `tuce`.
    pub fn keyword(word: &str) -> Option<TokenKind> {
        let kw = match word {
            "let" => TokenKind::Let,
            "mut" => TokenKind::Mut,
            "fn" => TokenKind::Fn,
            "if" => TokenKind::If,
            "else" => TokenKind::Else,
            "while" => TokenKind::While,
            "for" => TokenKind::For,
            "in" => TokenKind::In,
            "loop" => TokenKind::Loop,
            "break" => TokenKind::Break,
            "return" => TokenKind::Return,
            "true" => TokenKind::True,
            "false" => TokenKind::False,
            "nil" => TokenKind::Nil,
            _ => return None,
        };
        Some(kw)
    }
}

/// One ordered piece of an interpolated string template such as `"a {b} c"`.
#[derive(Debug, Clone, PartialEq)]
pub enum StrPart {
    /// A literal chunk of text, with escape sequences already resolved.
    Literal(String),
    /// The raw source of an embedded `{expr}`. Stage 2 re-lexes and parses
    /// `source`; `span` locates it in the original file for error reporting.
    Expr { source: String, span: Span },
}

/// A single lexed token.
#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
    /// The exact source text this token was scanned from.
    pub lexeme: String,
}
