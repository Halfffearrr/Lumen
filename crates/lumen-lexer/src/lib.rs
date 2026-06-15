//! lumen-lexer — stage 1 of the Lumen pipeline.
//!
//! Turns a Lumen source string into a flat `Vec<Token>`. Each token records the
//! [`Span`](lumen_common::Span) it occupied, which every later stage relies on
//! for error reporting. The single entry point is [`tokenize`].
//!
//! ```
//! use lumen_lexer::{tokenize, TokenKind};
//! let tokens = tokenize("let x = 1 + 2").unwrap();
//! assert_eq!(tokens[0].kind, TokenKind::Let);
//! ```

mod scanner;
pub mod token;

pub use scanner::{LexError, Scanner};
pub use token::{StrPart, Token, TokenKind};

/// Tokenize a full Lumen source string into tokens terminated by `Eof`.
///
/// Returns the first [`LexError`] encountered, which carries the source span of
/// the problem.
pub fn tokenize(source: &str) -> Result<Vec<Token>, LexError> {
    Scanner::new(source).scan_all()
}

#[cfg(test)]
mod tests {
    use super::*;
    use TokenKind::*;

    /// Convenience: tokenize and keep only the token kinds.
    fn kinds(src: &str) -> Vec<TokenKind> {
        tokenize(src).unwrap().into_iter().map(|t| t.kind).collect()
    }

    #[test]
    fn integers_and_floats() {
        assert_eq!(kinds("42"), vec![Int(42), Eof]);
        assert_eq!(kinds("3.5"), vec![Float(3.5), Eof]);
        assert_eq!(kinds("2.0"), vec![Float(2.0), Eof]);
        assert_eq!(kinds("0 100 7"), vec![Int(0), Int(100), Int(7), Eof]);
    }

    #[test]
    fn doc_acceptance_let_statement() {
        // The acceptance example from the project document.
        assert_eq!(
            kinds("let x = 1 + 2;"),
            vec![
                Let,
                Ident("x".into()),
                Eq,
                Int(1),
                Plus,
                Int(2),
                Semicolon,
                Eof
            ],
        );
    }

    #[test]
    fn keywords_and_identifiers() {
        assert_eq!(
            kinds("let mut fn if else while for in loop break return true false nil"),
            vec![
                Let, Mut, Fn, If, Else, While, For, In, Loop, Break, Return, True, False, Nil, Eof,
            ],
        );
        // A word that merely starts with a keyword is still one identifier.
        assert_eq!(kinds("lettuce"), vec![Ident("lettuce".into()), Eof]);
        assert_eq!(kinds("for_each"), vec![Ident("for_each".into()), Eof]);
    }

    #[test]
    fn range_is_not_confused_with_float() {
        assert_eq!(kinds("1..5"), vec![Int(1), DotDot, Int(5), Eof]);
        assert_eq!(kinds("1..=5"), vec![Int(1), DotDotEq, Int(5), Eof]);
        assert_eq!(kinds("1.5"), vec![Float(1.5), Eof]);
    }

    #[test]
    fn all_operators_and_delimiters() {
        assert_eq!(
            kinds("+ - * / % == != < <= > >= && || ! = ( ) { } [ ] , : ;"),
            vec![
                Plus, Minus, Star, Slash, Percent, EqEq, BangEq, Lt, LtEq, Gt, GtEq, AmpAmp,
                PipePipe, Bang, Eq, LParen, RParen, LBrace, RBrace, LBracket, RBracket, Comma,
                Colon, Semicolon, Eof,
            ],
        );
    }

    #[test]
    fn string_escapes_are_resolved() {
        assert_eq!(
            kinds(r#""a\nb\t\"c\\d""#),
            vec![Str("a\nb\t\"c\\d".into()), Eof]
        );
        assert_eq!(
            kinds(r#""\{literal\}""#),
            vec![Str("{literal}".into()), Eof]
        );
    }

    #[test]
    fn line_comments_are_skipped() {
        assert_eq!(kinds("1 // ignored\n2"), vec![Int(1), Int(2), Eof]);
        assert_eq!(kinds("// only a comment"), vec![Eof]);
    }

    #[test]
    fn string_interpolation_builds_a_template() {
        let tokens = tokenize(r#""hi {name}!""#).unwrap();
        match &tokens[0].kind {
            Template(parts) => {
                assert_eq!(parts.len(), 3);
                assert_eq!(parts[0], StrPart::Literal("hi ".into()));
                match &parts[1] {
                    StrPart::Expr { source, .. } => assert_eq!(source, "name"),
                    other => panic!("expected an expr part, got {other:?}"),
                }
                assert_eq!(parts[2], StrPart::Literal("!".into()));
            }
            other => panic!("expected a template, got {other:?}"),
        }
    }

    #[test]
    fn interpolation_balances_nested_braces() {
        let tokens = tokenize(r#""{ {a: 1} }""#).unwrap();
        match &tokens[0].kind {
            Template(parts) => match parts.iter().find(|p| matches!(p, StrPart::Expr { .. })) {
                Some(StrPart::Expr { source, .. }) => assert_eq!(source, " {a: 1} "),
                _ => panic!("expected an expr part with balanced braces"),
            },
            other => panic!("expected a template, got {other:?}"),
        }
    }

    #[test]
    fn spans_track_line_and_column() {
        // `let` on line 1, `x` on line 2 after two spaces of indentation.
        let tokens = tokenize("let\n  x").unwrap();
        assert_eq!((tokens[0].span.line, tokens[0].span.col), (1, 1));
        assert_eq!((tokens[1].span.line, tokens[1].span.col), (2, 3));
        // Byte offsets slice the original source back out.
        assert_eq!(tokens[1].span.start, 6); // "let\n  " is 6 bytes
        assert_eq!(tokens[1].lexeme, "x");
    }

    #[test]
    fn unicode_identifiers_and_string_contents() {
        let tokens = tokenize(r#"let 名字 = "你好""#).unwrap();
        assert_eq!(tokens[0].kind, Let);
        assert_eq!(tokens[1].kind, Ident("名字".into()));
        assert_eq!(tokens[2].kind, Eq);
        assert_eq!(tokens[3].kind, Str("你好".into()));
        assert_eq!(tokens[4].kind, Eof);
    }

    #[test]
    fn errors_carry_location() {
        assert!(matches!(
            tokenize("a @ b"),
            Err(LexError::UnexpectedChar { ch: '@', .. })
        ));
        assert!(matches!(
            tokenize(r#""no closing quote"#),
            Err(LexError::UnterminatedString { .. })
        ));
        assert!(matches!(
            tokenize(r#""open {expr"#),
            Err(LexError::UnterminatedInterpolation { .. })
        ));
        assert!(matches!(
            tokenize(r#""bad \q escape""#),
            Err(LexError::InvalidEscape { ch: 'q', .. })
        ));
        // A lone `&` is not a valid token (only `&&` is).
        assert!(matches!(
            tokenize("a & b"),
            Err(LexError::UnexpectedChar { ch: '&', .. })
        ));
    }

    #[test]
    fn input_always_ends_with_eof() {
        assert_eq!(kinds(""), vec![Eof]);
        assert_eq!(kinds("   \n\t  "), vec![Eof]);
    }
}
