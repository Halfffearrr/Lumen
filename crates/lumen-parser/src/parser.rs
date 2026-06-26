//! The Lumen parser: tokens -> AST.
//!
//! Statements use straightforward recursive descent. Expressions use **Pratt
//! parsing** (a.k.a. precedence climbing) via [`Parser::expr_bp`]: each operator
//! has a left/right *binding power*, and an operator is only folded into the
//! current expression while its left binding power is at least the caller's
//! minimum. Higher numbers bind tighter, so `1 + 2 * 3` parses as `1 + (2 * 3)`.
//!
//! A few grammar decisions worth knowing:
//! * `if` and blocks are expressions; a block's value is its trailing expression.
//! * A bare `{ ... }` in expression position is always a **dict** literal; blocks
//!   only appear as the body of `if`/`fn`/`while`/`for`/`loop`, parsed directly.
//! * Semicolons are optional separators; statement boundaries are found from the
//!   grammar, and a `;` after an expression marks it as a discarded statement.

use lumen_common::{Diagnostic, Span};
use lumen_lexer::{StrPart, Token, TokenKind};

use crate::ast::*;

/// Errors produced while parsing.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ParseError {
    #[error("expected {expected}, found {found}")]
    Expected {
        expected: String,
        found: String,
        span: Span,
    },
    #[error("invalid assignment target")]
    InvalidAssignTarget { span: Span },
    #[error("{message}")]
    Other { message: String, span: Span },
}

impl Diagnostic for ParseError {
    fn span(&self) -> Span {
        match self {
            ParseError::Expected { span, .. }
            | ParseError::InvalidAssignTarget { span }
            | ParseError::Other { span, .. } => *span,
        }
    }

    fn message(&self) -> String {
        self.to_string()
    }
}

/// Parse a full token stream into a program (list of statements).
pub fn parse(tokens: Vec<Token>) -> Result<Program, ParseError> {
    Parser::new(tokens).parse_program()
}

/// Parse a full token stream, recovering after statement-level errors so the
/// caller can report several syntax problems at once.
pub fn parse_recovering(tokens: Vec<Token>) -> Result<Program, Vec<ParseError>> {
    Parser::new(tokens).parse_program_recovering()
}

