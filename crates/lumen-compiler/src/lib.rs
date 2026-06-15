//! lumen-compiler — stage 3 of the Lumen pipeline.
//!
//! Lowers a parsed, resolved [`Program`](lumen_parser::ast::Program) into a
//! [`Chunk`] of stack-machine [`Instr`]uctions that `lumen-vm` executes.
//!
//! ```
//! use lumen_lexer::tokenize;
//! use lumen_parser::{parse, resolve};
//! use lumen_compiler::compile;
//!
//! let program = parse(tokenize("let x = 1 + 2").unwrap()).unwrap();
//! resolve(&program).unwrap();
//! let chunk = compile(&program).unwrap();
//! assert!(!chunk.code.is_empty());
//! ```

mod chunk;
mod compiler;
mod disasm;
pub mod opcode;

pub use chunk::{Chunk, Constant};
pub use compiler::{compile, CompileError};
pub use disasm::{disassemble, disassemble_instr};
pub use opcode::Instr;

#[cfg(test)]
mod tests {
    use super::*;
    use lumen_lexer::tokenize;
    use lumen_parser::{parse, resolve};

    /// Lex, parse, resolve and compile a snippet, panicking on any earlier error.
    fn chunk_of(src: &str) -> Chunk {
        let program = parse(tokenize(src).unwrap()).unwrap();
        resolve(&program).unwrap();
        compile(&program).unwrap()
    }

    /// The opcode stream of a compiled snippet.
    fn code(src: &str) -> Vec<Instr> {
        chunk_of(src).code
    }

    #[test]
    fn arithmetic_lowers_to_postfix_ops() {
        // 1 + 2 * 3  ==  1, 2, 3, Mul, Add  (multiplication first)
        assert_eq!(
            code("1 + 2 * 3"),
            vec![
                Instr::Constant(0), // 1
                Instr::Constant(1), // 2
                Instr::Constant(2), // 3
                Instr::Mul,
                Instr::Add,
                Instr::Pop, // expression statement discards its value
            ]
        );
    }

    #[test]
    fn constants_are_deduplicated() {
        // Both `1`s share one constant-pool slot.
        let chunk = chunk_of("1 + 1");
        assert_eq!(chunk.constants, vec![Constant::Int(1)]);
        assert_eq!(chunk.code[0], Instr::Constant(0));
        assert_eq!(chunk.code[1], Instr::Constant(0));
    }

    #[test]
    fn top_level_let_defines_a_global() {
        let chunk = chunk_of("let x = 42\nx");
        assert!(chunk
            .code
            .iter()
            .any(|i| matches!(i, Instr::DefineGlobal(_))));
        assert!(chunk.code.iter().any(|i| matches!(i, Instr::GetGlobal(_))));
    }

    #[test]
    fn block_locals_use_slots_and_are_popped() {
        // A block (here, an `if` branch) introduces a scope; `a` is a local read
        // via GetLocal, and the block keeps its trailing value while dropping the
        // local. (A bare `{ ... }` would parse as a dict, so we use a branch body.)
        let c = code("if true { let a = 1\n a + 1 } else { 0 }");
        assert!(c.contains(&Instr::GetLocal(0)));
        assert!(c.contains(&Instr::PopKeepTop(1)));
    }

    #[test]
    fn if_expression_jumps_over_branches() {
        let c = code("let x = if true { 1 } else { 2 }");
        // Exactly one conditional jump and one unconditional jump for the two arms.
        assert_eq!(
            c.iter()
                .filter(|i| matches!(i, Instr::JumpIfFalse(_)))
                .count(),
            1
        );
        assert_eq!(c.iter().filter(|i| matches!(i, Instr::Jump(_))).count(), 1);
    }

    #[test]
    fn while_loop_jumps_back() {
        let c = code("let mut i = 0\nwhile i < 3 { i = i + 1 }");
        // A back-edge Jump must target an earlier index than its own position.
        let back = c
            .iter()
            .enumerate()
            .any(|(pos, instr)| matches!(instr, Instr::Jump(t) if *t < pos));
        assert!(back, "while should emit a backward jump");
        assert!(c.iter().any(|i| matches!(i, Instr::JumpIfFalse(_))));
    }

    #[test]
    fn for_loop_uses_iterator_protocol() {
        let c = code("for i in 1..=3 { i }");
        assert!(c.contains(&Instr::GetIter));
        assert!(c.iter().any(|i| matches!(i, Instr::ForIter { .. })));
        assert!(c.contains(&Instr::MakeRange(true)));
    }

    #[test]
    fn break_unwinds_and_jumps_forward() {
        let c = code("loop { let a = 1\n break }");
        // `break` pops the loop-body local `a` before jumping out.
        let brk = c
            .iter()
            .position(|i| matches!(i, Instr::Jump(t) if *t == c.len()));
        // The forward break jump targets the end of the chunk (after the loop).
        assert!(c.contains(&Instr::Pop));
        assert!(brk.is_some(), "break should emit a forward jump to the end");
    }

    #[test]
    fn logical_operators_short_circuit() {
        assert!(
            code("true && false").contains(&Instr::JumpIfFalsePeek(usize_target("true && false")))
        );
        assert!(code("true || false")
            .iter()
            .any(|i| matches!(i, Instr::JumpIfTruePeek(_))));
    }

    // Helper to discover the patched target for the && case above.
    fn usize_target(src: &str) -> usize {
        code(src)
            .iter()
            .find_map(|i| match i {
                Instr::JumpIfFalsePeek(t) => Some(*t),
                _ => None,
            })
            .unwrap()
    }

    #[test]
    fn string_interpolation_builds_a_string() {
        let chunk = chunk_of(
            r#"let name = "x"
"hi {name}!""#,
        );
        assert!(chunk.code.iter().any(|i| matches!(i, Instr::BuildStr(_))));
    }

    #[test]
    fn functions_are_rejected_until_stage_4() {
        let program = parse(tokenize("fn f() { 1 }").unwrap()).unwrap();
        // (resolver accepts it; the compiler is what defers functions)
        let err = compile(&program).unwrap_err();
        assert!(matches!(err, CompileError::FunctionsNotYet { .. }));
    }

    #[test]
    fn disassembly_is_readable() {
        let chunk = chunk_of("1 + 2");
        let text = disassemble(&chunk, "test");
        assert!(text.contains("== test =="));
        assert!(text.contains("Constant"));
        assert!(text.contains("Add"));
    }
}
