//! The event loop and everything that waits on it: Promises (with a microtask
//! queue), `async`/`await`, timers on a virtual clock, generators, iterators
//! and Symbols.
//!
//! `await` is implemented by running the event loop until the awaited
//! promise settles (a nested loop), so an async function finishes in one go
//! when what it waits for can finish: promise chains, timers, and the host's
//! fetches, which complete synchronously. A promise that can never settle
//! (waiting for a click, say) makes the `await` throw instead of hanging.
//!
//! Generators run ahead of their consumer: the body runs to completion on the
//! first `next()`, buffering what it yields (at most [`MAX_YIELDS`], which
//! stops an endless generator), and `next()` hands the values out. Values
//! sent in through `next(x)` aren't seen by the body.
//!
//! Symbols are strings starting with `"\0@"`: hidden from property listings,
//! reported as "symbol" by typeof, unique per call to `Symbol()`.

use crate::interp::{Interp, Kind, Obj, Res, Slot, Value};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

pub const SYM_ITERATOR: &str = "\0@iterator";
pub const MAX_YIELDS: usize = 100_000;
pub const GEN_STOP: &str = "\0generator stopped";

pub enum PState {
    Pending(Vec<Reaction>),
    Fulfilled(Value),
    Rejected(Value),
}

pub struct Reaction {
    derived: Value,
    on_ok: Value,
    on_err: Value,
}

pub struct Timer {
    pub id: u32,
    pub due: f64,
    pub every: Option<f64>,
    pub f: Value,
    pub args: Vec<Value>,
}

fn arg(a: &[Value], i: usize) -> Value {
    a.get(i).cloned().unwrap_or(Value::Undefined)
}

fn hidden(o: &Obj, k: &str) -> Option<Value> {
    match o.borrow().get_own(k) {
        Some(Slot::Data(v)) => Some(v.clone()),
        _ => None,
    }
}

/// The items an iterator made by [`make_iter`] still has (and exhausts it).
pub fn iterator_rest(o: &Obj) -> Option<Vec<Value>> {
    let items = hidden(o, "\0items")?;
    let at = match hidden(o, "\0i") {
        Some(Value::Num(n)) => n as usize,
        _ => return None,
    };
    let Value::Obj(arr) = items else { return None };
    let rest = match &arr.borrow().kind {
        Kind::Array(a) => a.get(at..).map(|s| s.to_vec()).unwrap_or_default(),
        _ => return None,
    };
    o.borrow_mut().set_own("\0i", Value::Num((at + rest.len()) as f64));
    Some(rest)
}

/// An iterator over `items` (what `values()`, `entries()` and friends return).
pub fn make_iter(i: &Interp, items: Vec<Value>) -> Value {
    let it = i.obj(Kind::Plain);
    if let Value::Obj(o) = &it {
        let mut b = o.borrow_mut();
        b.proto = Some(i.iterator_proto.clone());
        b.set_own("\0items", i.array(items));
        b.set_own("\0i", Value::Num(0.0));
    }
    it
}

fn result(i: &Interp, value: Value, done: bool) -> Value {
    let r = i.obj(Kind::Plain);
    if let Value::Obj(o) = &r {
        let mut b = o.borrow_mut();
        b.set_own("value", value);
        b.set_own("done", Value::Bool(done));
    }
    r
}

fn pstate(v: &Value) -> Option<Rc<RefCell<PState>>> {
    match v {
        Value::Obj(o) => match &o.borrow().kind {
            Kind::Promise(p) => Some(p.clone()),
            _ => None,
        },
        _ => None,
    }
}

impl Interp {
    pub fn new_promise(&self) -> (Value, Rc<RefCell<PState>>) {
        let st = Rc::new(RefCell::new(PState::Pending(Vec::new())));
        (self.obj(Kind::Promise(st.clone())), st)
    }

    /// `Promise.resolve(v)`: v itself if it's a promise.
    pub fn promise_resolve(&mut self, v: Value) -> Value {
        if pstate(&v).is_some() {
            return v;
        }
        let (p, _) = self.new_promise();
        self.resolve_promise(&p, v);
        p
    }

