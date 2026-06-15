//! A `Chunk`: one unit of compiled Lumen bytecode plus its side tables.
//!
//! A chunk bundles three parallel pieces of information produced by the compiler
//! and consumed by the VM and disassembler:
//! * `code` — the linear instruction stream ([`Instr`]).
//! * `constants` — a pool of compile-time literals that instructions reference by
//!   index (so a 64-bit integer or a string lives once, not inline in the code).
//! * `lines` — for each instruction, the 1-based source line it came from, so a
//!   runtime error can point back at the offending line.
//!
//! `code[i]` and `lines[i]` always grow together through [`Chunk::push`], keeping
//! the two vectors the same length.

use std::rc::Rc;

use crate::opcode::Instr;

/// How one upvalue of a function is captured when its closure is created.
///
/// A nested function that refers to a variable of an enclosing function captures
/// it as an *upvalue*. Each captured variable gets one of these descriptors,
/// recorded on the [`FnProto`] and read by the VM's `Closure` instruction:
/// * `from_enclosing_local = true` — capture the **enclosing function's local**
///   at stack slot `index`.
/// * `from_enclosing_local = false` — capture the **enclosing function's
///   upvalue** number `index` (the variable lives even further out).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UpvalueDesc {
    pub index: u16,
    pub from_enclosing_local: bool,
}

/// A compiled function: its own bytecode plus the metadata the VM needs to call
/// it and to build a closure from it. Named functions and anonymous lambdas both
/// compile to one of these; it is stored in the enclosing chunk's constant pool
/// and instantiated into a runtime closure by [`Instr::Closure`].
#[derive(Debug, Clone, PartialEq)]
pub struct FnProto {
    /// For diagnostics / disassembly (`""` for anonymous lambdas).
    pub name: String,
    /// Number of parameters; the VM checks this against the argument count.
    pub arity: usize,
    pub chunk: Chunk,
    /// One descriptor per captured variable, in upvalue-index order.
    pub upvalues: Vec<UpvalueDesc>,
}

/// A value known at compile time and stored in a chunk's constant pool.
///
/// Only things the compiler can materialize ahead of time live here: numeric and
/// string literals, and compiled function prototypes. `bool` and `nil` are *not*
/// constants — they have dedicated zero-operand instructions ([`Instr::True`],
/// [`Instr::False`], [`Instr::Nil`]) — which keeps the pool small and the
/// disassembly readable.
#[derive(Debug, Clone, PartialEq)]
pub enum Constant {
    Int(i64),
    Float(f64),
    Str(String),
    Function(Rc<FnProto>),
}

/// A compiled chunk of bytecode together with its constant pool and line table.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Chunk {
    /// The instruction stream, executed in order by the VM unless a jump moves
    /// the instruction pointer.
    pub code: Vec<Instr>,
    /// The constant pool, indexed by [`Instr::Constant`].
    pub constants: Vec<Constant>,
    /// `lines[i]` is the source line that produced `code[i]`.
    pub lines: Vec<u32>,
}

impl Chunk {
    /// A fresh, empty chunk.
    pub fn new() -> Self {
        Chunk::default()
    }

    /// Append one instruction tagged with the source `line` it came from, and
    /// return its index in `code` (useful for later jump patching).
    pub fn push(&mut self, instr: Instr, line: u32) -> usize {
        self.code.push(instr);
        self.lines.push(line);
        self.code.len() - 1
    }

    /// Intern a constant and return its pool index.
    ///
    /// Equal constants are de-duplicated so, for example, the name `"count"`
    /// referenced by several global accesses occupies a single pool slot.
    pub fn add_constant(&mut self, value: Constant) -> u16 {
        if let Some(i) = self.constants.iter().position(|c| *c == value) {
            return i as u16;
        }
        self.constants.push(value);
        (self.constants.len() - 1) as u16
    }

    /// Append a compiled function to the pool and return its index. Functions are
    /// not de-duplicated (each `fn`/lambda gets its own slot) since comparing
    /// whole chunks for equality would be wasteful and offers nothing.
    pub fn add_function(&mut self, proto: Rc<FnProto>) -> u16 {
        self.constants.push(Constant::Function(proto));
        (self.constants.len() - 1) as u16
    }

    /// The number of instructions emitted so far. Equals the index the *next*
    /// pushed instruction will occupy, which is exactly the absolute jump target
    /// used by control-flow instructions.
    pub fn len(&self) -> usize {
        self.code.len()
    }

    /// True when no instructions have been emitted.
    pub fn is_empty(&self) -> bool {
        self.code.is_empty()
    }
}
