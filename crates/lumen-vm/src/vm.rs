//! The Lumen virtual machine: a stack-based bytecode interpreter.
//!
//! Execution is a classic fetch-decode-execute loop over a [`Chunk`]'s
//! instruction stream. An *instruction pointer* (`ip`) indexes the next
//! [`Instr`]; most instructions consume operands from the top of the value stack
//! and push a result, while jumps move `ip` directly. There are no call frames in
//! stage 3 — the whole program runs in one frame whose locals occupy the bottom
//! of the stack — so a local's compile-time slot is its absolute stack index.
//!
//! Runtime errors carry the source line of the instruction that raised them,
//! reconstructed from the chunk's line table.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{SystemTime, UNIX_EPOCH};

use lumen_common::BUILTINS;
use lumen_compiler::{Chunk, Constant, FnProto, Instr};

use crate::value::{values_equal, Closure, IterState, Native, Upvalue, Value};

/// An error raised while executing bytecode.
#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeError {
    pub message: String,
    /// The source line of the faulting instruction, filled in by the VM.
    pub line: Option<u32>,
}

impl RuntimeError {
    pub fn new(message: impl Into<String>) -> Self {
        RuntimeError {
            message: message.into(),
            line: None,
        }
    }
}

impl std::fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.line {
            Some(line) => write!(f, "runtime error (line {line}): {}", self.message),
            None => write!(f, "runtime error: {}", self.message),
        }
    }
}

impl std::error::Error for RuntimeError {}

/// One activation record. `base` is where this call's slots begin on the shared
/// value stack: slot 0 (the closure being called) sits at `stack[base]`, so a
/// local at compile-time slot `s` lives at `stack[base + s]`. `ip` is this
/// frame's instruction pointer into its own chunk.
struct CallFrame {
    closure: Rc<Closure>,
    ip: usize,
    base: usize,
}

/// What [`Vm::exec`] tells the run loop to do next.
enum Flow {
    /// Keep fetching instructions from the (possibly newly pushed) top frame.
    Continue,
    /// The top-level script returned; this is the program's result.
    Done(Value),
}

/// The virtual machine: the value stack, the call-frame stack, the list of
/// open upvalues, the global environment (seeded with built-ins), and a buffer
/// capturing everything `print`s.
#[derive(Debug)]
pub struct Vm {
    stack: Vec<Value>,
    frames: Vec<CallFrame>,
    /// Upvalues still pointing at live stack slots, so several closures capturing
    /// the same variable share one cell and it can be closed when the slot dies.
    open_upvalues: Vec<Rc<RefCell<Upvalue>>>,
    globals: HashMap<String, Value>,
    /// Everything written by `print`, newline-terminated. Buffering (rather than
    /// writing straight to stdout) keeps the VM testable; the CLI flushes this.
    pub output: String,
}

impl std::fmt::Debug for CallFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "CallFrame{{ ip: {}, base: {} }}", self.ip, self.base)
    }
}

impl Default for Vm {
    fn default() -> Self {
        Self::new()
    }
}

impl Vm {
    /// Create a VM with every built-in registered as a global.
    pub fn new() -> Self {
        let mut globals = HashMap::new();
        for b in BUILTINS {
            globals.insert(
                b.name.to_string(),
                Value::Native(Native {
                    name: b.name,
                    arity: b.arity,
                    func: builtin_fn(b.name),
                }),
            );
        }
        Vm {
            stack: Vec::new(),
            frames: Vec::new(),
            open_upvalues: Vec::new(),
            globals,
            output: String::new(),
        }
    }

    /// The names of every global currently bound (built-ins plus anything the
    /// program has defined). Used by the REPL to seed the resolver so references
    /// to globals from earlier lines are not flagged as undefined.
    pub fn global_names(&self) -> Vec<String> {
        self.globals.keys().cloned().collect()
    }

    // --- stack helpers -------------------------------------------------------

    fn push(&mut self, v: Value) {
        self.stack.push(v);
    }

