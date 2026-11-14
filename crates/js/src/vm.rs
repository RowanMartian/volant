//! The bytecode VM: runs what [`crate::compile`] makes.
//!
//! A run keeps one operand stack. A call from JavaScript to JavaScript pushes
//! a frame onto that run's frame list instead of recursing in Rust, so deep
//! recursion costs heap, not native stack. Each frame's arguments and locals
//! live in the stack (`[this f args… locals… operands…]`); variables that
//! closures capture live in heap environments ([`Env`]).
//!
//! Generators and async functions suspend for real: `yield` and `await` hand
//! the frame and its stack back to whoever resumes it (the generator's
//! `next()`, or the promise reaction that the awaited value settles).

use crate::compile::{Code, Op};
use crate::interp::{num_to_string, string_at, to_i32, Callable, Interp, Kind, Obj, Object, Res, Slot, Value};
use crate::promise::{PState, SYM_ITERATOR};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// A heap environment: the captured variables of a function or block.
pub struct Env {
    pub vars: RefCell<Vec<Value>>,
    pub parent: Option<Rc<Env>>,
}

impl Env {
    pub fn new(n: u32, parent: Option<Rc<Env>>) -> Self {
        Env { vars: RefCell::new(vec![Value::Undefined; n as usize]), parent }
    }
}

/// A function made from compiled code.
pub struct Closure {
    pub code: Rc<Code>,
    pub env: Option<Rc<Env>>,
    /// The parent class, for `super` in methods and constructors.
    pub sup: Option<Value>,
}

struct Handler {
    pc: u32,
    sp: usize,
    env: Option<Rc<Env>>,
}

pub struct Frame {
    code: Rc<Code>,
    pc: usize,
    /// Where the arguments start in the stack (this and the function are just below).
    bp: usize,
    argc: usize,
    /// Where the locals start (after the arguments).
    lp: usize,
    env: Option<Rc<Env>>,
    this: Value,
    func: Value,
    handlers: Vec<Handler>,
    /// A `new` call: an object it returns replaces `this`.
    construct: bool,
}

/// A frame that's waiting: a generator between `next()`s, an async function
/// at an `await`, a module linked but not yet run.
pub struct Suspended {
    frame: Frame,
    stack: Vec<Value>,
}

pub struct GenState {
    susp: Option<Suspended>,
    started: bool,
    running: bool,
    is_async: bool,
}

impl GenState {
    pub fn is_async(&self) -> bool {
        self.is_async
    }
}

/// The VM's own iterators over built-in collections.
pub enum IterState {
    /// An array, live (it sees items added while it runs).
    Array(Obj, usize),
    /// A snapshot (string characters, Set and Map entries, `for in` keys).
    List(Vec<Value>, usize),
}

pub(crate) enum Exit {
    Return(Value),
    Yield(Value),
    Await(Value, Suspended),
    InitDone(Suspended),
}

pub(crate) enum Input {
    Start,
    Next(Value),
    Throw(Value),
    /// Resuming a generator at a `yield`: the sent value, and whether it is a `return()`.
    Resume(Value, bool),
}

/// The hidden property holding a class's instance-field initialiser.
const FIELDS: &str = "\0fields";
/// A global variable's inline cache marks a `let`/`const` binding's position with this bit.
const IC_LEX: u32 = 1 << 31;

#[inline]
fn cstr(code: &Code, i: u32) -> &Rc<str> {
    match &code.consts[i as usize] {
        Value::Str(s) => s,
        _ => unreachable!("a name constant"),
    }
}

/// The own data property `k` of a plain object (or its prototype), using the cache.
#[inline]
fn fast_get(o: &Obj, k: &str, ic: &Cell<u32>) -> Option<Value> {
    let b = o.borrow();
    if !matches!(b.kind, Kind::Plain | Kind::Error | Kind::Function(_)) {
        return None;
    }
    if let Some(i) = b.find(k, ic.get()) {
        ic.set(i as u32);
        return match b.slot_at(i) {
            Slot::Data(v) => Some(v.clone()),
            _ => None,
        };
    }
    if matches!(b.kind, Kind::Function(_)) {
        return None;
    }
    let p = b.proto.as_ref()?.borrow();
    let i = p.find(k, ic.get())?;
    ic.set(i as u32);
    match p.slot_at(i) {
        Slot::Data(v) => Some(v.clone()),
        _ => None,
    }
}

fn env_at(env: &Option<Rc<Env>>, depth: u32) -> Option<&Rc<Env>> {
    let mut e = env.as_ref()?;
    for _ in 0..depth {
        e = e.parent.as_ref()?;
    }
    Some(e)
}

/// `name`, `length` and `prototype` of a compiled function, made when first read.
pub(crate) fn lazy_function_prop(i: &Interp, o: &Obj, k: &str) -> Option<Value> {
    if !matches!(k, "prototype" | "name" | "length") {
        return None;
    }
    let (name, nparams, wants_proto) = match &o.borrow().kind {
        Kind::Function(Callable::Closure(c)) => (c.code.name.clone(), c.code.nparams, !c.code.arrow && !c.code.method),
        // Built-in functions: a length (their arity isn't recorded) and an empty name if they have none.
        Kind::Function(Callable::Native(_)) if k != "prototype" => (Some("".into()), 0, false),
        _ => return None,
    };
    let v = match k {
        "name" => Value::Str(name.unwrap_or_else(|| "".into())),
        "length" => Value::Num(nparams as f64),
        _ if wants_proto => {
            let proto = Rc::new(RefCell::new(Object::new(Some(i.object_proto.clone()), Kind::Plain)));
            proto.borrow_mut().set_hidden("constructor", Value::Obj(o.clone()));
            Value::Obj(proto)
        }
        _ => return None,
    };
    o.borrow_mut().set_hidden(k, v.clone());
    Some(v)
}

fn result_obj(i: &Interp, value: Value, done: bool) -> Value {
    let r = i.obj(Kind::Plain);
    if let Value::Obj(o) = &r {
        let mut b = o.borrow_mut();
        b.set_own("value", value);
        b.set_own("done", Value::Bool(done));
    }
    r
}

fn set_sup(f: &Value, sup: Option<Value>) {
    if let Value::Obj(o) = f {
        if let Kind::Function(Callable::Closure(c)) = &mut o.borrow_mut().kind {
            c.sup = sup;
        }
    }
}

fn sup_of(f: &Value) -> Option<Value> {
    match f {
        Value::Obj(o) => match &o.borrow().kind {
            Kind::Function(Callable::Closure(c)) => c.sup.clone(),
            _ => None,
        },
        _ => None,
    }
}

impl Interp {
    fn take_stack(&mut self) -> Vec<Value> {
        self.stack_pool.pop().unwrap_or_else(|| Vec::with_capacity(64))
    }

    fn give_stack(&mut self, mut s: Vec<Value>) {
        if self.stack_pool.len() < 16 && s.capacity() <= 1 << 16 {
            s.clear();
            self.stack_pool.push(s);
        }
    }

    #[inline]
    fn enter(&mut self, code: &Code) {
        self.frames += 1;
        self.call_names.push((code.name.clone(), code.line));
    }

    #[inline]
    fn leave(&mut self) {
        self.frames = self.frames.saturating_sub(1);
        self.call_names.pop();
    }

