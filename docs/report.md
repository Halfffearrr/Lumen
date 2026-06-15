# Lumen — design report

## 1. Overview

Lumen is a small dynamically-typed scripting language implemented from scratch in
Rust as a **bytecode virtual machine**. It deliberately echoes Rust's own ideas
— immutability by default, expression-oriented control flow, ranges — so the
language has a coherent design story rather than being a grab-bag of features.

The execution pipeline, and the crate that owns each step:

```
source .lm
  → [lumen-lexer]    Lexer      → tokens (each with a Span)
  → [lumen-parser]   Parser      → AST          (recursive descent + Pratt)
  → [lumen-parser]   Resolver    → checked AST   (static checks)
  → [lumen-compiler] Compiler    → bytecode Chunk
  → [lumen-vm]       VM          → result        (stack machine + call frames)
```

Shared types (`Span`, the `Diagnostic` trait, the builtin table) live in
`lumen-common`; the `lumen` binary in `lumen-cli` wires it together with a REPL,
a runner, a disassembler, and a GC demo.

## 2. The pipeline, module by module

### Lexer (`lumen-lexer`)
Hand-written scanner: source string → `Vec<Token>`. Every token carries a `Span`
(byte range + line/column) from the very first stage — this is the foundation of
the source-caret error messages. It handles numbers (distinguishing `1.5` from
the `..` range operator), keywords vs identifiers, string escapes, and string
**interpolation**, where `"a {b} c"` is scanned into literal and expression
parts.

### Parser + Resolver (`lumen-parser`)
The parser is **recursive descent** for statements and **Pratt parsing**
(precedence climbing) for expressions, so `1 + 2 * 3` correctly groups the
multiplication. Two design choices are visible in the AST:

- **Expression-oriented.** `if` and blocks are expressions; a block's value is
  its trailing expression. `let x = if c { a } else { b }` is ordinary.
- **Immutability by default.** Each `let` records `is_mutable`.

The **resolver** is a separate static-analysis pass (buff6) embodying Rust's
"catch errors at compile time" spirit. It reports undefined variables,
assignment to immutable bindings, `break` outside a loop, and call-arity
mismatches — all *before* anything runs. It also hoists function declarations so
mutually-recursive functions (like the calculator's `expr`/`term`/`factor`)
resolve regardless of order.

### Compiler (`lumen-compiler`)
A single walk of the AST emitting stack-machine instructions, on two invariants:

1. **Every expression leaves exactly one value on the stack**, so a statement in
   expression position ends with a `Pop`. This is how "everything is an
   expression" is realised.
2. **Locals live directly on the stack.** A local's *slot* is the compile-time
   stack height when it is bound. Because a block can be an expression nested in
   a larger one (e.g. `print(if c { let a = 1; a })`), temporaries can sit below
   the block's locals, so the compiler simulates the stack height (`stack_effect`
   per instruction, with manual fix-ups where control flow merges).

Control flow uses forward jumps with placeholder targets that are **patched**
once the destination is known, and backward jumps whose target is already known.
Functions compile to their own `Chunk` stored as a constant; capturing an
enclosing variable threads an **upvalue** descriptor down through each
intervening function (`resolve_upvalue`).

### VM (`lumen-vm`)
A fetch-decode-execute loop over the current call frame's chunk. Each `CallFrame`
records its `closure`, `ip`, and stack `base`; a local at slot `s` is
`stack[base + s]`. `Call` pushes a frame (reusing the callee and arguments
already on the stack as its slots); `Return` lifts the result, discards the
frame's region, and resumes the caller. Runtime values are a single `Value` enum;
heap values (`Str`, `List`, `Dict`, `Closure`) share via `Rc`/`RefCell`.

## 3. Closures and upvalues (the hard part)

A closure is a function value plus the cells it captured. A captured variable is
represented by an `Upvalue` that is **open** (holding a stack index) while the
variable is still live, and **closed** (holding the value) once that stack slot
disappears. Several closures capturing the same variable share one cell, so they
see each other's writes.

Closing is driven by the VM, not the compiler: whenever a slot is removed (by
`Pop`, `PopKeepTop`, or `Return`) the VM closes any open upvalue pointing into the
removed region. This is what makes the classic "closures in a loop" case correct
— each iteration's closure captures that iteration's value, because the slot is
closed when the loop body pops it.

## 4. Memory management and concurrency

The live interpreter manages heap values with **reference counting** (`Rc`),
which is simple and deterministic. Its one blind spot is **reference cycles**,
which it cannot reclaim. `lumen-vm/src/gc.rs` implements a **tracing mark-sweep**
collector that does reclaim cycles, and — to satisfy the project's concurrency
requirement — runs its collection on a **background thread**:

- The heap is an arena behind `Arc<Mutex<Heap>>`.
- A `Collector` owns a worker thread that waits on an `mpsc` channel; the mutator
  sends `Collect`, the worker locks the heap, runs mark (a work-list traversal
  from the roots) then sweep (free everything unmarked), and replies with stats.

`lumen --gc-demo` builds a rooted chain plus an unrooted cycle and shows the
background collector reclaiming exactly the cycle. This separation — `Rc` on the
hot path, a concurrent tracing collector for the cycle case — is an honest,
testable demonstration of both the algorithm and thread-based concurrency.

## 5. How Rust's ideas show up

- **Ownership / borrowing.** The compiler suspends an enclosing function's state
  on a stack and restores it (`std::mem::take`) rather than aliasing it. The VM
  clones the current frame's `Rc<Closure>` once per instruction so the chunk can
  be read while `&mut self` mutates the stack — sidestepping a borrow conflict.
- **Lifetimes.** `Span` stores byte offsets into the source so error rendering
  can slice the original text without copying it around.
- **Enums + exhaustive `match`.** `Value`, `Instr`, and every error type are
  enums; the compiler's `stack_effect` and the VM's dispatch are total matches,
  so adding an instruction forces every site to be updated.
- **`Result` everywhere.** Each stage returns `Result<_, E>` with a typed error;
  `lumen-cli` unifies them via the `Diagnostic` trait to draw a source caret.
- **Interior mutability.** `Rc<RefCell<…>>` gives lists/dicts the aliasing a real
  heap object has; `Rc<RefCell<Upvalue>>` is exactly the shared-cell semantics a
  closure needs.

## 6. Performance

`examples/bench.lm` times naive recursive `fib(30)` with the `clock()` builtin.
In a release build it runs in roughly **0.4 s** — the same order of magnitude as
CPython on the same machine, and dramatically faster than a tree-walking
interpreter would be, because compiling to bytecode removes per-node match
dispatch and pointer chasing from the hot loop. (Numbers are machine-dependent;
re-run `cargo run --release -p lumen-cli -- examples/bench.lm`.)

## 7. Testing

Around 70 tests across the crates: unit tests per stage (lexer token kinds,
parser precedence/AST shape, compiler bytecode shape, GC mark/sweep/cycles), VM
end-to-end behaviour, and `lumen-vm/tests/integration.rs`, which runs every
bundled example through the whole pipeline and checks its output. `cargo fmt
--check` and `cargo clippy --all-targets` are clean.

## 8. If I had another week

- A real moving/generational GC wired into the live heap instead of `Rc`.
- `try`/`catch` for recoverable runtime errors.
- Constant folding and peephole optimisation in the compiler.
- More of the standard library (string methods, sorting, higher-order list ops).
