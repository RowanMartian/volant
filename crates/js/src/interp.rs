//! Values, objects with prototypes, conversions, property access and calls:
//! what the bytecode VM ([`crate::vm`]) and the built-ins share.

use std::cell::{Cell, RefCell};
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
    /// Compiled JavaScript, with the environment it closes over.
    Closure(crate::vm::Closure),
    Native(Native),
}

pub enum Kind {
    Plain,
    Array(Vec<Value>),
    Function(Callable),
    Error,
    /// Map and Set: entries in insertion order (a Set's values are its keys).
    Map(crate::table::Table),
    Set(crate::table::Table),
    Date(f64),
    /// A primitive in an object (`new String("x")`).
    Boxed(Value),
    /// A regular expression: (compiled, source, flags)
    RegExp(Rc<crate::regex::Regex>, Rc<str>, Rc<str>),
    Promise(Rc<RefCell<crate::promise::PState>>),
    /// `new Proxy(target, handler)`; a revoked proxy's handler is null.
    Proxy(Value, Value),
    /// A generator object (its suspended frame).
    Generator(Rc<RefCell<crate::vm::GenState>>),
    /// The VM's own iterators, for `for of` and spreads over built-in collections.
    Iter(crate::vm::IterState),
    /// An ArrayBuffer's bytes.
    Buffer(Vec<u8>),
    /// A typed array or DataView onto an ArrayBuffer.
    Typed(crate::typed::View),
}

#[derive(Clone)]
pub enum Slot {
    Data(Value),
    Accessor(Option<Value>, Option<Value>),
}

/// An own property: its key, its value (or accessors) and flags.
pub struct PropEntry {
    pub key: Rc<str>,
    pub slot: Slot,
    flags: u8,
}

/// Left out of `Object.keys` and `for in` (defineProperty's default).
const HIDDEN: u8 = 1;
/// Can't be assigned to (a global `const`, or a frozen property).
const READONLY: u8 = 2;

/// Objects with more own properties than this keep a hash index of them.
const INDEX_AT: usize = 12;

pub struct Object {
    /// Own properties in insertion order; property lookups remember the
    /// position they found a name at (an inline cache) and look there first.
    props: Vec<PropEntry>,
    index: Option<Box<HashMap<Rc<str>, u32, FxBuild>>>,
    pub proto: Option<Obj>,
    pub kind: Kind,
}

impl Object {
    pub fn new(proto: Option<Obj>, kind: Kind) -> Self {
        Object { props: Vec::new(), index: None, proto, kind }
    }