    /// Pop the top of the stack. A correct compiler never underflows, so this is
    /// an invariant violation rather than a user-facing error.
    fn pop(&mut self) -> Value {
        self.stack.pop().expect("VM stack underflow (compiler bug)")
    }

    fn peek(&self, depth: usize) -> &Value {
        &self.stack[self.stack.len() - 1 - depth]
    }

    /// Run a compiled program to completion, returning the script's result value.
    ///
    /// The chunk is wrapped in a synthetic top-level closure (the "script") and
    /// pushed as the first call frame. The loop then fetches and executes one
    /// instruction at a time from whichever frame is on top, following calls and
    /// returns by pushing and popping frames.
    pub fn run(&mut self, chunk: Chunk) -> Result<Value, RuntimeError> {
        let proto = Rc::new(FnProto {
            name: "<script>".to_string(),
            arity: 0,
            chunk,
            upvalues: Vec::new(),
        });
        let script = Rc::new(Closure {
            proto,
            upvalues: Vec::new(),
        });
        self.frames.push(CallFrame {
            closure: script,
            ip: 0,
            base: 0,
        });

        loop {
            let frame = self.frames.last().expect("a frame to execute");
            let closure = frame.closure.clone();
            let ip = frame.ip;
            // Chunks always end with `Return`, so the ip never runs off the end.
            let instr = closure.proto.chunk.code[ip];
            let line = closure.proto.chunk.lines[ip];
            self.frames.last_mut().unwrap().ip += 1;

            match self.exec(instr, &closure) {
                Ok(Flow::Continue) => {}
                Ok(Flow::Done(value)) => return Ok(value),
                Err(mut e) => {
                    e.line.get_or_insert(line);
                    return Err(e);
                }
            }
        }
    }

    /// Set the current frame's instruction pointer (used by jumps).
    fn set_ip(&mut self, target: usize) {
        self.frames.last_mut().unwrap().ip = target;
    }

