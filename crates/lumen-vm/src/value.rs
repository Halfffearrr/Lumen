//! Runtime values — what the VM pushes and pops on its stack.
//!
//! Lumen is dynamically typed, so a single [`Value`] enum spans every type the
//! language has. Scalars (`Int`, `Float`, `Bool`, `Nil`) are stored inline;
//! everything with identity or variable size (`Str`, `List`, `Dict`, iterators)
//! lives behind an [`Rc`] so values are cheap to clone and share. Interior
//! mutability ([`RefCell`]) lets `list`/`dict` be modified through any handle —
//! which is exactly the aliasing a real heap object has. (Stage 4 replaces this
//! `Rc<RefCell<_>>` scheme with a tracing GC.)

use std::cell::RefCell;
use std::fmt;
use std::rc::Rc;

use crate::vm::{RuntimeError, Vm};

/// The signature of a built-in (native) function: it receives the VM (so e.g.
/// `print` can reach the output buffer) and its already-evaluated arguments.
pub type NativeFn = fn(&mut Vm, &[Value]) -> Result<Value, RuntimeError>;

/// A built-in function value.
#[derive(Clone, Copy)]
pub struct Native {
    pub name: &'static str,
    /// Fixed arity, or `None` for variadic (e.g. `print`). Mirrors the table in
    /// `lumen-common` that the resolver also checks against.
    pub arity: Option<usize>,
    pub func: NativeFn,
}

impl fmt::Debug for Native {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<native fn {}>", self.name)
    }
}

/// An inclusive-or-exclusive integer range, produced by `a..b` / `a..=b`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RangeVal {
    pub start: i64,
    pub end: i64,
    pub inclusive: bool,
}

/// The mutable state backing a `for`-loop iterator.
#[derive(Debug)]
pub enum IterState {
    Range {
        next: i64,
        end: i64,
        inclusive: bool,
    },
    List {
        list: Rc<RefCell<Vec<Value>>>,
        idx: usize,
    },
}

impl IterState {
    /// Advance the iterator, yielding the next element or `None` when exhausted.
    pub fn next(&mut self) -> Option<Value> {
        match self {
            IterState::Range {
                next,
                end,
                inclusive,
            } => {
                let more = if *inclusive {
                    *next <= *end
                } else {
                    *next < *end
                };
                if !more {
                    return None;
                }
                let v = Value::Int(*next);
                *next += 1;
                Some(v)
            }
            IterState::List { list, idx } => {
                let borrowed = list.borrow();
                if *idx < borrowed.len() {
                    let v = borrowed[*idx].clone();
                    *idx += 1;
                    Some(v)
                } else {
                    None
                }
            }
        }
    }
}

/// A Lumen runtime value.
#[derive(Debug, Clone)]
pub enum Value {
    Int(i64),
    Float(f64),
    Bool(bool),
    Nil,
    Str(Rc<str>),
    List(Rc<RefCell<Vec<Value>>>),
    /// A dictionary, kept as ordered key/value pairs. Linear lookup is plenty for
    /// a teaching language and preserves insertion order for `keys`/`values`.
    Dict(Rc<RefCell<Vec<(Value, Value)>>>),
    Range(RangeVal),
    Iter(Rc<RefCell<IterState>>),
    Native(Native),
}

impl Value {
    /// Wrap a Rust string into a Lumen string value.
    pub fn str(s: impl AsRef<str>) -> Value {
        Value::Str(Rc::from(s.as_ref()))
    }

    /// Wrap a vector into a Lumen list value.
    pub fn list(items: Vec<Value>) -> Value {
        Value::List(Rc::new(RefCell::new(items)))
    }

    /// Lumen truthiness: only `nil` and `false` are falsey; everything else
    /// (including `0` and `""`) is truthy, matching the language's Lua-like rule.
    pub fn is_truthy(&self) -> bool {
        !matches!(self, Value::Nil | Value::Bool(false))
    }

    /// The type name shown by the `type` built-in and in error messages.
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Int(_) => "int",
            Value::Float(_) => "float",
            Value::Bool(_) => "bool",
            Value::Nil => "nil",
            Value::Str(_) => "str",
            Value::List(_) => "list",
            Value::Dict(_) => "dict",
            Value::Range(_) => "range",
            Value::Iter(_) => "iterator",
            Value::Native(_) => "function",
        }
    }
}

/// Structural value equality used by `==` / `!=`. Numeric `Int`/`Float` compare
/// by mathematical value; collections compare element-wise; unlike types are
/// never equal.
pub fn values_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => x == y,
        (Value::Float(x), Value::Float(y)) => x == y,
        (Value::Int(x), Value::Float(y)) => (*x as f64) == *y,
        (Value::Float(x), Value::Int(y)) => *x == (*y as f64),
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Nil, Value::Nil) => true,
        (Value::Str(x), Value::Str(y)) => x == y,
        (Value::Range(x), Value::Range(y)) => x == y,
        (Value::List(x), Value::List(y)) => {
            let (x, y) = (x.borrow(), y.borrow());
            x.len() == y.len() && x.iter().zip(y.iter()).all(|(a, b)| values_equal(a, b))
        }
        (Value::Dict(x), Value::Dict(y)) => {
            let (x, y) = (x.borrow(), y.borrow());
            x.len() == y.len()
                && x.iter()
                    .zip(y.iter())
                    .all(|((ka, va), (kb, vb))| values_equal(ka, kb) && values_equal(va, vb))
        }
        _ => false,
    }
}

impl fmt::Display for Value {
    /// User-facing rendering (what `print` and `str` produce). Strings print as
    /// their bare contents at the top level, but are quoted when nested inside a
    /// list or dict so the structure stays unambiguous.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Int(n) => write!(f, "{n}"),
            Value::Float(x) => {
                // Render whole floats as `2.0`, not `2`, so they read as floats.
                if x.is_finite() && *x == x.trunc() {
                    write!(f, "{x:.1}")
                } else {
                    write!(f, "{x}")
                }
            }
            Value::Bool(b) => write!(f, "{b}"),
            Value::Nil => write!(f, "nil"),
            Value::Str(s) => write!(f, "{s}"),
            Value::Range(r) => {
                let op = if r.inclusive { "..=" } else { ".." };
                write!(f, "{}{op}{}", r.start, r.end)
            }
            Value::List(items) => {
                let items = items.borrow();
                write!(f, "[")?;
                for (i, v) in items.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", Repr(v))?;
                }
                write!(f, "]")
            }
            Value::Dict(pairs) => {
                let pairs = pairs.borrow();
                write!(f, "{{")?;
                for (i, (k, v)) in pairs.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}: {}", Repr(k), Repr(v))?;
                }
                write!(f, "}}")
            }
            Value::Iter(_) => write!(f, "<iterator>"),
            Value::Native(n) => write!(f, "<fn {}>", n.name),
        }
    }
}

/// Wrapper that renders a value the way it should appear *inside* a collection:
/// identical to `Display` except strings get quotes.
struct Repr<'a>(&'a Value);

impl fmt::Display for Repr<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Value::Str(s) => write!(f, "{s:?}"),
            other => write!(f, "{other}"),
        }
    }
}