    /// Where the own property `k` is: `hint` first (where it was last time).
    #[inline]
    pub fn find(&self, k: &str, hint: u32) -> Option<usize> {
        if let Some(e) = self.props.get(hint as usize) {
            if std::ptr::eq(e.key.as_ptr(), k.as_ptr()) || *e.key == *k {
                return Some(hint as usize);
            }
        }
        match &self.index {
            Some(ix) => ix.get(k).map(|&i| i as usize),
            None => self.props.iter().position(|e| *e.key == *k),
        }
    }
    #[inline]
    pub fn slot_at(&self, i: usize) -> &Slot {
        &self.props[i].slot
    }
    #[inline]
    pub fn slot_at_mut(&mut self, i: usize) -> &mut Slot {
        &mut self.props[i].slot
    }
    pub fn readonly_at(&self, i: usize) -> bool {
        self.props[i].flags & READONLY != 0
    }
    pub fn set_readonly(&mut self, k: &str, yes: bool) {
        if let Some(i) = self.find(k, 0) {
            let f = &mut self.props[i].flags;
            *f = if yes { *f | READONLY } else { *f & !READONLY };
        }
    }
    pub fn len(&self) -> usize {
        self.props.len()
    }
    pub fn is_empty(&self) -> bool {
        self.props.is_empty()
    }
    /// Marks an own property enumerable or not.
    pub fn set_enumerable(&mut self, k: &str, yes: bool) {
        if let Some(i) = self.find(k, 0) {
            let f = &mut self.props[i].flags;
            *f = if yes { *f & !HIDDEN } else { *f | HIDDEN };
        }
    }
    pub fn is_enumerable(&self, k: &str) -> bool {
        self.find(k, 0).map(|i| self.props[i].flags & HIDDEN == 0).unwrap_or(true)
    }
    /// Every own key, enumerable or not (hidden internals excepted).
    pub fn all_keys(&self) -> Vec<Rc<str>> {
        let mut out: Vec<Rc<str>> = match &self.kind {
            Kind::Array(a) => (0..a.len()).map(|i| i.to_string().into()).collect(),
            Kind::Typed(t) if !t.data_view => (0..t.len).map(|i| i.to_string().into()).collect(),
            _ => Vec::new(),
        };
        let start = out.len();
        out.extend(self.props.iter().filter(|e| !e.key.starts_with('\0')).map(|e| e.key.clone()));
        indices_first(&mut out, start);
        if matches!(self.kind, Kind::Array(_)) {
            out.push("length".into());
        }
        out
    }
    pub fn get_own(&self, k: &str) -> Option<&Slot> {
        self.find(k, 0).map(|i| &self.props[i].slot)
    }
    fn push(&mut self, key: Rc<str>, slot: Slot, flags: u8) -> usize {
        let at = self.props.len();
        if let Some(ix) = &mut self.index {
            ix.insert(key.clone(), at as u32);
        }
        self.props.push(PropEntry { key, slot, flags });
        if self.index.is_none() && self.props.len() > INDEX_AT {
            let mut ix: HashMap<Rc<str>, u32, FxBuild> = HashMap::with_capacity_and_hasher(self.props.len() * 2, FxBuild);
            for (i, e) in self.props.iter().enumerate() {
                ix.insert(e.key.clone(), i as u32);
            }
            self.index = Some(Box::new(ix));
        }
        at
    }
    pub fn set_own(&mut self, k: &str, v: Value) {
        match self.find(k, 0) {
            Some(i) => self.props[i].slot = Slot::Data(v),
            None => {
                let flags = self.lazy_hidden(k);
                self.push(k.into(), Slot::Data(v), flags);
            }
        }
    }
    /// A function's `prototype`, `name` and `length` are made lazily, but
    /// they're never enumerable, even when assigned before first read.
    fn lazy_hidden(&self, k: &str) -> u8 {
        if matches!(k, "prototype" | "name" | "length") && matches!(self.kind, Kind::Function(_)) {
            HIDDEN
        } else {
            0
        }
    }
    /// Like [`set_own`](Self::set_own) with a key already shared; where it went.
    pub fn set_own_rc(&mut self, k: &Rc<str>, v: Value, hint: u32) -> usize {
        match self.find(k, hint) {
            Some(i) => {
                self.props[i].slot = Slot::Data(v);
                i
            }
            None => {
                let flags = self.lazy_hidden(k);
                self.push(k.clone(), Slot::Data(v), flags)
            }
        }
    }
    /// A non-enumerable own data property (methods, internals).
    pub fn set_hidden(&mut self, k: &str, v: Value) {
        match self.find(k, 0) {
            Some(i) => {
                self.props[i].slot = Slot::Data(v);
                self.props[i].flags |= HIDDEN;
            }
            None => {
                self.push(k.into(), Slot::Data(v), HIDDEN);
            }
        }
    }
    pub fn set_slot(&mut self, k: &str, s: Slot) {
        match self.find(k, 0) {
            Some(i) => self.props[i].slot = s,
            None => {
                self.push(k.into(), s, 0);
            }
        }
    }
    pub fn delete(&mut self, k: &str) -> bool {
        if let Some(i) = self.find(k, 0) {
            self.props.remove(i);
            if let Some(ix) = &mut self.index {
                ix.remove(k);
                for v in ix.values_mut() {
                    if *v as usize > i {
                        *v -= 1;
                    }
                }
            }
        }
        true
    }
    /// Own enumerable keys, in order (array indices first).
    pub fn own_keys(&self) -> Vec<Rc<str>> {
        let mut out: Vec<Rc<str>> = match &self.kind {
            Kind::Array(a) => (0..a.len()).map(|i| i.to_string().into()).collect(),
            Kind::Boxed(Value::Str(s)) => (0..s.chars().count()).map(|i| i.to_string().into()).collect(),
            Kind::Typed(t) if !t.data_view => (0..t.len).map(|i| i.to_string().into()).collect(),
            _ => Vec::new(),
        };
        let start = out.len();
        out.extend(self.props.iter().filter(|e| e.flags & HIDDEN == 0 && !e.key.starts_with('\0')).map(|e| e.key.clone()));
        indices_first(&mut out, start);
        out
    }
    /// The own properties, in order (for copying).
    pub fn entries(&self) -> impl Iterator<Item = (&Rc<str>, &Slot, bool)> {
        self.props.iter().map(|e| (&e.key, &e.slot, e.flags & HIDDEN == 0))
    }
}

/// Whether `k` is an array index ("0", "7", not "07" or "4294967295").
pub fn is_index(k: &str) -> bool {
    !k.is_empty() && k.len() <= 10 && k.bytes().all(|b| b.is_ascii_digit()) && (k == "0" || !k.starts_with('0')) && k.parse::<u64>().is_ok_and(|n| n < u32::MAX as u64)
}

/// Puts integer keys first, ascending, as OrdinaryOwnPropertyKeys does.
fn indices_first(keys: &mut [Rc<str>], start: usize) {
    let tail = &mut keys[start..];
    if tail.iter().any(|k| is_index(k)) {
        tail.sort_by_key(|k| if is_index(k) { (0, k.parse::<u64>().unwrap_or(0)) } else { (1, 0) });
    }
}

/// A small, fast hash for property names (FxHash).
#[derive(Clone, Copy, Default)]
pub struct FxBuild;

pub struct FxHasher(u64);

impl std::hash::BuildHasher for FxBuild {
    type Hasher = FxHasher;
    fn build_hasher(&self) -> FxHasher {
        FxHasher(0)
    }
}

impl std::hash::Hasher for FxHasher {
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        const K: u64 = 0x51_7c_c1_b7_27_22_0a_95;
        let mut h = self.0;
        let mut chunks = bytes.chunks_exact(8);
        for c in &mut chunks {
            let w = u64::from_le_bytes([c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7]]);
            h = (h.rotate_left(5) ^ w).wrapping_mul(K);
        }
        for &b in chunks.remainder() {
            h = (h.rotate_left(5) ^ b as u64).wrapping_mul(K);
        }
        self.0 = h;
    }
    #[inline]
    fn write_u8(&mut self, b: u8) {
        self.0 = (self.0.rotate_left(5) ^ b as u64).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
    }
    fn finish(&self) -> u64 {
        self.0
    }
}

pub type Res<T> = Result<T, Value>;

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

/// `a + b` as a shared string, built in one allocation (a String first would copy twice).
pub fn concat_str(a: &str, b: &str) -> Rc<str> {
    if b.is_empty() {
        return a.into();
    }
    let mut buf: Rc<[std::mem::MaybeUninit<u8>]> = Rc::new_uninit_slice(a.len() + b.len());
    let dst = Rc::get_mut(&mut buf).expect("a new Rc is unique");
    // SAFETY: u8 and MaybeUninit<u8> share a layout; every byte is written before assume_init.
    unsafe {
        std::ptr::copy_nonoverlapping(a.as_ptr(), dst.as_mut_ptr() as *mut u8, a.len());
        std::ptr::copy_nonoverlapping(b.as_ptr(), (dst.as_mut_ptr() as *mut u8).add(a.len()), b.len());
        let bytes: Rc<[u8]> = buf.assume_init();
        // Two valid UTF-8 strings joined are valid UTF-8, and str and [u8] share a layout.
        Rc::from_raw(Rc::into_raw(bytes) as *const str)
    }
}

