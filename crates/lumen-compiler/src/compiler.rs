//! The Lumen compiler: lowers a resolved AST into a [`Chunk`] of bytecode.
//!
//! It walks the tree once, emitting stack-machine instructions. Two invariants
//! make the rest simple:
//!
//! * **Every expression leaves exactly one value on the stack.** A statement in
//!   expression position therefore ends with an [`Instr::Pop`]. This is how the
//!   language's "everything is an expression" design is realized: a block's value
//!   is its trailing expression, an `if`'s value is the taken branch.
//! * **Locals live directly on the value stack.** Inside a block, `let x = e`
//!   simply leaves `e`'s value on the stack and remembers its slot; reads compile
//!   to [`Instr::GetLocal`]. At the top level, bindings are globals stored by name
//!   ([`Instr::DefineGlobal`]/[`Instr::GetGlobal`]). Scope exit pops the locals.
//!
//! Control flow uses forward jumps with placeholder targets that are *patched*
//! once the destination instruction index is known (see [`Compiler::patch_jump`]),
//! and backward `Jump`s whose absolute target is already known.
//!
//! User-defined functions, closures and `return` belong to stage 4; encountering
//! them here is a clean [`CompileError`] rather than a panic.

use lumen_common::{Diagnostic, Span};
use lumen_parser::ast::*;

use crate::chunk::{Chunk, Constant};
use crate::opcode::Instr;

/// An error detected while compiling. The resolver has already rejected most
/// semantic mistakes, so these are about features not yet implemented or
/// hard limits of the bytecode format.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum CompileError {
    #[error("user-defined functions are not supported yet (they arrive in stage 4)")]
    FunctionsNotYet { span: Span },
    #[error("'return' is only valid inside a function (functions arrive in stage 4)")]
    ReturnOutsideFunction { span: Span },
    #[error("'break' outside of a loop")]
    BreakOutsideLoop { span: Span },
    #[error("too many constants in one chunk (limit {limit})")]
    TooManyConstants { span: Span, limit: usize },
    #[error("too many local variables in scope (limit {limit})")]
    TooManyLocals { span: Span, limit: usize },
}

impl Diagnostic for CompileError {
    fn span(&self) -> Span {
        match self {
            CompileError::FunctionsNotYet { span }
            | CompileError::ReturnOutsideFunction { span }
            | CompileError::BreakOutsideLoop { span }
            | CompileError::TooManyConstants { span, .. }
            | CompileError::TooManyLocals { span, .. } => *span,
        }
    }

    fn message(&self) -> String {
        self.to_string()
    }
}

/// Compile a resolved program into a single top-level chunk.
pub fn compile(program: &Program) -> Result<Chunk, CompileError> {
    let mut c = Compiler::new();
    for stmt in program {
        c.stmt(stmt)?;
    }
    Ok(c.chunk)
}

/// A local variable, tracked at compile time. `slot` is the value's absolute
/// position on the stack (with no call frames in stage 3, the frame base is 0).
///
/// The slot is *not* simply the index into [`Compiler::locals`]: a block can be
/// an expression nested inside a larger one (e.g. `print(if c { let a = 1; a })`),
/// so temporaries already on the stack sit *below* the block's locals. The slot
/// is therefore the compile-time stack height at the moment the local is bound,
/// which is why the compiler tracks [`Compiler::height`].
struct Local {
    name: String,
    depth: u32,
    slot: u16,
}

/// Bookkeeping for one enclosing loop, used to wire up `break`.
struct LoopCtx {
    /// Number of locals live just before the loop's own locals were pushed.
    /// `break` pops everything above this and jumps to the loop's exit.
    base: usize,
    /// Indices of placeholder `Jump`s emitted by `break`, patched to the exit.
    breaks: Vec<usize>,
}

struct Compiler {
    chunk: Chunk,
    locals: Vec<Local>,
    scope_depth: u32,
    loops: Vec<LoopCtx>,
    /// The number of values that will be on the stack at this point in the
    /// emitted code — the compiler's running simulation of stack height. Local
    /// slots are read off this. It is kept accurate through straight-line code by
    /// [`Compiler::emit`] (via [`stack_effect`]) and adjusted by hand where
    /// control flow merges paths (see `if_expr`, `for_stmt`, `break_stmt`).
    height: usize,
}

impl Compiler {
    fn new() -> Self {
        Compiler {
            chunk: Chunk::new(),
            locals: Vec::new(),
            scope_depth: 0,
            loops: Vec::new(),
            height: 0,
        }
    }

