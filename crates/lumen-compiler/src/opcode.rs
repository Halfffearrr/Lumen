//! The Lumen bytecode instruction set.
//!
//! Lumen is a **stack machine**: almost every instruction pops its operands off a
//! value stack and pushes its result back. We represent bytecode as a flat
//! `Vec<Instr>` of this typed enum rather than a packed `Vec<u8>`. That keeps the
//! VM and disassembler simple and type-safe while still being "bytecode" in every
//! way that matters: a linear instruction stream executed by a fetch-decode loop.
//!
//! Operands that index into a side table use `u16` (constants, name slots, local
//! and upvalue indices); jump operands hold the **absolute target index** within
//! the owning chunk's `code` vector.

/// A single bytecode instruction.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Instr {
    /// Push `constants[idx]` onto the stack.
    Constant(u16),
    /// Push the literal `nil` / `true` / `false`.
    Nil,
    True,
    False,
    /// Discard the top of the stack.
    Pop,
    /// Discard `n` values located *just below* the top, keeping the top. Used to
    /// drop a block's locals while preserving the block's value.
    PopKeepTop(u16),

    // --- unary / binary operators (pop operands, push result) ---
    Neg,
    Not,
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,

    // --- variables ---
    /// Read/write a local at `base + slot` on the value stack.
    GetLocal(u16),
    SetLocal(u16),
    /// Read/write a global by the name stored in `constants[idx]`.
    GetGlobal(u16),
    SetGlobal(u16),
    /// Bind a new global from the top of the stack (used by top-level `let`).
    DefineGlobal(u16),
    /// Read/write a captured variable through the current closure's upvalue.
    GetUpvalue(u16),
    SetUpvalue(u16),
    /// Move a captured local off the stack into its heap cell on scope exit.
    CloseUpvalue,

    // --- control flow (operand = absolute target index in `code`) ---
    Jump(usize),
    /// Pop the top; jump if it is falsey. (used by `if` / `while`)
    JumpIfFalse(usize),
    /// Peek the top (do not pop); jump if it is falsey. (used by `&&`)
    JumpIfFalsePeek(usize),
    /// Peek the top (do not pop); jump if it is truthy. (used by `||`)
    JumpIfTruePeek(usize),

    // --- collections ---
    /// Pop `n` values and build a list from them (in stack order).
    BuildList(u16),
    /// Pop `2n` values (key, value, key, value, ...) and build a dict.
    BuildDict(u16),
    /// `obj[index]`: pop index then obj, push the element.
    Index,
    /// `obj[index] = value`: pop value, index, obj; store; push value back.
    SetIndex,
    /// Pop end then start, push a range (`inclusive` decides `..` vs `..=`).
    MakeRange(bool),
    /// Pop an iterable (range or list) and push an iterator over it.
    GetIter,
    /// Drive a `for` loop. Reads the iterator local at `slot`; if it is
    /// exhausted, jump to `exit`; otherwise push the next element and fall
    /// through.
    ForIter {
        slot: u16,
        exit: usize,
    },

    /// Pop `n` values, convert each to a string, concatenate, push the result.
    /// Implements string interpolation `"a {b} c"`.
    BuildStr(u16),

    // --- functions ---
    /// Call the value `argc` slots below the top with `argc` arguments.
    Call(u8),
    /// Wrap `constants[idx]` (a function) into a closure, capturing upvalues from
    /// the current frame, and push it.
    Closure(u16),
    /// Return the top of the stack from the current function.
    Return,
}