    /// Execute one instruction of the current frame (`closure`), whose ip is
    /// already advanced past it. Returns whether to continue or finish.
    fn exec(&mut self, instr: Instr, closure: &Rc<Closure>) -> Result<Flow, RuntimeError> {
        let chunk = &closure.proto.chunk;
        let base = self.frames.last().unwrap().base;
        match instr {
            Instr::Constant(i) => {
                let v = constant_to_value(&chunk.constants[i as usize]);
                self.push(v);
            }
            Instr::Nil => self.push(Value::Nil),
            Instr::True => self.push(Value::Bool(true)),
            Instr::False => self.push(Value::Bool(false)),
            Instr::Pop => {
                // If the slot being discarded was captured, close its upvalue
                // first so closures keep seeing the value.
                if !self.open_upvalues.is_empty() {
                    self.close_upvalues_from(self.stack.len() - 1);
                }
                self.pop();
            }
            Instr::PopKeepTop(n) => {
                let top = self.pop();
                if !self.open_upvalues.is_empty() {
                    self.close_upvalues_from(self.stack.len() - n as usize);
                }
                for _ in 0..n {
                    self.pop();
                }
                self.push(top);
            }

            Instr::Neg => {
                let v = self.pop();
                let r = match v {
                    Value::Int(n) => Value::Int(n.wrapping_neg()),
                    Value::Float(x) => Value::Float(-x),
                    other => {
                        return Err(RuntimeError::new(format!(
                            "cannot negate {}",
                            other.type_name()
                        )))
                    }
                };
                self.push(r);
            }
            Instr::Not => {
                let v = self.pop();
                self.push(Value::Bool(!v.is_truthy()));
            }
            Instr::Add => self.binary(|a, b| arith(a, b, "+"))?,
            Instr::Sub => self.binary(|a, b| arith(a, b, "-"))?,
            Instr::Mul => self.binary(|a, b| arith(a, b, "*"))?,
            Instr::Div => self.binary(|a, b| arith(a, b, "/"))?,
            Instr::Mod => self.binary(|a, b| arith(a, b, "%"))?,
            Instr::Eq => self.binary(|a, b| Ok(Value::Bool(values_equal(&a, &b))))?,
            Instr::Ne => self.binary(|a, b| Ok(Value::Bool(!values_equal(&a, &b))))?,
            Instr::Lt => self.binary(|a, b| compare(a, b, "<"))?,
            Instr::Le => self.binary(|a, b| compare(a, b, "<="))?,
            Instr::Gt => self.binary(|a, b| compare(a, b, ">"))?,
            Instr::Ge => self.binary(|a, b| compare(a, b, ">="))?,

            Instr::GetLocal(slot) => {
                let v = self.stack[base + slot as usize].clone();
                self.push(v);
            }
            Instr::SetLocal(slot) => {
                // Assignment is an expression: leave the value on the stack.
                self.stack[base + slot as usize] = self.peek(0).clone();
            }
            Instr::GetGlobal(i) => {
                let name = constant_str(chunk, i);
                match self.globals.get(name) {
                    Some(v) => {
                        let v = v.clone();
                        self.push(v);
                    }
                    None => return Err(RuntimeError::new(format!("undefined variable '{name}'"))),
                }
            }
            Instr::SetGlobal(i) => {
                let name = constant_str(chunk, i);
                if !self.globals.contains_key(name) {
                    return Err(RuntimeError::new(format!("undefined variable '{name}'")));
                }
                let v = self.peek(0).clone();
                self.globals.insert(name.to_string(), v);
            }
            Instr::DefineGlobal(i) => {
                let name = constant_str(chunk, i).to_string();
                let v = self.pop();
                self.globals.insert(name, v);
            }

            Instr::Jump(target) => self.set_ip(target),
            Instr::JumpIfFalse(target) => {
                let v = self.pop();
                if !v.is_truthy() {
                    self.set_ip(target);
                }
            }
            Instr::JumpIfFalsePeek(target) => {
                if !self.peek(0).is_truthy() {
                    self.set_ip(target);
                }
            }
            Instr::JumpIfTruePeek(target) => {
                if self.peek(0).is_truthy() {
                    self.set_ip(target);
                }
            }

            Instr::BuildList(n) => {
                let items = self.pop_n(n as usize);
                self.push(Value::list(items));
            }
            Instr::BuildDict(n) => {
                let flat = self.pop_n(2 * n as usize);
                let mut pairs = Vec::with_capacity(n as usize);
                let mut it = flat.into_iter();
                while let (Some(k), Some(v)) = (it.next(), it.next()) {
                    pairs.push((k, v));
                }
                self.push(Value::Dict(std::rc::Rc::new(std::cell::RefCell::new(
                    pairs,
                ))));
            }
            Instr::Index => {
                let index = self.pop();
                let object = self.pop();
                self.push(index_get(&object, &index)?);
            }
            Instr::SetIndex => {
                let value = self.pop();
                let index = self.pop();
                let object = self.pop();
                index_set(&object, &index, value.clone())?;
                self.push(value);
            }
            Instr::MakeRange(inclusive) => {
                let end = self.pop();
                let start = self.pop();
                match (start, end) {
                    (Value::Int(start), Value::Int(end)) => {
                        self.push(Value::Range(crate::value::RangeVal {
                            start,
                            end,
                            inclusive,
                        }));
                    }
                    (a, b) => {
                        return Err(RuntimeError::new(format!(
                            "range bounds must be integers, got {} and {}",
                            a.type_name(),
                            b.type_name()
                        )))
                    }
                }
            }
            Instr::GetIter => {
                let v = self.pop();
                let state = match v {
                    Value::Range(r) => IterState::Range {
                        next: r.start,
                        end: r.end,
                        inclusive: r.inclusive,
                    },
                    Value::List(list) => IterState::List { list, idx: 0 },
                    other => {
                        return Err(RuntimeError::new(format!(
                            "{} is not iterable",
                            other.type_name()
                        )))
                    }
                };
                self.push(Value::Iter(std::rc::Rc::new(std::cell::RefCell::new(
                    state,
                ))));
            }
            Instr::ForIter { slot, exit } => {
                // The iterator lives in a local slot; advance it in place.
                let iter = match &self.stack[base + slot as usize] {
                    Value::Iter(it) => it.clone(),
                    other => {
                        return Err(RuntimeError::new(format!(
                            "for-loop expected an iterator, found {}",
                            other.type_name()
                        )))
                    }
                };
                let next = iter.borrow_mut().next();
                match next {
                    Some(elem) => self.push(elem),
                    None => self.set_ip(exit),
                }
            }
            Instr::BuildStr(n) => {
                let parts = self.pop_n(n as usize);
                let mut s = String::new();
                for p in &parts {
                    use std::fmt::Write;
                    let _ = write!(s, "{p}");
                }
                self.push(Value::str(s));
            }

            Instr::Closure(idx) => self.op_closure(closure, base, idx)?,
            Instr::GetUpvalue(i) => {
                let v = read_upvalue(&closure.upvalues[i as usize], &self.stack);
                self.push(v);
            }
            Instr::SetUpvalue(i) => {
                let v = self.peek(0).clone();
                write_upvalue(&closure.upvalues[i as usize], &mut self.stack, v);
            }
            Instr::CloseUpvalue => {
                // The compiler relies on implicit closing (see `Pop`/`Return`);
                // this is a defensive no-op for an explicit close of the top.
                self.close_upvalues_from(self.stack.len().saturating_sub(1));
            }

            Instr::Call(argc) => return self.op_call(argc as usize),
            Instr::Return => return Ok(self.op_return(base)),
        }
        Ok(Flow::Continue)
    }