    // --- low-level emit helpers ---------------------------------------------

    /// Emit one instruction tagged with `line`, updating the simulated stack
    /// height by its net effect; return its code index.
    fn emit(&mut self, instr: Instr, line: u32) -> usize {
        let effect = stack_effect(&instr);
        self.height = (self.height as isize + effect) as usize;
        self.chunk.push(instr, line)
    }

    /// Intern a constant, erroring if the pool would exceed the `u16` index space.
    fn intern(&mut self, value: Constant, span: Span) -> Result<u16, CompileError> {
        if self.chunk.constants.len() >= u16::MAX as usize {
            return Err(CompileError::TooManyConstants {
                span,
                limit: u16::MAX as usize,
            });
        }
        Ok(self.chunk.add_constant(value))
    }

    /// Emit a load of a constant value.
    fn emit_constant(&mut self, value: Constant, span: Span) -> Result<(), CompileError> {
        let idx = self.intern(value, span)?;
        self.emit(Instr::Constant(idx), span.line);
        Ok(())
    }

    /// Intern a name as a string constant (for global access / definition).
    fn name_constant(&mut self, name: &str, span: Span) -> Result<u16, CompileError> {
        self.intern(Constant::Str(name.to_string()), span)
    }

    /// Overwrite a previously-emitted jump's target with `target`.
    fn patch_jump(&mut self, at: usize, target: usize) {
        self.chunk.code[at] = match self.chunk.code[at] {
            Instr::Jump(_) => Instr::Jump(target),
            Instr::JumpIfFalse(_) => Instr::JumpIfFalse(target),
            Instr::JumpIfFalsePeek(_) => Instr::JumpIfFalsePeek(target),
            Instr::JumpIfTruePeek(_) => Instr::JumpIfTruePeek(target),
            Instr::ForIter { slot, .. } => Instr::ForIter { slot, exit: target },
            other => unreachable!("patch_jump on non-jump instruction {other:?}"),
        };
    }

    // --- scopes & locals -----------------------------------------------------

    fn begin_scope(&mut self) {
        self.scope_depth += 1;
    }

    /// Register a new local whose initializer value is already on top of the
    /// stack, and return its slot (that value's absolute stack position).
    fn add_local(&mut self, name: &str, span: Span) -> Result<u16, CompileError> {
        if self.height == 0 || self.height - 1 > u16::MAX as usize {
            return Err(CompileError::TooManyLocals {
                span,
                limit: u16::MAX as usize,
            });
        }
        let slot = (self.height - 1) as u16;
        self.locals.push(Local {
            name: name.to_string(),
            depth: self.scope_depth,
            slot,
        });
        Ok(slot)
    }

    /// Find a local by name (innermost first); `None` means it must be a global.
    fn resolve_local(&self, name: &str) -> Option<u16> {
        self.locals
            .iter()
            .rev()
            .find(|l| l.name == name)
            .map(|l| l.slot)
    }

    /// Leave a block scope, but keep the single value sitting on top of the
    /// stack (the block's result). Pops the block's locals out from under it.
    fn end_scope_keep_value(&mut self, line: u32) {
        let mut dropped = 0u16;
        while matches!(self.locals.last(), Some(l) if l.depth == self.scope_depth) {
            self.locals.pop();
            dropped += 1;
        }
        if dropped > 0 {
            self.emit(Instr::PopKeepTop(dropped), line);
        }
        self.scope_depth -= 1;
    }

    // --- statements ----------------------------------------------------------

    fn stmt(&mut self, stmt: &Stmt) -> Result<(), CompileError> {
        match stmt {
            // Mutability was already enforced by the resolver, so the compiler
            // ignores `is_mutable` and just stores the value.
            Stmt::Let {
                name, value, span, ..
            } => {
                self.expr(value)?;
                if self.scope_depth == 0 {
                    let idx = self.name_constant(name, *span)?;
                    self.emit(Instr::DefineGlobal(idx), span.line);
                } else {
                    // The value already on the stack *is* the local's slot.
                    self.add_local(name, *span)?;
                }
                Ok(())
            }
            Stmt::Function(f) => Err(CompileError::FunctionsNotYet { span: f.span }),
            Stmt::While { cond, body, span } => self.while_stmt(cond, body, *span),
            Stmt::For {
                var,
                iter,
                body,
                span,
            } => self.for_stmt(var, iter, body, *span),
            Stmt::Loop { body, span } => self.loop_stmt(body, *span),
            Stmt::Break(span) => self.break_stmt(*span),
            Stmt::Return { span, .. } => Err(CompileError::ReturnOutsideFunction { span: *span }),
            // An expression used as a statement: evaluate, then discard its value.
            Stmt::Expr(e) => {
                self.expr(e)?;
                self.emit(Instr::Pop, e.span.line);
                Ok(())
            }
        }
    }

