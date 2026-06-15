//! The `lumen` command-line entry point.
//!
//! ```text
//! lumen                             # start the interactive REPL (buff2)
//! lumen <script.lm>                 # compile and run a script
//! lumen --disassemble <script.lm>   # print the compiled bytecode (buff8)
//! lumen --gc-demo                   # demonstrate the concurrent mark-sweep GC (buff4)
//! ```
//!
//! Errors are rendered with a source caret pointing at the offending span
//! (buff1) — implemented here directly rather than via a crate, so it stays
//! dependency-free and works off the [`Span`](lumen_common::Span) every stage
//! already records.

use std::io::{self, BufRead, Write};
use std::process::ExitCode;

use lumen_common::{Diagnostic, Span};
use lumen_compiler::{compile, compile_repl, disassemble};
use lumen_lexer::tokenize;
use lumen_parser::{parse, resolve, resolve_with_globals};
use lumen_vm::gc::Collector;
use lumen_vm::{LumenError, Value, Vm};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.as_slice() {
        [] => repl(),
        [flag, path] if flag == "--disassemble" || flag == "-d" => disassemble_file(path),
        [flag] if flag == "--gc-demo" => gc_demo(),
        [path] if !path.starts_with('-') => run_file(path),
        _ => {
            eprintln!("usage: lumen [--disassemble | --gc-demo] [<script.lm>]");
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

/// Compile and run a script, printing its output (or a source-located error).
fn run_file(path: &str) -> Result<(), String> {
    let source = read(path)?;
    match lumen_vm::interpret(&source) {
        Ok(vm) => {
            print!("{}", vm.output);
            Ok(())
        }
        Err(e) => Err(report(&source, &e)),
    }
}

/// Compile a script and print a human-readable listing of its bytecode.
fn disassemble_file(path: &str) -> Result<(), String> {
    let source = read(path)?;
    let tokens = tokenize(&source).map_err(|e| render(&source, e.span(), &e.message()))?;
    let program = parse(tokens).map_err(|e| render(&source, e.span(), &e.message()))?;
    resolve(&program).map_err(|errs| render_all(&source, &errs))?;
    let chunk = compile(&program).map_err(|e| render(&source, e.span(), &e.message()))?;
    print!("{}", disassemble(&chunk, path));
    Ok(())
}

/// A read-eval-print loop. State (globals, defined functions) persists across
/// lines in one [`Vm`]; a bare expression line echoes its value.
fn repl() -> Result<(), String> {
    let mut vm = Vm::new();
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    println!("Lumen REPL — enter statements or expressions, Ctrl-D to exit.");
    loop {
        print!("lumen> ");
        stdout.flush().ok();
        let mut line = String::new();
        match stdin.lock().read_line(&mut line) {
            Ok(0) => {
                println!();
                return Ok(()); // EOF
            }
            Ok(_) => {}
            Err(e) => return Err(e.to_string()),
        }
        if line.trim().is_empty() {
            continue;
        }
        eval_repl_line(&mut vm, &line);
    }
}

/// Evaluate one REPL line against the persistent VM, printing output, the echoed
/// result value, or a rendered error — without aborting the session.
fn eval_repl_line(vm: &mut Vm, line: &str) {
    let tokens = match tokenize(line) {
        Ok(t) => t,
        Err(e) => return eprint!("{}", render(line, e.span(), &e.message())),
    };
    let program = match parse(tokens) {
        Ok(p) => p,
        Err(e) => return eprint!("{}", render(line, e.span(), &e.message())),
    };
    if let Err(errs) = resolve_with_globals(&program, &vm.global_names()) {
        return eprint!("{}", render_all(line, &errs));
    }
    let chunk = match compile_repl(&program) {
        Ok(c) => c,
        Err(e) => return eprint!("{}", render(line, e.span(), &e.message())),
    };

    let before = vm.output.len();
    match vm.run(chunk) {
        Ok(value) => {
            print!("{}", &vm.output[before..]);
            if !matches!(value, Value::Nil) {
                println!("{value}");
            }
        }
        Err(e) => {
            print!("{}", &vm.output[before..]);
            eprint!("{}", render_runtime(line, &e));
        }
    }
}

// --- error rendering (buff1) -------------------------------------------------

/// Render any pipeline error with source context.
fn report(source: &str, err: &LumenError) -> String {
    match err {
        LumenError::Lex(e) => render(source, e.span(), &e.message()),
        LumenError::Parse(e) => render(source, e.span(), &e.message()),
        LumenError::Compile(e) => render(source, e.span(), &e.message()),
        LumenError::Resolve(errs) => render_all(source, errs),
        LumenError::Runtime(e) => render_runtime(source, e),
    }
}

/// Render several diagnostics (e.g. all resolver errors) back to back.
fn render_all<D: Diagnostic>(source: &str, errs: &[D]) -> String {
    errs.iter()
        .map(|e| render(source, e.span(), &e.message()))
        .collect::<Vec<_>>()
        .join("")
}

/// Draw the offending source line with a caret under `span` and the message.
///
/// ```text
/// error: undefined variable 'x'
///    3 | print(x)
///      |       ^
/// ```
fn render(source: &str, span: Span, message: &str) -> String {
    let line_no = span.line as usize;
    let line_text = source.lines().nth(line_no.saturating_sub(1)).unwrap_or("");
    let gutter = format!("{line_no:>4} | ");
    let pad = " ".repeat(gutter.len() + span.col.saturating_sub(1) as usize);
    let width = source
        .get(span.start..span.end)
        .map(|s| s.chars().count())
        .unwrap_or(1)
        .max(1);
    format!(
        "error: {message}\n{gutter}{line_text}\n{pad}{} \n",
        "^".repeat(width)
    )
}

/// Render a runtime error, which carries a line number rather than a full span.
fn render_runtime(source: &str, err: &lumen_vm::RuntimeError) -> String {
    match err.line {
        Some(line) => {
            let line_text = source
                .lines()
                .nth(line.saturating_sub(1) as usize)
                .unwrap_or("");
            format!("runtime error: {}\n{line:>4} | {line_text}\n", err.message)
        }
        None => format!("runtime error: {}\n", err.message),
    }
}

fn read(path: &str) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|e| format!("cannot read '{path}': {e}"))
}

/// Build a small object graph — including a rooted chain and an unrooted cycle —
/// then let the background-thread collector reclaim the unreachable objects.
fn gc_demo() -> Result<(), String> {
    let gc = Collector::new();
    let heap = gc.heap();

    let (rooted, before) = {
        let mut h = heap.lock().unwrap();
        // A rooted chain a -> b that must survive.
        let a = h.alloc(1);
        let b = h.alloc(2);
        h.add_root(a);
        h.add_ref(a, b);
        // An unrooted cycle c <-> d that reference counting could never free.
        let c = h.alloc(3);
        let d = h.alloc(4);
        h.add_ref(c, d);
        h.add_ref(d, c);
        (a, h.live())
    };

    println!("allocated {before} objects (2 rooted + reachable, 2 in an unreachable cycle)");
    let stats = gc.collect_now(); // marking + sweeping happen on the GC thread
    println!(
        "after concurrent mark-sweep: {} live, {} freed",
        stats.live, stats.freed
    );
    println!(
        "rooted object still holds value {:?}",
        heap.lock().unwrap().value(rooted)
    );
    Ok(())
}