    /// A frame for `code` whose arguments are already on the stack at `bp`.
    #[allow(clippy::too_many_arguments)]
    fn make_frame(&self, stack: &mut Vec<Value>, code: Rc<Code>, env: Option<Rc<Env>>, this: Value, func: Value, bp: usize, argc: usize, construct: bool) -> Frame {
        let this = if !code.strict && !code.arrow && matches!(this, Value::Undefined | Value::Null) { Value::Obj(self.global_obj.clone()) } else { this };
        let lp = bp + argc;
        stack.resize(lp + code.nlocals as usize, Value::Undefined);
        let env = if code.has_env { Some(Rc::new(Env::new(code.env_size, env))) } else { env };
        Frame { code, pc: 0, bp, argc, lp, env, this, func, handlers: Vec::new(), construct }
    }

    /// A base class's instance fields, set up on `this` before its constructor runs.
    fn run_fields(&mut self, ctor: &Value, this: &Value) -> Res<()> {
        let init = match ctor {
            Value::Obj(o) => match o.borrow().get_own(FIELDS) {
                Some(Slot::Data(f)) => f.clone(),
                _ => return Ok(()),
            },
            _ => return Ok(()),
        };
        self.call(&init, this.clone(), vec![])?;
        Ok(())
    }

    /// Runs a compiled script with `this`; its completion value.
    pub(crate) fn run_code(&mut self, code: Rc<Code>, this: Value) -> Res<Value> {
        let mut stack = self.take_stack();
        stack.push(this.clone());
        stack.push(Value::Undefined);
        let frame = self.make_frame(&mut stack, code, None, this, Value::Undefined, 2, 0, false);
        match self.execute(stack, frame, Input::Start)? {
            Exit::Return(v) => Ok(v),
            _ => Ok(Value::Undefined),
        }
    }

    /// Calls a compiled function (from Rust: a native, a timer, an event).
    pub(crate) fn call_closure(&mut self, f: &Value, this: Value, args: Vec<Value>, construct: bool) -> Res<Value> {
        let (code, env, base_class) = match f {
            Value::Obj(o) => match &o.borrow().kind {
                Kind::Function(Callable::Closure(c)) => (c.code.clone(), c.env.clone(), c.code.class_ctor && c.sup.is_none()),
                _ => return self.throw("TypeError", "not a function"),
            },
            _ => return self.throw("TypeError", "not a function"),
        };
        if base_class {
            self.run_fields(f, &this)?;
        }
        let mut stack = if code.is_gen || code.is_async { Vec::with_capacity(16) } else { self.take_stack() };
        stack.push(this.clone());
        stack.push(f.clone());
        let argc = args.len();
        stack.extend(args);
        let frame = self.make_frame(&mut stack, code.clone(), env, this, f.clone(), 2, argc, construct);
        if code.is_gen {
            let st = GenState { susp: Some(Suspended { frame, stack }), started: false, running: false, is_async: code.is_async };
            let g = self.obj(Kind::Generator(Rc::new(RefCell::new(st))));
            if let Value::Obj(o) = &g {
                o.borrow_mut().proto = Some(self.generator_proto.clone());
            }
            return Ok(g);
        }
        if code.is_async {
            let (p, _) = self.new_promise();
            let _ = &code;
            self.drive_async(Suspended { frame, stack }, Input::Start, p.clone());
            return Ok(p);
        }
        match self.execute(stack, frame, Input::Start)? {
            Exit::Return(v) => Ok(v),
            _ => Ok(Value::Undefined),
        }
    }

    /// Runs (or resumes) an async function's frame; settles `p` when it ends.
    pub(crate) fn drive_async(&mut self, s: Suspended, input: Input, p: Value) {
        match self.execute(s.stack, s.frame, input) {
            Ok(Exit::Return(v)) => self.resolve_promise(&p, v),
            Err(e) => self.reject_promise(&p, e),
            Ok(Exit::Await(v, s)) => {
                let awaited = self.promise_resolve(v);
                let cell = Rc::new(RefCell::new(Some(s)));
                let (c1, p1) = (cell.clone(), p.clone());
                let on_ok = self.native(move |i, _t, a| {
                    if let Some(s) = c1.borrow_mut().take() {
                        i.drive_async(s, Input::Next(a.into_iter().next().unwrap_or(Value::Undefined)), p1.clone());
                    }
                    Ok(Value::Undefined)
                });
                let on_err = self.native(move |i, _t, a| {
                    if let Some(s) = cell.borrow_mut().take() {
                        i.drive_async(s, Input::Throw(a.into_iter().next().unwrap_or(Value::Undefined)), p.clone());
                    }
                    Ok(Value::Undefined)
                });
                self.then_promise(&awaited, on_ok, on_err);
            }
            Ok(Exit::InitDone(s)) => self.drive_async(s, Input::Start, p),
            Ok(Exit::Yield(_)) => self.resolve_promise(&p, Value::Undefined),
        }
    }

    /// `gen.next(v)` / `gen.throw(e)` / `gen.return(v)` (`kind` 0, 1, 2).
    pub(crate) fn gen_resume(&mut self, g: &Value, kind: u8, v: Value) -> Res<Value> {
        let Value::Obj(o) = g else {
            return self.throw("TypeError", "not a generator");
        };
        let st = match &o.borrow().kind {
            Kind::Generator(st) => st.clone(),
            _ => return self.throw("TypeError", "not a generator"),
        };
        if st.borrow().running {
            return self.throw("TypeError", "the generator is already running");
        }
        let susp = st.borrow_mut().susp.take();
        let Some(mut susp) = susp else {
            return match kind {
                1 => Err(v),
                2 => Ok(result_obj(self, v, true)),
                _ => Ok(result_obj(self, Value::Undefined, true)),
            };
        };
        let started = std::mem::replace(&mut st.borrow_mut().started, true);
        let input = match kind {
            // return() before the first next() finishes without running the body.
            2 if !started => return Ok(result_obj(self, v, true)),
            2 => Input::Resume(v, true),
            1 => Input::Throw(v),
            _ if started => Input::Resume(v, false),
            _ => Input::Start,
        };
        st.borrow_mut().running = true;
        // The generator's frame runs on its own stack; it comes back on `yield`.
        let stack = std::mem::take(&mut susp.stack);
        let r = self.execute_gen(stack, susp.frame, input);
        let mut b = st.borrow_mut();
        b.running = false;
        match r {
            Ok((Exit::Yield(y), s)) => {
                b.susp = s;
                drop(b);
                Ok(result_obj(self, y, false))
            }
            Ok((Exit::Return(r), _)) => {
                drop(b);
                Ok(result_obj(self, r, true))
            }
            Ok(_) => {
                drop(b);
                Ok(result_obj(self, Value::Undefined, true))
            }
            Err(e) => Err(e),
        }
    }

    /// Like [`execute`](Self::execute), keeping the frame when it yields.
    fn execute_gen(&mut self, stack: Vec<Value>, frame: Frame, input: Input) -> Res<(Exit, Option<Suspended>)> {
        self.yielded = None;
        let r = self.execute(stack, frame, input)?;
        Ok((r, self.yielded.take()))
    }

    // ------------------------------------------------------------ modules

    /// A module's top-level frame, run up to its `InitDone` (functions hoisted).
    pub(crate) fn module_frame(&mut self, code: Rc<Code>) -> Res<(Suspended, Rc<Env>)> {
        let mut stack = Vec::with_capacity(32);
        stack.push(Value::Undefined);
        stack.push(Value::Undefined);
        let frame = self.make_frame(&mut stack, code, None, Value::Undefined, Value::Undefined, 2, 0, false);
        let env = frame.env.clone().expect("a module environment");
        match self.execute(stack, frame, Input::Start)? {
            Exit::InitDone(s) => Ok((s, env)),
            _ => self.throw("SyntaxError", "module without a body"),
        }
    }

