//! lumen-vm — stage 3 of the Lumen pipeline: run compiled bytecode.
//!
//! This crate hosts the runtime [`Value`] type and the stack [`Vm`], and offers
//! [`interpret`], the end-to-end convenience that drives the whole pipeline
//! (lex → parse → resolve → compile → run) and is what the CLI will call.
//!
//! ```
//! let vm = lumen_vm::interpret("print(1 + 2)").unwrap();
//! assert_eq!(vm.output, "3\n");
//! ```

mod value;
mod vm;

pub use value::{Native, RangeVal, Value};
pub use vm::{RuntimeError, Vm};

use lumen_compiler::{compile, CompileError};
use lumen_lexer::{tokenize, LexError};
use lumen_parser::{parse, resolve, ParseError, ResolveError};

/// Any error that can stop a Lumen program before it produces output, tagged by
/// the pipeline stage that raised it.
#[derive(Debug)]
pub enum LumenError {
    Lex(LexError),
    Parse(ParseError),
    Resolve(Vec<ResolveError>),
    Compile(CompileError),
    Runtime(RuntimeError),
}

impl std::fmt::Display for LumenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LumenError::Lex(e) => write!(f, "lex error: {e}"),
            LumenError::Parse(e) => write!(f, "parse error: {e}"),
            LumenError::Resolve(errs) => {
                writeln!(f, "{} resolve error(s):", errs.len())?;
                for e in errs {
                    writeln!(f, "  {e}")?;
                }
                Ok(())
            }
            LumenError::Compile(e) => write!(f, "compile error: {e}"),
            LumenError::Runtime(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for LumenError {}

/// Compile and run a Lumen source string, returning the VM so callers can read
/// its captured [`Vm::output`].
pub fn interpret(source: &str) -> Result<Vm, LumenError> {
    let tokens = tokenize(source).map_err(LumenError::Lex)?;
    let program = parse(tokens).map_err(LumenError::Parse)?;
    resolve(&program).map_err(LumenError::Resolve)?;
    let chunk = compile(&program).map_err(LumenError::Compile)?;
    let mut vm = Vm::new();
    vm.run(&chunk).map_err(LumenError::Runtime)?;
    Ok(vm)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run a snippet and return what it printed.
    fn out(src: &str) -> String {
        interpret(src).expect("program should run").output
    }

    #[test]
    fn arithmetic_and_precedence() {
        assert_eq!(out("print(1 + 2 * 3)"), "7\n");
        assert_eq!(out("print((1 + 2) * 3)"), "9\n");
        assert_eq!(out("print(7 / 2)"), "3\n"); // integer division
        assert_eq!(out("print(7 % 3)"), "1\n");
        assert_eq!(out("print(7.0 / 2)"), "3.5\n"); // float promotes
    }

    #[test]
    fn globals_and_mutation() {
        assert_eq!(out("let mut x = 1\nx = x + 4\nprint(x)"), "5\n");
    }

    #[test]
    fn if_is_an_expression() {
        assert_eq!(
            out("let n = 20\nprint(if n > 10 { \"high\" } else { \"low\" })"),
            "high\n"
        );
        // if with no else yields nil on the false path
        assert_eq!(out("print(if false { 1 })"), "nil\n");
    }

    #[test]
    fn block_value_is_its_tail() {
        // A block's value is its trailing expression. Blocks appear as branch
        // bodies (a bare `{ ... }` is a dict), so exercise it through an `if`.
        assert_eq!(
            out("print(if true { let a = 2\n let b = 3\n a * b } else { 0 })"),
            "6\n"
        );
    }

    #[test]
    fn while_loop_accumulates() {
        assert_eq!(
            out("let mut i = 0\nlet mut s = 0\nwhile i < 5 { s = s + i\n i = i + 1 }\nprint(s)"),
            "10\n"
        );
    }

    #[test]
    fn for_over_inclusive_range() {
        assert_eq!(
            out("let mut s = 0\nfor i in 1..=5 { s = s + i }\nprint(s)"),
            "15\n"
        );
    }

    #[test]
    fn for_over_list() {
        assert_eq!(
            out("let mut s = 0\nfor x in [10, 20, 30] { s = s + x }\nprint(s)"),
            "60\n"
        );
    }

    #[test]
    fn loop_with_break() {
        assert_eq!(
            out("let mut i = 0\nloop { if i == 3 { break }\n i = i + 1 }\nprint(i)"),
            "3\n"
        );
    }

    #[test]
    fn string_interpolation() {
        assert_eq!(
            out(r#"let n = "Lumen"
print("hello, {n}! {1 + 1}")"#),
            "hello, Lumen! 2\n"
        );
    }

    #[test]
    fn logical_short_circuit() {
        assert_eq!(out("print(true && false)"), "false\n");
        assert_eq!(out("print(false || 42)"), "42\n");
        assert_eq!(out("print(1 < 2 && 2 < 3)"), "true\n");
    }

    #[test]
    fn lists_dicts_and_indexing() {
        assert_eq!(out("let xs = [1, 2, 3]\nprint(xs[1])"), "2\n");
        assert_eq!(
            out("let mut xs = [1, 2, 3]\nxs[0] = 9\nprint(xs)"),
            "[9, 2, 3]\n"
        );
        assert_eq!(
            out(r#"let d = {"a": 1, "b": 2}
print(d["b"])"#),
            "2\n"
        );
    }

    #[test]
    fn builtins() {
        assert_eq!(out("print(len([1, 2, 3]))"), "3\n");
        assert_eq!(out("print(type(3.5))"), "float\n");
        assert_eq!(out("print(sqrt(16.0))"), "4.0\n");
        assert_eq!(out("let mut xs = [1]\npush(xs, 2)\nprint(xs)"), "[1, 2]\n");
    }

    #[test]
    fn fizzbuzz_runs() {
        let src = r#"for i in 1..=15 {
    if i % 15 == 0 { print("FizzBuzz") }
    else if i % 3 == 0 { print("Fizz") }
    else if i % 5 == 0 { print("Buzz") }
    else { print(i) }
}"#;
        let expected = "1\n2\nFizz\n4\nBuzz\nFizz\n7\n8\nFizz\nBuzz\n11\nFizz\n13\n14\nFizzBuzz\n";
        assert_eq!(out(src), expected);
    }

    #[test]
    fn runtime_error_reports_a_line() {
        let err = interpret("let x = 1\nprint(x / 0)").unwrap_err();
        match err {
            LumenError::Runtime(e) => {
                assert!(e.message.contains("division by zero"));
                assert_eq!(e.line, Some(2));
            }
            other => panic!("expected a runtime error, got {other:?}"),
        }
    }

    #[test]
    fn error_builtin_propagates() {
        let err = interpret(r#"error("boom")"#).unwrap_err();
        assert!(matches!(err, LumenError::Runtime(e) if e.message == "boom"));
    }
}
