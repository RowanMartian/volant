//! The interpreter: values, objects with prototypes, scopes, calls, and the
//! evaluation of every statement and expression the parser produces. It walks
//! the syntax tree directly; a step budget stops runaway scripts.

use crate::parse::{Expr, Func, Member, Prop, Stmt};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

pub type Obj = Rc<RefCell<Object>>;

#[derive(Clone)]
pub enum Value {
    Undefined,
    Null,
    Bool(bool),
    Num(f64),
    Str(Rc<str>),
    Obj(Obj),
}

impl std::fmt::Debug for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Value::Undefined => write!(f, "undefined"),
            Value::Null => write!(f, "null"),
            Value::Bool(b) => write!(f, "{b}"),
            Value::Num(n) => write!(f, "{}", num_to_string(*n)),
            Value::Str(s) => write!(f, "{s:?}"),
            Value::Obj(_) => write!(f, "[object]"),
        }
    }
}

pub type Native = Rc<dyn Fn(&mut Interp, Value, Vec<Value>) -> Res<Value>>;

pub enum Callable {
    Closure { func: Rc<Func>, scope: Scope, this: Option<Value>, sup: Option<Value> },
    Native(Native),
}

pub enum Kind {
    Plain,
    Array(Vec<Value>),
    Function(Callable),
    Error,
    /// Map and Set: entries in insertion order (a Set's values are its keys).
    Map(Vec<(Value, Value)>),
    Set(Vec<Value>),
    Date(f64),
    /// A primitive in an object (`new String("x")`).
    Boxed(Value),
}

#[derive(Clone)]
pub enum Slot {
    Data(Value),
    Accessor(Option<Value>, Option<Value>),
}

pub struct Object {
    keys: Vec<Rc<str>>,
    map: HashMap<Rc<str>, Slot>,
    pub proto: Option<Obj>,
    pub kind: Kind,
}

impl Object {
    pub fn new(proto: Option<Obj>, kind: Kind) -> Self {
        Object { keys: Vec::new(), map: HashMap::new(), proto, kind }
    }
    pub fn get_own(&self, k: &str) -> Option<&Slot> {
        self.map.get(k)
    }
    pub fn set_own(&mut self, k: &str, v: Value) {
        if let Some(slot) = self.map.get_mut(k) {
            *slot = Slot::Data(v);
        } else {
            let k: Rc<str> = k.into();
            self.keys.push(k.clone());
            self.map.insert(k, Slot::Data(v));
        }
    }
    pub fn set_slot(&mut self, k: &str, s: Slot) {
        if !self.map.contains_key(k) {
            self.keys.push(k.into());
        }
        self.map.insert(k.into(), s);
    }
    pub fn delete(&mut self, k: &str) -> bool {
        if self.map.remove(k).is_some() {
            self.keys.retain(|x| &**x != k);
        }
        true
    }
    /// Own enumerable keys, in order (array indices first).
    pub fn own_keys(&self) -> Vec<Rc<str>> {
        let mut out: Vec<Rc<str>> = match &self.kind {
            Kind::Array(a) => (0..a.len()).map(|i| i.to_string().into()).collect(),
            Kind::Boxed(Value::Str(s)) => (0..s.chars().count()).map(|i| i.to_string().into()).collect(),
            _ => Vec::new(),
        };
        out.extend(self.keys.iter().filter(|k| !k.starts_with('\0')).cloned());
        out
    }
}

pub type Res<T> = Result<T, Value>;

pub enum Flow {
    Normal,
    Return(Value),
    Break(Option<Rc<str>>),
    Continue(Option<Rc<str>>),
}

/// The hidden property holding a class's instance-field initialiser.
const FIELDS: &str = "\0fields";

pub struct ScopeData {
    vars: HashMap<Rc<str>, (Value, bool)>,
    parent: Option<Scope>,
    /// `this` (and `arguments`) for a function scope; arrows look outward.
    this: Option<Value>,
}

pub type Scope = Rc<RefCell<ScopeData>>;

fn scope(parent: Option<&Scope>, _func: bool, this: Option<Value>) -> Scope {
    Rc::new(RefCell::new(ScopeData { vars: HashMap::new(), parent: parent.cloned(), this }))
}

/// How a number prints in JavaScript.
pub fn num_to_string(n: f64) -> String {
    if n.is_nan() {
        return "NaN".into();
    }
    if n.is_infinite() {
        return if n > 0.0 { "Infinity" } else { "-Infinity" }.into();
    }
    if n == 0.0 {
        return "0".into();
    }
    if n.fract() == 0.0 && n.abs() < 1e21 {
        return format!("{}", n as i128);
    }
    let a = n.abs();
    if !(1e-6..1e21).contains(&a) {
        let s = format!("{n:e}");
        return match s.split_once('e') {
            Some((m, e)) if !e.starts_with('-') => format!("{m}e+{e}"),
            _ => s,
        };
    }
    format!("{n}")
}

pub struct Interp {
    pub global: Scope,
    pub global_obj: Obj,
    pub object_proto: Obj,
    pub function_proto: Obj,
    pub array_proto: Obj,
    pub string_proto: Obj,
    pub number_proto: Obj,
    pub boolean_proto: Obj,
    pub error_proto: Obj,
    pub map_proto: Obj,
    pub set_proto: Obj,
    pub date_proto: Obj,
    /// What `console.log` printed.
    pub output: Vec<String>,
    /// Callbacks from setTimeout and friends, run after the script.
    pub tasks: Vec<(Value, Vec<Value>)>,
    steps: u64,
    pub max_steps: u64,
    depth: u32,
}

impl Default for Interp {
    fn default() -> Self {
        Self::new()
    }
}

impl Interp {
    pub fn new() -> Self {
        let object_proto = Rc::new(RefCell::new(Object::new(None, Kind::Plain)));
        let mk = |kind: Kind| Rc::new(RefCell::new(Object::new(Some(object_proto.clone()), kind)));
        let function_proto = mk(Kind::Plain);
        let global_obj = mk(Kind::Plain);
        let mut i = Interp {
            global: scope(None, true, Some(Value::Undefined)),
            array_proto: mk(Kind::Array(Vec::new())),
            string_proto: mk(Kind::Plain),
            number_proto: mk(Kind::Plain),
            boolean_proto: mk(Kind::Plain),
            error_proto: mk(Kind::Plain),
            map_proto: mk(Kind::Plain),
            set_proto: mk(Kind::Plain),
            date_proto: mk(Kind::Plain),
            global_obj,
            object_proto,
            function_proto,
            output: Vec::new(),
            tasks: Vec::new(),
            steps: 0,
            max_steps: 50_000_000,
            depth: 0,
        };
        crate::builtins::install(&mut i);
        i
    }

    // ------------------------------------------------------------ values

    pub fn obj(&self, kind: Kind) -> Value {
        let proto = match &kind {
            Kind::Array(_) => self.array_proto.clone(),
            Kind::Function(_) => self.function_proto.clone(),
            Kind::Error => self.error_proto.clone(),
            Kind::Map(_) => self.map_proto.clone(),
            Kind::Set(_) => self.set_proto.clone(),
            Kind::Date(_) => self.date_proto.clone(),
            _ => self.object_proto.clone(),
        };
        Value::Obj(Rc::new(RefCell::new(Object::new(Some(proto), kind))))
    }

    pub fn array(&self, items: Vec<Value>) -> Value {
        self.obj(Kind::Array(items))
    }

    pub fn native(&self, f: impl Fn(&mut Interp, Value, Vec<Value>) -> Res<Value> + 'static) -> Value {
        self.obj(Kind::Function(Callable::Native(Rc::new(f))))
    }

