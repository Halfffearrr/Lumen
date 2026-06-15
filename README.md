# Lumen

A small, dynamically-typed scripting language with a **bytecode virtual machine**,
written from scratch in Rust (no parser generator, no VM crate). Lumen borrows
core ideas from Rust itself: **immutable-by-default** bindings, **expression-oriented**
blocks and `if`, **range** syntax, and a clean modern brace style.

```text
source .lm → Lexer → Tokens → Parser → AST → Resolver
           → Compiler → Bytecode → VM → result
```

## Quick start

```sh
cargo build                         # build everything
cargo test                          # run all unit + integration tests
cargo run -p lumen-cli -- examples/fizzbuzz.lm   # run a script
cargo run -p lumen-cli              # start the REPL (Ctrl-D to exit)
```

Or build the `lumen` binary and use it directly:

```sh
cargo build --release
./target/release/lumen examples/fib.lm
./target/release/lumen --disassemble examples/fizzbuzz.lm   # dump bytecode (buff8)
./target/release/lumen --gc-demo                            # concurrent GC demo (buff4)
```

## The language by example

```rust
// Immutable by default; opt into mutation with `let mut`.
let pi = 3.14
let mut count = 0

// `if` is an expression — it yields the taken branch's value.
let level = if count > 10 { "high" } else { "low" }

// Ranges and for-loops; `..` excludes the end, `..=` includes it.
for i in 1..=5 {
    count = count + i
}

// String interpolation with `{ ... }`.
print("count is {count}, level {level}")

// Functions, recursion, and closures that capture outer variables.
fn make_counter() {
    let mut n = 0
    return fn() { n = n + 1; return n }
}
let next = make_counter()
print(next())   // 1
print(next())   // 2
```

See [`examples/`](examples/): `fizzbuzz.lm`, `tour.lm`, `fib.lm`, `closures.lm`,
`calc.lm` (a calculator written *in Lumen*), and `bench.lm`.

## Features

- Values: `int` (i64), `float` (f64), `bool`, `nil`, `str`, `list`, `dict`, `range`, functions.
- Immutable-by-default bindings (`let` / `let mut`), checked statically by the resolver.
- Expression-oriented: `if`/`else` and blocks produce values.
- Control flow: `if`, `while`, `for … in range/list`, `loop` + `break`.
- First-class functions: recursion, anonymous `fn(x){…}`, and closures with upvalue capture.
- String interpolation: `"hi {name}, {1 + 1}"`.
- Standard library: `print len type str int float sqrt abs floor min max push pop keys values error clock`.

## Crates

| Crate            | Responsibility                                            |
|------------------|-----------------------------------------------------------|
| `lumen-common`   | Shared source locations (`Span`), `Diagnostic`, builtins  |
| `lumen-lexer`    | Source text → token stream (stage 1)                      |
| `lumen-parser`   | Tokens → AST + resolver static checks (stage 2)           |
| `lumen-compiler` | AST → bytecode `Chunk` + disassembler (stage 3)           |
| `lumen-vm`       | Stack VM, runtime values, mark-sweep GC (stages 3–4)      |
| `lumen-cli`      | `lumen` binary: run / REPL / disassemble / gc-demo        |

## "Buffs" (bonus features)

| # | Feature                    | Where                                         |
|---|----------------------------|-----------------------------------------------|
| 1 | Source-caret error messages | `lumen-cli` (`render`), uses every node's `Span` |
| 2 | REPL                       | `lumen` with no arguments                     |
| 4 | Concurrent mark-sweep GC   | `lumen-vm/src/gc.rs`, `lumen --gc-demo`       |
| 5 | Standard library           | `lumen-vm/src/vm.rs` builtins                 |
| 6 | Static checks (resolver)   | `lumen-parser/src/resolver.rs`                |
| 7 | Self-hosting step          | `examples/calc.lm` (a calculator in Lumen)    |
| 8 | Disassembler               | `lumen --disassemble`                         |

## Docs

- [`docs/grammar.md`](docs/grammar.md) — the grammar (EBNF).
- [`docs/bytecode.md`](docs/bytecode.md) — the bytecode instruction set.
- [`docs/report.md`](docs/report.md) — design write-up.

## Build & test

```sh
cargo build
cargo test
cargo fmt --check
cargo clippy --all-targets
```
