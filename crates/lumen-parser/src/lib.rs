//! lumen-parser — stage 2 of the Lumen pipeline.
//!
//! Two passes over the token stream:
//! 1. [`parse`] turns tokens into an [`ast::Program`] using recursive descent for
//!    statements and Pratt parsing for expressions.
//! 2. [`resolve`] statically checks that program for undefined variables,
//!    assignments to immutable bindings, stray `break`s, and call-arity errors.
//!
//! ```
//! use lumen_lexer::tokenize;
//! use lumen_parser::{parse, resolve};
//! let tokens = tokenize("let x = 1 + 2\nprint(x)").unwrap();
//! let program = parse(tokens).unwrap();
//! resolve(&program).unwrap();
//! assert_eq!(program.len(), 2);
//! ```

pub mod ast;
mod parser;
mod resolver;

pub use parser::{parse, ParseError, Parser};
pub use resolver::{resolve, resolve_with_globals, ResolveError};

#[cfg(test)]
mod tests {
    use super::ast::*;
    use super::*;
    use lumen_lexer::tokenize;

    /// Parse source into a program, panicking on lex/parse errors.
    fn prog(src: &str) -> Program {
        parse(tokenize(src).unwrap()).unwrap()
    }

    /// Parse a single expression by wrapping it in a trivial program.
    fn expr(src: &str) -> Expr {
        match prog(src).pop().unwrap() {
            Stmt::Expr(e) => e,
            other => panic!("expected an expression statement, got {other:?}"),
        }
    }

    #[test]
    fn let_binding_records_mutability() {
        let p = prog("let x = 1\nlet mut y = 2");
        assert!(matches!(
            p[0],
            Stmt::Let {
                is_mutable: false,
                ..
            }
        ));
        assert!(matches!(
            p[1],
            Stmt::Let {
                is_mutable: true,
                ..
            }
        ));
    }

    #[test]
    fn pratt_precedence_groups_multiplication_first() {
        // 1 + 2 * 3  ==  1 + (2 * 3)
        let e = expr("1 + 2 * 3");
        match e.kind {
            ExprKind::Binary {
                op: BinOp::Add,
                right,
                ..
            } => {
                assert!(matches!(
                    right.kind,
                    ExprKind::Binary { op: BinOp::Mul, .. }
                ));
            }
            other => panic!("expected a top-level +, got {other:?}"),
        }
    }

    #[test]
    fn comparison_binds_looser_than_arithmetic() {
        // a + b < c * d  ==  (a + b) < (c * d)
        let e = expr("a + b < c * d");
        match e.kind {
            ExprKind::Binary {
                op: BinOp::Lt,
                left,
                right,
            } => {
                assert!(matches!(left.kind, ExprKind::Binary { op: BinOp::Add, .. }));
                assert!(matches!(
                    right.kind,
                    ExprKind::Binary { op: BinOp::Mul, .. }
                ));
            }
            other => panic!("expected a top-level <, got {other:?}"),
        }
    }

    #[test]
    fn assignment_is_right_associative_and_needs_a_target() {
        let e = expr("a = b = 1");
        assert!(matches!(e.kind, ExprKind::Assign { .. }));
        // `1 = 2` is not a valid assignment target.
        let err = parse(tokenize("1 = 2").unwrap()).unwrap_err();
        assert!(matches!(err, ParseError::InvalidAssignTarget { .. }));
    }

