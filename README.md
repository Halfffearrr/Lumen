# Lumen

A small, dynamically-typed scripting language with a bytecode virtual machine,
written from scratch in Rust. Lumen borrows core ideas from Rust itself:
immutable-by-default bindings, expression-oriented blocks, range syntax, and a
clean modern brace style.

```text
source .lm  ->  Lexer  ->  Tokens  ->  Parser  ->  AST  ->  Resolver
            ->  Compiler  ->  Bytecode  ->  VM  ->  result
```

## Status

Built stage by stage. **Stage 1 (current): workspace skeleton + lexer.**

| Stage | Scope                                   | State |
|-------|-----------------------------------------|-------|
| 1     | Workspace + lexer (source -> tokens)    | done  |
| 2     | Parser + AST + resolver                 | todo  |
| 3     | Compiler + bytecode + VM                | todo  |
| 4     | Functions / closures / GC               | todo  |
| 5     | Stdlib, REPL, pretty errors, benches    | todo  |

## Crates

| Crate          | Responsibility                              |
|----------------|---------------------------------------------|
| `lumen-common` | Shared source locations (`Span`, `Pos`)     |
| `lumen-lexer`  | Scans source text into a token stream       |

## Build & test

```sh
cargo build
cargo test
```
