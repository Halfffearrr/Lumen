//! The Lumen resolver: a static-analysis pass over the AST (buff6).
//!
//! Run after parsing and before compilation, it walks every scope and reports
//! mistakes the compiler would otherwise turn into confusing runtime failures:
//! * use of an **undefined variable**;
//! * **assignment to an immutable binding** (the heart of Lumen's
//!   immutable-by-default design);
//! * a `break` outside any loop;
//! * a **call with the wrong number of arguments** to a function whose arity is
//!   known (user `fn` declarations and built-ins).
//!
//! This embodies Rust's "catch errors at compile time" philosophy in miniature.

use std::collections::HashMap;

use lumen_common::{Diagnostic, Span, BUILTINS};

use crate::ast::*;

/// An error found during resolution.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ResolveError {
    #[error("undefined variable '{name}'")]
    Undefined { name: String, span: Span },
    #[error("cannot assign to immutable binding '{name}' (declare it with `let mut`)")]
    Immutable { name: String, span: Span },
    #[error("'break' outside of a loop")]
    BreakOutsideLoop { span: Span },
    #[error("function '{name}' expects {expected} argument(s), but got {got}")]
    ArityMismatch {
        name: String,
        expected: usize,
        got: usize,
        span: Span,
    },
}

impl Diagnostic for ResolveError {
    fn span(&self) -> Span {
        match self {
            ResolveError::Undefined { span, .. }
            | ResolveError::Immutable { span, .. }
            | ResolveError::BreakOutsideLoop { span }
            | ResolveError::ArityMismatch { span, .. } => *span,
        }
    }

    fn message(&self) -> String {
        self.to_string()
    }
}

/// What the resolver remembers about a name in scope.
#[derive(Debug, Clone, Copy)]
struct Binding {
    is_mutable: bool,
    /// Fixed call arity when the name denotes a function, else `None`.
    arity: Option<usize>,
}

/// Resolve a whole program, returning all errors found (empty `Ok` on success).
pub fn resolve(program: &Program) -> Result<(), Vec<ResolveError>> {
    resolve_with_globals(program, &[])
}

/// Resolve a program as if `predefined` global names were already in scope.
///
/// This supports the REPL: globals bound by earlier lines are not present in the
/// current line's AST, so they are seeded here (as mutable, arity-unknown) to
/// avoid spurious "undefined variable" errors. Arity is left unchecked for them;
/// the VM still checks it at call time.
pub fn resolve_with_globals(
    program: &Program,
    predefined: &[String],
) -> Result<(), Vec<ResolveError>> {
    let mut r = Resolver::new();
    r.begin_scope(); // the global scope, on top of the built-ins scope
    for name in predefined {
        r.define(
            name,
            Binding {
                is_mutable: true,
                arity: None,
            },
        );
    }
    r.resolve_block_stmts(program);
    r.end_scope();
    if r.errors.is_empty() {
        Ok(())
    } else {
        Err(r.errors)
    }
}

struct Resolver {
    /// Innermost scope is last. Scope 0 holds the built-ins.
    scopes: Vec<HashMap<String, Binding>>,
    errors: Vec<ResolveError>,
    loop_depth: usize,
}

impl Resolver {
    fn new() -> Self {
        let mut builtins = HashMap::new();
        for b in BUILTINS {
            builtins.insert(
                b.name.to_string(),
                Binding {
                    is_mutable: false,
                    arity: b.arity,
                },
            );
        }
        Resolver {
            scopes: vec![builtins],
            errors: Vec::new(),
            loop_depth: 0,
        }
    }

    fn begin_scope(&mut self) {
        self.scopes.push(HashMap::new());
    }

    fn end_scope(&mut self) {
        self.scopes.pop();
    }

    fn define(&mut self, name: &str, binding: Binding) {
        self.scopes
            .last_mut()
            .expect("at least one scope")
            .insert(name.to_string(), binding);
    }

    /// Look a name up from the innermost scope outward.
    fn lookup(&self, name: &str) -> Option<Binding> {
        self.scopes.iter().rev().find_map(|s| s.get(name).copied())
    }

    fn error(&mut self, err: ResolveError) {
        self.errors.push(err);
    }

    // ----------------------------------------------------------------- walk

    fn resolve_block_stmts(&mut self, stmts: &[Stmt]) {
        // Hoist function declarations: bind every `fn` name in this scope before
        // resolving any bodies, so functions can refer to one another regardless
        // of order (mutual recursion, e.g. a recursive-descent parser).
        for stmt in stmts {
            if let Stmt::Function(func) = stmt {
                if let Some(name) = &func.name {
                    self.define(
                        name,
                        Binding {
                            is_mutable: false,
                            arity: Some(func.params.len()),
                        },
                    );
                }
            }
        }
        for stmt in stmts {
            self.resolve_stmt(stmt);
        }
    }