    #[test]
    fn if_is_an_expression_with_blocks() {
        let e = expr(r#"if x > 0 { "pos" } else { "neg" }"#);
        match e.kind {
            ExprKind::If {
                then_branch,
                else_branch,
                ..
            } => {
                assert!(matches!(
                    then_branch.tail.as_deref(),
                    Some(Expr {
                        kind: ExprKind::Str(_),
                        ..
                    })
                ));
                assert!(else_branch.is_some());
            }
            other => panic!("expected an if expression, got {other:?}"),
        }
    }

    #[test]
    fn range_parses_below_arithmetic() {
        // 1..n+1  ==  1 ..(exclusive) (n + 1)
        let e = expr("1..n+1");
        match e.kind {
            ExprKind::Range {
                inclusive: false,
                end,
                ..
            } => {
                assert!(matches!(end.kind, ExprKind::Binary { op: BinOp::Add, .. }));
            }
            other => panic!("expected an exclusive range, got {other:?}"),
        }
        assert!(matches!(
            expr("1..=5").kind,
            ExprKind::Range {
                inclusive: true,
                ..
            }
        ));
    }

    #[test]
    fn calls_and_indexing_are_postfix() {
        assert!(matches!(expr("f(1, 2)").kind, ExprKind::Call { .. }));
        assert!(matches!(expr("xs[0]").kind, ExprKind::Index { .. }));
        // -f(x) == -(f(x)): the call binds tighter than unary minus.
        match expr("-f(x)").kind {
            ExprKind::Unary {
                op: UnaryOp::Neg,
                operand,
            } => {
                assert!(matches!(operand.kind, ExprKind::Call { .. }));
            }
            other => panic!("expected unary neg over a call, got {other:?}"),
        }
    }

    #[test]
    fn list_and_dict_literals() {
        assert!(matches!(expr("[1, 2, 3]").kind, ExprKind::List(_)));
        match expr(r#"{"a": 1, "b": 2}"#).kind {
            ExprKind::Dict(pairs) => assert_eq!(pairs.len(), 2),
            other => panic!("expected a dict, got {other:?}"),
        }
    }

    #[test]
    fn string_interpolation_lowers_to_segments() {
        match expr(r#""hi {name}, {1 + 1}!""#).kind {
            ExprKind::Interp(segs) => {
                // "hi ", name, ", ", (1 + 1), "!"
                assert_eq!(segs.len(), 5);
                assert!(matches!(segs[0], StrSeg::Literal(_)));
                assert!(matches!(segs[1], StrSeg::Expr(_)));
                assert!(matches!(segs[3], StrSeg::Expr(_)));
            }
            other => panic!("expected interpolation, got {other:?}"),
        }
    }

    #[test]
    fn function_declaration_and_lambda() {
        let p = prog("fn add(a, b) { return a + b }");
        match &p[0] {
            Stmt::Function(f) => {
                assert_eq!(f.name.as_deref(), Some("add"));
                assert_eq!(f.params.len(), 2);
            }
            other => panic!("expected a function declaration, got {other:?}"),
        }
        assert!(matches!(
            expr("fn(x) { x * x }").kind,
            ExprKind::Function(_)
        ));
    }

    // ---- resolver ----

    #[test]
    fn resolver_accepts_well_formed_program() {
        let p = prog(
            "fn fib(n) { if n < 2 { return n }\n return fib(n - 1) + fib(n - 2) }\nprint(fib(10))",
        );
        assert!(resolve(&p).is_ok());
    }

    #[test]
    fn resolver_flags_undefined_variable() {
        let p = prog("print(undefined_var)");
        let errs = resolve(&p).unwrap_err();
        assert!(matches!(errs[0], ResolveError::Undefined { .. }));
    }

    #[test]
    fn resolver_flags_assignment_to_immutable() {
        let p = prog("let x = 1\nx = 2");
        let errs = resolve(&p).unwrap_err();
        assert!(matches!(errs[0], ResolveError::Immutable { .. }));
        // but `let mut` is fine
        assert!(resolve(&prog("let mut x = 1\nx = 2")).is_ok());
    }

    #[test]
    fn resolver_checks_arity_and_break_context() {
        let p = prog("fn f(a, b) { return a }\nf(1)");
        let errs = resolve(&p).unwrap_err();
        assert!(matches!(
            errs[0],
            ResolveError::ArityMismatch {
                expected: 2,
                got: 1,
                ..
            }
        ));

        let errs = resolve(&prog("break")).unwrap_err();
        assert!(matches!(errs[0], ResolveError::BreakOutsideLoop { .. }));
    }
}