    fn while_stmt(&mut self, cond: &Expr, body: &Block, span: Span) -> Result<(), CompileError> {
        let base = self.locals.len();
        self.loops.push(LoopCtx {
            base,
            breaks: Vec::new(),
        });

        let loop_start = self.chunk.len();
        self.expr(cond)?;
        let exit_jump = self.emit(Instr::JumpIfFalse(0), span.line);
        self.block_value(body)?;
        self.emit(Instr::Pop, span.line); // discard the body's value
        self.emit(Instr::Jump(loop_start), span.line);

        let exit = self.chunk.len();
        self.patch_jump(exit_jump, exit);
        let ctx = self.loops.pop().expect("loop context");
        for b in ctx.breaks {
            self.patch_jump(b, exit);
        }
        Ok(())
    }

    fn loop_stmt(&mut self, body: &Block, span: Span) -> Result<(), CompileError> {
        let base = self.locals.len();
        self.loops.push(LoopCtx {
            base,
            breaks: Vec::new(),
        });

        let loop_start = self.chunk.len();
        self.block_value(body)?;
        self.emit(Instr::Pop, span.line);
        self.emit(Instr::Jump(loop_start), span.line);

        let end = self.chunk.len();
        let ctx = self.loops.pop().expect("loop context");
        for b in ctx.breaks {
            self.patch_jump(b, end);
        }
        Ok(())
    }

    /// `for var in iter { body }` lowers to an explicit iterator protocol:
    /// build an iterator once, then each turn `ForIter` either pushes the next
    /// element (the loop variable) or jumps past the loop when exhausted.
    fn for_stmt(
        &mut self,
        var: &str,
        iter: &Expr,
        body: &Block,
        span: Span,
    ) -> Result<(), CompileError> {
        self.begin_scope();
        let base = self.locals.len();

        self.expr(iter)?;
        self.emit(Instr::GetIter, span.line);
        let iter_slot = self.add_local("$iter", span)?; // hidden iterator local

        self.loops.push(LoopCtx {
            base,
            breaks: Vec::new(),
        });

        let loop_start = self.chunk.len();
        let for_iter = self.emit(
            Instr::ForIter {
                slot: iter_slot,
                exit: 0,
            },
            span.line,
        );
        // The element pushed by ForIter is the loop variable for this iteration.
        self.add_local(var, span)?;
        self.block_value(body)?;
        self.emit(Instr::Pop, span.line); // discard the body's value
        self.locals.pop(); // the loop variable leaves compile-time scope
        self.emit(Instr::Pop, span.line); // pop the element before looping back
        self.emit(Instr::Jump(loop_start), span.line);

        let exit = self.chunk.len();
        self.patch_jump(for_iter, exit); // exhausted -> here, iterator still on stack
        self.emit(Instr::Pop, span.line); // pop the iterator
        self.locals.pop(); // drop the hidden iterator local

        let end = self.chunk.len();
        let ctx = self.loops.pop().expect("loop context");
        for b in ctx.breaks {
            self.patch_jump(b, end);
        }
        self.scope_depth -= 1;
        Ok(())
    }

    fn break_stmt(&mut self, span: Span) -> Result<(), CompileError> {
        let base = match self.loops.last() {
            Some(ctx) => ctx.base,
            None => return Err(CompileError::BreakOutsideLoop { span }),
        };
        // Unwind every local declared since the loop began, then jump to its exit.
        // Code after an unconditional `break` is unreachable, so restore the
        // simulated height afterwards to keep it accurate for the rest of the block.
        let saved_height = self.height;
        for _ in 0..(self.locals.len() - base) {
            self.emit(Instr::Pop, span.line);
        }
        let jump = self.emit(Instr::Jump(0), span.line);
        self.loops
            .last_mut()
            .expect("loop context")
            .breaks
            .push(jump);
        self.height = saved_height;
        Ok(())
    }

    // --- expressions (each leaves exactly one value on the stack) ------------