    /// Pop two operands (in left/right order), apply `op`, push the result.
    fn binary(
        &mut self,
        op: impl FnOnce(Value, Value) -> Result<Value, RuntimeError>,
    ) -> Result<(), RuntimeError> {
        let b = self.pop();
        let a = self.pop();
        let r = op(a, b)?;
        self.push(r);
        Ok(())
    }

    /// Pop `n` values, returning them in the order they were pushed.
    fn pop_n(&mut self, n: usize) -> Vec<Value> {
        let at = self.stack.len() - n;
        self.stack.split_off(at)
    }

    /// Build a closure from the function constant `idx` of the enclosing closure,
    /// capturing each upvalue either from a local of the current frame or from one
    /// of the enclosing closure's own upvalues.
    fn op_closure(
        &mut self,
        enclosing: &Rc<Closure>,
        base: usize,
        idx: u16,
    ) -> Result<(), RuntimeError> {
        let proto = match &enclosing.proto.chunk.constants[idx as usize] {
            Constant::Function(p) => p.clone(),
            other => unreachable!("Closure operand is not a function: {other:?}"),
        };
        let mut upvalues = Vec::with_capacity(proto.upvalues.len());
        for desc in &proto.upvalues {
            let cell = if desc.from_enclosing_local {
                self.capture_upvalue(base + desc.index as usize)
            } else {
                enclosing.upvalues[desc.index as usize].clone()
            };
            upvalues.push(cell);
        }
        self.push(Value::Closure(Rc::new(Closure { proto, upvalues })));
        Ok(())
    }

    /// Find the open upvalue for stack slot `idx`, or create and register one so
    /// every closure capturing that slot shares the same cell.
    fn capture_upvalue(&mut self, idx: usize) -> Rc<RefCell<Upvalue>> {
        for cell in &self.open_upvalues {
            if matches!(&*cell.borrow(), Upvalue::Open(i) if *i == idx) {
                return cell.clone();
            }
        }
        let cell = Rc::new(RefCell::new(Upvalue::Open(idx)));
        self.open_upvalues.push(cell.clone());
        cell
    }