pub struct Interp {
    pub global_obj: Obj,
    /// The global `let`, `const` and `class` declarations of scripts.
    pub global_lex: Obj,
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
    pub regexp_proto: Obj,
    pub promise_proto: Obj,
    pub generator_proto: Obj,
    pub iterator_proto: Obj,
    /// What the built-in collections' iterators share (`next`), under %IteratorPrototype%.
    pub list_iter_proto: Obj,
    pub buffer_proto: Obj,
    pub symbol_proto: Obj,
    /// %TypedArray%.prototype, and each typed array's own prototype (in `typed::ELEMS` order).
    pub typed_proto: Obj,
    pub typed_protos: Vec<Obj>,
    pub data_view_proto: Obj,
    /// What `console.log` printed.
    pub output: Vec<String>,
    /// Uncaught exceptions from callbacks (timers, promise jobs, events).
    pub errors: Vec<String>,
    /// Promise reactions and queueMicrotask callbacks.
    pub microtasks: std::collections::VecDeque<(Value, Vec<Value>)>,
    /// setTimeout / setInterval, on a virtual clock.
    pub timers: Vec<crate::promise::Timer>,
    /// The virtual clock, in ms since the page started.
    pub now: f64,
    pub timer_seq: u32,
    /// Asked for a global name that isn't defined, before it's a ReferenceError
    /// (a browser's window finds elements by id this way).
    pub global_fallback: Option<Rc<dyn Fn(&mut Interp, &str) -> Option<Value>>>,
    /// Operand stacks to reuse (each run of the VM takes one).
    pub(crate) stack_pool: Vec<Vec<Value>>,
    /// JS frames active across all runs (a recursion limit independent of the native stack).
    pub(crate) frames: u32,
    /// A generator's frame as it yields (handed back to its `next()`).
    pub(crate) yielded: Option<crate::vm::Suspended>,
    regex_cache: HashMap<(Rc<str>, Rc<str>), Rc<crate::regex::Regex>>,
    pub(crate) steps: u64,
    pub max_steps: u64,
    /// How long one run may take (set again by each [`Interp::reset_steps`]),
    /// whatever the step count says: steps differ a lot in cost.
    pub time_limit: Option<std::time::Duration>,
    pub(crate) deadline: Option<std::time::Instant>,
    pub(crate) depth: u32,
    /// How deep JS calls may nest. Each level costs native stack, so callers
    /// that raise it run the interpreter on a thread with a large stack.
    pub max_depth: u32,
    /// The names of the JS functions being called, innermost last (for error messages).
    pub call_names: Vec<(Option<Rc<str>>, u32)>,
    /// The script each of those came from (for stack traces).
    pub(crate) call_urls: Vec<Option<Rc<str>>>,
    /// Fetches modules (`import`); without one, imports fail.
    pub loader: Option<Rc<dyn crate::module::Loader>>,
    /// Modules by URL, loaded once each.
    pub(crate) modules: HashMap<Rc<str>, crate::module::ModuleRef>,
    /// What classic scripts' `import()` resolves against (the document's URL).
    pub base_url: Rc<str>,
    /// When set, a sample every 128 steps (loop iterations and calls) of the
    /// function running (a JS profile).
    pub profile: Option<HashMap<(Option<Rc<str>>, u32, Option<Rc<str>>), u64>>,
    /// When the profile last took a sample: each sample weighs the time since.
    pub(crate) profile_at: Option<std::time::Instant>,
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
        let global_lex = Rc::new(RefCell::new(Object::new(None, Kind::Plain)));
        let mut i = Interp {
            global_lex,
            array_proto: mk(Kind::Array(Vec::new())),
            string_proto: mk(Kind::Plain),
            number_proto: mk(Kind::Plain),
            boolean_proto: mk(Kind::Plain),
            error_proto: mk(Kind::Plain),
            map_proto: mk(Kind::Plain),
            set_proto: mk(Kind::Plain),
            date_proto: mk(Kind::Plain),
            regexp_proto: mk(Kind::Plain),
            promise_proto: mk(Kind::Plain),
            generator_proto: mk(Kind::Plain),
            iterator_proto: mk(Kind::Plain),
            list_iter_proto: mk(Kind::Plain),
            buffer_proto: mk(Kind::Plain),
            symbol_proto: mk(Kind::Plain),
            typed_proto: mk(Kind::Plain),
            typed_protos: (0..crate::typed::ELEMS.len()).map(|_| mk(Kind::Plain)).collect(),
            data_view_proto: mk(Kind::Plain),
            global_obj,
            object_proto,
            function_proto,
            output: Vec::new(),
            errors: Vec::new(),
            microtasks: Default::default(),
            timers: Vec::new(),
            now: 0.0,
            timer_seq: 0,
            global_fallback: None,
            stack_pool: Vec::new(),
            frames: 0,
            yielded: None,
            regex_cache: HashMap::new(),
            steps: 0,
            max_steps: 5_000_000,
            time_limit: None,
            deadline: None,
            depth: 0,
            max_depth: 400,
            call_names: Vec::new(),
            call_urls: Vec::new(),
            loader: None,
            modules: HashMap::new(),
            base_url: "".into(),
            profile: None,
            profile_at: None,
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
            Kind::RegExp(..) => self.regexp_proto.clone(),
            Kind::Promise(_) => self.promise_proto.clone(),
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
        #[rustfmt::skip]
        fn dom_exception_code(name: &str) -> Option<u16> {
            Some(match name {
                "IndexSizeError" => 1, "HierarchyRequestError" => 3, "WrongDocumentError" => 4, "InvalidCharacterError" => 5,
                "NoModificationAllowedError" => 7, "NotFoundError" => 8, "NotSupportedError" => 9, "InUseAttributeError" => 10,
                "InvalidStateError" => 11, "SyntaxError" => 12, "InvalidModificationError" => 13, "NamespaceError" => 14,
                "InvalidAccessError" => 15, "TypeMismatchError" => 17, "SecurityError" => 18, "NetworkError" => 19, "AbortError" => 20,
                "URLMismatchError" => 21, "QuotaExceededError" => 22, "TimeoutError" => 23, "InvalidNodeTypeError" => 24, "DataCloneError" => 25,
                n if n.ends_with("Error") => 0,
                _ => return None,
            })
        }
        let e = self.obj(Kind::Error);
        if let Value::Obj(o) = &e {
            // RangeError, TypeError, …: their own prototype, so `instanceof` works.
            // "DOM:SyntaxError" and the like: the DOMException, not the JavaScript error of that name.
            let (name, dom) = match name.strip_prefix("DOM:") {
                Some(n) => (n, true),
                None => (name, false),
            };
            if name != "Error" {
                let proto = if dom { None } else { match self.global_obj.borrow().get_own(name) {
                    Some(Slot::Data(Value::Obj(c))) => match c.borrow().get_own("prototype") {
                        Some(Slot::Data(Value::Obj(p))) => Some(p.clone()),
                        _ => None,
                    },
                    _ => None,
                } };
                if proto.is_some() {
                    o.borrow_mut().proto = proto;
                } else if let Some(code) = dom_exception_code(name) {
                    // HierarchyRequestError and the rest: DOMExceptions, with their legacy codes.
                    if let Some(Slot::Data(Value::Obj(c))) = self.global_obj.borrow().get_own("DOMException") {
                        if let Some(Slot::Data(Value::Obj(p))) = c.borrow().get_own("prototype") {
                            o.borrow_mut().proto = Some(p.clone());
                        }
                    }
                    o.borrow_mut().set_own("code", Value::Num(code as f64));
                }
            }
            let mut o = o.borrow_mut();
            o.set_own("name", Value::Str(name.into()));
            o.set_own("message", Value::Str(msg.into()));
            let stack = self.stack_trace();
            if std::env::var_os("VOLANT_TRACE_ERRORS").is_some() {
                eprintln!("{name}: {msg}\n{stack}");
            }
            o.set_own("stack", Value::Str(format!("{name}: {msg}\n{stack}").into()));
        }
        e
    }

