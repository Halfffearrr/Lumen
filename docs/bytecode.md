# Lumen bytecode

Lumen compiles to a **stack machine**. A compiled function is a `Chunk`:

- `code: Vec<Instr>` — the instruction stream (a typed enum, not packed bytes,
  so the VM and disassembler stay type-safe).
- `constants: Vec<Constant>` — interned literals (`Int`, `Float`, `Str`) and
  nested function prototypes (`Function`), referenced by index.
- `lines: Vec<u32>` — `lines[i]` is the source line of `code[i]`, for runtime
  error reporting.

Operands that index a side table are `u16`; jump operands hold an **absolute**
target index within the owning chunk's `code`. Inspect any program's bytecode
with `lumen --disassemble script.lm`.

## Instruction set

| Instruction        | Stack effect (before → after)        | Meaning |
|--------------------|--------------------------------------|---------|
| `Constant(i)`      | → v                                  | Push `constants[i]`. |
| `Nil` / `True` / `False` | → v                            | Push the literal. |
| `Pop`              | a →                                  | Discard the top. |
| `PopKeepTop(n)`    | x₁…xₙ t → t                          | Drop `n` values *below* the top, keep the top (drops a block's locals while preserving its value). |
| `Neg` / `Not`      | a → r                                | Numeric negate / logical not. |
| `Add Sub Mul Div Mod` | a b → r                           | Arithmetic; `Add` also concatenates strings. |
| `Eq Ne Lt Le Gt Ge`| a b → r                             | Comparisons → bool. |
| `GetLocal(s)`      | → v                                  | Push `stack[base + s]`. |
| `SetLocal(s)`      | v → v                                | `stack[base + s] = peek`; leaves the value (assignment is an expression). |
| `GetGlobal(i)`     | → v                                  | Push global named `constants[i]`. |
| `SetGlobal(i)`     | v → v                                | Assign an existing global; leaves the value. |
| `DefineGlobal(i)`  | v →                                  | Bind a new global from the top. |
| `GetUpvalue(i)`    | → v                                  | Read captured variable `i` of the current closure. |
| `SetUpvalue(i)`    | v → v                                | Write captured variable `i`; leaves the value. |
| `Jump(t)`          | —                                    | `ip = t`. |
| `JumpIfFalse(t)`   | c →                                  | Pop; if falsey, `ip = t` (used by `if`/`while`). |
| `JumpIfFalsePeek(t)` | c → c                              | If top falsey, `ip = t` without popping (used by `&&`). |
| `JumpIfTruePeek(t)`| c → c                                | If top truthy, `ip = t` without popping (used by `\|\|`). |
| `BuildList(n)`     | x₁…xₙ → list                         | Pop `n`, build a list. |
| `BuildDict(n)`     | k₁ v₁ … kₙ vₙ → dict                 | Pop `2n`, build a dict. |
| `Index`            | obj idx → v                          | `obj[idx]`. |
| `SetIndex`         | obj idx v → v                        | `obj[idx] = v`; leaves the value. |
| `MakeRange(incl)`  | start end → range                    | Build `start..end` / `start..=end`. |
| `GetIter`          | iterable → iterator                  | Begin iterating a range or list. |
| `ForIter{slot,exit}` | → [elem]                           | Read the iterator local at `slot`; push the next element, or `ip = exit` when exhausted. |
| `BuildStr(n)`      | x₁…xₙ → str                          | Concatenate `n` values to a string (interpolation). |
| `Call(argc)`       | f a₁…a_argc → r                      | Call `f` with `argc` args. |
| `Closure(i)`       | → closure                            | Instantiate `constants[i]` (a function), capturing its upvalues. |
| `Return`           | v → (frame popped)                   | Return `v` from the current function. |

Truthiness: only `nil` and `false` are falsey. Two ints stay an int under
arithmetic (integer `/` and `%`); any float promotes to float.

## How source constructs lower

- **`if` expression** — `cond` then `JumpIfFalse` to the else arm; each arm
  leaves one value; an unconditional `Jump` skips the else arm.
- **`while`** — `cond`, `JumpIfFalse` to exit, body, `Jump` back to the condition.
- **`for x in it`** — `GetIter`, then `ForIter{slot,exit}` each turn pushes the
  next element (the loop variable) or jumps past the loop.
- **`break`** — pops the locals declared since the loop began, then `Jump`s to the
  loop's exit (targets patched once known).
- **`&&` / `||`** — short-circuit via the peeking conditional jumps.
- **Functions** — compile to a `Function` constant; `Closure` builds a runtime
  closure; `Call` pushes a new call frame whose `base` points at the callee, so
  a local at compile-time slot `s` lives at `stack[base + s]`.
- **Closures** — captured variables become shared cells (upvalues). While the
  captured local is on the stack the upvalue is *open*; when the slot is popped
  (scope exit or `Return`) the VM *closes* it, copying the value into the cell.