    /// Close every open upvalue at or above stack index `threshold`, lifting its
    /// value off the (about-to-be-popped) stack into the shared cell.
    fn close_upvalues_from(&mut self, threshold: usize) {
        let mut still_open = Vec::new();
        for cell in std::mem::take(&mut self.open_upvalues) {
            let idx = match &*cell.borrow() {
                Upvalue::Open(i) => *i,
                Upvalue::Closed(_) => continue,
            };
            if idx >= threshold {
                let v = self.stack[idx].clone();
                *cell.borrow_mut() = Upvalue::Closed(v);
            } else {
                still_open.push(cell);
            }
        }
        self.open_upvalues = still_open;
    }

    /// Invoke the callable sitting `argc` slots below the top of the stack with
    /// the `argc` arguments above it. Built-ins run immediately; a user closure
    /// pushes a new frame whose slots reuse the callee and arguments in place.
    fn op_call(&mut self, argc: usize) -> Result<Flow, RuntimeError> {
        let callee_index = self.stack.len() - 1 - argc;
        let callee = self.stack[callee_index].clone();
        match callee {
            Value::Native(native) => {
                if let Some(arity) = native.arity {
                    if arity != argc {
                        return Err(arity_error(native.name, arity, argc));
                    }
                }
                let args = self.pop_n(argc);
                self.pop(); // the callee
                let result = (native.func)(self, &args)?;
                self.push(result);
                Ok(Flow::Continue)
            }
            Value::Closure(closure) => {
                if closure.proto.arity != argc {
                    let name = if closure.proto.name.is_empty() {
                        "<anonymous>"
                    } else {
                        &closure.proto.name
                    };
                    return Err(arity_error(name, closure.proto.arity, argc));
                }
                self.frames.push(CallFrame {
                    closure,
                    ip: 0,
                    base: callee_index,
                });
                Ok(Flow::Continue)
            }
            other => Err(RuntimeError::new(format!(
                "{} is not callable",
                other.type_name()
            ))),
        }
    }

    /// Return from the current frame: take the result, close this frame's
    /// upvalues, discard its stack region, and either finish (the script) or hand
    /// the result back to the caller.
    fn op_return(&mut self, base: usize) -> Flow {
        let result = self.pop();
        self.close_upvalues_from(base);
        self.stack.truncate(base);
        self.frames.pop();
        if self.frames.is_empty() {
            Flow::Done(result)
        } else {
            self.push(result);
            Flow::Continue
        }
    }
}

/// Read an upvalue cell: from the stack while open, from the cell once closed.
fn read_upvalue(cell: &Rc<RefCell<Upvalue>>, stack: &[Value]) -> Value {
    match &*cell.borrow() {
        Upvalue::Open(idx) => stack[*idx].clone(),
        Upvalue::Closed(v) => v.clone(),
    }
}

/// Write through an upvalue cell to wherever the variable currently lives.
fn write_upvalue(cell: &Rc<RefCell<Upvalue>>, stack: &mut [Value], value: Value) {
    let mut slot = cell.borrow_mut();
    match &mut *slot {
        Upvalue::Open(idx) => stack[*idx] = value,
        Upvalue::Closed(v) => *v = value,
    }
}

/// A uniform arity-mismatch error for both native and user functions.
fn arity_error(name: &str, expected: usize, got: usize) -> RuntimeError {
    RuntimeError::new(format!(
        "{name} expects {expected} argument(s), but got {got}"
    ))
}

// --- constant / chunk decoding ----------------------------------------------

fn constant_to_value(c: &Constant) -> Value {
    match c {
        Constant::Int(n) => Value::Int(*n),
        Constant::Float(x) => Value::Float(*x),
        Constant::Str(s) => Value::str(s),
        // Functions become closures via the `Closure` instruction, never `Constant`.
        Constant::Function(_) => {
            unreachable!("function constants are instantiated by the Closure instruction")
        }
    }
}

/// Read a constant known to be a string (global names always are).
fn constant_str(chunk: &Chunk, idx: u16) -> &str {
    match &chunk.constants[idx as usize] {
        Constant::Str(s) => s,
        other => unreachable!("expected a name constant, found {other:?}"),
    }
}

