//! The Lumen abstract syntax tree (AST).
//!
//! The parser (stage 2) builds this tree; the resolver checks it; the compiler
//! (stage 3) walks it to emit bytecode. Every [`Expr`] and most statements carry
//! a [`Span`] so later stages can report errors at the exact source location.
//!
//! Two language design choices are visible here:
//! * **Expression-oriented.** `if` and blocks are [`Expr`]s, not statements, so
//!   `let x = if c { a } else { b }` is ordinary. A [`Block`]'s value is its
//!   trailing expression (`tail`), mirroring Rust.
//! * **Immutability by default.** A `let` binding records `is_mutable`; the
//!   resolver rejects assignments to immutable bindings.

use lumen_common::Span;

/// A complete program: the list of top-level statements.
pub type Program = Vec<Stmt>;

/// A statement: something executed for its effect rather than its value.
#[derive(Debug, Clone, PartialEq)]
pub enum Stmt {
    /// `let [mut] name = value`
    Let {
        name: String,
        is_mutable: bool,
        value: Expr,
        span: Span,
    },
    /// A named function declaration `fn name(params) { body }`. Unlike a
    /// `let`-bound lambda, the name is in scope inside the body, which is what
    /// makes recursion work.
    Function(Function),
    /// `while cond { body }`
    While { cond: Expr, body: Block, span: Span },
    /// `for var in iter { body }` — `iter` is a range or a list.
    For {
        var: String,
        iter: Expr,
        body: Block,
        span: Span,
    },
    /// `loop { body }` — runs until a `break`.
    Loop { body: Block, span: Span },
    /// `break` out of the innermost loop.
    Break(Span),
    /// `return [value]` from the enclosing function.
    Return { value: Option<Expr>, span: Span },
    /// An expression evaluated as a statement; its value is discarded.
    Expr(Expr),
}

impl Stmt {
    /// The source span of this statement, for error reporting.
    pub fn span(&self) -> Span {
        match self {
            Stmt::Let { span, .. }
            | Stmt::While { span, .. }
            | Stmt::For { span, .. }
            | Stmt::Loop { span, .. }
            | Stmt::Break(span)
            | Stmt::Return { span, .. } => *span,
            Stmt::Function(f) => f.span,
            Stmt::Expr(e) => e.span,
        }
    }
}

/// A function definition, shared by named declarations ([`Stmt::Function`]) and
/// anonymous lambdas ([`ExprKind::Function`]).
#[derive(Debug, Clone, PartialEq)]
pub struct Function {
    /// `Some` for `fn name(...)`, `None` for an anonymous `fn(...)`.
    pub name: Option<String>,
    pub params: Vec<Param>,
    pub body: Block,
    pub span: Span,
}

/// A single function parameter.
#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    pub name: String,
    pub span: Span,
}

/// A brace-delimited block. Its value (when used as an expression) is the
/// trailing `tail` expression, or `nil` if there is none.
#[derive(Debug, Clone, PartialEq)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    pub tail: Option<Box<Expr>>,
    pub span: Span,
}

/// An expression node: a [`ExprKind`] plus its source [`Span`].
#[derive(Debug, Clone, PartialEq)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

impl Expr {
    pub fn new(kind: ExprKind, span: Span) -> Self {
        Expr { kind, span }
    }
}

/// The shape of an expression.
#[derive(Debug, Clone, PartialEq)]
pub enum ExprKind {
    // --- Literals ---
    Int(i64),
    Float(f64),
    Str(String),
    Bool(bool),
    Nil,
    /// A string interpolation `"a {b} c"`, as ordered segments.
    Interp(Vec<StrSeg>),
    List(Vec<Expr>),
    Dict(Vec<(Expr, Expr)>),

    // --- Names ---
    Ident(String),

    // --- Operators ---
    Unary {
        op: UnaryOp,
        operand: Box<Expr>,
    },
    Binary {
        op: BinOp,
        left: Box<Expr>,
        right: Box<Expr>,
    },
    /// `&&` / `||`, kept separate from [`BinOp`] because they short-circuit.
    Logical {
        op: LogicOp,
        left: Box<Expr>,
        right: Box<Expr>,
    },
    /// `target = value`; `target` is always an [`ExprKind::Ident`] or
    /// [`ExprKind::Index`] (validated by the parser).
    Assign {
        target: Box<Expr>,
        value: Box<Expr>,
    },
    /// `start..end` (exclusive) or `start..=end` (inclusive).
    Range {
        start: Box<Expr>,
        end: Box<Expr>,
        inclusive: bool,
    },

    // --- Postfix ---
    Call {
        callee: Box<Expr>,
        args: Vec<Expr>,
    },
    Index {
        object: Box<Expr>,
        index: Box<Expr>,
    },

    // --- Expression-oriented control flow ---
    /// `if cond { then } [else { ... }]`. `else_branch`, when present, is either
    /// a block expression (`else { ... }`) or another `if` (`else if ...`).
    If {
        cond: Box<Expr>,
        then_branch: Block,
        else_branch: Option<Box<Expr>>,
    },
    Block(Block),
    Function(Function),
}

/// One segment of an interpolated string.
#[derive(Debug, Clone, PartialEq)]
pub enum StrSeg {
    /// A literal chunk with escapes already resolved.
    Literal(String),
    /// An embedded `{expr}`.
    Expr(Box<Expr>),
}

/// Unary prefix operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    /// `-x`
    Neg,
    /// `!x`
    Not,
}

/// Binary operators that produce a value from two evaluated operands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
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
}

/// Short-circuiting logical operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogicOp {
    And,
    Or,
}