    /// Runs a linked module's body; an error it throws before its first `await`.
    pub(crate) fn module_run(&mut self, s: Suspended) -> Res<()> {
        let (p, st) = self.new_promise();
        self.drive_async(s, Input::Start, p);
        let r = match &*st.borrow() {
            PState::Rejected(e) => Err(e.clone()),
            _ => Ok(()),
        };
        r
    }

    // ------------------------------------------------------------ the loop

    pub(crate) fn execute(&mut self, mut stack: Vec<Value>, mut f: Frame, input: Input) -> Res<Exit> {
        let mut frames: Vec<Frame> = Vec::new();
        self.enter(&f.code);
        // A call from native code (a callback, a getter) counts against the budget too.
        self.steps += 1;
        if self.steps > self.max_steps {
            self.leave();
            return Err(self.error("RangeError", "the script ran too long and was stopped"));
        }
        let mut code = f.code.clone();
        let mut pc = f.pc;
        let mut pending = None;
        match input {
            Input::Start => {}
            Input::Next(v) => stack.push(v),
            Input::Throw(e) => pending = Some(e),
            Input::Resume(v, ret) => {
                stack.push(v);
                stack.push(Value::Bool(ret));
            }
        }
        let frame_limit = self.max_depth.saturating_mul(16).max(1000);
        'outer: loop {
            let err: Value = match pending.take() {
                Some(e) => e,
                None => 'inner: loop {
                    macro_rules! tri {
                        ($e:expr) => {
                            match $e {
                                Ok(v) => v,
                                Err(e) => break 'inner e,
                            }
                        };
                    }
                    macro_rules! pop {
                        () => {
                            stack.pop().unwrap_or(Value::Undefined)
                        };
                    }
                    macro_rules! top {
                        () => {
                            stack.last_mut().expect("an operand")
                        };
                    }
                    macro_rules! num2 {
                        ($a:ident, $b:ident, $body:expr) => {{
                            let rb = pop!();
                            let ra = pop!();
                            let ($a, $b) = match (&ra, &rb) {
                                (Value::Num(x), Value::Num(y)) => (*x, *y),
                                _ => (tri!(self.to_number(&ra)), tri!(self.to_number(&rb))),
                            };
                            stack.push(Value::Num($body));
                        }};
                    }
                    macro_rules! int2 {
                        ($a:ident, $b:ident, $body:expr) => {{
                            let rb = pop!();
                            let ra = pop!();
                            let (x, y) = match (&ra, &rb) {
                                (Value::Num(x), Value::Num(y)) => (*x, *y),
                                _ => (tri!(self.to_number(&ra)), tri!(self.to_number(&rb))),
                            };
                            let ($a, $b) = (to_i32(x), to_i32(y));
                            stack.push(Value::Num($body));
                        }};
                    }
                    // The budget is counted at each loop's backward jump and each call
                    // (not at every instruction), so straight-line code pays nothing.
                    macro_rules! tick {
                        () => {
                            self.steps += 1;
                            if self.steps & 127 == 0 {
                                if self.steps > self.max_steps {
                                    break 'inner self.error("RangeError", "the script ran too long and was stopped");
                                }
                                if let Some(p) = &mut self.profile {
                                    let at = self.call_names.last().cloned().unwrap_or((None, 0));
                                    *p.entry(at).or_insert(0) += 1;
                                }
                            }
                        };
                    }
                    macro_rules! jump {
                        ($to:expr) => {{
                            let to = $to as usize;
                            if to < pc {
                                tick!();
                            }
                            pc = to
                        }};
                    }
                    // A comparison and the conditional jump after it, taken together:
                    // the boolean never goes onto the stack.
                    macro_rules! branch {
                        ($b:expr) => {{
                            let b: bool = $b;
                            match code.ops.get(pc) {
                                Some(Op::JumpIfFalse(to)) => {
                                    if b {
                                        pc += 1
                                    } else {
                                        jump!(*to)
                                    }
                                }
                                Some(Op::JumpIfTrue(to)) => {
                                    if b {
                                        jump!(*to)
                                    } else {
                                        pc += 1
                                    }
                                }
                                _ => stack.push(Value::Bool(b)),
                            }
                        }};
                    }
                    macro_rules! cmp {
                        ($test:expr) => {{
                            let rb = pop!();
                            let ra = pop!();
                            let r = match (&ra, &rb) {
                                (Value::Num(x), Value::Num(y)) => x.partial_cmp(y),
                                (Value::Str(x), Value::Str(y)) => Some(str_cmp(x, y)),
                                _ => tri!(self.compare(&ra, &rb)),
                            };
                            let t: fn(std::cmp::Ordering) -> bool = $test;
                            branch!(r.map(t).unwrap_or(false));
                        }};
                    }
                    let op = code.ops[pc];
                    pc += 1;
                    match op {
                        Op::Nop => {}
                        Op::Undef => stack.push(Value::Undefined),
                        Op::Null => stack.push(Value::Null),
                        Op::True => stack.push(Value::Bool(true)),
                        Op::False => stack.push(Value::Bool(false)),
                        Op::Int(n) => stack.push(Value::Num(n as f64)),
                        Op::Const(c) => stack.push(code.consts[c as usize].clone()),
                        Op::Pop => {
                            stack.pop();
                        }
                        Op::Dup => {
                            let v = top!().clone();
                            stack.push(v);
                        }
                        Op::Dup2 => {
                            let n = stack.len();
                            let (a, b) = (stack[n - 2].clone(), stack[n - 1].clone());
                            stack.push(a);
                            stack.push(b);
                        }
                        Op::Swap => {
                            let n = stack.len();
                            stack.swap(n - 1, n - 2);
                        }
                        Op::Over => {
                            let v = stack[stack.len() - 2].clone();
                            stack.push(v);
                        }
                        Op::Rot3 => {
                            let n = stack.len();
                            stack[n - 3..].rotate_left(1);
                        }
                        Op::Nip => {
                            let v = pop!();
                            *top!() = v;
                        }
                        Op::IncLocal(i, d) => {
                            let slot = f.lp + i as usize;
                            match &mut stack[slot] {
                                Value::Num(n) => *n += d as f64,
                                v => {
                                    let v = v.clone();
                                    let n = tri!(self.to_number(&v));
                                    stack[slot] = Value::Num(n + d as f64);
                                }
                            }
                        }
                        Op::GetLocal(i) => stack.push(stack[f.lp + i as usize].clone()),
                        Op::SetLocal(i) => {
                            let v = top!().clone();
                            stack[f.lp + i as usize] = v;
                        }
                        Op::PutLocal(i) => {
                            let v = pop!();
                            stack[f.lp + i as usize] = v;
                        }
                        Op::GetEnv(d, i) => {
                            let v = match env_at(&f.env, d) {
                                Some(e) => e.vars.borrow()[i as usize].clone(),
                                None => break 'inner self.error("ReferenceError", "a variable's environment is missing"),
                            };
                            stack.push(v);
                        }
                        Op::SetEnv(d, i) | Op::PutEnv(d, i) => {
                            let v = if matches!(op, Op::PutEnv(..)) { pop!() } else { top!().clone() };
                            match env_at(&f.env, d) {
                                Some(e) => e.vars.borrow_mut()[i as usize] = v,
                                None => break 'inner self.error("ReferenceError", "a variable's environment is missing"),
                            }
                        }
                        Op::GetGlobal(k, ic) => {
                            let v = tri!(self.get_global(cstr(&code, k), &code.ics[ic as usize]));
                            stack.push(v);
                        }
                        Op::SetGlobal(k, ic) => {
                            let v = top!().clone();
                            tri!(self.set_global(cstr(&code, k), v, &code.ics[ic as usize]));
                        }
                        Op::PutGlobal(k, ic) => {
                            let v = pop!();
                            tri!(self.set_global(cstr(&code, k), v, &code.ics[ic as usize]));
                        }
                        Op::TypeofGlobal(k) => {
                            let k = cstr(&code, k);
                            let v = if self.global_lex.borrow().get_own(k).is_some() || self.has_property(&self.global_obj.clone(), k) {
                                let v = tri!(self.get_global(k, &Cell::new(0)));
                                Interp::typeof_(&v)
                            } else {
                                "undefined"
                            };
                            stack.push(Value::Str(v.into()));
                        }
                        Op::DeclareLex(k, constant) => {
                            let v = pop!();
                            let k = cstr(&code, k);
                            let mut lex = self.global_lex.borrow_mut();
                            lex.set_own_rc(k, v, 0);
                            lex.set_readonly(k, constant);
                        }
                        Op::DeclareVar(k) => {
                            let k = cstr(&code, k);
                            if self.global_lex.borrow().get_own(k).is_none() && self.global_obj.borrow().get_own(k).is_none() {
                                self.global_obj.borrow_mut().set_own_rc(k, Value::Undefined, 0);
                            }
                        }
                        Op::ConstAssign(k) => break 'inner self.error("TypeError", &format!("assignment to constant '{}'", cstr(&code, k))),
                        Op::PushEnv(n) => f.env = Some(Rc::new(Env::new(n, f.env.take()))),
                        Op::PopEnv => f.env = f.env.take().and_then(|e| e.parent.clone()),
                        Op::CloneEnv => {
                            if let Some(e) = &f.env {
                                let copy = Env { vars: RefCell::new(e.vars.borrow().clone()), parent: e.parent.clone() };
                                f.env = Some(Rc::new(copy));
                            }
                        }
                        Op::WithGet(k, skip) => {
                            let w = pop!();
                            let k = cstr(&code, k);
                            if tri!(self.has(&w, k)) {
                                let v = tri!(self.get(&w, k));
                                stack.push(v);
                                pc += skip as usize;
                            }
                        }
                        Op::WithSet(k, flags) => {
                            let w = pop!();
                            let k = cstr(&code, k);
                            if tri!(self.has(&w, k)) {
                                let v = top!().clone();
                                tri!(self.set(&w, k, v));
                                if flags & 0x8000_0000 != 0 {
                                    stack.pop();
                                }
                                pc += (flags & 0x7fff_ffff) as usize;
                            }
                        }
                        Op::GetProp(k, ic) => {
                            let key = cstr(&code, k);
                            let cell = &code.ics[ic as usize];
                            let fast = match stack.last() {
                                Some(Value::Obj(o)) => fast_get(o, key, cell),
                                _ => None,
                            };
                            let v = match fast {
                                Some(v) => v,
                                None => {
                                    let o = pop!();
                                    let v = tri!(self.get_hint(&o, key, Some(cell)));
                                    stack.push(v);
                                    continue;
                                }
                            };
                            *top!() = v;
                        }
                        Op::GetMethod(k, ic) => {
                            let key = cstr(&code, k);
                            let cell = &code.ics[ic as usize];
                            let fast = match stack.last() {
                                Some(Value::Obj(o)) => fast_get(o, key, cell),
                                _ => None,
                            };
                            let v = match fast {
                                Some(v) => v,
                                None => {
                                    let o = top!().clone();
                                    tri!(self.get_hint(&o, key, Some(cell)))
                                }
                            };
                            stack.push(v);
                        }
                        Op::SetProp(k, ic) => {
                            let v = pop!();
                            let o = pop!();
                            tri!(self.set_hint(&o, cstr(&code, k), v.clone(), Some(&code.ics[ic as usize])));
                            stack.push(v);
                        }
                        Op::GetElem => {
                            let k = pop!();
                            let o = pop!();
                            let v = tri!(self.get_elem(&o, &k));
                            stack.push(v);
                        }
                        Op::GetElemMethod => {
                            let k = pop!();
                            let o = top!().clone();
                            let v = tri!(self.get_elem(&o, &k));
                            stack.push(v);
                        }
                        Op::SetElem => {
                            let v = pop!();
                            let k = pop!();
                            let o = pop!();
                            tri!(self.set_elem(&o, &k, v.clone()));
                            stack.push(v);
                        }
                        Op::DelProp(k) => {
                            let o = pop!();
                            let r = tri!(self.delete_property(&o, cstr(&code, k)));
                            stack.push(Value::Bool(r));
                        }
                        Op::DelElem => {
                            let k = pop!();
                            let o = pop!();
                            let k = tri!(self.key(&k));
                            let r = tri!(self.delete_property(&o, &k));
                            stack.push(Value::Bool(r));
                        }
                        Op::DelGlobal(k) => {
                            self.global_obj.borrow_mut().delete(cstr(&code, k));
                            stack.push(Value::Bool(true));
                        }
                        Op::In => {
                            let o = pop!();
                            let k = pop!();
                            if !matches!(o, Value::Obj(_)) {
                                break 'inner self.error("TypeError", "'in' needs an object");
                            }
                            let k = tri!(self.key(&k));
                            let r = tri!(self.has(&o, &k));
                            stack.push(Value::Bool(r));
                        }
                        Op::InstanceOf => {
                            let c = pop!();
                            let v = pop!();
                            let r = tri!(self.instance_of(&v, &c));
                            stack.push(Value::Bool(r));
                        }
                        Op::Add => {
                            let b = pop!();
                            let a = pop!();
                            let r = match (&a, &b) {
                                (Value::Num(x), Value::Num(y)) => Value::Num(x + y),
                                (Value::Str(x), Value::Str(y)) => Value::Str(crate::interp::concat_str(x, y)),
                                _ => tri!(self.add_slow(&a, &b)),
                            };
                            stack.push(r);
                        }
                        Op::Sub => num2!(x, y, x - y),
                        Op::Mul => num2!(x, y, x * y),
                        Op::Div => num2!(x, y, x / y),
                        Op::Mod => num2!(x, y, {
                            // Integers (the usual case) skip the floating-point remainder.
                            if x.fract() == 0.0 && y.fract() == 0.0 && x.abs() < 9.0e15 && y.abs() < 9.0e15 && y != 0.0 {
                                let r = (x as i64 % y as i64) as f64;
                                if r == 0.0 && x < 0.0 {
                                    -0.0
                                } else {
                                    r
                                }
                            } else {
                                x % y
                            }
                        }),
                        Op::Exp => num2!(x, y, x.powf(y)),
                        Op::BitAnd => int2!(x, y, (x & y) as f64),
                        Op::BitOr => int2!(x, y, (x | y) as f64),
                        Op::BitXor => int2!(x, y, (x ^ y) as f64),
                        Op::Shl => int2!(x, y, x.wrapping_shl(y as u32 & 31) as f64),
                        Op::Shr => int2!(x, y, x.wrapping_shr(y as u32 & 31) as f64),
                        Op::UShr => int2!(x, y, (x as u32).wrapping_shr(y as u32 & 31) as f64),
                        Op::Eq | Op::Ne => {
                            let b = pop!();
                            let a = pop!();
                            let r = match (&a, &b) {
                                (Value::Num(x), Value::Num(y)) => x == y,
                                (Value::Str(x), Value::Str(y)) => x == y,
                                _ => tri!(self.loose_eq(&a, &b)),
                            };
                            stack.push(Value::Bool(r == matches!(op, Op::Eq)));
                        }
                        Op::StrictEq | Op::StrictNe => {
                            let b = pop!();
                            let a = pop!();
                            let r = Interp::strict_eq(&a, &b);
                            branch!(r == matches!(op, Op::StrictEq));
                        }
                        Op::Lt => cmp!(|o| o.is_lt()),
                        Op::Gt => cmp!(|o| o.is_gt()),
                        Op::Le => cmp!(|o| o.is_le()),
                        Op::Ge => cmp!(|o| o.is_ge()),
                        Op::Not => {
                            let t = top!();
                            *t = Value::Bool(!Interp::truthy(t));
                        }
                        Op::Neg => {
                            let v = pop!();
                            let n = match v {
                                Value::Num(n) => n,
                                v => tri!(self.to_number(&v)),
                            };
                            stack.push(Value::Num(-n));
                        }
                        Op::ToNum => {
                            if !matches!(stack.last(), Some(Value::Num(_))) {
                                let v = pop!();
                                let n = tri!(self.to_number(&v));
                                stack.push(Value::Num(n));
                            }
                        }
                        Op::BitNot => {
                            let v = pop!();
                            let n = tri!(self.to_number(&v));
                            stack.push(Value::Num(!to_i32(n) as f64));
                        }
                        Op::Typeof => {
                            let v = pop!();
                            stack.push(Value::Str(Interp::typeof_(&v).into()));
                        }
                        Op::Inc | Op::Dec => {
                            if let Some(Value::Num(n)) = stack.last_mut() {
                                *n += if matches!(op, Op::Inc) { 1.0 } else { -1.0 };
                            }
                        }
                        Op::Jump(to) => jump!(to),
                        Op::JumpIfFalse(to) => {
                            if !Interp::truthy(&pop!()) {
                                jump!(to);
                            }
                        }
                        Op::JumpIfTrue(to) => {
                            if Interp::truthy(&pop!()) {
                                jump!(to);
                            }
                        }
                        Op::JumpIfFalseKeep(to) => {
                            if !Interp::truthy(top!()) {
                                jump!(to);
                            } else {
                                stack.pop();
                            }
                        }
                        Op::JumpIfTrueKeep(to) => {
                            if Interp::truthy(top!()) {
                                jump!(to);
                            } else {
                                stack.pop();
                            }
                        }
                        Op::JumpIfNotNullishKeep(to) => {
                            if !matches!(top!(), Value::Undefined | Value::Null) {
                                jump!(to);
                            } else {
                                stack.pop();
                            }
                        }
                        Op::JumpIfNullish(to) => {
                            if matches!(top!(), Value::Undefined | Value::Null) {
                                *top!() = Value::Undefined;
                                jump!(to);
                            }
                        }
                        Op::JumpIfNullishCall(to) => {
                            if matches!(top!(), Value::Undefined | Value::Null) {
                                stack.pop();
                                *top!() = Value::Undefined;
                                jump!(to);
                            }
                        }
                        Op::Return => {
                            let mut v = pop!();
                            if f.construct && !matches!(v, Value::Obj(_)) {
                                v = f.this.clone();
                            }
                            self.leave();
                            match frames.pop() {
                                Some(prev) => {
                                    stack.truncate(f.bp - 2);
                                    stack.push(v);
                                    f = prev;
                                    code = f.code.clone();
                                    pc = f.pc;
                                }
                                None => {
                                    self.give_stack(stack);
                                    return Ok(Exit::Return(v));
                                }
                            }
                        }
                        Op::Throw => break 'inner pop!(),
                        Op::TryStart(to) => f.handlers.push(Handler { pc: to, sp: stack.len(), env: f.env.clone() }),
                        Op::TryEnd => {
                            f.handlers.pop();
                        }
                        Op::Call(_) | Op::CallSpread | Op::New(_) | Op::NewSpread => {
                            let is_new = matches!(op, Op::New(_) | Op::NewSpread);
                            let argc = match op {
                                Op::Call(n) | Op::New(n) => n as usize,
                                _ => {
                                    // The arguments array (made just now by the spread) goes onto the stack.
                                    let list = pop!();
                                    let items = match &list {
                                        Value::Obj(o) => match &mut o.borrow_mut().kind {
                                            Kind::Array(a) => std::mem::take(a),
                                            _ => Vec::new(),
                                        },
                                        _ => Vec::new(),
                                    };
                                    let n = items.len();
                                    stack.extend(items);
                                    n
                                }
                            };
                            let fpos = stack.len() - argc - 1;
                            let inline = match &stack[fpos] {
                                Value::Obj(o) => match &o.borrow().kind {
                                    Kind::Function(Callable::Closure(c)) if !c.code.is_gen && !c.code.is_async && !(is_new && c.code.arrow) => {
                                        Some((c.code.clone(), c.env.clone(), c.code.class_ctor && c.sup.is_none()))
                                    }
                                    _ => None,
                                },
                                _ => None,
                            };
                            tick!();
                            if let Some((c2, env2, base_class)) = inline {
                                if self.frames >= frame_limit {
                                    break 'inner self.recursion_error();
                                }
                                let func = stack[fpos].clone();
                                let (this, bp) = if is_new {
                                    let this = tri!(self.new_this(&func));
                                    stack.insert(fpos, this.clone());
                                    (this, fpos + 2)
                                } else {
                                    (stack[fpos - 1].clone(), fpos + 1)
                                };
                                if base_class {
                                    tri!(self.run_fields(&func, &this));
                                }
                                f.pc = pc;
                                let nf = self.make_frame(&mut stack, c2, env2, this, func, bp, argc, is_new);
                                frames.push(std::mem::replace(&mut f, nf));
                                code = f.code.clone();
                                pc = 0;
                                self.enter(&code);
                                continue;
                            }
                            if !Interp::is_callable(&stack[fpos]) {
                                let at = (pc - 1) as u32;
                                let what = code.call_names.iter().find(|(p, _)| *p == at).map(|(_, d)| d.to_string()).unwrap_or_else(|| "expression".into());
                                break 'inner self.error("TypeError", &format!("{what} is not a {}", if is_new { "constructor" } else { "function" }));
                            }
                            let args: Vec<Value> = stack.drain(fpos + 1..).collect();
                            let func = pop!();
                            let r = if is_new {
                                tri!(self.construct(&func, args))
                            } else {
                                let this = pop!();
                                tri!(self.call(&func, this, args))
                            };
                            stack.push(r);
                        }
                        Op::SuperCall(_) | Op::SuperCallSpread => {
                            let args: Vec<Value> = match op {
                                Op::SuperCall(n) => stack.drain(stack.len() - n as usize..).collect(),
                                _ => {
                                    let list = pop!();
                                    tri!(self.iterate(&list))
                                }
                            };
                            let parent = sup_of(&f.func).unwrap_or(Value::Undefined);
                            let this = f.this.clone();
                            let native = matches!(&parent, Value::Obj(o) if matches!(o.borrow().kind, Kind::Function(Callable::Native(_))));
                            if native {
                                // A built-in parent (Error, say): copy what it makes onto this.
                                let made = tri!(self.construct(&parent, args));
                                if let (Value::Obj(src), Value::Obj(dst)) = (&made, &this) {
                                    if !Rc::ptr_eq(src, dst) {
                                        let keys = src.borrow().own_keys();
                                        for k in keys {
                                            let v = tri!(self.get(&made, &k));
                                            dst.borrow_mut().set_own(&k, v);
                                        }
                                    }
                                }
                            } else if Interp::is_callable(&parent) {
                                if matches!(&parent, Value::Obj(o) if matches!(o.borrow().kind, Kind::Proxy(..))) {
                                    tri!(self.call(&parent, this.clone(), args));
                                } else {
                                    tri!(self.call_closure(&parent, this.clone(), args, true));
                                }
                            } else {
                                break 'inner self.error("TypeError", "super() outside a derived class");
                            }
                            let ctor = f.func.clone();
                            tri!(self.run_fields(&ctor, &this));
                            stack.push(Value::Undefined);
                        }
                        Op::Closure(i) => {
                            let c = code.funcs[i as usize].clone();
                            let fo = Object::new(Some(self.function_proto.clone()), Kind::Function(Callable::Closure(Closure { code: c, env: f.env.clone(), sup: None })));
                            stack.push(Value::Obj(Rc::new(RefCell::new(fo))));
                        }
                        Op::This => stack.push(f.this.clone()),
                        Op::NewTarget => stack.push(if f.construct { f.func.clone() } else { Value::Undefined }),
                        Op::Callee => stack.push(f.func.clone()),
                        Op::Arguments => {
                            let a = stack[f.bp..f.bp + f.argc].to_vec();
                            stack.push(self.array(a));
                        }
                        Op::LoadSuper => stack.push(sup_of(&f.func).unwrap_or(Value::Undefined)),
                        Op::Arg(i) => {
                            let v = if (i as usize) < f.argc { stack[f.bp + i as usize].clone() } else { Value::Undefined };
                            stack.push(v);
                        }
                        Op::ArgsRest(i) => {
                            let from = (f.bp + i as usize).min(f.bp + f.argc);
                            let a = stack[from..f.bp + f.argc].to_vec();
                            stack.push(self.array(a));
                        }
                        Op::NewObject => stack.push(self.obj(Kind::Plain)),
                        Op::NewArray(n) => {
                            let items: Vec<Value> = stack.drain(stack.len() - n as usize..).collect();
                            stack.push(self.array(items));
                        }
                        Op::ArrayPush => {
                            let v = pop!();
                            if let Value::Obj(o) = top!() {
                                if let Kind::Array(a) = &mut o.borrow_mut().kind {
                                    a.push(v);
                                }
                            }
                        }
                        Op::ArraySpread => {
                            let it = pop!();
                            let items = tri!(self.iterate(&it));
                            if let Value::Obj(o) = top!() {
                                if let Kind::Array(a) = &mut o.borrow_mut().kind {
                                    a.extend(items);
                                }
                            }
                        }
                        Op::InitProp(k) => {
                            let v = pop!();
                            if let Value::Obj(o) = top!() {
                                o.borrow_mut().set_own_rc(cstr(&code, k), v, 0);
                            }
                        }
                        Op::InitElem => {
                            let v = pop!();
                            let k = pop!();
                            let k = tri!(self.key(&k));
                            if let Value::Obj(o) = top!() {
                                o.borrow_mut().set_own(&k, v);
                            }
                        }
                        Op::InitAccessor(k, getter) => {
                            let fv = pop!();
                            if let Value::Obj(o) = top!() {
                                define_accessor(o, cstr(&code, k), fv, getter, true);
                            }
                        }
                        Op::InitAccessorElem(getter) => {
                            let fv = pop!();
                            let k = pop!();
                            let k = tri!(self.key(&k));
                            if let Value::Obj(o) = top!() {
                                define_accessor(o, &k, fv, getter, true);
                            }
                        }
                        Op::ObjSpread => {
                            let src = pop!();
                            let dst = top!().clone();
                            tri!(self.copy_props(&dst, &src, &[]));
                        }
                        Op::InitProto => {
                            let p = pop!();
                            if let Value::Obj(o) = top!() {
                                match p {
                                    Value::Obj(p) => o.borrow_mut().proto = Some(p),
                                    Value::Null => o.borrow_mut().proto = None,
                                    _ => {}
                                }
                            }
                        }
                        Op::MakeClass(has_sup) => {
                            let ctor = pop!();
                            let sup = if has_sup { Some(pop!()) } else { None };
                            let proto = tri!(self.get(&ctor, "prototype"));
                            if let Some(s) = &sup {
                                match s {
                                    Value::Null => {
                                        if let Value::Obj(p) = &proto {
                                            p.borrow_mut().proto = None;
                                        }
                                    }
                                    s if Interp::is_callable(s) => {
                                        let pp = tri!(self.get(s, "prototype"));
                                        if let (Value::Obj(p), Value::Obj(pp)) = (&proto, &pp) {
                                            p.borrow_mut().proto = Some(pp.clone());
                                        }
                                        if let (Value::Obj(c), Value::Obj(so)) = (&ctor, s) {
                                            c.borrow_mut().proto = Some(so.clone());
                                        }
                                        set_sup(&ctor, Some(s.clone()));
                                    }
                                    other => break 'inner self.error("TypeError", &format!("class extends {other:?}, which isn't a constructor")),
                                }
                            }
                            stack.push(ctor);
                            stack.push(proto);
                        }
                        Op::DefMethod(_, flags) | Op::DefMethodElem(flags) => {
                            let fv = pop!();
                            let key = match op {
                                Op::DefMethod(k, _) => cstr(&code, k).clone(),
                                _ => {
                                    let k = pop!();
                                    tri!(self.key(&k))
                                }
                            };
                            let n = stack.len();
                            let (ctor, proto) = (stack[n - 2].clone(), stack[n - 1].clone());
                            set_sup(&fv, sup_of(&ctor));
                            let target = if flags & 1 != 0 { ctor } else { proto };
                            if let Value::Obj(t) = &target {
                                if flags & 6 != 0 {
                                    define_accessor(t, &key, fv, flags & 2 != 0, false);
                                } else {
                                    t.borrow_mut().set_hidden(&key, fv);
                                }
                            }
                        }
                        Op::SetFields => {
                            let fv = pop!();
                            let ctor = stack[stack.len() - 2].clone();
                            set_sup(&fv, sup_of(&ctor));
                            if let Value::Obj(c) = &ctor {
                                c.borrow_mut().set_hidden(FIELDS, fv);
                            }
                        }
                        Op::RunStatic => {
                            let fv = pop!();
                            let ctor = stack[stack.len() - 2].clone();
                            set_sup(&fv, sup_of(&ctor));
                            tri!(self.call(&fv, ctor, vec![]));
                        }
                        Op::GetIter => {
                            let v = pop!();
                            let it = tri!(self.get_iter(v));
                            stack.push(it);
                        }
                        Op::IterNext(exit) => {
                            let it = top!().clone();
                            match tri!(self.iter_next(&it)) {
                                Some(v) => stack.push(v),
                                None => jump!(exit),
                            }
                        }
                        Op::IterValue => {
                            let it = top!().clone();
                            let v = tri!(self.iter_next(&it)).unwrap_or(Value::Undefined);
                            stack.push(v);
                        }
                        Op::IterRest => {
                            let it = top!().clone();
                            let mut out = Vec::new();
                            while let Some(v) = tri!(self.iter_next(&it)) {
                                out.push(v);
                            }
                            stack.push(self.array(out));
                        }
                        Op::IterClose => {
                            let it = pop!();
                            tri!(self.iter_close(&it));
                        }
                        Op::ForInKeys => {
                            let v = pop!();
                            let keys = tri!(self.for_in_keys(&v));
                            stack.push(self.obj(Kind::Iter(IterState::List(keys, 0))));
                        }
                        Op::ObjRest(c) => {
                            let src = top!().clone();
                            let names: Vec<&str> = match &code.consts[c as usize] {
                                Value::Str(s) if !s.is_empty() => s.split('\0').collect(),
                                _ => Vec::new(),
                            };
                            let rest = self.obj(Kind::Plain);
                            tri!(self.copy_props(&rest, &src, &names));
                            stack.push(rest);
                        }
                        Op::RequireObject => {
                            if matches!(top!(), Value::Undefined | Value::Null) {
                                let what = if matches!(top!(), Value::Null) { "null" } else { "undefined" };
                                break 'inner self.error("TypeError", &format!("can't destructure {what}"));
                            }
                        }
                        Op::Template(n) => {
                            let parts: Vec<Value> = stack.drain(stack.len() - n as usize..).collect();
                            let mut s = String::new();
                            for p in &parts {
                                match p {
                                    Value::Str(x) => s.push_str(x),
                                    Value::Num(x) => s.push_str(&num_to_string(*x)),
                                    p => s.push_str(&tri!(self.to_str(p))),
                                }
                            }
                            stack.push(Value::Str(s.into()));
                        }
                        Op::Regex(c) => {
                            let (Value::Str(body), Value::Str(flags)) = (code.consts[c as usize].clone(), code.consts[c as usize + 1].clone()) else { unreachable!() };
                            let r = tri!(self.regexp(&body, &flags));
                            stack.push(r);
                        }
                        Op::Yield => {
                            let v = pop!();
                            if !frames.is_empty() || !code.is_gen {
                                break 'inner self.error("SyntaxError", "yield outside a generator");
                            }
                            f.pc = pc;
                            self.leave();
                            self.yielded = Some(Suspended { frame: f, stack });
                            return Ok(Exit::Yield(v));
                        }
                        Op::Await => {
                            let v = pop!();
                            if frames.is_empty() && code.is_async && !code.is_gen {
                                f.pc = pc;
                                self.leave();
                                return Ok(Exit::Await(v, Suspended { frame: f, stack }));
                            }
                            // Not suspendable here: run the event loop until it settles.
                            let r = tri!(self.await_value(v));
                            stack.push(r);
                        }
                        Op::InitDone => {
                            if frames.is_empty() {
                                f.pc = pc;
                                self.leave();
                                return Ok(Exit::InitDone(Suspended { frame: f, stack }));
                            }
                        }
                        Op::Import => {
                            let spec = pop!();
                            let spec = tri!(self.to_str(&spec));
                            let base = code.url.clone().unwrap_or_else(|| self.base_url.clone());
                            let p = self.dynamic_import(&spec, &base);
                            stack.push(p);
                        }
                        Op::ImportMeta => {
                            let base = code.url.clone().unwrap_or_else(|| self.base_url.clone());
                            stack.push(self.import_meta(base));
                        }
                        Op::SetName(c) => {
                            if let Some(Value::Obj(o)) = stack.last() {
                                let named = matches!(o.borrow().get_own("name"), Some(Slot::Data(Value::Str(s))) if !s.is_empty());
                                if !named {
                                    o.borrow_mut().set_hidden("name", code.consts[c as usize].clone());
                                }
                            }
                        }
                    }
                },
            };
            // An exception: to the innermost handler, out through the frames.
            // The step budget can't be caught.
            let uncatchable = self.steps > self.max_steps;
            loop {
                if !uncatchable {
                    if let Some(h) = f.handlers.pop() {
                        stack.truncate(h.sp);
                        f.env = h.env;
                        stack.push(err);
                        pc = h.pc as usize;
                        continue 'outer;
                    }
                }
                self.leave();
                match frames.pop() {
                    Some(prev) => {
                        stack.truncate(f.bp.saturating_sub(2));
                        f = prev;
                        code = f.code.clone();
                    }
                    None => {
                        self.give_stack(stack);
                        return Err(err);
                    }
                }
            }
        }
    }

    // ------------------------------------------------------------ helpers

    fn get_global(&mut self, k: &Rc<str>, ic: &Cell<u32>) -> Res<Value> {
        let c = ic.get();
        {
            let lex = self.global_lex.borrow();
            if !lex.is_empty() {
                if let Some(i) = lex.find(k, if c & IC_LEX != 0 { c & !IC_LEX } else { u32::MAX }) {
                    ic.set(i as u32 | IC_LEX);
                    if let Slot::Data(v) = lex.slot_at(i) {
                        return Ok(v.clone());
                    }
                }
            }
            let g = self.global_obj.borrow();
            if let Some(i) = g.find(k, if c & IC_LEX == 0 { c } else { 0 }) {
                ic.set(i as u32);
                if let Slot::Data(v) = g.slot_at(i) {
                    return Ok(v.clone());
                }
            }
        }
        let g = Value::Obj(self.global_obj.clone());
        if self.has_property(&self.global_obj.clone(), k) {
            return self.get(&g, k);
        }
        if let Some(f) = self.global_fallback.clone() {
            if let Some(v) = f(self, k) {
                return Ok(v);
            }
        }
        self.throw("ReferenceError", &format!("{k} is not defined"))
    }

    fn set_global(&mut self, k: &Rc<str>, v: Value, ic: &Cell<u32>) -> Res<()> {
        let c = ic.get();
        {
            let mut lex = self.global_lex.borrow_mut();
            if !lex.is_empty() {
                if let Some(i) = lex.find(k, if c & IC_LEX != 0 { c & !IC_LEX } else { u32::MAX }) {
                    if lex.readonly_at(i) {
                        drop(lex);
                        return self.throw("TypeError", &format!("assignment to constant '{k}'"));
                    }
                    ic.set(i as u32 | IC_LEX);
                    *lex.slot_at_mut(i) = Slot::Data(v);
                    return Ok(());
                }
            }
        }
        {
            let mut g = self.global_obj.borrow_mut();
            if let Some(i) = g.find(k, if c & IC_LEX == 0 { c } else { 0 }) {
                ic.set(i as u32);
                if let Slot::Data(d) = g.slot_at_mut(i) {
                    *d = v;
                    return Ok(());
                }
            }
        }
        let g = Value::Obj(self.global_obj.clone());
        self.set(&g, k, v)
    }

    fn get_elem(&mut self, o: &Value, k: &Value) -> Res<Value> {
        if let Value::Num(n) = k {
            let i = *n as usize;
            if i as f64 == *n {
                match o {
                    Value::Obj(ob) => match &ob.borrow().kind {
                        Kind::Array(a) => {
                            if let Some(v) = a.get(i) {
                                return Ok(v.clone());
                            }
                        }
                        Kind::Typed(t) if !t.data_view => return Ok(t.get(i).map(Value::Num).unwrap_or(Value::Undefined)),
                        _ => {}
                    },
                    Value::Str(s) => return Ok(string_at(s, i)),
                    _ => {}
                }
            }
        }
        let key = match k {
            Value::Str(s) => s.clone(),
            k => self.key(k)?,
        };
        self.get(o, &key)
    }

    fn set_elem(&mut self, o: &Value, k: &Value, v: Value) -> Res<()> {
        if let (Value::Num(n), Value::Obj(ob)) = (k, o) {
            let i = *n as usize;
            if i as f64 == *n {
                if let (Kind::Typed(t), Value::Num(x)) = (&ob.borrow().kind, &v) {
                    if !t.data_view {
                        t.put(i, *x);
                        return Ok(());
                    }
                }
                if let Kind::Array(a) = &mut ob.borrow_mut().kind {
                    if i < a.len() {
                        a[i] = v;
                        return Ok(());
                    }
                    if i == a.len() {
                        a.push(v);
                        return Ok(());
                    }
                }
            }
        }
        let key = match k {
            Value::Str(s) => s.clone(),
            k => self.key(k)?,
        };
        self.set(o, &key, v)
    }

    fn add_slow(&mut self, a: &Value, b: &Value) -> Res<Value> {
        let (pa, pb) = (self.to_primitive(a, false)?, self.to_primitive(b, false)?);
        if matches!(pa, Value::Str(_)) || matches!(pb, Value::Str(_)) {
            let (x, y) = (self.to_str(&pa)?, self.to_str(&pb)?);
            Ok(Value::Str(crate::interp::concat_str(&x, &y)))
        } else {
            Ok(Value::Num(self.to_number(&pa)? + self.to_number(&pb)?))
        }
    }

    fn compare(&mut self, a: &Value, b: &Value) -> Res<Option<std::cmp::Ordering>> {
        let (pa, pb) = (self.to_primitive(a, true)?, self.to_primitive(b, true)?);
        Ok(if let (Value::Str(x), Value::Str(y)) = (&pa, &pb) {
            Some(str_cmp(x, y))
        } else {
            let (x, y) = (self.to_number(&pa)?, self.to_number(&pb)?);
            x.partial_cmp(&y)
        })
    }

    /// Copies `src`'s own enumerable properties onto `dst` (spreads and rests).
    fn copy_props(&mut self, dst: &Value, src: &Value, except: &[&str]) -> Res<()> {
        let Value::Obj(d) = dst else { return Ok(()) };
        match src {
            Value::Obj(_) => {
                for k in self.keys_of(src)? {
                    if except.contains(&&*k) {
                        continue;
                    }
                    let v = self.get(src, &k)?;
                    d.borrow_mut().set_own(&k, v);
                }
                // Symbol-keyed properties come along too.
                if let Value::Obj(s) = src {
                    let syms: Vec<(Rc<str>, Value)> = s
                        .borrow()
                        .entries()
                        .filter(|(k, s, e)| *e && k.starts_with("\0@") && matches!(s, Slot::Data(_)))
                        .map(|(k, s, _)| (k.clone(), if let Slot::Data(v) = s { v.clone() } else { Value::Undefined }))
                        .collect();
                    for (k, v) in syms {
                        d.borrow_mut().set_own(&k, v);
                    }
                }
            }
            Value::Str(s) => {
                for (i, c) in s.chars().enumerate() {
                    d.borrow_mut().set_own(&i.to_string(), Value::Str(c.to_string().into()));
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn get_iter(&mut self, v: Value) -> Res<Value> {
        let list = match &v {
            Value::Str(s) => Some(s.chars().map(|c| Value::Str(c.to_string().into())).collect()),
            Value::Obj(o) => match &o.borrow().kind {
                Kind::Array(_) => return Ok(self.obj(Kind::Iter(IterState::Array(o.clone(), 0)))),
                Kind::Typed(t) if !t.data_view => Some(t.values().into_iter().map(Value::Num).collect()),
                Kind::Set(s) => Some(s.keys().cloned().collect()),
                Kind::Map(m) => Some(m.iter().map(|(k, v)| self.array(vec![k.clone(), v.clone()])).collect()),
                _ => None,
            },
            _ => None,
        };
        if let Some(l) = list {
            return Ok(self.obj(Kind::Iter(IterState::List(l, 0))));
        }
        if matches!(v, Value::Undefined | Value::Null) {
            return self.throw("TypeError", &format!("{v:?} is not iterable"));
        }
        let f = self.get(&v, SYM_ITERATOR)?;
        if !Interp::is_callable(&f) {
            // An iterator itself, or an array-like.
            if Interp::is_callable(&self.get(&v, "next")?) {
                return Ok(v);
            }
            let items = self.iterate(&v)?;
            return Ok(self.obj(Kind::Iter(IterState::List(items, 0))));
        }
        let it = self.call(&f, v, vec![])?;
        if !matches!(it, Value::Obj(_)) {
            return self.throw("TypeError", "the iterator isn't an object");
        }
        Ok(it)
    }

    /// The iterator's next value, or None when it's done.
    pub(crate) fn iter_next(&mut self, it: &Value) -> Res<Option<Value>> {
        let Value::Obj(o) = it else { return Ok(None) };
        {
            let mut b = o.borrow_mut();
            match &mut b.kind {
                Kind::Iter(IterState::Array(a, i)) => {
                    let v = match &a.borrow().kind {
                        Kind::Array(items) => items.get(*i).cloned(),
                        _ => None,
                    };
                    if v.is_some() {
                        *i += 1;
                    }
                    return Ok(v);
                }
                Kind::Iter(IterState::List(l, i)) => {
                    let v = l.get(*i).cloned();
                    if v.is_some() {
                        *i += 1;
                    }
                    return Ok(v);
                }
                _ => {}
            }
        }
        let is_gen = matches!(o.borrow().kind, Kind::Generator(_));
        let r = if is_gen {
            self.gen_resume(it, 0, Value::Undefined)?
        } else {
            let next = self.get(it, "next")?;
            self.call(&next, it.clone(), vec![])?
        };
        if !matches!(r, Value::Obj(_)) {
            return self.throw("TypeError", "the iterator's result isn't an object");
        }
        if Interp::truthy(&self.get(&r, "done")?) {
            return Ok(None);
        }
        Ok(Some(self.get(&r, "value")?))
    }

    fn iter_close(&mut self, it: &Value) -> Res<()> {
        let Value::Obj(o) = it else { return Ok(()) };
        if matches!(o.borrow().kind, Kind::Iter(_)) {
            return Ok(());
        }
        let ret = self.get(it, "return")?;
        if Interp::is_callable(&ret) {
            self.call(&ret, it.clone(), vec![])?;
        }
        Ok(())
    }

    /// `for (k in v)`: own enumerable keys, then inherited ones (not the built-in prototypes').
    fn for_in_keys(&mut self, v: &Value) -> Res<Vec<Value>> {
        Ok(match v {
            Value::Obj(_) if Interp::proxy_parts(v).is_some() => self.keys_of(v)?.into_iter().map(Value::Str).collect(),
            Value::Obj(o) => {
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
        })
    }
}

/// Adds a getter or setter to `o`'s accessor property `k`.
fn define_accessor(o: &Obj, k: &str, f: Value, getter: bool, enumerable: bool) {
    let old = o.borrow().get_own(k).cloned();
    let (mut g, mut s) = match old {
        Some(Slot::Accessor(g, s)) => (g, s),
        _ => (None, None),
    };
    if getter {
        g = Some(f);
    } else {
        s = Some(f);
    }
    let mut b = o.borrow_mut();
    b.set_slot(k, Slot::Accessor(g, s));
    if !enumerable {
        b.set_enumerable(k, false);
    }
}

/// Compares strings by UTF-16 code units, as JavaScript does.
pub fn str_cmp(x: &str, y: &str) -> std::cmp::Ordering {
    // UTF-8 order matches code-point order, which only differs from UTF-16
    // order between astral characters and U+E000..U+FFFF.
    let astral = |s: &str| s.bytes().any(|b| b >= 0xF0);
    if astral(x) || astral(y) {
        x.encode_utf16().cmp(y.encode_utf16())
    } else {
        x.cmp(y)
    }
}