// --- operators ---------------------------------------------------------------

/// Arithmetic for `+ - * / %`. Two ints stay an int (integer division/modulo);
/// any float promotes to float; `+` also concatenates two strings.
fn arith(a: Value, b: Value, op: &str) -> Result<Value, RuntimeError> {
    use Value::{Float, Int, Str};
    match (&a, &b) {
        (Int(x), Int(y)) => {
            let r = match op {
                "+" => x.wrapping_add(*y),
                "-" => x.wrapping_sub(*y),
                "*" => x.wrapping_mul(*y),
                "/" => {
                    if *y == 0 {
                        return Err(RuntimeError::new("division by zero"));
                    }
                    x.wrapping_div(*y)
                }
                "%" => {
                    if *y == 0 {
                        return Err(RuntimeError::new("modulo by zero"));
                    }
                    x.wrapping_rem(*y)
                }
                _ => unreachable!(),
            };
            Ok(Int(r))
        }
        (Str(x), Str(y)) if op == "+" => Ok(Value::str(format!("{x}{y}"))),
        _ => match (as_f64(&a), as_f64(&b)) {
            (Some(x), Some(y)) => {
                let r = match op {
                    "+" => x + y,
                    "-" => x - y,
                    "*" => x * y,
                    "/" => x / y,
                    "%" => x % y,
                    _ => unreachable!(),
                };
                Ok(Float(r))
            }
            _ => Err(RuntimeError::new(format!(
                "cannot apply '{op}' to {} and {}",
                a.type_name(),
                b.type_name()
            ))),
        },
    }
}

/// Ordering comparisons `< <= > >=` over numbers and strings.
fn compare(a: Value, b: Value, op: &str) -> Result<Value, RuntimeError> {
    use std::cmp::Ordering;
    let ord = match (&a, &b) {
        (Value::Int(x), Value::Int(y)) => x.cmp(y),
        (Value::Str(x), Value::Str(y)) => x.cmp(y),
        _ => match (as_f64(&a), as_f64(&b)) {
            (Some(x), Some(y)) => x
                .partial_cmp(&y)
                .ok_or_else(|| RuntimeError::new("cannot compare NaN"))?,
            _ => {
                return Err(RuntimeError::new(format!(
                    "cannot compare {} and {}",
                    a.type_name(),
                    b.type_name()
                )))
            }
        },
    };
    let result = match op {
        "<" => ord == Ordering::Less,
        "<=" => ord != Ordering::Greater,
        ">" => ord == Ordering::Greater,
        ">=" => ord != Ordering::Less,
        _ => unreachable!(),
    };
    Ok(Value::Bool(result))
}

/// View a numeric value as `f64` for mixed-type arithmetic/comparison.
fn as_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Int(n) => Some(*n as f64),
        Value::Float(x) => Some(*x),
        _ => None,
    }
}

// --- indexing ----------------------------------------------------------------

fn index_get(object: &Value, index: &Value) -> Result<Value, RuntimeError> {
    match (object, index) {
        (Value::List(list), Value::Int(i)) => {
            let list = list.borrow();
            list_index(list.len(), *i)
                .map(|i| list[i].clone())
                .ok_or_else(|| RuntimeError::new(format!("list index {i} out of range")))
        }
        (Value::Str(s), Value::Int(i)) => {
            let chars: Vec<char> = s.chars().collect();
            list_index(chars.len(), *i)
                .map(|i| Value::str(chars[i].to_string()))
                .ok_or_else(|| RuntimeError::new(format!("string index {i} out of range")))
        }
        (Value::Dict(pairs), key) => Ok(pairs
            .borrow()
            .iter()
            .find(|(k, _)| values_equal(k, key))
            .map(|(_, v)| v.clone())
            .unwrap_or(Value::Nil)),
        _ => Err(RuntimeError::new(format!(
            "cannot index {} with {}",
            object.type_name(),
            index.type_name()
        ))),
    }
}