    /// Resolves `p` with `v`, following `v` if it's a promise or thenable.
    pub fn resolve_promise(&mut self, p: &Value, v: Value) {
        if let (Value::Obj(a), Value::Obj(b)) = (p, &v) {
            if Rc::ptr_eq(a, b) {
                let e = self.error("TypeError", "a promise can't resolve to itself");
                return self.settle(p, false, e);
            }
        }
        if let Value::Obj(_) = &v {
            let then = match self.get(&v, "then") {
                Ok(t) => t,
                Err(e) => return self.settle(p, false, e),
            };
            if Self::is_callable(&then) {
                let (res, rej) = self.resolving_functions(p);
                let job = self.native(move |i, _t, _a| {
                    if let Err(e) = i.call(&then, v.clone(), vec![res.clone(), rej.clone()]) {
                        i.call(&rej, Value::Undefined, vec![e])?;
                    }
                    Ok(Value::Undefined)
                });
                self.microtasks.push_back((job, Vec::new()));
                return;
            }
        }
        self.settle(p, true, v);
    }

    pub fn reject_promise(&mut self, p: &Value, e: Value) {
        self.settle(p, false, e);
    }

    fn settle(&mut self, p: &Value, ok: bool, v: Value) {
        let Some(st) = pstate(p) else { return };
        let reactions = {
            let mut s = st.borrow_mut();
            if !matches!(*s, PState::Pending(_)) {
                return;
            }
            let new = if ok { PState::Fulfilled(v.clone()) } else { PState::Rejected(v.clone()) };
            match std::mem::replace(&mut *s, new) {
                PState::Pending(r) => r,
                _ => Vec::new(),
            }
        };
        for r in reactions {
            self.queue_reaction(r, ok, v.clone());
        }
    }

    fn queue_reaction(&mut self, r: Reaction, ok: bool, v: Value) {
        let job = self.native(move |i, _t, _a| {
            let handler = if ok { &r.on_ok } else { &r.on_err };
            let outcome = if Self::is_callable(handler) {
                i.call(handler, Value::Undefined, vec![v.clone()])
            } else if ok {
                Ok(v.clone())
            } else {
                Err(v.clone())
            };
            if !matches!(r.derived, Value::Undefined) {
                match outcome {
                    Ok(x) => i.resolve_promise(&r.derived, x),
                    Err(e) => i.reject_promise(&r.derived, e),
                }
            }
            Ok(Value::Undefined)
        });
        self.microtasks.push_back((job, Vec::new()));
    }

    /// `p.then(on_ok, on_err)`: the derived promise.
    pub fn then_promise(&mut self, p: &Value, on_ok: Value, on_err: Value) -> Value {
        let (derived, _) = self.new_promise();
        let Some(st) = pstate(p) else { return derived };
        let r = Reaction { derived: derived.clone(), on_ok, on_err };
        let settled = match &mut *st.borrow_mut() {
            PState::Pending(list) => {
                list.push(r);
                return derived;
            }
            PState::Fulfilled(v) => (true, v.clone()),
            PState::Rejected(v) => (false, v.clone()),
        };
        self.queue_reaction(r, settled.0, settled.1);
        derived
    }

    /// The resolve and reject functions handed to an executor (each works once).
    pub fn resolving_functions(&self, p: &Value) -> (Value, Value) {
        let done = Rc::new(Cell::new(false));
        let (p1, d1) = (p.clone(), done.clone());
        let res = self.native(move |i, _t, a| {
            if !d1.replace(true) {
                i.resolve_promise(&p1, arg(&a, 0));
            }
            Ok(Value::Undefined)
        });
        let p2 = p.clone();
        let rej = self.native(move |i, _t, a| {
            if !done.replace(true) {
                i.reject_promise(&p2, arg(&a, 0));
            }
            Ok(Value::Undefined)
        });
        (res, rej)
    }

    /// Runs queued promise reactions until there are none.
    pub fn run_microtasks(&mut self) {
        let mut n = 0u64;
        while let Some((f, args)) = self.microtasks.pop_front() {
            if let Err(e) = self.call(&f, Value::Undefined, args) {
                let d = self.describe(&e);
                self.errors.push(d);
            }
            n += 1;
            if n > 5_000_000 {
                self.microtasks.clear();
                self.errors.push("RangeError: too many promise reactions".into());
                break;
            }
        }
    }