    fn resolve_stmt(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::Let {
                name,
                is_mutable,
                value,
                ..
            } => {
                // The initializer is resolved before the name is bound, so
                // `let x = x` refers to an outer `x` (or errors), never itself.
                self.resolve_expr(value);
                self.define(
                    name,
                    Binding {
                        is_mutable: *is_mutable,
                        arity: None,
                    },
                );
            }
            Stmt::Function(func) => {
                // Bind the name first so the body can call itself (recursion).
                if let Some(name) = &func.name {
                    self.define(
                        name,
                        Binding {
                            is_mutable: false,
                            arity: Some(func.params.len()),
                        },
                    );
                }
                self.resolve_function(func);
            }
            Stmt::While { cond, body, .. } => {
                self.resolve_expr(cond);
                self.loop_depth += 1;
                self.resolve_block(body);
                self.loop_depth -= 1;
            }
            Stmt::For {
                var, iter, body, ..
            } => {
                self.resolve_expr(iter);
                self.loop_depth += 1;
                self.begin_scope();
                // The loop variable is immutable within the body, like Rust.
                self.define(
                    var,
                    Binding {
                        is_mutable: false,
                        arity: None,
                    },
                );
                self.resolve_block_inner(body);
                self.end_scope();
                self.loop_depth -= 1;
            }
            Stmt::Loop { body, .. } => {
                self.loop_depth += 1;
                self.resolve_block(body);
                self.loop_depth -= 1;
            }
            Stmt::Break(span) => {
                if self.loop_depth == 0 {
                    self.error(ResolveError::BreakOutsideLoop { span: *span });
                }
            }
            Stmt::Return { value, .. } => {
                if let Some(value) = value {
                    self.resolve_expr(value);
                }
            }
            Stmt::Expr(expr) => self.resolve_expr(expr),
        }
    }

    /// Resolve a function's parameters and body in a fresh scope.
    fn resolve_function(&mut self, func: &Function) {
        self.begin_scope();
        for param in &func.params {
            // Parameters are immutable by default (Lumen's Rust-like theme).
            self.define(
                &param.name,
                Binding {
                    is_mutable: false,
                    arity: None,
                },
            );
        }
        self.resolve_block_inner(&func.body);
        self.end_scope();
    }

    /// Resolve a block in its own new scope.
    fn resolve_block(&mut self, block: &Block) {
        self.begin_scope();
        self.resolve_block_inner(block);
        self.end_scope();
    }

    /// Resolve a block's contents without opening a new scope (used when the
    /// caller already opened one, e.g. for parameters or a loop variable).
    fn resolve_block_inner(&mut self, block: &Block) {
        self.resolve_block_stmts(&block.stmts);
        if let Some(tail) = &block.tail {
            self.resolve_expr(tail);
        }
    }

    fn resolve_expr(&mut self, expr: &Expr) {
        match &expr.kind {
            ExprKind::Int(_)
            | ExprKind::Float(_)
            | ExprKind::Str(_)
            | ExprKind::Bool(_)
            | ExprKind::Nil => {}

            ExprKind::Ident(name) => {
                if self.lookup(name).is_none() {
                    self.error(ResolveError::Undefined {
                        name: name.clone(),
                        span: expr.span,
                    });
                }
            }

            ExprKind::Interp(segs) => {
                for seg in segs {
                    if let StrSeg::Expr(e) = seg {
                        self.resolve_expr(e);
                    }
                }
            }

            ExprKind::List(items) => {
                for item in items {
                    self.resolve_expr(item);
                }
            }

            ExprKind::Dict(pairs) => {
                for (k, v) in pairs {
                    self.resolve_expr(k);
                    self.resolve_expr(v);
                }
            }

            ExprKind::Unary { operand, .. } => self.resolve_expr(operand),

            ExprKind::Binary { left, right, .. } | ExprKind::Logical { left, right, .. } => {
                self.resolve_expr(left);
                self.resolve_expr(right);
            }

            ExprKind::Range { start, end, .. } => {
                self.resolve_expr(start);
                self.resolve_expr(end);
            }

            ExprKind::Assign { target, value } => {
                self.resolve_expr(value);
                self.resolve_assign_target(target);
            }

            ExprKind::Call { callee, args } => {
                self.resolve_expr(callee);
                for arg in args {
                    self.resolve_expr(arg);
                }
                self.check_call_arity(callee, args, expr.span);
            }

            ExprKind::Index { object, index } => {
                self.resolve_expr(object);
                self.resolve_expr(index);
            }

            ExprKind::If {
                cond,
                then_branch,
                else_branch,
            } => {
                self.resolve_expr(cond);
                self.resolve_block(then_branch);
                if let Some(else_branch) = else_branch {
                    self.resolve_expr(else_branch);
                }
            }

            ExprKind::Block(block) => self.resolve_block(block),

            ExprKind::Function(func) => {
                // A named lambda can refer to itself inside its body.
                self.begin_scope();
                if let Some(name) = &func.name {
                    self.define(
                        name,
                        Binding {
                            is_mutable: false,
                            arity: Some(func.params.len()),
                        },
                    );
                }
                self.resolve_function(func);
                self.end_scope();
            }
        }
    }

    /// Check the left-hand side of an assignment.
    fn resolve_assign_target(&mut self, target: &Expr) {
        match &target.kind {
            ExprKind::Ident(name) => match self.lookup(name) {
                None => self.error(ResolveError::Undefined {
                    name: name.clone(),
                    span: target.span,
                }),
                Some(b) if !b.is_mutable => self.error(ResolveError::Immutable {
                    name: name.clone(),
                    span: target.span,
                }),
                Some(_) => {}
            },
            // Element assignment (`list[i] = x`) mutates the object, not the
            // binding, so it does not require a `mut` binding — just resolve it.
            ExprKind::Index { object, index } => {
                self.resolve_expr(object);
                self.resolve_expr(index);
            }
            _ => {}
        }
    }

    /// If `callee` is a name with a known arity, verify the argument count.
    fn check_call_arity(&mut self, callee: &Expr, args: &[Expr], span: Span) {
        if let ExprKind::Ident(name) = &callee.kind {
            if let Some(Binding {
                arity: Some(arity), ..
            }) = self.lookup(name)
            {
                if arity != args.len() {
                    self.error(ResolveError::ArityMismatch {
                        name: name.clone(),
                        expected: arity,
                        got: args.len(),
                        span,
                    });
                }
            }
        }
    }
}