fn index_set(object: &Value, index: &Value, value: Value) -> Result<(), RuntimeError> {
    match (object, index) {
        (Value::List(list), Value::Int(i)) => {
            let mut list = list.borrow_mut();
            match list_index(list.len(), *i) {
                Some(i) => {
                    list[i] = value;
                    Ok(())
                }
                None => Err(RuntimeError::new(format!("list index {i} out of range"))),
            }
        }
        (Value::Dict(pairs), key) => {
            let mut pairs = pairs.borrow_mut();
            if let Some(slot) = pairs.iter_mut().find(|(k, _)| values_equal(k, key)) {
                slot.1 = value;
            } else {
                pairs.push((key.clone(), value));
            }
            Ok(())
        }
        _ => Err(RuntimeError::new(format!(
            "cannot assign to index of {}",
            object.type_name()
        ))),
    }
}

/// Translate a (non-negative) Lumen index into a bounds-checked Rust index.
fn list_index(len: usize, i: i64) -> Option<usize> {
    if i >= 0 && (i as usize) < len {
        Some(i as usize)
    } else {
        None
    }
}

// --- built-in functions ------------------------------------------------------

/// Map a built-in's name to its implementation. Every entry in
/// `lumen_common::BUILTINS` must have a case here.
fn builtin_fn(name: &str) -> crate::value::NativeFn {
    match name {
        "print" => bi_print,
        "len" => bi_len,
        "type" => bi_type,
        "str" => bi_str,
        "sqrt" => bi_sqrt,
        "abs" => bi_abs,
        "floor" => bi_floor,
        "push" => bi_push,
        "pop" => bi_pop,
        "keys" => bi_keys,
        "values" => bi_values,
        "error" => bi_error,
        "clock" => bi_clock,
        "min" => bi_min,
        "max" => bi_max,
        "int" => bi_int,
        "float" => bi_float,
        other => unreachable!("no implementation for built-in '{other}'"),
    }
}

fn bi_print(vm: &mut Vm, args: &[Value]) -> Result<Value, RuntimeError> {
    let line = args
        .iter()
        .map(|v| v.to_string())
        .collect::<Vec<_>>()
        .join(" ");
    vm.output.push_str(&line);
    vm.output.push('\n');
    Ok(Value::Nil)
}

fn bi_len(_: &mut Vm, args: &[Value]) -> Result<Value, RuntimeError> {
    let n = match &args[0] {
        Value::Str(s) => s.chars().count(),
        Value::List(l) => l.borrow().len(),
        Value::Dict(d) => d.borrow().len(),
        other => {
            return Err(RuntimeError::new(format!(
                "len() expects a string, list or dict, got {}",
                other.type_name()
            )))
        }
    };
    Ok(Value::Int(n as i64))
}

fn bi_type(_: &mut Vm, args: &[Value]) -> Result<Value, RuntimeError> {
    Ok(Value::str(args[0].type_name()))
}

fn bi_str(_: &mut Vm, args: &[Value]) -> Result<Value, RuntimeError> {
    Ok(Value::str(args[0].to_string()))
}

fn bi_sqrt(_: &mut Vm, args: &[Value]) -> Result<Value, RuntimeError> {
    match as_f64(&args[0]) {
        Some(x) => Ok(Value::Float(x.sqrt())),
        None => Err(RuntimeError::new(format!(
            "sqrt() expects a number, got {}",
            args[0].type_name()
        ))),
    }
}

fn bi_abs(_: &mut Vm, args: &[Value]) -> Result<Value, RuntimeError> {
    match &args[0] {
        Value::Int(n) => Ok(Value::Int(n.wrapping_abs())),
        Value::Float(x) => Ok(Value::Float(x.abs())),
        other => Err(RuntimeError::new(format!(
            "abs() expects a number, got {}",
            other.type_name()
        ))),
    }
}