    /// Runs one job: a microtask, or else the next timer (advancing the clock).
    /// False when there's nothing left to run.
    pub fn run_one_job(&mut self, until: f64) -> bool {
        if let Some((f, args)) = self.microtasks.pop_front() {
            if let Err(e) = self.call(&f, Value::Undefined, args) {
                let d = self.describe(&e);
                self.errors.push(d);
            }
            return true;
        }
        let Some(k) = (0..self.timers.len()).min_by(|&a, &b| {
            let (x, y) = (&self.timers[a], &self.timers[b]);
            x.due.partial_cmp(&y.due).unwrap_or(std::cmp::Ordering::Equal).then(x.id.cmp(&y.id))
        }) else {
            return false;
        };
        if self.timers[k].due > until {
            return false;
        }
        let t = self.timers.remove(k);
        self.now = self.now.max(t.due);
        if let Some(every) = t.every {
            self.timers.push(Timer { id: t.id, due: t.due + every, every: t.every, f: t.f.clone(), args: t.args.clone() });
        }
        if let Err(e) = self.call(&t.f, Value::Undefined, t.args) {
            let d = self.describe(&e);
            self.errors.push(d);
        }
        self.run_microtasks();
        true
    }

    /// Runs the event loop until nothing is due before `until` (virtual ms), at most `max` jobs.
    pub fn run_jobs(&mut self, until: f64, max: usize) -> usize {
        let mut n = 0;
        while n < max && self.run_one_job(until) {
            n += 1;
        }
        n
    }

    pub fn add_timer(&mut self, f: Value, delay: f64, args: Vec<Value>, repeat: bool) -> u32 {
        self.timer_seq += 1;
        let delay = if delay.is_finite() { delay.max(0.0) } else { 0.0 };
        let id = self.timer_seq;
        self.timers.push(Timer { id, due: self.now + delay, every: repeat.then_some(delay.max(4.0)), f, args });
        id
    }

    /// `await v`: runs the event loop until the promise settles.
    pub fn await_value(&mut self, v: Value) -> Res<Value> {
        let p = self.promise_resolve(v);
        let st = pstate(&p).expect("a promise");
        let mut guard = 0u64;
        loop {
            match &*st.borrow() {
                PState::Fulfilled(x) => return Ok(x.clone()),
                PState::Rejected(e) => return Err(e.clone()),
                PState::Pending(_) => {}
            }
            guard += 1;
            if guard > 10_000_000 || !self.run_one_job(f64::INFINITY) {
                return self.throw("Error", "await: the promise never settled");
            }
        }
    }

    pub fn make_generator(&mut self, f: Value, this: Value, args: Vec<Value>) -> Value {
        let start = self.native(move |i, _t, _a| i.call_body(&f, this.clone(), args.clone()));
        let g = self.obj(Kind::Plain);
        if let Value::Obj(o) = &g {
            let mut b = o.borrow_mut();
            b.proto = Some(self.generator_proto.clone());
            b.set_own("\0start", start);
        }
        g
    }

    /// Runs a generator's body (once), buffering what it yields.
    fn start_generator(&mut self, o: &Obj) -> Res<()> {
        let Some(start) = hidden(o, "\0start") else { return Ok(()) };
        o.borrow_mut().delete("\0start");
        self.gen_stack.push(Vec::new());
        let r = self.call(&start, Value::Undefined, vec![]);
        let buf = self.gen_stack.pop().unwrap_or_default();
        let ret = match r {
            Ok(v) => v,
            Err(Value::Str(s)) if &*s == GEN_STOP => Value::Undefined,
            Err(e) => {
                o.borrow_mut().set_own("\0err", e);
                Value::Undefined
            }
        };
        let items = self.array(buf);
        let mut b = o.borrow_mut();
        b.set_own("\0items", items);
        b.set_own("\0i", Value::Num(0.0));
        b.set_own("\0ret", ret);
        Ok(())
    }
}

fn this_obj(i: &Interp, t: &Value) -> Res<Obj> {
    match t {
        Value::Obj(o) => Ok(o.clone()),
        _ => i.throw("TypeError", "called on a non-object"),
    }
}