/// A cursor over a token stream that builds AST nodes.
pub struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    pub fn new(tokens: Vec<Token>) -> Self {
        Parser { tokens, pos: 0 }
    }

    /// Parse the whole program until end of input.
    pub fn parse_program(&mut self) -> Result<Program, ParseError> {
        let mut stmts = Vec::new();
        while !self.is_at_end() {
            stmts.push(self.parse_item()?);
        }
        Ok(stmts)
    }

    /// Parse the whole program, keeping valid statements after a syntax error.
    ///
    /// This is intentionally conservative: it synchronizes at semicolons and
    /// obvious statement starts, which is enough for command-line diagnostics
    /// without making the core parser harder to reason about.
    pub fn parse_program_recovering(&mut self) -> Result<Program, Vec<ParseError>> {
        let mut stmts = Vec::new();
        let mut errors = Vec::new();
        while !self.is_at_end() {
            match self.parse_item() {
                Ok(stmt) => stmts.push(stmt),
                Err(err) => {
                    errors.push(err);
                    self.synchronize();
                }
            }
        }
        if errors.is_empty() {
            Ok(stmts)
        } else {
            Err(errors)
        }
    }

    // ------------------------------------------------------------- token cursor

    fn peek(&self) -> &Token {
        // `pos` is clamped in `advance`, so this never goes out of bounds; the
        // last token is always `Eof`.
        &self.tokens[self.pos]
    }

    fn advance(&mut self) -> Token {
        let tok = self.tokens[self.pos].clone();
        if self.pos + 1 < self.tokens.len() {
            self.pos += 1;
        }
        tok
    }

    fn is_at_end(&self) -> bool {
        matches!(self.peek().kind, TokenKind::Eof)
    }

    /// Does the current token have the same variant as `kind` (payload ignored)?
    fn check(&self, kind: &TokenKind) -> bool {
        std::mem::discriminant(&self.peek().kind) == std::mem::discriminant(kind)
    }

    /// Consume the current token if it matches `kind`.
    fn eat(&mut self, kind: &TokenKind) -> bool {
        if self.check(kind) {
            self.advance();
            true
        } else {
            false
        }
    }

    /// Consume the current token, requiring it to match `kind`.
    fn expect(&mut self, kind: &TokenKind, desc: &str) -> Result<Token, ParseError> {
        if self.check(kind) {
            Ok(self.advance())
        } else {
            Err(self.expected_err(desc))
        }
    }

    fn expect_ident(&mut self) -> Result<(String, Span), ParseError> {
        let tok = self.peek().clone();
        if let TokenKind::Ident(name) = tok.kind {
            self.advance();
            Ok((name, tok.span))
        } else {
            Err(self.expected_err("identifier"))
        }
    }

    fn expected_err(&self, expected: &str) -> ParseError {
        ParseError::Expected {
            expected: expected.to_string(),
            found: describe(&self.peek().kind),
            span: self.peek().span,
        }
    }

    /// Skip tokens after a parse error until the next likely statement boundary.
    fn synchronize(&mut self) {
        if self.is_at_end() {
            return;
        }
        self.advance();
        while !self.is_at_end() {
            if matches!(
                self.tokens.get(self.pos.wrapping_sub(1)).map(|t| &t.kind),
                Some(TokenKind::Semicolon)
            ) {
                return;
            }
            if self.is_stmt_start() || self.can_start_expr() {
                return;
            }
            if self.check(&TokenKind::RBrace) {
                return;
            }
            self.advance();
        }
    }

    // ----------------------------------------------------------------- items

    /// Parse a statement or an expression-statement (the unit of a program and
    /// of a block, minus block tail handling).
    fn parse_item(&mut self) -> Result<Stmt, ParseError> {
        if self.is_stmt_start() {
            self.parse_stmt()
        } else {
            let expr = self.expr_bp(0)?;
            self.eat(&TokenKind::Semicolon);
            Ok(Stmt::Expr(expr))
        }
    }

    /// True when the current token begins a dedicated statement form (as opposed
    /// to an expression). `fn` only counts when it names a function.
    fn is_stmt_start(&self) -> bool {
        use TokenKind::*;
        match &self.peek().kind {
            Let | While | For | Loop | Break | Return => true,
            Fn => matches!(
                self.tokens.get(self.pos + 1).map(|t| &t.kind),
                Some(Ident(_))
            ),
            _ => false,
        }
    }

    fn parse_stmt(&mut self) -> Result<Stmt, ParseError> {
        match &self.peek().kind {
            TokenKind::Let => self.parse_let(),
            TokenKind::Fn => self.parse_fn_decl(),
            TokenKind::While => self.parse_while(),
            TokenKind::For => self.parse_for(),
            TokenKind::Loop => self.parse_loop(),
            TokenKind::Break => {
                let span = self.advance().span;
                self.eat(&TokenKind::Semicolon);
                Ok(Stmt::Break(span))
            }
            TokenKind::Return => self.parse_return(),
            _ => unreachable!("parse_stmt called on non-statement token"),
        }
    }

    fn parse_let(&mut self) -> Result<Stmt, ParseError> {
        let kw = self.expect(&TokenKind::Let, "'let'")?;
        let is_mutable = self.eat(&TokenKind::Mut);
        let (name, _) = self.expect_ident()?;
        self.expect(&TokenKind::Eq, "'=' in let binding")?;
        let value = self.expr_bp(0)?;
        let span = kw.span.merge(value.span);
        self.eat(&TokenKind::Semicolon);
        Ok(Stmt::Let {
            name,
            is_mutable,
            value,
            span,
        })
    }

    fn parse_fn_decl(&mut self) -> Result<Stmt, ParseError> {
        let kw = self.expect(&TokenKind::Fn, "'fn'")?;
        let (name, _) = self.expect_ident()?;
        let (params, body) = self.parse_fn_rest()?;
        let span = kw.span.merge(body.span);
        Ok(Stmt::Function(Function {
            name: Some(name),
            params,
            body,
            span,
        }))
    }

    /// Parse the `(params) { body }` part shared by declarations and lambdas.
    fn parse_fn_rest(&mut self) -> Result<(Vec<Param>, Block), ParseError> {
        self.expect(&TokenKind::LParen, "'(' after function name")?;
        let mut params = Vec::new();
        while !self.check(&TokenKind::RParen) {
            let (name, span) = self.expect_ident()?;
            params.push(Param { name, span });
            if !self.eat(&TokenKind::Comma) {
                break;
            }
        }
        self.expect(&TokenKind::RParen, "')' after parameters")?;
        let body = self.parse_block()?;
        Ok((params, body))
    }

    fn parse_while(&mut self) -> Result<Stmt, ParseError> {
        let kw = self.expect(&TokenKind::While, "'while'")?;
        let cond = self.expr_bp(0)?;
        let body = self.parse_block()?;
        let span = kw.span.merge(body.span);
        Ok(Stmt::While { cond, body, span })
    }

    fn parse_for(&mut self) -> Result<Stmt, ParseError> {
        let kw = self.expect(&TokenKind::For, "'for'")?;
        let (var, _) = self.expect_ident()?;
        self.expect(&TokenKind::In, "'in' in for loop")?;
        let iter = self.expr_bp(0)?;
        let body = self.parse_block()?;
        let span = kw.span.merge(body.span);
        Ok(Stmt::For {
            var,
            iter,
            body,
            span,
        })
    }

    fn parse_loop(&mut self) -> Result<Stmt, ParseError> {
        let kw = self.expect(&TokenKind::Loop, "'loop'")?;
        let body = self.parse_block()?;
        let span = kw.span.merge(body.span);
        Ok(Stmt::Loop { body, span })
    }

    fn parse_return(&mut self) -> Result<Stmt, ParseError> {
        let kw = self.expect(&TokenKind::Return, "'return'")?;
        let value = if self.can_start_expr() {
            Some(self.expr_bp(0)?)
        } else {
            None
        };
        let span = match &value {
            Some(v) => kw.span.merge(v.span),
            None => kw.span,
        };
        self.eat(&TokenKind::Semicolon);
        Ok(Stmt::Return { value, span })
    }

    /// Parse `{ ... }`. Statements accumulate in `stmts`; a final expression with
    /// no terminating `;` becomes the block's `tail` (its value).
    fn parse_block(&mut self) -> Result<Block, ParseError> {
        let open = self.expect(&TokenKind::LBrace, "'{'")?;
        let mut stmts = Vec::new();
        let mut tail = None;
        while !self.check(&TokenKind::RBrace) && !self.is_at_end() {
            if self.is_stmt_start() {
                stmts.push(self.parse_stmt()?);
            } else {
                let expr = self.expr_bp(0)?;
                if self.eat(&TokenKind::Semicolon) {
                    stmts.push(Stmt::Expr(expr));
                } else if self.check(&TokenKind::RBrace) {
                    tail = Some(Box::new(expr));
                    break;
                } else {
                    stmts.push(Stmt::Expr(expr));
                }
            }
        }
        let close = self.expect(&TokenKind::RBrace, "'}' to close block")?;
        Ok(Block {
            stmts,
            tail,
            span: open.span.merge(close.span),
        })
    }

    // ------------------------------------------------------------- expressions

    /// Pratt parser core. Parses a prefix expression, then folds in any infix or
    /// postfix operator whose left binding power is at least `min_bp`.
    fn expr_bp(&mut self, min_bp: u8) -> Result<Expr, ParseError> {
        let mut lhs = self.prefix()?;
        loop {
            // Postfix call / index bind tighter than every infix operator.
            match &self.peek().kind {
                TokenKind::LParen if POSTFIX_BP >= min_bp => {
                    lhs = self.finish_call(lhs)?;
                    continue;
                }
                TokenKind::LBracket if POSTFIX_BP >= min_bp => {
                    lhs = self.finish_index(lhs)?;
                    continue;
                }
                _ => {}
            }
            let Some((l_bp, r_bp)) = infix_bp(&self.peek().kind) else {
                break;
            };
            if l_bp < min_bp {
                break;
            }
            let op = self.advance();
            lhs = self.finish_infix(lhs, &op, r_bp)?;
        }
        Ok(lhs)
    }

    /// Parse a prefix expression: a unary operator applied to an operand, or a
    /// primary.
    fn prefix(&mut self) -> Result<Expr, ParseError> {
        let op = match self.peek().kind {
            TokenKind::Minus => Some(UnaryOp::Neg),
            TokenKind::Bang => Some(UnaryOp::Not),
            _ => None,
        };
        if let Some(op) = op {
            let tok = self.advance();
            let operand = self.expr_bp(UNARY_BP)?;
            let span = tok.span.merge(operand.span);
            Ok(Expr::new(
                ExprKind::Unary {
                    op,
                    operand: Box::new(operand),
                },
                span,
            ))
        } else {
            self.primary()
        }
    }

    fn finish_infix(&mut self, lhs: Expr, op: &Token, r_bp: u8) -> Result<Expr, ParseError> {
        let right = self.expr_bp(r_bp)?;
        let span = lhs.span.merge(right.span);
        let kind = match &op.kind {
            TokenKind::Eq => {
                if !is_assign_target(&lhs) {
                    return Err(ParseError::InvalidAssignTarget { span: lhs.span });
                }
                ExprKind::Assign {
                    target: Box::new(lhs),
                    value: Box::new(right),
                }
            }
            TokenKind::DotDot => ExprKind::Range {
                start: Box::new(lhs),
                end: Box::new(right),
                inclusive: false,
            },
            TokenKind::DotDotEq => ExprKind::Range {
                start: Box::new(lhs),
                end: Box::new(right),
                inclusive: true,
            },
            TokenKind::AmpAmp => ExprKind::Logical {
                op: LogicOp::And,
                left: Box::new(lhs),
                right: Box::new(right),
            },
            TokenKind::PipePipe => ExprKind::Logical {
                op: LogicOp::Or,
                left: Box::new(lhs),
                right: Box::new(right),
            },
            other => ExprKind::Binary {
                op: binop_of(other),
                left: Box::new(lhs),
                right: Box::new(right),
            },
        };
        Ok(Expr::new(kind, span))
    }

    fn finish_call(&mut self, callee: Expr) -> Result<Expr, ParseError> {
        self.expect(&TokenKind::LParen, "'('")?;
        let mut args = Vec::new();
        while !self.check(&TokenKind::RParen) {
            args.push(self.expr_bp(0)?);
            if !self.eat(&TokenKind::Comma) {
                break;
            }
        }
        let close = self.expect(&TokenKind::RParen, "')' after arguments")?;
        let span = callee.span.merge(close.span);
        Ok(Expr::new(
            ExprKind::Call {
                callee: Box::new(callee),
                args,
            },
            span,
        ))
    }

    fn finish_index(&mut self, object: Expr) -> Result<Expr, ParseError> {
        self.expect(&TokenKind::LBracket, "'['")?;
        let index = self.expr_bp(0)?;
        let close = self.expect(&TokenKind::RBracket, "']' after index")?;
        let span = object.span.merge(close.span);
        Ok(Expr::new(
            ExprKind::Index {
                object: Box::new(object),
                index: Box::new(index),
            },
            span,
        ))
    }

    /// Parse a primary expression: a literal, name, grouping, collection, or one
    /// of the expression-valued control-flow forms.
    fn primary(&mut self) -> Result<Expr, ParseError> {
        let tok = self.peek().clone();
        let kind = match tok.kind {
            TokenKind::Int(n) => {
                self.advance();
                ExprKind::Int(n)
            }
            TokenKind::Float(f) => {
                self.advance();
                ExprKind::Float(f)
            }
            TokenKind::Str(s) => {
                self.advance();
                ExprKind::Str(s)
            }
            TokenKind::True => {
                self.advance();
                ExprKind::Bool(true)
            }
            TokenKind::False => {
                self.advance();
                ExprKind::Bool(false)
            }
            TokenKind::Nil => {
                self.advance();
                ExprKind::Nil
            }
            TokenKind::Ident(name) => {
                self.advance();
                ExprKind::Ident(name)
            }
            TokenKind::Template(parts) => {
                self.advance();
                ExprKind::Interp(self.lower_template(parts)?)
            }
            TokenKind::LParen => return self.grouping(),
            TokenKind::LBracket => return self.list_literal(),
            TokenKind::LBrace => return self.dict_literal(),
            TokenKind::If => return self.parse_if(),
            TokenKind::Fn => return self.fn_expr(),
            _ => return Err(self.expected_err("expression")),
        };
        Ok(Expr::new(kind, tok.span))
    }

    fn grouping(&mut self) -> Result<Expr, ParseError> {
        let open = self.expect(&TokenKind::LParen, "'('")?;
        let inner = self.expr_bp(0)?;
        let close = self.expect(&TokenKind::RParen, "')'")?;
        // Keep the inner kind but widen the span to include the parentheses.
        Ok(Expr::new(inner.kind, open.span.merge(close.span)))
    }

    fn list_literal(&mut self) -> Result<Expr, ParseError> {
        let open = self.expect(&TokenKind::LBracket, "'['")?;
        let mut items = Vec::new();
        while !self.check(&TokenKind::RBracket) {
            items.push(self.expr_bp(0)?);
            if !self.eat(&TokenKind::Comma) {
                break;
            }
        }
        let close = self.expect(&TokenKind::RBracket, "']' to close list")?;
        Ok(Expr::new(
            ExprKind::List(items),
            open.span.merge(close.span),
        ))
    }

    fn dict_literal(&mut self) -> Result<Expr, ParseError> {
        let open = self.expect(&TokenKind::LBrace, "'{'")?;
        let mut pairs = Vec::new();
        while !self.check(&TokenKind::RBrace) {
            let key = self.expr_bp(0)?;
            self.expect(&TokenKind::Colon, "':' between dict key and value")?;
            let value = self.expr_bp(0)?;
            pairs.push((key, value));
            if !self.eat(&TokenKind::Comma) {
                break;
            }
        }
        let close = self.expect(&TokenKind::RBrace, "'}' to close dict")?;
        Ok(Expr::new(
            ExprKind::Dict(pairs),
            open.span.merge(close.span),
        ))
    }

    fn parse_if(&mut self) -> Result<Expr, ParseError> {
        let kw = self.expect(&TokenKind::If, "'if'")?;
        let cond = self.expr_bp(0)?;
        let then_branch = self.parse_block()?;
        let (else_branch, end) = if self.eat(&TokenKind::Else) {
            if self.check(&TokenKind::If) {
                let inner = self.parse_if()?; // else if ...
                let span = inner.span;
                (Some(Box::new(inner)), span)
            } else {
                let blk = self.parse_block()?;
                let span = blk.span;
                (Some(Box::new(Expr::new(ExprKind::Block(blk), span))), span)
            }
        } else {
            (None, then_branch.span)
        };
        let span = kw.span.merge(end);
        Ok(Expr::new(
            ExprKind::If {
                cond: Box::new(cond),
                then_branch,
                else_branch,
            },
            span,
        ))
    }

    fn fn_expr(&mut self) -> Result<Expr, ParseError> {
        let kw = self.expect(&TokenKind::Fn, "'fn'")?;
        // An anonymous function may still carry a name (e.g. for self-reference).
        let name = if self.check(&TokenKind::Ident(String::new())) {
            Some(self.expect_ident()?.0)
        } else {
            None
        };
        let (params, body) = self.parse_fn_rest()?;
        let span = kw.span.merge(body.span);
        Ok(Expr::new(
            ExprKind::Function(Function {
                name,
                params,
                body,
                span,
            }),
            span,
        ))
    }

    /// Lower lexer string-template parts into AST segments, re-parsing the source
    /// of each `{expr}` interpolation.
    fn lower_template(&self, parts: Vec<StrPart>) -> Result<Vec<StrSeg>, ParseError> {
        let mut segs = Vec::with_capacity(parts.len());
        for part in parts {
            match part {
                StrPart::Literal(s) => segs.push(StrSeg::Literal(s)),
                StrPart::Expr { source, span } => {
                    segs.push(StrSeg::Expr(Box::new(parse_interpolated(&source, span)?)));
                }
            }
        }
        Ok(segs)
    }

    /// True when the current token can begin an expression (used to decide
    /// whether `return` has a value).
    fn can_start_expr(&self) -> bool {
        use TokenKind::*;
        matches!(
            self.peek().kind,
            Int(_)
                | Float(_)
                | Str(_)
                | Template(_)
                | Ident(_)
                | True
                | False
                | Nil
                | Minus
                | Bang
                | LParen
                | LBracket
                | LBrace
                | If
                | Fn
        )
    }
}