fn bi_floor(_: &mut Vm, args: &[Value]) -> Result<Value, RuntimeError> {
    match &args[0] {
        Value::Int(n) => Ok(Value::Int(*n)),
        Value::Float(x) => Ok(Value::Int(x.floor() as i64)),
        other => Err(RuntimeError::new(format!(
            "floor() expects a number, got {}",
            other.type_name()
        ))),
    }
}

fn bi_push(_: &mut Vm, args: &[Value]) -> Result<Value, RuntimeError> {
    match &args[0] {
        Value::List(list) => {
            list.borrow_mut().push(args[1].clone());
            Ok(Value::Nil)
        }
        other => Err(RuntimeError::new(format!(
            "push() expects a list, got {}",
            other.type_name()
        ))),
    }
}

fn bi_pop(_: &mut Vm, args: &[Value]) -> Result<Value, RuntimeError> {
    match &args[0] {
        Value::List(list) => Ok(list.borrow_mut().pop().unwrap_or(Value::Nil)),
        other => Err(RuntimeError::new(format!(
            "pop() expects a list, got {}",
            other.type_name()
        ))),
    }
}

fn bi_keys(_: &mut Vm, args: &[Value]) -> Result<Value, RuntimeError> {
    match &args[0] {
        Value::Dict(d) => Ok(Value::list(
            d.borrow().iter().map(|(k, _)| k.clone()).collect(),
        )),
        other => Err(RuntimeError::new(format!(
            "keys() expects a dict, got {}",
            other.type_name()
        ))),
    }
}

fn bi_values(_: &mut Vm, args: &[Value]) -> Result<Value, RuntimeError> {
    match &args[0] {
        Value::Dict(d) => Ok(Value::list(
            d.borrow().iter().map(|(_, v)| v.clone()).collect(),
        )),
        other => Err(RuntimeError::new(format!(
            "values() expects a dict, got {}",
            other.type_name()
        ))),
    }
}

fn bi_error(_: &mut Vm, args: &[Value]) -> Result<Value, RuntimeError> {
    Err(RuntimeError::new(args[0].to_string()))
}

fn bi_clock(_: &mut Vm, _args: &[Value]) -> Result<Value, RuntimeError> {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    Ok(Value::Float(secs))
}

fn bi_min(_: &mut Vm, args: &[Value]) -> Result<Value, RuntimeError> {
    match compare(args[0].clone(), args[1].clone(), "<")? {
        Value::Bool(true) => Ok(args[0].clone()),
        _ => Ok(args[1].clone()),
    }
}

fn bi_max(_: &mut Vm, args: &[Value]) -> Result<Value, RuntimeError> {
    match compare(args[0].clone(), args[1].clone(), ">")? {
        Value::Bool(true) => Ok(args[0].clone()),
        _ => Ok(args[1].clone()),
    }
}

fn bi_int(_: &mut Vm, args: &[Value]) -> Result<Value, RuntimeError> {
    match &args[0] {
        Value::Int(n) => Ok(Value::Int(*n)),
        Value::Float(x) => Ok(Value::Int(*x as i64)),
        Value::Str(s) => s
            .trim()
            .parse::<i64>()
            .map(Value::Int)
            .map_err(|_| RuntimeError::new(format!("cannot convert {s:?} to int"))),
        other => Err(RuntimeError::new(format!(
            "int() expects a number or string, got {}",
            other.type_name()
        ))),
    }
}

fn bi_float(_: &mut Vm, args: &[Value]) -> Result<Value, RuntimeError> {
    match &args[0] {
        Value::Int(n) => Ok(Value::Float(*n as f64)),
        Value::Float(x) => Ok(Value::Float(*x)),
        Value::Str(s) => s
            .trim()
            .parse::<f64>()
            .map(Value::Float)
            .map_err(|_| RuntimeError::new(format!("cannot convert {s:?} to float"))),
        other => Err(RuntimeError::new(format!(
            "float() expects a number or string, got {}",
            other.type_name()
        ))),
    }
}