fn def(i: &Interp, o: &Obj, name: &str, f: impl Fn(&mut Interp, Value, Vec<Value>) -> Res<Value> + 'static) {
    let fv = i.native(f);
    if let Value::Obj(fo) = &fv {
        fo.borrow_mut().set_own("name", Value::Str(name.trim_start_matches("\0@").into()));
    }
    o.borrow_mut().set_own(name, fv);
}

fn global(i: &Interp, name: &str, v: Value) {
    i.global_obj.borrow_mut().set_own(name, v);
}

/// The next value of an iterator made by make_iter (or a generator after it started).
fn next_item(i: &mut Interp, o: &Obj) -> Res<Value> {
    let (Some(Value::Obj(items)), Some(Value::Num(at))) = (hidden(o, "\0items"), hidden(o, "\0i")) else {
        return Ok(result(i, Value::Undefined, true));
    };
    let at = at as usize;
    let item = match &items.borrow().kind {
        Kind::Array(a) => a.get(at).cloned(),
        _ => None,
    };
    match item {
        Some(v) => {
            o.borrow_mut().set_own("\0i", Value::Num((at + 1) as f64));
            Ok(result(i, v, false))
        }
        None => {
            if let Some(e) = hidden(o, "\0err") {
                o.borrow_mut().delete("\0err");
                return Err(e);
            }
            let ret = hidden(o, "\0ret").unwrap_or(Value::Undefined);
            o.borrow_mut().set_own("\0ret", Value::Undefined);
            Ok(result(i, ret, true))
        }
    }
}