    pub fn error(&self, name: &str, msg: &str) -> Value {
        let e = self.obj(Kind::Error);
        if let Value::Obj(o) = &e {
            let mut o = o.borrow_mut();
            o.set_own("name", Value::Str(name.into()));
            o.set_own("message", Value::Str(msg.into()));
        }
        e
    }

    pub fn throw<T>(&self, name: &str, msg: &str) -> Res<T> {
        Err(self.error(name, msg))
    }

    pub fn truthy(v: &Value) -> bool {
        match v {
            Value::Undefined | Value::Null => false,
            Value::Bool(b) => *b,
            Value::Num(n) => *n != 0.0 && !n.is_nan(),
            Value::Str(s) => !s.is_empty(),
            Value::Obj(_) => true,
        }
    }

    pub fn typeof_(v: &Value) -> &'static str {
        match v {
            Value::Undefined => "undefined",
            Value::Null => "object",
            Value::Bool(_) => "boolean",
            Value::Num(_) => "number",
            Value::Str(_) => "string",
            Value::Obj(o) => {
                if matches!(o.borrow().kind, Kind::Function(_)) {
                    "function"
                } else {
                    "object"
                }
            }
        }
    }

    pub fn is_callable(v: &Value) -> bool {
        matches!(v, Value::Obj(o) if matches!(o.borrow().kind, Kind::Function(_)))
    }

    /// ToPrimitive, preferring strings (`valueOf` for numbers first when `number`).
    pub fn to_primitive(&mut self, v: &Value, number: bool) -> Res<Value> {
        let Value::Obj(o) = v else { return Ok(v.clone()) };
        if let Kind::Boxed(p) = &o.borrow().kind {
            return Ok(p.clone());
        }
        if let Kind::Date(t) = &o.borrow().kind {
            return Ok(if number { Value::Num(*t) } else { Value::Str(crate::builtins::date_string(*t).into()) });
        }
        let order = if number { ["valueOf", "toString"] } else { ["toString", "valueOf"] };
        for m in order {
            let f = self.get(v, m)?;
            if Self::is_callable(&f) {
                let r = self.call(&f, v.clone(), vec![])?;
                if !matches!(r, Value::Obj(_)) {
                    return Ok(r);
                }
            }
        }
        self.throw("TypeError", "can't convert object to a primitive")
    }

    pub fn to_number(&mut self, v: &Value) -> Res<f64> {
        Ok(match v {
            Value::Undefined => f64::NAN,
            Value::Null => 0.0,
            Value::Bool(b) => *b as u8 as f64,
            Value::Num(n) => *n,
            Value::Str(s) => str_to_number(s),
            Value::Obj(_) => {
                let p = self.to_primitive(v, true)?;
                self.to_number(&p)?
            }
        })
    }

    pub fn to_str(&mut self, v: &Value) -> Res<Rc<str>> {
        Ok(match v {
            Value::Undefined => "undefined".into(),
            Value::Null => "null".into(),
            Value::Bool(b) => if *b { "true" } else { "false" }.into(),
            Value::Num(n) => num_to_string(*n).into(),
            Value::Str(s) => s.clone(),
            Value::Obj(_) => {
                let p = self.to_primitive(v, false)?;
                self.to_str(&p)?
            }
        })
    }

    /// A property key from any value.
    pub fn key(&mut self, v: &Value) -> Res<Rc<str>> {
        match v {
            Value::Num(n) if n.fract() == 0.0 && *n >= 0.0 && *n < 4294967295.0 => Ok((*n as u64).to_string().into()),
            _ => self.to_str(v),
        }
    }

    pub fn strict_eq(a: &Value, b: &Value) -> bool {
        match (a, b) {
            (Value::Undefined, Value::Undefined) | (Value::Null, Value::Null) => true,
            (Value::Bool(x), Value::Bool(y)) => x == y,
            (Value::Num(x), Value::Num(y)) => x == y,
            (Value::Str(x), Value::Str(y)) => x == y,
            (Value::Obj(x), Value::Obj(y)) => Rc::ptr_eq(x, y),
            _ => false,
        }
    }

    /// SameValueZero (Map keys, includes()): like === but NaN equals NaN.
    pub fn same_value_zero(a: &Value, b: &Value) -> bool {
        match (a, b) {
            (Value::Num(x), Value::Num(y)) if x.is_nan() && y.is_nan() => true,
            _ => Self::strict_eq(a, b),
        }
    }

    pub fn loose_eq(&mut self, a: &Value, b: &Value) -> Res<bool> {
        Ok(match (a, b) {
            (Value::Undefined | Value::Null, Value::Undefined | Value::Null) => true,
            (Value::Undefined | Value::Null, _) | (_, Value::Undefined | Value::Null) => false,
            (Value::Num(_), Value::Str(_)) | (Value::Str(_), Value::Num(_)) | (Value::Bool(_), _) | (_, Value::Bool(_)) => {
                if let (Value::Obj(_), _) | (_, Value::Obj(_)) = (a, b) {
                    let (pa, pb) = (self.to_primitive(a, true)?, self.to_primitive(b, true)?);
                    return self.loose_eq(&pa, &pb);
                }
                self.to_number(a)? == self.to_number(b)?
            }
            (Value::Obj(_), Value::Obj(_)) => Self::strict_eq(a, b),
            (Value::Obj(_), _) => {
                let p = self.to_primitive(a, true)?;
                self.loose_eq(&p, b)?
            }
            (_, Value::Obj(_)) => {
                let p = self.to_primitive(b, true)?;
                self.loose_eq(a, &p)?
            }
            _ => Self::strict_eq(a, b),
        })
    }

    // ------------------------------------------------------------ properties

    fn proto_of(&self, v: &Value) -> Option<Obj> {
        match v {
            Value::Str(_) => Some(self.string_proto.clone()),
            Value::Num(_) => Some(self.number_proto.clone()),
            Value::Bool(_) => Some(self.boolean_proto.clone()),
            Value::Obj(o) => Some(o.clone()),
            _ => None,
        }
    }

    pub fn get(&mut self, v: &Value, k: &str) -> Res<Value> {
        match v {
            Value::Undefined | Value::Null => return self.throw("TypeError", &format!("can't read '{k}' of {}", if matches!(v, Value::Null) { "null" } else { "undefined" })),
            Value::Str(s) => {
                if k == "length" {
                    return Ok(Value::Num(s.encode_utf16().count() as f64));
                }
                if let Ok(i) = k.parse::<usize>() {
                    return Ok(s.chars().nth(i).map(|c| Value::Str(c.to_string().into())).unwrap_or(Value::Undefined));
                }
            }
            Value::Obj(o) => {
                let b = o.borrow();
                match &b.kind {
                    Kind::Array(a) => {
                        if k == "length" {
                            return Ok(Value::Num(a.len() as f64));
                        }
                        if let Ok(i) = k.parse::<usize>() {
                            return Ok(a.get(i).cloned().unwrap_or(Value::Undefined));
                        }
                    }
                    Kind::Map(m) if k == "size" => return Ok(Value::Num(m.len() as f64)),
                    Kind::Set(s) if k == "size" => return Ok(Value::Num(s.len() as f64)),
                    Kind::Boxed(Value::Str(s)) if k == "length" => return Ok(Value::Num(s.encode_utf16().count() as f64)),
                    _ => {}
                }
            }
            _ => {}
        }
        let mut cur = self.proto_of(v);
        // A primitive's own "properties" start at its prototype.
        let mut steps = 0;
        while let Some(o) = cur {
            steps += 1;
            if steps > 1000 {
                break;
            }
            let slot = o.borrow().get_own(k).cloned();
            match slot {
                Some(Slot::Data(val)) => return Ok(val),
                Some(Slot::Accessor(getter, _)) => {
                    return match getter {
                        Some(g) => self.call(&g, v.clone(), vec![]),
                        None => Ok(Value::Undefined),
                    }
                }
                None => {}
            }
            if k == "__proto__" {
                return Ok(o.borrow().proto.clone().map(Value::Obj).unwrap_or(Value::Null));
            }
            cur = o.borrow().proto.clone();
        }
        Ok(Value::Undefined)
    }

    pub fn set(&mut self, v: &Value, k: &str, val: Value) -> Res<()> {
        let Value::Obj(o) = v else {
            if matches!(v, Value::Undefined | Value::Null) {
                return self.throw("TypeError", &format!("can't set '{k}' of {v:?}"));
            }
            return Ok(()); // setting on a primitive does nothing
        };
        {
            let mut b = o.borrow_mut();
            if let Kind::Array(a) = &mut b.kind {
                if k == "length" {
                    let n = match &val {
                        Value::Num(n) if *n >= 0.0 && n.fract() == 0.0 => *n as usize,
                        _ => return Err(Value::Str("RangeError: invalid array length".into())),
                    };
                    a.resize(n, Value::Undefined);
                    return Ok(());
                }
                if let Ok(i) = k.parse::<usize>() {
                    if i < 10_000_000 {
                        if i >= a.len() {
                            a.resize(i + 1, Value::Undefined);
                        }
                        a[i] = val;
                        return Ok(());
                    }
                }
            }
        }
        if k == "__proto__" {
            if let Value::Obj(p) = &val {
                o.borrow_mut().proto = Some(p.clone());
            } else if matches!(val, Value::Null) {
                o.borrow_mut().proto = None;
            }
            return Ok(());
        }
        // A setter anywhere up the chain runs; otherwise it's an own data property.
        let mut cur = Some(o.clone());
        while let Some(c) = cur {
            let slot = c.borrow().get_own(k).cloned();
            if let Some(Slot::Accessor(_, setter)) = slot {
                if let Some(s) = setter {
                    self.call(&s, v.clone(), vec![val])?;
                }
                return Ok(());
            }
            cur = c.borrow().proto.clone();
        }
        o.borrow_mut().set_own(k, val);
        Ok(())
    }

    pub fn has_property(&self, o: &Obj, k: &str) -> bool {
        if let Kind::Array(a) = &o.borrow().kind {
            if k == "length" || k.parse::<usize>().map(|i| i < a.len()).unwrap_or(false) {
                return true;
            }
        }
        let mut cur = Some(o.clone());
        while let Some(c) = cur {
            if c.borrow().get_own(k).is_some() {
                return true;
            }
            cur = c.borrow().proto.clone();
        }
        false
    }

    // ------------------------------------------------------------ running

    fn tick(&mut self) -> Res<()> {
        self.steps += 1;
        if self.steps > self.max_steps {
            return self.throw("RangeError", "the script ran too long and was stopped");
        }
        Ok(())
    }

    /// Runs a whole script in the global scope; returns the last expression statement's value.
    pub fn run(&mut self, src: &str) -> Result<Value, String> {
        let prog = crate::parse::parse(src).map_err(|e| format!("SyntaxError: {e}"))?;
        let g = self.global.clone();
        self.hoist(&prog, &g, true);
        let mut last = Value::Undefined;
        for s in &prog {
            match s {
                Stmt::Expr(e) => last = self.eval(e, &g).map_err(|v| self.describe(&v))?,
                s => match self.exec(s, &g) {
                    Ok(_) => {}
                    Err(v) => return Err(self.describe(&v)),
                },
            }
        }
        // Timers and queued callbacks, in order (no real clock yet).
        let mut n = 0;
        while !self.tasks.is_empty() && n < 10_000 {
            let (f, args) = self.tasks.remove(0);
            self.call(&f, Value::Undefined, args).map_err(|v| self.describe(&v))?;
            n += 1;
        }
        Ok(last)
    }

    /// "TypeError: message" for an exception value.
    pub fn describe(&mut self, v: &Value) -> String {
        if let Value::Obj(o) = v {
            if matches!(o.borrow().kind, Kind::Error) {
                let name = self.get(v, "name").ok().and_then(|n| self.to_str(&n).ok()).unwrap_or_else(|| "Error".into());
                let msg = self.get(v, "message").ok().and_then(|n| self.to_str(&n).ok()).unwrap_or_default();
                return format!("{name}: {msg}");
            }
        }
        format!("Uncaught {}", self.to_str(v).map(|s| s.to_string()).unwrap_or_else(|_| "exception".into()))
    }

    /// Declares `var`s and functions before a body runs (hoisting).
    fn hoist(&mut self, body: &[Stmt], sc: &Scope, func_scope: bool) {
        for s in body {
            match s {
                Stmt::Func(f) => {
                    let fv = self.closure(f, sc);
                    if let Some(n) = &f.name {
                        sc.borrow_mut().vars.insert(n.clone(), (fv, true));
                    }
                }
                _ if func_scope => self.hoist_vars(s, sc),
                _ => {}
            }
        }
    }

    fn hoist_vars(&mut self, s: &Stmt, sc: &Scope) {
        let declare = |p: &Expr| {
            let mut names = Vec::new();
            pattern_names(p, &mut names);
            for n in names {
                sc.borrow_mut().vars.entry(n).or_insert((Value::Undefined, true));
            }
        };
        match s {
            Stmt::Var("var", decls) => decls.iter().for_each(|(p, _)| declare(p)),
            Stmt::ForIn("var", p, ..) => declare(p),
            Stmt::If(_, a, b) => {
                self.hoist_vars(a, sc);
                if let Some(b) = b {
                    self.hoist_vars(b, sc);
                }
            }
            Stmt::Block(b) => b.iter().for_each(|s| self.hoist_vars(s, sc)),
            Stmt::For(i, _, _, body) => {
                if let Some(i) = i {
                    self.hoist_vars(i, sc);
                }
                self.hoist_vars(body, sc);
            }
            Stmt::ForIn(_, _, _, _, body) | Stmt::While(_, body) | Stmt::DoWhile(body, _) | Stmt::Labeled(_, body) => self.hoist_vars(body, sc),
            Stmt::Try(a, c, f) => {
                a.iter().for_each(|s| self.hoist_vars(s, sc));
                if let Some((_, c)) = c {
                    c.iter().for_each(|s| self.hoist_vars(s, sc));
                }
                if let Some(f) = f {
                    f.iter().for_each(|s| self.hoist_vars(s, sc));
                }
            }
            Stmt::Switch(_, cases) => cases.iter().for_each(|(_, b)| b.iter().for_each(|s| self.hoist_vars(s, sc))),
            _ => {}
        }
    }

    fn closure(&self, f: &Rc<Func>, sc: &Scope) -> Value {
        let this = if f.arrow { Some(self.lookup_this(sc)) } else { None };
        let fv = self.obj(Kind::Function(Callable::Closure { func: f.clone(), scope: sc.clone(), this, sup: None }));
        if let Value::Obj(o) = &fv {
            let mut b = o.borrow_mut();
            b.set_own("name", Value::Str(f.name.clone().unwrap_or_else(|| "".into())));
            b.set_own("length", Value::Num(f.params.iter().filter(|p| !matches!(p, Expr::Default(..) | Expr::Rest(_))).count() as f64));
            if !f.arrow {
                // Every ordinary function can be a constructor.
                let proto = Rc::new(RefCell::new(Object::new(Some(self.object_proto.clone()), Kind::Plain)));
                proto.borrow_mut().set_own("constructor", fv.clone());
                b.set_own("prototype", Value::Obj(proto));
            }
        }
        fv
    }

    fn lookup_this(&self, sc: &Scope) -> Value {
        let mut cur = Some(sc.clone());
        while let Some(s) = cur {
            if let Some(t) = &s.borrow().this {
                return t.clone();
            }
            cur = s.borrow().parent.clone();
        }
        Value::Undefined
    }

    fn lookup(&mut self, sc: &Scope, name: &str) -> Res<Value> {
        let mut cur = Some(sc.clone());
        while let Some(s) = cur {
            if let Some((v, _)) = s.borrow().vars.get(name) {
                return Ok(v.clone());
            }
            cur = s.borrow().parent.clone();
        }
        // Then the global object (properties set as `window.x = ...`).
        let g = Value::Obj(self.global_obj.clone());
        if self.global_obj.borrow().get_own(name).is_some() {
            return self.get(&g, name);
        }
        self.throw("ReferenceError", &format!("{name} is not defined"))
    }

    fn assign_var(&mut self, sc: &Scope, name: &str, v: Value) -> Res<()> {
        let mut cur = Some(sc.clone());
        while let Some(s) = cur {
            if let Some(slot) = s.borrow_mut().vars.get_mut(name) {
                if !slot.1 {
                    return self.throw("TypeError", &format!("assignment to constant '{name}'"));
                }
                slot.0 = v;
                return Ok(());
            }
            cur = s.borrow().parent.clone();
        }
        // Undeclared: becomes a global (sloppy mode).
        self.global_obj.borrow_mut().set_own(name, v);
        Ok(())
    }

    fn declare(&mut self, sc: &Scope, kind: &str, pat: &Expr, v: Value) -> Res<()> {
        match pat {
            Expr::Ident(n) => {
                if kind == "var" {
                    // var lives in the function scope (already hoisted there).
                    return self.assign_var(sc, n, v);
                }
                sc.borrow_mut().vars.insert(n.clone(), (v, kind != "const"));
                Ok(())
            }
            _ => self.destructure(sc, Some(kind), pat, v),
        }
    }

    /// Binds (or assigns, when `kind` is None) a pattern to a value.
    fn destructure(&mut self, sc: &Scope, kind: Option<&str>, pat: &Expr, v: Value) -> Res<()> {
        match pat {
            Expr::Ident(_) => match kind {
                Some(k) => self.declare(sc, k, pat, v),
                None => self.assign_to(sc, pat, v),
            },
            Expr::Member(..) => self.assign_to(sc, pat, v),
            Expr::Default(t, d) => {
                let v = if matches!(v, Value::Undefined) { self.eval(d, sc)? } else { v };
                self.destructure(sc, kind, t, v)
            }
            Expr::ArrayPat(items) => {
                let list = self.iterate(&v)?;
                for (i, it) in items.iter().enumerate() {
                    match it {
                        None => {}
                        Some(Expr::Rest(t)) => {
                            let rest = self.array(list.get(i..).map(|s| s.to_vec()).unwrap_or_default());
                            self.destructure(sc, kind, t, rest)?;
                        }
                        Some(p) => self.destructure(sc, kind, p, list.get(i).cloned().unwrap_or(Value::Undefined))?,
                    }
                }
                Ok(())
            }
            Expr::ObjectPat(props) => {
                if matches!(v, Value::Undefined | Value::Null) {
                    return self.throw("TypeError", "can't destructure undefined or null");
                }
                let mut used = Vec::new();
                for (k, p) in props {
                    if let Expr::Rest(t) = p {
                        let rest = self.obj(Kind::Plain);
                        if let (Value::Obj(src), Value::Obj(dst)) = (&v, &rest) {
                            let keys = src.borrow().own_keys();
                            for key in keys.into_iter().filter(|k| !used.contains(k)) {
                                let val = self.get(&v, &key)?;
                                dst.borrow_mut().set_own(&key, val);
                            }
                        }
                        self.destructure(sc, kind, t, rest)?;
                        continue;
                    }
                    let kv = self.eval(k, sc)?;
                    let key = self.key(&kv)?;
                    used.push(key.clone());
                    let val = self.get(&v, &key)?;
                    self.destructure(sc, kind, p, val)?;
                }
                Ok(())
            }
            _ => self.throw("SyntaxError", "invalid destructuring target"),
        }
    }

    fn assign_to(&mut self, sc: &Scope, target: &Expr, v: Value) -> Res<()> {
        match target {
            Expr::Ident(n) => self.assign_var(sc, n, v),
            Expr::Member(o, k, _) => {
                let obj = self.eval(o, sc)?;
                let kv = self.eval(k, sc)?;
                let key = self.key(&kv)?;
                self.set(&obj, &key, v)
            }
            Expr::ArrayPat(_) | Expr::ObjectPat(_) => self.destructure(sc, None, target, v),
            _ => self.throw("SyntaxError", "invalid assignment target"),
        }
    }

    /// The values of something iterable (arrays, strings, Map, Set, array-likes).
    pub fn iterate(&mut self, v: &Value) -> Res<Vec<Value>> {
        match v {
            Value::Str(s) => Ok(s.chars().map(|c| Value::Str(c.to_string().into())).collect()),
            Value::Obj(o) => {
                let kind_list = match &o.borrow().kind {
                    Kind::Array(a) => Some(a.clone()),
                    Kind::Set(s) => Some(s.clone()),
                    Kind::Map(m) => Some(m.iter().map(|(k, v)| self.array(vec![k.clone(), v.clone()])).collect()),
                    _ => None,
                };
                if let Some(l) = kind_list {
                    return Ok(l);
                }
                // An iterator object (has next()), or an array-like (has length).
                let next = self.get(v, "next")?;
                if Self::is_callable(&next) {
                    let mut out = Vec::new();
                    for _ in 0..10_000_000 {
                        let r = self.call(&next, v.clone(), vec![])?;
                        if Self::truthy(&self.get(&r, "done")?) {
                            break;
                        }
                        out.push(self.get(&r, "value")?);
                    }
                    return Ok(out);
                }
                let len = self.get(v, "length")?;
                let n = self.to_number(&len)?;
                if n.is_nan() {
                    return self.throw("TypeError", "not iterable");
                }
                (0..n as usize).map(|i| self.get(v, &i.to_string())).collect()
            }
            _ => self.throw("TypeError", &format!("{v:?} is not iterable")),
        }
    }

    // ------------------------------------------------------------ calls

    pub fn call(&mut self, f: &Value, this: Value, args: Vec<Value>) -> Res<Value> {
        self.depth += 1;
        if self.depth > 400 {
            self.depth -= 1;
            return self.throw("RangeError", "too much recursion");
        }
        let r = self.call_inner(f, this, args);
        self.depth -= 1;
        r
    }

    fn call_inner(&mut self, f: &Value, this: Value, args: Vec<Value>) -> Res<Value> {
        let Value::Obj(o) = f else { return self.throw("TypeError", &format!("{f:?} is not a function")) };
        let callable = match &o.borrow().kind {
            Kind::Function(Callable::Native(n)) => Ok(n.clone()),
            Kind::Function(Callable::Closure { func, scope, this: lexical, sup }) => Err((func.clone(), scope.clone(), lexical.clone(), sup.clone())),
            _ => return self.throw("TypeError", "not a function"),
        };
        match callable {
            Ok(native) => native(self, this, args),
            Err((func, parent, lexical, sup)) => {
                let this_v = if func.arrow { lexical } else { Some(this) };
                let sc = scope(Some(&parent), true, if func.arrow { None } else { this_v.clone() });
                if !func.arrow {
                    let arguments = self.array(args.clone());
                    sc.borrow_mut().vars.insert("arguments".into(), (arguments, true));
                }
                let fields = o.borrow().get_own(FIELDS).and_then(|s| if let Slot::Data(v) = s { Some(v.clone()) } else { None });
                let derived = sup.is_some();
                if let Some(s) = sup {
                    sc.borrow_mut().vars.insert("super".into(), (s, false));
                    sc.borrow_mut().vars.insert(FIELDS.into(), (fields.clone().unwrap_or(Value::Undefined), false));
                }
                if let (Some(init), false) = (&fields, derived) {
                    self.call(init, this_v.clone().unwrap_or(Value::Undefined), vec![])?;
                }
                if let Some(name) = &func.name {
                    if !sc.borrow().vars.contains_key(name) {
                        sc.borrow_mut().vars.insert(name.clone(), (f.clone(), true));
                    }
                }
                for (i, p) in func.params.iter().enumerate() {
                    match p {
                        Expr::Rest(t) => {
                            let rest = self.array(args.get(i..).map(|s| s.to_vec()).unwrap_or_default());
                            self.destructure(&sc, Some("let"), t, rest)?;
                        }
                        p => {
                            let v = args.get(i).cloned().unwrap_or(Value::Undefined);
                            self.destructure(&sc, Some("let"), p, v)?;
                        }
                    }
                }
                self.hoist(&func.body, &sc, true);
                for s in &func.body {
                    match self.exec(s, &sc)? {
                        Flow::Return(v) => return Ok(v),
                        Flow::Normal => {}
                        _ => break,
                    }
                }
                Ok(Value::Undefined)
            }
        }
    }

    pub fn construct(&mut self, f: &Value, args: Vec<Value>) -> Res<Value> {
        if !Self::is_callable(f) {
            return self.throw("TypeError", "not a constructor");
        }
        let is_native = matches!(f, Value::Obj(o) if matches!(o.borrow().kind, Kind::Function(Callable::Native(_))));
        if is_native {
            // Native constructors build their own objects (marked by a `new` this).
            let marker = self.obj(Kind::Plain);
            if let Value::Obj(m) = &marker {
                m.borrow_mut().set_own("__new__", Value::Bool(true));
            }
            return self.call(f, marker, args);
        }
        let proto = match self.get(f, "prototype")? {
            Value::Obj(p) => p,
            _ => self.object_proto.clone(),
        };
        let kind = if self.inherits_error(&proto) { Kind::Error } else { Kind::Plain };
        let obj = Value::Obj(Rc::new(RefCell::new(Object::new(Some(proto), kind))));
        let r = self.call(f, obj.clone(), args)?;
        Ok(if matches!(r, Value::Obj(_)) { r } else { obj })
    }

    fn inherits_error(&self, p: &Obj) -> bool {
        let mut cur = Some(p.clone());
        while let Some(c) = cur {
            if Rc::ptr_eq(&c, &self.error_proto) {
                return true;
            }
            cur = c.borrow().proto.clone();
        }
        false
    }

    pub fn instance_of(&mut self, v: &Value, f: &Value) -> Res<bool> {
        let Value::Obj(o) = v else { return Ok(false) };
        let Value::Obj(p) = self.get(f, "prototype")? else { return Ok(false) };
        let mut cur = o.borrow().proto.clone();
        while let Some(c) = cur {
            if Rc::ptr_eq(&c, &p) {
                return Ok(true);
            }
            cur = c.borrow().proto.clone();
        }
        Ok(false)
    }

    // ------------------------------------------------------------ statements

    fn block(&mut self, body: &[Stmt], sc: &Scope) -> Res<Flow> {
        let inner = scope(Some(sc), false, None);
        self.hoist(body, &inner, false);
        for s in body {
            let f = self.exec(s, &inner)?;
            if !matches!(f, Flow::Normal) {
                return Ok(f);
            }
        }
        Ok(Flow::Normal)
    }

    /// Runs a loop body; tells the loop whether to stop (and what to return).
    fn loop_body(&mut self, body: &Stmt, sc: &Scope, label: Option<&Rc<str>>) -> Res<Option<Flow>> {
        match self.exec(body, sc)? {
            Flow::Normal => Ok(None),
            Flow::Continue(None) => Ok(None),
            Flow::Continue(Some(l)) if Some(&l) == label => Ok(None),
            Flow::Break(None) => Ok(Some(Flow::Normal)),
            Flow::Break(Some(l)) if Some(&l) == label => Ok(Some(Flow::Normal)),
            other => Ok(Some(other)),
        }
    }

    pub fn exec(&mut self, s: &Stmt, sc: &Scope) -> Res<Flow> {
        self.exec_labeled(s, sc, None)
    }

    fn exec_labeled(&mut self, s: &Stmt, sc: &Scope, label: Option<&Rc<str>>) -> Res<Flow> {
        self.tick()?;
        Ok(match s {
            Stmt::Expr(e) => {
                self.eval(e, sc)?;
                Flow::Normal
            }
            Stmt::Var(kind, decls) => {
                for (p, init) in decls {
                    let v = match init {
                        Some(e) => {
                            let v = self.eval(e, sc)?;
                            // `const f = function () {}` names the function.
                            if let (Expr::Ident(n), Value::Obj(o)) = (p, &v) {
                                if matches!(init, Some(Expr::Func(f)) if f.name.is_none()) {
                                    o.borrow_mut().set_own("name", Value::Str(n.clone()));
                                }
                            }
                            v
                        }
                        None if *kind == "var" => continue,
                        None => Value::Undefined,
                    };
                    self.declare(sc, kind, p, v)?;
                }
                Flow::Normal
            }
            Stmt::Func(_) | Stmt::Empty => Flow::Normal,
            Stmt::Return(e) => Flow::Return(match e {
                Some(e) => self.eval(e, sc)?,
                None => Value::Undefined,
            }),
            Stmt::If(c, a, b) => {
                let cv = self.eval(c, sc)?;
                if Self::truthy(&cv) {
                    self.exec(a, sc)?
                } else if let Some(b) = b {
                    self.exec(b, sc)?
                } else {
                    Flow::Normal
                }
            }
            Stmt::Block(b) => self.block(b, sc)?,
            Stmt::For(init, test, update, body) => {
                let lsc = scope(Some(sc), false, None);
                if let Some(i) = init {
                    self.exec(i, &lsc)?;
                }
                loop {
                    self.tick()?;
                    if let Some(t) = test {
                        let tv = self.eval(t, &lsc)?;
                        if !Self::truthy(&tv) {
                            break;
                        }
                    }
                    // Each iteration gets its own copy of the loop variables (closures capture it).
                    let iter = scope(Some(sc), false, None);
                    iter.borrow_mut().vars = lsc.borrow().vars.clone();
                    if let Some(stop) = self.loop_body(body, &iter, label)? {
                        return Ok(stop);
                    }
                    lsc.borrow_mut().vars = iter.borrow().vars.clone();
                    if let Some(u) = update {
                        self.eval(u, &lsc)?;
                    }
                }
                Flow::Normal
            }
            Stmt::ForIn(kind, pat, of, iter, body) => {
                let it = self.eval(iter, sc)?;
                let items: Vec<Value> = if *of {
                    self.iterate(&it)?
                } else {
                    match &it {
                        Value::Obj(o) => {
                            // Own keys, then inherited enumerable ones (not built-in prototypes).
                            let mut keys = o.borrow().own_keys();
                            let mut cur = o.borrow().proto.clone();
                            while let Some(p) = cur {
                                if [&self.object_proto, &self.array_proto, &self.function_proto].iter().any(|b| Rc::ptr_eq(b, &p)) {
                                    break;
                                }
                                for k in p.borrow().own_keys() {
                                    if !keys.contains(&k) {
                                        keys.push(k);
                                    }
                                }
                                cur = p.borrow().proto.clone();
                            }
                            keys.into_iter().map(Value::Str).collect()
                        }
                        Value::Str(s) => (0..s.chars().count()).map(|i| Value::Str(i.to_string().into())).collect(),
                        _ => Vec::new(),
                    }
                };
                for v in items {
                    self.tick()?;
                    let isc = scope(Some(sc), false, None);
                    if kind.is_empty() {
                        self.assign_to(&isc, pat, v)?;
                    } else {
                        self.declare(&isc, kind, pat, v)?;
                    }
                    if let Some(stop) = self.loop_body(body, &isc, label)? {
                        return Ok(stop);
                    }
                }
                Flow::Normal
            }
            Stmt::While(c, body) => {
                loop {
                    let cv = self.eval(c, sc)?;
                    if !Self::truthy(&cv) {
                        break;
                    }
                    if let Some(stop) = self.loop_body(body, sc, label)? {
                        return Ok(stop);
                    }
                }
                Flow::Normal
            }
            Stmt::DoWhile(body, c) => {
                loop {
                    if let Some(stop) = self.loop_body(body, sc, label)? {
                        return Ok(stop);
                    }
                    let cv = self.eval(c, sc)?;
                    if !Self::truthy(&cv) {
                        break;
                    }
                }
                Flow::Normal
            }
            Stmt::Break(l) => Flow::Break(l.clone()),
            Stmt::Continue(l) => Flow::Continue(l.clone()),
            Stmt::Throw(e) => return Err(self.eval(e, sc)?),
            Stmt::Try(body, catch, finally) => {
                let r = self.block(body, sc);
                let r = match (r, catch) {
                    (Err(exc), Some((param, cbody))) => {
                        if self.steps > self.max_steps {
                            Err(exc) // the step budget can't be caught
                        } else {
                            let csc = scope(Some(sc), false, None);
                            if let Some(p) = param {
                                self.destructure(&csc, Some("let"), p, exc)?;
                            }
                            self.block(cbody, &csc)
                        }
                    }
                    (r, _) => r,
                };
                if let Some(f) = finally {
                    let ff = self.block(f, sc)?;
                    if !matches!(ff, Flow::Normal) {
                        return Ok(ff);
                    }
                }
                r?
            }
            Stmt::Switch(d, cases) => {
                let dv = self.eval(d, sc)?;
                let ssc = scope(Some(sc), false, None);
                let mut start = None;
                for (i, (t, _)) in cases.iter().enumerate() {
                    if let Some(t) = t {
                        let tv = self.eval(t, &ssc)?;
                        if Self::strict_eq(&dv, &tv) {
                            start = Some(i);
                            break;
                        }
                    }
                }
                let start = start.or_else(|| cases.iter().position(|(t, _)| t.is_none()));
                if let Some(start) = start {
                    for (_, body) in &cases[start..] {
                        for s in body {
                            match self.exec(s, &ssc)? {
                                Flow::Normal => {}
                                Flow::Break(None) => return Ok(Flow::Normal),
                                other => return Ok(other),
                            }
                        }
                    }
                }
                Flow::Normal
            }
            Stmt::Labeled(l, body) => match self.exec_labeled(body, sc, Some(l))? {
                Flow::Break(Some(b)) if &b == l => Flow::Normal,
                other => other,
            },
            Stmt::Class(name, sup, members) => {
                let c = self.class(Some(name.clone()), sup.as_ref(), members, sc)?;
                sc.borrow_mut().vars.insert(name.clone(), (c, true));
                Flow::Normal
            }
        })
    }

    fn class(&mut self, name: Option<Rc<str>>, sup: Option<&Expr>, members: &[(Expr, Member, bool)], sc: &Scope) -> Res<Value> {
        let parent = match sup {
            Some(e) => Some(self.eval(e, sc)?),
            None => None,
        };
        let ctor_func = members.iter().find_map(|(k, m, st)| match m {
            Member::Method(f) if !st && matches!(k, Expr::Str(n) if &**n == "constructor") => Some(f.clone()),
            _ => None,
        });
        let ctor_func = ctor_func.unwrap_or_else(|| {
            // The default constructor passes its arguments up to the parent's.
            let body = if parent.is_some() {
                vec![Stmt::Expr(Expr::Call(Box::new(Expr::Ident("super".into())), vec![Expr::Spread(Box::new(Expr::Ident("arguments".into())))], false))]
            } else {
                vec![]
            };
            Rc::new(Func { name: name.clone(), params: vec![], body, arrow: false, expr_body: false })
        });
        let ctor = self.closure(&ctor_func, sc);
        let proto = match self.get(&ctor, "prototype")? {
            Value::Obj(p) => p,
            _ => return self.throw("TypeError", "class without prototype"),
        };
        if let Some(n) = &name {
            self.set(&ctor, "name", Value::Str(n.clone()))?;
        }
        if let Some(p) = &parent {
            if let Value::Obj(pp) = self.get(p, "prototype")? {
                proto.borrow_mut().proto = Some(pp);
            }
            if let (Value::Obj(c), Value::Obj(po)) = (&ctor, p) {
                c.borrow_mut().proto = Some(po.clone());
            }
        }
        let set_super = |v: &Value, s: Option<Value>| {
            if let Value::Obj(o) = v {
                if let Kind::Function(Callable::Closure { sup, .. }) = &mut o.borrow_mut().kind {
                    *sup = s;
                }
            }
        };
        set_super(&ctor, parent.clone());
        let mut fields = Vec::new();
        let mut statics = Vec::new();
        for (k, member, is_static) in members {
            if !is_static && matches!(k, Expr::Str(n) if &**n == "constructor") {
                continue;
            }
            let kv = self.eval(k, sc)?;
            let key = self.key(&kv)?;
            let target = if *is_static {
                match &ctor {
                    Value::Obj(c) => c.clone(),
                    _ => unreachable!(),
                }
            } else {
                proto.clone()
            };
            match member {
                Member::Method(f) => {
                    let m = self.closure(f, sc);
                    set_super(&m, parent.clone());
                    target.borrow_mut().set_own(&key, m);
                }
                Member::Get(f) | Member::Set(f) => {
                    let m = self.closure(f, sc);
                    set_super(&m, parent.clone());
                    let old = target.borrow().get_own(&key).cloned();
                    let (mut g, mut st) = match old {
                        Some(Slot::Accessor(g, s)) => (g, s),
                        _ => (None, None),
                    };
                    if matches!(member, Member::Get(_)) {
                        g = Some(m);
                    } else {
                        st = Some(m);
                    }
                    target.borrow_mut().set_slot(&key, Slot::Accessor(g, st));
                }
                Member::Field(init) => {
                    let value = init.clone().unwrap_or(Expr::Undefined);
                    let assign = Stmt::Expr(Expr::Assign("=", Box::new(Expr::Member(Box::new(Expr::This), Box::new(Expr::Str(key)), false)), Box::new(value)));
                    if *is_static {
                        statics.push(assign);
                    } else {
                        fields.push(assign);
                    }
                }
            }
        }
        // Instance fields run as a hidden initialiser: before the constructor
        // body in a base class, right after super() in a derived one.
        if !fields.is_empty() {
            let init = Rc::new(Func { name: None, params: vec![], body: fields, arrow: false, expr_body: false });
            let f = self.closure(&init, sc);
            set_super(&f, parent.clone());
            if let Value::Obj(c) = &ctor {
                c.borrow_mut().set_own(FIELDS, f);
            }
        }
        if !statics.is_empty() {
            let init = Rc::new(Func { name: None, params: vec![], body: statics, arrow: false, expr_body: false });
            let f = self.closure(&init, sc);
            self.call(&f, ctor.clone(), vec![])?;
        }
        Ok(ctor)
    }

    // ------------------------------------------------------------ expressions

    fn args(&mut self, list: &[Expr], sc: &Scope) -> Res<Vec<Value>> {
        let mut out = Vec::with_capacity(list.len());
        for a in list {
            if let Expr::Spread(e) = a {
                let v = self.eval(e, sc)?;
                out.extend(self.iterate(&v)?);
            } else {
                out.push(self.eval(a, sc)?);
            }
        }
        Ok(out)
    }

    pub fn eval(&mut self, e: &Expr, sc: &Scope) -> Res<Value> {
        self.tick()?;
        Ok(match e {
            Expr::Num(n) => Value::Num(*n),
            Expr::Str(s) => Value::Str(s.clone()),
            Expr::Bool(b) => Value::Bool(*b),
            Expr::Null => Value::Null,
            Expr::Undefined => Value::Undefined,
            Expr::Template(strs, exprs) => {
                let mut out = String::new();
                for (i, s) in strs.iter().enumerate() {
                    out.push_str(s);
                    if let Some(e) = exprs.get(i) {
                        let v = self.eval(e, sc)?;
                        out.push_str(&self.to_str(&v)?);
                    }
                }
                Value::Str(out.into())
            }
            Expr::Ident(n) => self.lookup(sc, n)?,
            Expr::This => self.lookup_this(sc),
            Expr::Array(items) => {
                let v = self.args(items, sc)?;
                self.array(v)
            }
            Expr::Object(props) => {
                let o = self.obj(Kind::Plain);
                let Value::Obj(oo) = &o else { unreachable!() };
                for p in props {
                    match p {
                        Prop::KeyValue(k, v) => {
                            let kv = self.eval(k, sc)?;
                            let key = self.key(&kv)?;
                            let val = self.eval(v, sc)?;
                            if let (Expr::Func(f), Value::Obj(fo)) = (v, &val) {
                                if f.name.is_none() {
                                    fo.borrow_mut().set_own("name", Value::Str(key.clone()));
                                }
                            }
                            if &*key == "__proto__" && matches!(k, Expr::Str(_)) {
                                self.set(&o, "__proto__", val)?;
                            } else {
                                oo.borrow_mut().set_own(&key, val);
                            }
                        }
                        Prop::Spread(e) => {
                            let v = self.eval(e, sc)?;
                            if let Value::Obj(src) = &v {
                                let keys = src.borrow().own_keys();
                                for key in keys {
                                    let val = self.get(&v, &key)?;
                                    oo.borrow_mut().set_own(&key, val);
                                }
                            }
                        }
                        Prop::Getter(k, f) | Prop::Setter(k, f) => {
                            let kv = self.eval(k, sc)?;
                            let key = self.key(&kv)?;
                            let fv = self.closure(f, sc);
                            let existing = oo.borrow().get_own(&key).cloned();
                            let (mut g, mut s) = match existing {
                                Some(Slot::Accessor(g, s)) => (g, s),
                                _ => (None, None),
                            };
                            if matches!(p, Prop::Getter(..)) {
                                g = Some(fv);
                            } else {
                                s = Some(fv);
                            }
                            oo.borrow_mut().set_slot(&key, Slot::Accessor(g, s));
                        }
                    }
                }
                o
            }
            Expr::Func(f) => self.closure(f, sc),
            Expr::Unary(op, x) => {
                if *op == "typeof" {
                    // typeof an undeclared name is "undefined", not an error.
                    if let Expr::Ident(n) = &**x {
                        return Ok(Value::Str(match self.lookup(sc, n) {
                            Ok(v) => Self::typeof_(&v),
                            Err(_) => "undefined",
                        }
                        .into()));
                    }
                    let v = self.eval(x, sc)?;
                    return Ok(Value::Str(Self::typeof_(&v).into()));
                }
                if *op == "delete" {
                    if let Expr::Member(o, k, _) = &**x {
                        let ov = self.eval(o, sc)?;
                        let kv = self.eval(k, sc)?;
                        let key = self.key(&kv)?;
                        if let Value::Obj(oo) = &ov {
                            if let Kind::Array(a) = &mut oo.borrow_mut().kind {
                                if let Ok(i) = key.parse::<usize>() {
                                    if i < a.len() {
                                        a[i] = Value::Undefined;
                                    }
                                    return Ok(Value::Bool(true));
                                }
                            }
                            oo.borrow_mut().delete(&key);
                        }
                    }
                    return Ok(Value::Bool(true));
                }
                let v = self.eval(x, sc)?;
                match *op {
                    "!" => Value::Bool(!Self::truthy(&v)),
                    "-" => Value::Num(-self.to_number(&v)?),
                    "+" => Value::Num(self.to_number(&v)?),
                    "~" => Value::Num(!(to_i32(self.to_number(&v)?)) as f64),
                    "void" => Value::Undefined,
                    _ => return self.throw("SyntaxError", "unknown operator"),
                }
            }
            Expr::Update(op, prefix, target) => {
                let old = self.eval(target, sc)?;
                let n = self.to_number(&old)?;
                let new = if *op == "++" { n + 1.0 } else { n - 1.0 };
                self.assign_to(sc, target, Value::Num(new))?;
                Value::Num(if *prefix { new } else { n })
            }
            Expr::Binary(op, a, b) => {
                let av = self.eval(a, sc)?;
                let bv = self.eval(b, sc)?;
                self.binary(op, &av, &bv)?
            }
            Expr::Logical(op, a, b) => {
                let av = self.eval(a, sc)?;
                let take_left = match *op {
                    "&&" => !Self::truthy(&av),
                    "||" => Self::truthy(&av),
                    _ => !matches!(av, Value::Undefined | Value::Null),
                };
                if take_left {
                    av
                } else {
                    self.eval(b, sc)?
                }
            }
            Expr::Assign(op, target, value) => {
                if *op == "=" {
                    let v = self.eval(value, sc)?;
                    if let (Expr::Ident(n), Expr::Func(f), Value::Obj(o)) = (&**target, &**value, &v) {
                        if f.name.is_none() {
                            o.borrow_mut().set_own("name", Value::Str(n.clone()));
                        }
                    }
                    self.assign_to(sc, target, v.clone())?;
                    return Ok(v);
                }
                let cur = self.eval(target, sc)?;
                let v = match *op {
                    "&&=" | "||=" | "??=" => {
                        let keep = match *op {
                            "&&=" => !Self::truthy(&cur),
                            "||=" => Self::truthy(&cur),
                            _ => !matches!(cur, Value::Undefined | Value::Null),
                        };
                        if keep {
                            return Ok(cur);
                        }
                        self.eval(value, sc)?
                    }
                    _ => {
                        let rv = self.eval(value, sc)?;
                        let bop = &op[..op.len() - 1];
                        self.binary(bop, &cur, &rv)?
                    }
                };
                self.assign_to(sc, target, v.clone())?;
                v
            }
            Expr::Cond(c, a, b) => {
                let cv = self.eval(c, sc)?;
                if Self::truthy(&cv) {
                    self.eval(a, sc)?
                } else {
                    self.eval(b, sc)?
                }
            }
            Expr::Member(o, k, optional) => {
                if let Expr::Ident(n) = &**o {
                    if &**n == "super" {
                        // super.method: from the parent class's prototype.
                        let parent = self.lookup(sc, "super")?;
                        let pp = self.get(&parent, "prototype")?;
                        let kv = self.eval(k, sc)?;
                        let key = self.key(&kv)?;
                        return self.get(&pp, &key);
                    }
                }
                let ov = self.eval(o, sc)?;
                if *optional && matches!(ov, Value::Undefined | Value::Null) {
                    return Ok(Value::Undefined);
                }
                let kv = self.eval(k, sc)?;
                let key = self.key(&kv)?;
                self.get(&ov, &key)?
            }
            Expr::Call(callee, args, optional) => {
                // super(...) runs the parent constructor on this object.
                if let Expr::Ident(n) = &**callee {
                    if &**n == "super" {
                        let parent = self.lookup(sc, "super")?;
                        let this = self.lookup_this(sc);
                        let a = self.args(args, sc)?;
                        let is_native = matches!(&parent, Value::Obj(o) if matches!(o.borrow().kind, Kind::Function(Callable::Native(_))));
                        if is_native {
                            // A built-in parent (Error): copy what it makes onto this.
                            let made = self.construct(&parent, a)?;
                            if let (Value::Obj(src), Value::Obj(dst)) = (&made, &this) {
                                let keys = src.borrow().own_keys();
                                for k in keys {
                                    let v = self.get(&made, &k)?;
                                    dst.borrow_mut().set_own(&k, v);
                                }
                            }
                        } else {
                            self.call(&parent, this.clone(), a)?;
                        }
                        let init = self.lookup(sc, FIELDS)?;
                        if Self::is_callable(&init) {
                            self.call(&init, this, vec![])?;
                        }
                        return Ok(Value::Undefined);
                    }
                }
                let (f, this) = match &**callee {
                    Expr::Member(o, k, opt_member) => {
                        let this = if matches!(&**o, Expr::Ident(n) if &**n == "super") { self.lookup_this(sc) } else { self.eval(o, sc)? };
                        if *opt_member && matches!(this, Value::Undefined | Value::Null) {
                            return Ok(Value::Undefined);
                        }
                        let f = if matches!(&**o, Expr::Ident(n) if &**n == "super") {
                            self.eval(callee, sc)?
                        } else {
                            let kv = self.eval(k, sc)?;
                            let key = self.key(&kv)?;
                            let f = self.get(&this, &key)?;
                            if !Self::is_callable(&f) && !*optional {
                                return self.throw("TypeError", &format!("{} is not a function", describe_callee(callee)));
                            }
                            f
                        };
                        (f, this)
                    }
                    other => (self.eval(other, sc)?, Value::Undefined),
                };
                if *optional && matches!(f, Value::Undefined | Value::Null) {
                    return Ok(Value::Undefined);
                }
                if !Self::is_callable(&f) {
                    return self.throw("TypeError", &format!("{} is not a function", describe_callee(callee)));
                }
                let a = self.args(args, sc)?;
                self.call(&f, this, a)?
            }
            Expr::New(callee, args) => {
                let f = self.eval(callee, sc)?;
                let a = self.args(args, sc)?;
                if !Self::is_callable(&f) {
                    return self.throw("TypeError", &format!("{} is not a constructor", describe_callee(callee)));
                }
                self.construct(&f, a)?
            }
            Expr::Seq(list) => {
                let mut last = Value::Undefined;
                for e in list {
                    last = self.eval(e, sc)?;
                }
                last
            }
            Expr::Spread(_) | Expr::ArrayPat(_) | Expr::ObjectPat(_) | Expr::Default(..) | Expr::Rest(_) => {
                return self.throw("SyntaxError", "unexpected pattern");
            }
        })
    }

    pub fn binary(&mut self, op: &str, a: &Value, b: &Value) -> Res<Value> {
        Ok(match op {
            "+" => {
                let (pa, pb) = (self.to_primitive(a, false)?, self.to_primitive(b, false)?);
                if matches!(pa, Value::Str(_)) || matches!(pb, Value::Str(_)) {
                    let mut s = self.to_str(&pa)?.to_string();
                    s.push_str(&self.to_str(&pb)?);
                    Value::Str(s.into())
                } else {
                    Value::Num(self.to_number(&pa)? + self.to_number(&pb)?)
                }
            }
            "-" | "*" | "/" | "%" | "**" => {
                let (x, y) = (self.to_number(a)?, self.to_number(b)?);
                Value::Num(match op {
                    "-" => x - y,
                    "*" => x * y,
                    "/" => x / y,
                    "%" => x % y,
                    _ => x.powf(y),
                })
            }
            "&" | "|" | "^" | "<<" | ">>" | ">>>" => {
                let (x, y) = (self.to_number(a)?, self.to_number(b)?);
                let (xi, yi) = (to_i32(x), to_i32(y) as u32 & 31);
                Value::Num(match op {
                    "&" => (xi & to_i32(y)) as f64,
                    "|" => (xi | to_i32(y)) as f64,
                    "^" => (xi ^ to_i32(y)) as f64,
                    "<<" => xi.wrapping_shl(yi) as f64,
                    ">>" => xi.wrapping_shr(yi) as f64,
                    _ => (xi as u32).wrapping_shr(yi) as f64,
                })
            }
            "==" => Value::Bool(self.loose_eq(a, b)?),
            "!=" => Value::Bool(!self.loose_eq(a, b)?),
            "===" => Value::Bool(Self::strict_eq(a, b)),
            "!==" => Value::Bool(!Self::strict_eq(a, b)),
            "<" | ">" | "<=" | ">=" => {
                let (pa, pb) = (self.to_primitive(a, true)?, self.to_primitive(b, true)?);
                let ord = if let (Value::Str(x), Value::Str(y)) = (&pa, &pb) {
                    Some(x.cmp(y))
                } else {
                    let (x, y) = (self.to_number(&pa)?, self.to_number(&pb)?);
                    x.partial_cmp(&y)
                };
                Value::Bool(match ord {
                    None => false,
                    Some(o) => match op {
                        "<" => o.is_lt(),
                        ">" => o.is_gt(),
                        "<=" => o.is_le(),
                        _ => o.is_ge(),
                    },
                })
            }
            "instanceof" => Value::Bool(self.instance_of(a, b)?),
            "in" => {
                let Value::Obj(o) = b else { return self.throw("TypeError", "'in' needs an object") };
                let k = self.key(a)?;
                Value::Bool(self.has_property(o, &k))
            }
            _ => return self.throw("SyntaxError", &format!("unknown operator {op}")),
        })
    }
}

