//! The `lumen` command-line entry point.
//!
//! Stage 3 keeps this deliberately small — just enough to run a script and to
//! inspect the bytecode it compiles to:
//!
//! ```text
//! lumen <script.lm>                 # compile and run, printing its output
//! lumen --disassemble <script.lm>   # print the compiled bytecode (buff8)
//! ```
//!
//! The richer front end (a `clap` argument parser, a `rustyline` REPL, and
//! `ariadne` source-highlighted errors) is stage 5; this is the thin shell that
//! makes the stage-3 pipeline runnable end to end.

use std::process::ExitCode;

use lumen_compiler::{compile, disassemble};
use lumen_lexer::tokenize;
use lumen_parser::{parse, resolve};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.as_slice() {
        [flag, path] if flag == "--disassemble" || flag == "-d" => disassemble_file(path),
        [path] if !path.starts_with('-') => run_file(path),
        _ => {
            eprintln!("usage: lumen [--disassemble] <script.lm>");
            return ExitCode::from(64); // EX_USAGE
        }
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(msg) => {
            eprintln!("{msg}");
            ExitCode::FAILURE
        }
    }
}

/// Compile and run a script, printing whatever it produced.
fn run_file(path: &str) -> Result<(), String> {
    let source = read(path)?;
    match lumen_vm::interpret(&source) {
        Ok(vm) => {
            print!("{}", vm.output);
            Ok(())
        }
        Err(e) => Err(e.to_string()),
    }
}

/// Compile a script and print a human-readable listing of its bytecode.
fn disassemble_file(path: &str) -> Result<(), String> {
    let source = read(path)?;
    let tokens = tokenize(&source).map_err(|e| format!("lex error: {e}"))?;
    let program = parse(tokens).map_err(|e| format!("parse error: {e}"))?;
    resolve(&program).map_err(|errs| {
        errs.iter()
            .map(|e| format!("resolve error: {e}"))
            .collect::<Vec<_>>()
            .join("\n")
    })?;
    let chunk = compile(&program).map_err(|e| format!("compile error: {e}"))?;
    print!("{}", disassemble(&chunk, path));
    Ok(())
}

fn read(path: &str) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|e| format!("cannot read '{path}': {e}"))
}