/// Re-lex and parse the source of a `{expr}` interpolation into a single
/// expression. Errors are reported at the interpolation's location in the
/// original file (`span`).
fn parse_interpolated(source: &str, span: Span) -> Result<Expr, ParseError> {
    let tokens = lumen_lexer::tokenize(source).map_err(|e| ParseError::Other {
        message: format!("in interpolation: {e}"),
        span,
    })?;
    let mut parser = Parser::new(tokens);
    let expr = parser.expr_bp(0).map_err(|e| ParseError::Other {
        message: format!("in interpolation: {}", e.message()),
        span,
    })?;
    if !parser.is_at_end() {
        return Err(ParseError::Other {
            message: "unexpected tokens after interpolation expression".to_string(),
            span,
        });
    }
    Ok(expr)
}

// --- binding powers (higher binds tighter) ---

/// Binding power of unary prefix operators' operands.
const UNARY_BP: u8 = 19;
/// Binding power of postfix call/index, the tightest of all.
const POSTFIX_BP: u8 = 21;

/// The (left, right) binding power of an infix operator, or `None` if `kind` is
/// not an infix operator. A right binding power lower than the left makes an
/// operator right-associative (used for `=`).
fn infix_bp(kind: &TokenKind) -> Option<(u8, u8)> {
    use TokenKind::*;
    Some(match kind {
        Eq => (2, 1),                       // assignment, right-assoc
        DotDot | DotDotEq => (4, 5),        // range
        PipePipe => (6, 7),                 // ||
        AmpAmp => (8, 9),                   // &&
        EqEq | BangEq => (10, 11),          // == !=
        Lt | LtEq | Gt | GtEq => (12, 13),  // comparisons
        Plus | Minus => (14, 15),           // + -
        Star | Slash | Percent => (16, 17), // * / %
        _ => return None,
    })
}