    fn expr(&mut self, expr: &Expr) -> Result<(), CompileError> {
        let span = expr.span;
        match &expr.kind {
            ExprKind::Int(n) => self.emit_constant(Constant::Int(*n), span)?,
            ExprKind::Float(f) => self.emit_constant(Constant::Float(*f), span)?,
            ExprKind::Str(s) => self.emit_constant(Constant::Str(s.clone()), span)?,
            ExprKind::Bool(true) => {
                self.emit(Instr::True, span.line);
            }
            ExprKind::Bool(false) => {
                self.emit(Instr::False, span.line);
            }
            ExprKind::Nil => {
                self.emit(Instr::Nil, span.line);
            }
            ExprKind::Interp(segs) => {
                for seg in segs {
                    match seg {
                        StrSeg::Literal(s) => self.emit_constant(Constant::Str(s.clone()), span)?,
                        StrSeg::Expr(e) => self.expr(e)?,
                    }
                }
                self.emit(Instr::BuildStr(segs.len() as u16), span.line);
            }
            ExprKind::List(items) => {
                for item in items {
                    self.expr(item)?;
                }
                self.emit(Instr::BuildList(items.len() as u16), span.line);
            }
            ExprKind::Dict(pairs) => {
                for (k, v) in pairs {
                    self.expr(k)?;
                    self.expr(v)?;
                }
                self.emit(Instr::BuildDict(pairs.len() as u16), span.line);
            }
            ExprKind::Ident(name) => match self.resolve_local(name) {
                Some(slot) => {
                    self.emit(Instr::GetLocal(slot), span.line);
                }
                None => {
                    let idx = self.name_constant(name, span)?;
                    self.emit(Instr::GetGlobal(idx), span.line);
                }
            },
            ExprKind::Unary { op, operand } => {
                self.expr(operand)?;
                self.emit(
                    match op {
                        UnaryOp::Neg => Instr::Neg,
                        UnaryOp::Not => Instr::Not,
                    },
                    span.line,
                );
            }
            ExprKind::Binary { op, left, right } => {
                self.expr(left)?;
                self.expr(right)?;
                self.emit(binop_instr(*op), span.line);
            }
            ExprKind::Logical { op, left, right } => self.logical(*op, left, right)?,
            ExprKind::Assign { target, value } => self.assign(target, value)?,
            ExprKind::Range {
                start,
                end,
                inclusive,
            } => {
                self.expr(start)?;
                self.expr(end)?;
                self.emit(Instr::MakeRange(*inclusive), span.line);
            }
            ExprKind::Call { callee, args } => {
                self.expr(callee)?;
                for arg in args {
                    self.expr(arg)?;
                }
                self.emit(Instr::Call(args.len() as u8), span.line);
            }
            ExprKind::Index { object, index } => {
                self.expr(object)?;
                self.expr(index)?;
                self.emit(Instr::Index, span.line);
            }
            ExprKind::If {
                cond,
                then_branch,
                else_branch,
            } => self.if_expr(cond, then_branch, else_branch.as_deref(), span)?,
            ExprKind::Block(block) => self.block_value(block)?,
            ExprKind::Function(f) => return Err(CompileError::FunctionsNotYet { span: f.span }),
        }
        Ok(())
    }

    /// Short-circuiting `&&` / `||` via a peeking conditional jump: the left
    /// operand stays on the stack and becomes the result when it short-circuits.
    fn logical(&mut self, op: LogicOp, left: &Expr, right: &Expr) -> Result<(), CompileError> {
        self.expr(left)?;
        let line = left.span.line;
        let short = match op {
            LogicOp::And => self.emit(Instr::JumpIfFalsePeek(0), line),
            LogicOp::Or => self.emit(Instr::JumpIfTruePeek(0), line),
        };
        self.emit(Instr::Pop, line); // discard the left operand, take the right
        self.expr(right)?;
        let end = self.chunk.len();
        self.patch_jump(short, end);
        Ok(())
    }

    fn assign(&mut self, target: &Expr, value: &Expr) -> Result<(), CompileError> {
        match &target.kind {
            ExprKind::Ident(name) => {
                self.expr(value)?;
                match self.resolve_local(name) {
                    Some(slot) => self.emit(Instr::SetLocal(slot), target.span.line),
                    None => {
                        let idx = self.name_constant(name, target.span)?;
                        self.emit(Instr::SetGlobal(idx), target.span.line)
                    }
                };
            }
            ExprKind::Index { object, index } => {
                self.expr(object)?;
                self.expr(index)?;
                self.expr(value)?;
                self.emit(Instr::SetIndex, target.span.line);
            }
            // The parser guarantees assignment targets are idents or indexes.
            other => unreachable!("invalid assignment target reached compiler: {other:?}"),
        }
        Ok(())
    }