pub fn install(i: &mut Interp) {
    // ---------------------------------------------------------- Symbol
    let counter = Rc::new(Cell::new(0u32));
    let symbol = i.native(move |i, _t, a| {
        let desc = match arg(&a, 0) {
            Value::Undefined => String::new(),
            d => i.to_str(&d)?.to_string(),
        };
        counter.set(counter.get() + 1);
        Ok(Value::Str(format!("\0@{}:{desc}", counter.get()).into()))
    });
    if let Value::Obj(so) = &symbol {
        let mut b = so.borrow_mut();
        for name in ["iterator", "asyncIterator", "hasInstance", "toPrimitive", "toStringTag", "species", "match", "replace", "search", "split", "unscopables"] {
            b.set_own(name, Value::Str(format!("\0@{name}").into()));
        }
    }
    if let Value::Obj(so) = &symbol {
        def(i, so, "for", |i, _t, a| {
            let k = i.to_str(&arg(&a, 0))?;
            Ok(Value::Str(format!("\0@for:{k}").into()))
        });
    }
    global(i, "Symbol", symbol);

    // ---------------------------------------------------------- iterators
    let ip = i.iterator_proto.clone();
    def(i, &ip, "next", |i, t, _a| {
        let o = this_obj(i, &t)?;
        next_item(i, &o)
    });
    def(i, &ip, SYM_ITERATOR, |_i, t, _a| Ok(t));
    def(i, &ip, "toArray", |i, t, _a| {
        let o = this_obj(i, &t)?;
        Ok(i.array(iterator_rest(&o).unwrap_or_default()))
    });
    let gp = i.generator_proto.clone();
    gp.borrow_mut().proto = Some(ip.clone());
    def(i, &gp, "next", |i, t, _a| {
        let o = this_obj(i, &t)?;
        i.start_generator(&o)?;
        next_item(i, &o)
    });
    def(i, &gp, "return", |i, t, a| {
        let o = this_obj(i, &t)?;
        o.borrow_mut().delete("\0start");
        o.borrow_mut().set_own("\0items", i.array(Vec::new()));
        o.borrow_mut().set_own("\0i", Value::Num(0.0));
        Ok(result(i, arg(&a, 0), true))
    });
    def(i, &gp, "throw", |i, t, a| {
        let o = this_obj(i, &t)?;
        o.borrow_mut().delete("\0start");
        o.borrow_mut().set_own("\0items", i.array(Vec::new()));
        o.borrow_mut().set_own("\0i", Value::Num(0.0));
        Err(arg(&a, 0))
    });
    // Built-in collections hand out iterators.
    let ap = i.array_proto.clone();
    let array_values = i.native(|i, t, _a| {
        let items = i.iterate(&t)?;
        Ok(make_iter(i, items))
    });
    ap.borrow_mut().set_own(SYM_ITERATOR, array_values.clone());
    ap.borrow_mut().set_own("values", array_values);
    def(i, &ap, "keys", |i, t, _a| {
        let n = i.iterate(&t)?.len();
        Ok(make_iter(i, (0..n).map(|k| Value::Num(k as f64)).collect()))
    });
    def(i, &ap, "entries", |i, t, _a| {
        let items = i.iterate(&t)?;
        let pairs = items.into_iter().enumerate().map(|(k, v)| i.array(vec![Value::Num(k as f64), v])).collect();
        Ok(make_iter(i, pairs))
    });
    let sp = i.string_proto.clone();
    def(i, &sp, SYM_ITERATOR, |i, t, _a| {
        let s = i.to_str(&t)?;
        Ok(make_iter(i, s.chars().map(|c| Value::Str(c.to_string().into())).collect()))
    });
    for proto in [i.map_proto.clone(), i.set_proto.clone()] {
        for name in ["keys", "values", "entries"] {
            let old = match proto.borrow().get_own(name) {
                Some(Slot::Data(v)) => v.clone(),
                _ => continue,
            };
            def(i, &proto, name, move |i, t, a| {
                let arr = i.call(&old, t, a)?;
                let items = i.iterate(&arr)?;
                Ok(make_iter(i, items))
            });
        }
        let entries = if Rc::ptr_eq(&proto, &i.map_proto) { "entries" } else { "values" };
        let f = match proto.borrow().get_own(entries) {
            Some(Slot::Data(v)) => v.clone(),
            _ => Value::Undefined,
        };
        proto.borrow_mut().set_own(SYM_ITERATOR, f);
    }

    // ---------------------------------------------------------- Promise
    let pp = i.promise_proto.clone();
    let promise = i.native(|i, t, a| {
        let is_new = matches!(&t, Value::Obj(o) if o.borrow().get_own("__new__").is_some());
        if !is_new {
            return i.throw("TypeError", "Promise must be called with new");
        }
        let exec = arg(&a, 0);
        if !Interp::is_callable(&exec) {
            return i.throw("TypeError", "Promise needs an executor function");
        }
        let (p, _) = i.new_promise();
        let (res, rej) = i.resolving_functions(&p);
        if let Err(e) = i.call(&exec, Value::Undefined, vec![res, rej.clone()]) {
            i.call(&rej, Value::Undefined, vec![e])?;
        }
        Ok(p)
    });
    let Value::Obj(po) = promise.clone() else { unreachable!() };
    po.borrow_mut().set_own("prototype", Value::Obj(pp.clone()));
    po.borrow_mut().set_own("name", Value::Str("Promise".into()));
    pp.borrow_mut().set_own("constructor", promise.clone());
    global(i, "Promise", promise);
    def(i, &pp, "then", |i, t, a| {
        if pstate(&t).is_none() {
            return i.throw("TypeError", "then called on a non-promise");
        }
        Ok(i.then_promise(&t, arg(&a, 0), arg(&a, 1)))
    });
    def(i, &pp, "catch", |i, t, a| Ok(i.then_promise(&t, Value::Undefined, arg(&a, 0))));
    def(i, &pp, "finally", |i, t, a| {
        let f = arg(&a, 0);
        let f2 = f.clone();
        let ok = i.native(move |i, _t, a| {
            if Interp::is_callable(&f) {
                i.call(&f, Value::Undefined, vec![])?;
            }
            Ok(arg(&a, 0))
        });
        let err = i.native(move |i, _t, a| {
            if Interp::is_callable(&f2) {
                i.call(&f2, Value::Undefined, vec![])?;
            }
            Err(arg(&a, 0))
        });
        Ok(i.then_promise(&t, ok, err))
    });
    def(i, &po, "resolve", |i, _t, a| Ok(i.promise_resolve(arg(&a, 0))));
    def(i, &po, "reject", |i, _t, a| {
        let (p, _) = i.new_promise();
        i.reject_promise(&p, arg(&a, 0));
        Ok(p)
    });
    def(i, &po, "withResolvers", |i, _t, _a| {
        let (p, _) = i.new_promise();
        let (res, rej) = i.resolving_functions(&p);
        let o = i.obj(Kind::Plain);
        i.set(&o, "promise", p)?;
        i.set(&o, "resolve", res)?;
        i.set(&o, "reject", rej)?;
        Ok(o)
    });
    // all / allSettled / any / race
    for (name, mode) in [("all", 0), ("allSettled", 1), ("any", 2), ("race", 3)] {
        def(i, &po, name, move |i, _t, a| {
            let items = i.iterate(&arg(&a, 0))?;
            let (out, _) = i.new_promise();
            let n = items.len();
            if n == 0 {
                match mode {
                    2 => {
                        let e = i.error("AggregateError", "all promises were rejected");
                        i.reject_promise(&out, e);
                    }
                    3 => {}
                    _ => {
                        let empty = i.array(Vec::new());
                        i.resolve_promise(&out, empty);
                    }
                }
                return Ok(out);
            }
            let results = Rc::new(RefCell::new(vec![Value::Undefined; n]));
            let left = Rc::new(Cell::new(n));
            for (k, item) in items.into_iter().enumerate() {
                let p = i.promise_resolve(item);
                let mk = |i: &mut Interp, ok: bool| {
                    let (results, left, out) = (results.clone(), left.clone(), out.clone());
                    i.native(move |i, _t, a| {
                        let v = arg(&a, 0);
                        match (mode, ok) {
                            (3, true) => i.resolve_promise(&out, v),
                            (3, false) | (0, false) => i.reject_promise(&out, v),
                            (2, true) => i.resolve_promise(&out, v),
                            _ => {
                                let entry = if mode == 1 {
                                    let o = i.obj(Kind::Plain);
                                    i.set(&o, "status", Value::Str(if ok { "fulfilled" } else { "rejected" }.into()))?;
                                    i.set(&o, if ok { "value" } else { "reason" }, v)?;
                                    o
                                } else {
                                    v
                                };
                                results.borrow_mut()[k] = entry;
                                left.set(left.get() - 1);
                                if left.get() == 0 {
                                    if mode == 2 {
                                        let e = i.error("AggregateError", "all promises were rejected");
                                        i.reject_promise(&out, e);
                                    } else {
                                        let arr = i.array(results.borrow().clone());
                                        i.resolve_promise(&out, arr);
                                    }
                                }
                            }
                        }
                        Ok(Value::Undefined)
                    })
                };
                let on_ok = mk(i, true);
                let on_err = mk(i, false);
                i.then_promise(&p, on_ok, on_err);
            }
            Ok(out)
        });
    }

    // ---------------------------------------------------------- timers
    for (name, repeat) in [("setTimeout", false), ("setInterval", true)] {
        let f = i.native(move |i, _t, a| {
            let f = arg(&a, 0);
            if !Interp::is_callable(&f) {
                return Ok(Value::Num(0.0));
            }
            let delay = match a.get(1) {
                Some(d) => i.to_number(d)?,
                None => 0.0,
            };
            let id = i.add_timer(f, delay, a.into_iter().skip(2).collect(), repeat);
            Ok(Value::Num(id as f64))
        });
        global(i, name, f);
    }
    let raf = i.native(|i, _t, a| {
        let f = arg(&a, 0);
        let now = i.now + 16.0;
        let id = i.add_timer(f, 16.0, vec![Value::Num(now)], false);
        Ok(Value::Num(id as f64))
    });
    global(i, "requestAnimationFrame", raf);
    let idle = i.native(|i, _t, a| {
        let id = i.add_timer(arg(&a, 0), 1.0, vec![], false);
        Ok(Value::Num(id as f64))
    });
    global(i, "requestIdleCallback", idle);
    let clear = i.native(|i, _t, a| {
        if let Value::Num(id) = arg(&a, 0) {
            i.timers.retain(|t| t.id as f64 != id);
        }
        Ok(Value::Undefined)
    });
    for n in ["clearTimeout", "clearInterval", "cancelAnimationFrame", "cancelIdleCallback"] {
        global(i, n, clear.clone());
    }
    let micro = i.native(|i, _t, a| {
        i.microtasks.push_back((arg(&a, 0), Vec::new()));
        Ok(Value::Undefined)
    });
    global(i, "queueMicrotask", micro);
}