/// Map an arithmetic/comparison token to its [`BinOp`]. The caller guarantees
/// `kind` is one of these.
fn binop_of(kind: &TokenKind) -> BinOp {
    use TokenKind::*;
    match kind {
        Plus => BinOp::Add,
        Minus => BinOp::Sub,
        Star => BinOp::Mul,
        Slash => BinOp::Div,
        Percent => BinOp::Mod,
        EqEq => BinOp::Eq,
        BangEq => BinOp::Ne,
        Lt => BinOp::Lt,
        LtEq => BinOp::Le,
        Gt => BinOp::Gt,
        GtEq => BinOp::Ge,
        _ => unreachable!("binop_of called on non-binary token"),
    }
}

fn is_assign_target(expr: &Expr) -> bool {
    matches!(expr.kind, ExprKind::Ident(_) | ExprKind::Index { .. })
}

/// A short human description of a token, for "found ..." in error messages.
fn describe(kind: &TokenKind) -> String {
    use TokenKind::*;
    match kind {
        Eof => "end of input".to_string(),
        Ident(name) => format!("identifier '{name}'"),
        Int(n) => format!("integer '{n}'"),
        Float(f) => format!("float '{f}'"),
        Str(_) | Template(_) => "string".to_string(),
        other => format!("'{}'", symbol(other)),
    }
}

/// The literal spelling of a fixed token (keywords, operators, delimiters).
fn symbol(kind: &TokenKind) -> &'static str {
    use TokenKind::*;
    match kind {
        Let => "let",
        Mut => "mut",
        Fn => "fn",
        If => "if",
        Else => "else",
        While => "while",
        For => "for",
        In => "in",
        Loop => "loop",
        Break => "break",
        Return => "return",
        True => "true",
        False => "false",
        Nil => "nil",
        Plus => "+",
        Minus => "-",
        Star => "*",
        Slash => "/",
        Percent => "%",
        Eq => "=",
        EqEq => "==",
        BangEq => "!=",
        Lt => "<",
        LtEq => "<=",
        Gt => ">",
        GtEq => ">=",
        AmpAmp => "&&",
        PipePipe => "||",
        Bang => "!",
        DotDot => "..",
        DotDotEq => "..=",
        LParen => "(",
        RParen => ")",
        LBrace => "{",
        RBrace => "}",
        LBracket => "[",
        RBracket => "]",
        Comma => ",",
        Colon => ":",
        Semicolon => ";",
        _ => "?",
    }
}