    /// `if` as an expression: leaves the taken branch's value (or `nil` when the
    /// condition is false and there is no `else`).
    fn if_expr(
        &mut self,
        cond: &Expr,
        then_branch: &Block,
        else_branch: Option<&Expr>,
        span: Span,
    ) -> Result<(), CompileError> {
        self.expr(cond)?;
        let else_jump = self.emit(Instr::JumpIfFalse(0), span.line);
        // Both arms execute from the same runtime stack height; record it so the
        // else arm is compiled against the right height after the then arm raised
        // the simulated height by one.
        let height_at_branch = self.height;
        self.block_value(then_branch)?;
        let end_jump = self.emit(Instr::Jump(0), span.line);

        let else_start = self.chunk.len();
        self.patch_jump(else_jump, else_start);
        self.height = height_at_branch;
        match else_branch {
            Some(e) => self.expr(e)?,
            None => {
                self.emit(Instr::Nil, span.line);
            }
        }
        let end = self.chunk.len();
        self.patch_jump(end_jump, end);
        Ok(())
    }

    /// Compile a block in expression position: run its statements, then leave its
    /// trailing expression (or `nil`) as the single result, dropping locals.
    fn block_value(&mut self, block: &Block) -> Result<(), CompileError> {
        self.begin_scope();
        for stmt in &block.stmts {
            self.stmt(stmt)?;
        }
        match &block.tail {
            Some(tail) => self.expr(tail)?,
            None => {
                self.emit(Instr::Nil, block.span.line);
            }
        }
        self.end_scope_keep_value(block.span.line);
        Ok(())
    }
}

/// The net change an instruction makes to the stack height along its
/// *fall-through* (sequential) path. Branch instructions are accounted for at
/// their merge points by the compiler resetting `height` directly; here they
/// report the height of the path that continues in straight-line order.
fn stack_effect(instr: &Instr) -> isize {
    match instr {
        // push one
        Instr::Constant(_)
        | Instr::Nil
        | Instr::True
        | Instr::False
        | Instr::GetLocal(_)
        | Instr::GetGlobal(_)
        | Instr::GetUpvalue(_) => 1,
        // pop one
        Instr::Pop | Instr::DefineGlobal(_) | Instr::JumpIfFalse(_) => -1,
        Instr::PopKeepTop(n) => -(*n as isize),
        // pop one, push one (no net change)
        Instr::Neg
        | Instr::Not
        | Instr::SetLocal(_)
        | Instr::SetGlobal(_)
        | Instr::SetUpvalue(_)
        | Instr::GetIter
        | Instr::Jump(_)
        | Instr::JumpIfFalsePeek(_)
        | Instr::JumpIfTruePeek(_)
        | Instr::CloseUpvalue
        | Instr::Closure(_) => 0,
        // pop two, push one
        Instr::Add
        | Instr::Sub
        | Instr::Mul
        | Instr::Div
        | Instr::Mod
        | Instr::Eq
        | Instr::Ne
        | Instr::Lt
        | Instr::Le
        | Instr::Gt
        | Instr::Ge
        | Instr::Index
        | Instr::MakeRange(_) => -1,
        // pop three, push one
        Instr::SetIndex => -2,
        // variadic builders: pop k, push one
        Instr::BuildList(n) | Instr::BuildStr(n) => 1 - *n as isize,
        Instr::BuildDict(n) => 1 - 2 * *n as isize,
        // fall-through pushes the next element (the exhausted path is a jump)
        Instr::ForIter { .. } => 1,
        // pop callee + argc args, push the result
        Instr::Call(argc) => -(*argc as isize),
        // a function return leaves the callee site with one value (stage 4)
        Instr::Return => 0,
    }
}

/// Map a value-producing binary operator to its instruction.
fn binop_instr(op: BinOp) -> Instr {
    match op {
        BinOp::Add => Instr::Add,
        BinOp::Sub => Instr::Sub,
        BinOp::Mul => Instr::Mul,
        BinOp::Div => Instr::Div,
        BinOp::Mod => Instr::Mod,
        BinOp::Eq => Instr::Eq,
        BinOp::Ne => Instr::Ne,
        BinOp::Lt => Instr::Lt,
        BinOp::Le => Instr::Le,
        BinOp::Gt => Instr::Gt,
        BinOp::Ge => Instr::Ge,
    }
}