    /// The JS functions being run, innermost first ("    at name (line N)").
    pub fn stack_trace(&self) -> String {
        let mut out = String::new();
        for (k, (n, line)) in self.call_names.iter().enumerate().rev().take(10) {
            let at = crate::parse::Where::unpack(*line);
            match self.call_urls.get(k).cloned().flatten() {
                Some(u) => out.push_str(&format!("    at {} ({u}: {at})\n", n.as_deref().unwrap_or("<anonymous>"))),
                None => out.push_str(&format!("    at {} ({at})\n", n.as_deref().unwrap_or("<anonymous>"))),
            }
        }
        out
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
            Value::Str(s) if s.starts_with("\0@") => "symbol",
            Value::Str(_) => "string",
            Value::Obj(_) => {
                if Self::is_callable(v) {
                    "function"
                } else {
                    "object"
                }
            }
        }
    }

    pub fn is_callable(v: &Value) -> bool {
        match v {
            Value::Obj(o) => match &o.borrow().kind {
                Kind::Function(_) => true,
                Kind::Proxy(t, _) => Self::is_callable(t),
                _ => false,
            },
            _ => false,
        }
    }

    /// ToPrimitive, preferring strings (`valueOf` for numbers first when `number`).
    pub fn to_primitive(&mut self, v: &Value, number: bool) -> Res<Value> {
        let Value::Obj(o) = v else {
            return Ok(v.clone());
        };
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
            Value::Str(s) if s.starts_with("\0@") => format!("Symbol({})", crate::promise::symbol_description(s).unwrap_or_default()).into(),
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
            // Strings, symbols included, are keys as they are.
            Value::Str(s) => Ok(s.clone()),
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
            Value::Str(s) if s.starts_with("\0@") => Some(self.symbol_proto.clone()),
            Value::Str(_) => Some(self.string_proto.clone()),
            Value::Num(_) => Some(self.number_proto.clone()),
            Value::Bool(_) => Some(self.boolean_proto.clone()),
            Value::Obj(o) => Some(o.clone()),
            _ => None,
        }
    }

    pub fn get(&mut self, v: &Value, k: &str) -> Res<Value> {
        self.get_hint(v, k, None)
    }

    /// A property read; `ic` remembers where the name was found, to look there first next time.
    pub(crate) fn get_hint(&mut self, v: &Value, k: &str, ic: Option<&Cell<u32>>) -> Res<Value> {
        match v {
            Value::Undefined | Value::Null => return self.throw("TypeError", &format!("can't read '{}' of {}", show_key(k), if matches!(v, Value::Null) { "null" } else { "undefined" })),
            Value::Str(s) => {
                if k == "length" {
                    return Ok(Value::Num(utf16_len(s) as f64));
                }
                if let Some(i) = index_of(k) {
                    return Ok(string_at(s, i));
                }
            }
            Value::Obj(o) => {
                let b = o.borrow();
                match &b.kind {
                    Kind::Plain | Kind::Function(_) | Kind::Error => {}
                    Kind::Proxy(t, h) => {
                        let (t, h) = (t.clone(), h.clone());
                        drop(b);
                        return self.proxy_get(t, h, k, v);
                    }
                    Kind::Array(a) => {
                        if k == "length" {
                            return Ok(Value::Num(a.len() as f64));
                        }
                        if let Some(i) = index_of(k) {
                            return Ok(a.get(i).cloned().unwrap_or(Value::Undefined));
                        }
                    }
                    Kind::Typed(t) if !t.data_view => {
                        if k == "length" {
                            return Ok(Value::Num(t.len as f64));
                        }
                        if let Some(i) = index_of(k) {
                            return Ok(t.get(i).map(Value::Num).unwrap_or(Value::Undefined));
                        }
                    }
                    Kind::Map(m) if k == "size" => return Ok(Value::Num(m.len() as f64)),
                    Kind::Set(s) if k == "size" => return Ok(Value::Num(s.len() as f64)),
                    Kind::Boxed(Value::Str(s)) if k == "length" => return Ok(Value::Num(utf16_len(s) as f64)),
                    _ => {}
                }
            }
            _ => {}
        }
        let hint = ic.map(|c| c.get()).unwrap_or(0);
        // A primitive's own "properties" start at its prototype.
        let mut cur = self.proto_of(v);
        let mut first = true;
        let mut steps = 0;
        while let Some(o) = cur {
            let slot = {
                let b = o.borrow();
                match b.find(k, hint) {
                    Some(i) => {
                        if let Some(c) = ic {
                            c.set(i as u32);
                        }
                        match b.slot_at(i) {
                            Slot::Data(val) => return Ok(val.clone()),
                            Slot::Accessor(g, _) => Some(g.clone()),
                        }
                    }
                    None => None,
                }
            };
            if let Some(getter) = slot {
                return match getter {
                    Some(g) => self.call(&g, v.clone(), vec![]),
                    None => Ok(Value::Undefined),
                };
            }
            if first {
                first = false;
                if let Some(val) = crate::vm::lazy_function_prop(self, &o, k) {
                    return Ok(val);
                }
            }
            if k == "__proto__" {
                return Ok(o.borrow().proto.clone().map(Value::Obj).unwrap_or(Value::Null));
            }
            steps += 1;
            if steps > 1000 {
                break;
            }
            cur = o.borrow().proto.clone();
        }
        Ok(Value::Undefined)
    }

    pub fn set(&mut self, v: &Value, k: &str, val: Value) -> Res<()> {
        self.set_hint(v, k, val, None)
    }

    pub(crate) fn set_hint(&mut self, v: &Value, k: &str, val: Value, ic: Option<&Cell<u32>>) -> Res<()> {
        let Value::Obj(o) = v else {
            if matches!(v, Value::Undefined | Value::Null) {
                return self.throw("TypeError", &format!("can't set '{}' of {v:?}", show_key(k)));
            }
            return Ok(()); // setting on a primitive does nothing
        };
        {
            let mut b = o.borrow_mut();
            match &mut b.kind {
                Kind::Plain | Kind::Function(_) | Kind::Error => {}
                Kind::Proxy(..) => {
                    drop(b);
                    let (t, h) = Self::proxy_parts(v).expect("a proxy");
                    return self.proxy_set(t, h, k, val, v);
                }
                Kind::Typed(t) if !t.data_view => {
                    if let Some(i) = index_of(k) {
                        let t = t.clone();
                        drop(b);
                        let n = match &val {
                            Value::Num(n) => *n,
                            v => self.to_number(v)?,
                        };
                        t.put(i, n);
                        return Ok(());
                    }
                }
                Kind::Array(a) => {
                    if k == "length" {
                        let n = match &val {
                            Value::Num(n) if *n >= 0.0 && n.fract() == 0.0 => *n as usize,
                            _ => {
                                drop(b);
                                return self.throw("RangeError", "invalid array length");
                            }
                        };
                        a.resize(n, Value::Undefined);
                        return Ok(());
                    }
                    if let Some(i) = index_of(k) {
                        if i < 10_000_000 {
                            if i >= a.len() {
                                a.resize(i + 1, Value::Undefined);
                            }
                            a[i] = val;
                            return Ok(());
                        }
                    }
                }
                _ => {}
            }
            // An own data property: just replace it.
            let hint = ic.map(|c| c.get()).unwrap_or(0);
            if let Some(i) = b.find(k, hint) {
                if let Some(c) = ic {
                    c.set(i as u32);
                }
                if let Slot::Data(d) = b.slot_at_mut(i) {
                    *d = val;
                    return Ok(());
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
        // A setter anywhere up the chain runs; otherwise it's a new own data property.
        let mut cur = Some(o.clone());
        while let Some(c) = cur {
            let setter = match c.borrow().get_own(k) {
                Some(Slot::Accessor(_, s)) => Some(s.clone()),
                _ => None,
            };
            if let Some(s) = setter {
                if let Some(s) = s {
                    self.call(&s, v.clone(), vec![val])?;
                }
                return Ok(());
            }
            cur = c.borrow().proto.clone();
        }
        let i = o.borrow_mut().set_own_rc(&Rc::from(k), val, 0);
        if let Some(c) = ic {
            c.set(i as u32);
        }
        Ok(())
    }

    pub fn has_property(&self, o: &Obj, k: &str) -> bool {
        match &o.borrow().kind {
            Kind::Array(a) if k == "length" || k.parse::<usize>().map(|i| i < a.len()).unwrap_or(false) => return true,
            Kind::Typed(t) if !t.data_view && k.parse::<usize>().map(|i| i < t.len).unwrap_or(false) => return true,
            _ => {}
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

    /// Runs a whole script in the global scope; returns the last expression statement's value.
    /// Starts a fresh step budget (for each script and each event a page handles).
    /// The busiest functions in the profile, one per line.
    pub fn profile_report(&self, top: usize) -> String {
        let Some(p) = &self.profile else { return String::new() };
        let total: u64 = p.values().sum::<u64>().max(1);
        let mut rows: Vec<_> = p.iter().collect();
        rows.sort_by(|a, b| b.1.cmp(a.1));
        let head = format!("{:.2} s of script time, by function (time between samples goes to the function running at the sample)\n", total as f64 / 1e9);
        head + &rows
            .iter()
            .take(top)
            .map(|((name, at, url), n)| {
                let file = url.as_deref().map(|u| u.rsplit('/').next().unwrap_or(u).split('?').next().unwrap_or(u)).unwrap_or("inline");
                format!("{:5.1}%  {} ({file}: {})\n", **n as f64 * 100.0 / total as f64, name.as_deref().unwrap_or("<anonymous>"), crate::parse::Where::unpack(*at))
            })
            .collect::<String>()
    }

    pub fn reset_steps(&mut self, budget: u64) {
        self.steps = 0;
        self.max_steps = budget;
        self.deadline = self.time_limit.map(|d| std::time::Instant::now() + d);
        if self.profile.is_some() {
            self.profile_at = Some(std::time::Instant::now());
        }
    }

    /// Past the step budget or the time limit (which then counts as the budget).
    #[cold]
    pub(crate) fn out_of_time(&mut self) -> bool {
        if self.steps > self.max_steps {
            return true;
        }
        if matches!(self.deadline, Some(d) if std::time::Instant::now() > d) {
            self.max_steps = self.steps.saturating_sub(1);
            return true;
        }
        false
    }

    /// A RegExp object for a literal (compiled once per pattern).
    pub fn regexp(&mut self, body: &Rc<str>, flags: &Rc<str>) -> Res<Value> {
        let key = (body.clone(), flags.clone());
        let re = match self.regex_cache.get(&key) {
            Some(r) => r.clone(),
            None => {
                let r = match crate::regex::Regex::new(body, flags) {
                    Ok(r) => Rc::new(r),
                    Err(e) => return self.throw("SyntaxError", &e),
                };
                self.regex_cache.insert(key, r.clone());
                r
            }
        };
        let o = self.obj(Kind::RegExp(re, body.clone(), flags.clone()));
        if let Value::Obj(oo) = &o {
            oo.borrow_mut().set_own("lastIndex", Value::Num(0.0));
        }
        Ok(o)
    }

    /// Runs a classic script; its completion value (the last expression statement's).
    pub fn run(&mut self, src: &str) -> Result<Value, String> {
        self.run_at(src, None)
    }

    /// A classic script, from `url` when it's an external one.
    pub fn run_at(&mut self, src: &str, url: Option<&str>) -> Result<Value, String> {
        let prog = crate::parse::parse(src).map_err(|e| format!("SyntaxError: {e}"))?;
        let code = match url {
            Some(u) => crate::compile::Compiler::script_at(&prog, u),
            None => crate::compile::Compiler::script(&prog),
        }
        .map_err(|e| format!("SyntaxError: {e}"))?;
        let this = Value::Obj(self.global_obj.clone());
        let r = self.run_code(code, this);
        // Promise reactions run as soon as the script ends; timers wait for the caller's event loop.
        self.run_microtasks();
        r.map_err(|v| self.describe(&v))
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

    /// The values of something iterable (arrays, strings, Map, Set, array-likes).
    pub fn iterate(&mut self, v: &Value) -> Res<Vec<Value>> {
        match v {
            Value::Str(s) => Ok(s.chars().map(|c| Value::Str(c.to_string().into())).collect()),
            Value::Obj(o) => {
                let kind_list = match &o.borrow().kind {
                    Kind::Array(a) => Some(a.clone()),
                    Kind::Typed(t) if !t.data_view => Some(t.values().into_iter().map(Value::Num).collect()),
                    Kind::Set(s) => Some(s.keys().cloned().collect()),
                    Kind::Map(m) => Some(m.iter().map(|(k, v)| self.array(vec![k.clone(), v.clone()])).collect()),
                    _ => None,
                };
                if let Some(l) = kind_list {
                    return Ok(l);
                }
                // One of our own iterators: the rest of its items.
                if let Some(rest) = crate::promise::iterator_rest(o) {
                    return Ok(rest);
                }
                // An iterable: [Symbol.iterator]() gives the iterator.
                let it_fn = self.get(v, crate::promise::SYM_ITERATOR)?;
                if Self::is_callable(&it_fn) {
                    let it = self.call(&it_fn, v.clone(), vec![])?;
                    if let Value::Obj(io) = &it {
                        if let Some(rest) = crate::promise::iterator_rest(io) {
                            return Ok(rest);
                        }
                    }
                    if !Self::strict_eq(&it, v) {
                        return self.iterate(&it);
                    }
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
        if self.depth > self.max_depth {
            self.depth -= 1;
            return Err(self.recursion_error());
        }
        let r = self.call_inner(f, this, args);
        self.depth -= 1;
        r
    }

    /// "too much recursion", naming the functions going round (to make the loop findable).
    pub(crate) fn recursion_error(&self) -> Value {
        let mut names: Vec<String> = Vec::new();
        for (n, line) in self.call_names.iter().rev().take(12) {
            names.push(format!("{} {}", n.as_deref().unwrap_or("function"), crate::parse::Where::unpack(*line)));
        }
        self.error("RangeError", &format!("too much recursion (in {})", names.join(" ← ")))
    }

    fn call_inner(&mut self, f: &Value, this: Value, args: Vec<Value>) -> Res<Value> {
        let Value::Obj(o) = f else {
            return self.throw("TypeError", &format!("{f:?} is not a function"));
        };
        let native = match &o.borrow().kind {
            Kind::Function(Callable::Native(n)) => Some(n.clone()),
            Kind::Function(Callable::Closure(_)) => None,
            Kind::Proxy(..) => {
                let (target, handler) = Self::proxy_parts(f).expect("a proxy");
                let trap = self.get(&handler, "apply")?;
                if !Self::is_callable(&trap) {
                    return self.call(&target, this, args);
                }
                let list = self.array(args);
                return self.call(&trap, handler, vec![target, this, list]);
            }
            _ => return self.throw("TypeError", "not a function"),
        };
        match native {
            Some(n) => n(self, this, args),
            None => self.call_closure(f, this, args, false),
        }
    }

    /// `Reflect.construct(f, args, new_target)`: builds as `f` does, with
    /// `new_target.prototype` as the new object's prototype.
    pub fn construct_as(&mut self, f: &Value, args: Vec<Value>, new_target: &Value) -> Res<Value> {
        if !Self::is_callable(new_target) {
            return self.throw("TypeError", "Reflect.construct: newTarget is not a constructor");
        }
        let same = matches!((f, new_target), (Value::Obj(a), Value::Obj(b)) if Rc::ptr_eq(a, b));
        let closure = matches!(f, Value::Obj(o) if matches!(o.borrow().kind, Kind::Function(Callable::Closure(_))));
        if same || !Self::is_callable(f) || Self::proxy_parts(f).is_some() {
            return self.construct(f, args);
        }
        if closure {
            let obj = self.new_this(new_target)?;
            self.depth += 1;
            if self.depth > self.max_depth {
                self.depth -= 1;
                return Err(self.recursion_error());
            }
            let r = self.call_closure_as(f, obj, args, true, new_target.clone());
            self.depth -= 1;
            return r;
        }
        // A built-in: build as usual, then give it new_target's prototype.
        let made = self.construct(f, args)?;
        if let (Value::Obj(o), Value::Obj(p)) = (&made, self.get(new_target, "prototype")?) {
            o.borrow_mut().proto = Some(p);
        }
        Ok(made)
    }

    pub fn construct(&mut self, f: &Value, args: Vec<Value>) -> Res<Value> {
        if !Self::is_callable(f) {
            return self.throw("TypeError", "not a constructor");
        }
        if let Some((target, handler)) = Self::proxy_parts(f) {
            let trap = self.get(&handler, "construct")?;
            if !Self::is_callable(&trap) {
                return self.construct(&target, args);
            }
            let list = self.array(args);
            return self.call(&trap, handler, vec![target, list, f.clone()]);
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
        let obj = self.new_this(f)?;
        self.depth += 1;
        if self.depth > self.max_depth {
            self.depth -= 1;
            return Err(self.recursion_error());
        }
        let r = self.call_closure(f, obj, args, true);
        self.depth -= 1;
        r
    }

    /// The object `new f()` starts with: f.prototype's child (an Error if it inherits from Error).
    pub(crate) fn new_this(&mut self, f: &Value) -> Res<Value> {
        let proto = match self.get(f, "prototype")? {
            Value::Obj(p) => p,
            _ => self.object_proto.clone(),
        };
        let kind = if self.inherits_error(&proto) { Kind::Error } else { Kind::Plain };
        Ok(Value::Obj(Rc::new(RefCell::new(Object::new(Some(proto), kind)))))
    }

    pub(crate) fn inherits_error(&self, p: &Obj) -> bool {
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
        // A constructor's [Symbol.hasInstance] decides, if it has one.
        if let Value::Obj(fo) = f {
            let own = match fo.borrow().get_own("\0@hasInstance") {
                Some(Slot::Data(h)) => Some(h.clone()),
                _ => None,
            };
            if let Some(h) = own.filter(Self::is_callable) {
                let r = self.call(&h, f.clone(), vec![v.clone()])?;
                return Ok(Self::truthy(&r));
            }
        }
        let Value::Obj(o) = v else { return Ok(false) };
        let Value::Obj(p) = self.get(f, "prototype")? else {
            return Ok(false);
        };
        let mut cur = o.borrow().proto.clone();
        while let Some(c) = cur {
            if Rc::ptr_eq(&c, &p) {
                return Ok(true);
            }
            cur = c.borrow().proto.clone();
        }
        Ok(false)
    }
}

/// A property key as an array index, if it is one.
#[inline]
pub fn index_of(k: &str) -> Option<usize> {
    let b = k.as_bytes();
    if b.is_empty() || b.len() > 10 || (b[0] == b'0' && b.len() > 1) || !b.iter().all(|c| c.is_ascii_digit()) {
        return None;
    }
    k.parse().ok()
}

/// How a key reads in an error message (symbols without their marker).
fn show_key(k: &str) -> String {
    match k.strip_prefix("\0@") {
        Some(s) => format!("Symbol({})", s.split_once(':').map(|x| x.1).unwrap_or(s)),
        None => k.to_string(),
    }
}

/// Strings shorter than this are measured each time; longer ones go through [`STR_INFO`].
const CACHE_AT: usize = 256;

/// What's known about a long string being indexed: whether it's ASCII, its
/// UTF-16 length, and where the last lookup landed (loops walk strings in order).
struct StrInfo {
    s: Rc<str>,
    ascii: bool,
    len16: usize,
    at16: usize,
    at_byte: usize,
}

thread_local! {
    /// The last few long strings indexed. Each entry holds its string, so the
    /// address it's found by can't be reused by another string while cached.
    static STR_INFO: RefCell<([Option<StrInfo>; 4], usize)> = const { RefCell::new(([None, None, None, None], 0)) };
}

fn with_info<T>(s: &Rc<str>, f: impl FnOnce(&mut StrInfo) -> T) -> T {
    STR_INFO.with(|c| {
        let mut c = c.borrow_mut();
        let (slots, next) = &mut *c;
        if let Some(e) = slots.iter_mut().flatten().find(|e| Rc::ptr_eq(&e.s, s)) {
            return f(e);
        }
        let ascii = s.is_ascii();
        let len16 = if ascii { s.len() } else { s.encode_utf16().count() };
        let k = *next;
        *next = (k + 1) % slots.len();
        slots[k] = Some(StrInfo { s: s.clone(), ascii, len16, at16: 0, at_byte: 0 });
        f(slots[k].as_mut().expect("just set"))
    })
}

/// Where UTF-16 unit `k` is: the byte offset of its character, and whether it's
/// that character's second (low surrogate) unit.
fn locate(e: &mut StrInfo, k: usize) -> Option<(usize, bool)> {
    if k >= e.len16 {
        return None;
    }
    if e.ascii {
        return Some((k, false));
    }
    if k < e.at16 {
        (e.at16, e.at_byte) = (0, 0);
    }
    let (mut u, mut b) = (e.at16, e.at_byte);
    loop {
        let c = e.s[b..].chars().next()?;
        let w = c.len_utf16();
        if k < u + w {
            (e.at16, e.at_byte) = (u, b);
            return Some((b, k > u));
        }
        u += w;
        b += c.len_utf8();
    }
}

/// A string's length in UTF-16 units, as JS counts it.
pub fn utf16_len(s: &Rc<str>) -> usize {
    if s.len() >= CACHE_AT {
        return with_info(s, |e| e.len16);
    }
    if s.is_ascii() {
        s.len()
    } else {
        s.encode_utf16().count()
    }
}

/// Where UTF-16 index `k` falls: the byte offset of the character holding it,
/// and whether it's that character's second unit. Past the end: `(len, false)`.
fn place16(s: &Rc<str>, k: usize) -> (usize, bool) {
    if s.len() >= CACHE_AT {
        return with_info(s, |e| locate(e, k)).unwrap_or((s.len(), false));
    }
    if s.is_ascii() {
        return (k.min(s.len()), false);
    }
    let mut u = 0;
    for (b, c) in s.char_indices() {
        let w = c.len_utf16();
        if k < u + w {
            return (b, k > u);
        }
        u += w;
    }
    (s.len(), false)
}

/// The byte offset where UTF-16 index `k` starts (after the character, if `k`
/// is inside a surrogate pair).
pub fn byte_at16(s: &Rc<str>, k: usize) -> usize {
    match place16(s, k) {
        (b, true) => b + s[b..].chars().next().map(char::len_utf8).unwrap_or(0),
        (b, false) => b,
    }
}

/// The UTF-16 index of a byte offset (on a character boundary).
pub fn index16(s: &Rc<str>, byte: usize) -> usize {
    let ascii = if s.len() >= CACHE_AT { with_info(s, |e| e.ascii) } else { s.is_ascii() };
    if ascii {
        byte
    } else {
        s[..byte.min(s.len())].encode_utf16().count()
    }
}

/// UTF-16 units a..b of a string, as `slice` and `substring` cut it (half a
/// surrogate pair, which a Rust string can't hold, becomes U+FFFD).
pub fn slice16(s: &Rc<str>, a: usize, b: usize) -> Rc<str> {
    if a >= b {
        return "".into();
    }
    let (ba, a_mid) = place16(s, a);
    let (bb, b_mid) = place16(s, b);
    if !a_mid && !b_mid {
        return if ba == 0 && bb == s.len() { s.clone() } else { s[ba..bb].into() };
    }
    let width = |at: usize| s[at..].chars().next().map(char::len_utf8).unwrap_or(0);
    let mut out = String::new();
    let from = if a_mid {
        out.push('\u{FFFD}');
        ba + width(ba)
    } else {
        ba
    };
    if from < bb {
        out.push_str(&s[from..bb]);
    }
    if b_mid && !(a_mid && ba == bb) {
        out.push('\u{FFFD}');
    }
    out.into()
}

/// UTF-16 unit `k` of a string (`charCodeAt`).
pub fn utf16_unit(s: &Rc<str>, k: usize) -> Option<u16> {
    if s.len() < CACHE_AT {
        return s.encode_utf16().nth(k);
    }
    with_info(s, |e| {
        let (b, low) = locate(e, k)?;
        let c = e.s[b..].chars().next()?;
        let mut buf = [0u16; 2];
        let units = c.encode_utf16(&mut buf);
        units.get(low as usize).copied()
    })
}

/// The character at index i (ASCII strings index directly).
#[inline]
pub fn string_at(s: &Rc<str>, i: usize) -> Value {
    let at = if s.len() < CACHE_AT {
        if s.is_ascii() {
            return match s.as_bytes().get(i) {
                Some(_) => Value::Str(s[i..i + 1].into()),
                None => Value::Undefined,
            };
        }
        let mut u = 0;
        s.char_indices().find_map(|(b, c)| {
            let w = c.len_utf16();
            let hit = (i < u + w).then_some((b, i > u));
            u += w;
            hit
        })
    } else {
        with_info(s, |e| locate(e, i))
    };
    match at {
        // Half of a surrogate pair can't stand alone in a Rust string.
        Some((_, true)) => Value::Str("\u{FFFD}".into()),
        Some((b, false)) => {
            let c = s[b..].chars().next().unwrap_or('\u{FFFD}');
            Value::Str(s[b..b + c.len_utf8()].into())
        }
        None => Value::Undefined,
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