fn describe_callee(e: &Expr) -> String {
    match e {
        Expr::Ident(n) => n.to_string(),
        Expr::Member(o, k, _) => match &**k {
            Expr::Str(s) => format!("{}.{s}", describe_callee(o)),
            _ => format!("{}[...]", describe_callee(o)),
        },
        Expr::This => "this".into(),
        _ => "expression".into(),
    }
}

pub fn to_i32(n: f64) -> i32 {
    if !n.is_finite() {
        return 0;
    }
    (n.trunc() as i64 as u64 & 0xffff_ffff) as u32 as i32
}

pub fn str_to_number(s: &str) -> f64 {
    let t = s.trim();
    if t.is_empty() {
        return 0.0;
    }
    if let Some(h) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        return u64::from_str_radix(h, 16).map(|v| v as f64).unwrap_or(f64::NAN);
    }
    match t {
        "Infinity" | "+Infinity" => f64::INFINITY,
        "-Infinity" => f64::NEG_INFINITY,
        _ if t.contains(|c: char| c.is_ascii_alphabetic() && c != 'e' && c != 'E') => f64::NAN,
        _ => t.parse().unwrap_or(f64::NAN),
    }
}

/// The names a pattern binds.
fn pattern_names(p: &Expr, out: &mut Vec<Rc<str>>) {
    match p {
        Expr::Ident(n) => out.push(n.clone()),
        Expr::Default(t, _) | Expr::Rest(t) => pattern_names(t, out),
        Expr::ArrayPat(items) => items.iter().flatten().for_each(|i| pattern_names(i, out)),
        Expr::ObjectPat(props) => props.iter().for_each(|(_, v)| pattern_names(v, out)),
        _ => {}
    }
}
