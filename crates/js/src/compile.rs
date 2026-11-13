//! The compiler: syntax tree → bytecode for [`crate::vm`].
//!
//! Every variable is resolved here, once: to a slot in its function's frame
//! (a register), or, when a nested function captures it, to a slot in a heap
//! environment that the closure keeps alive. Names found nowhere are globals.
//! Blocks whose captured `let`s need a fresh binding per loop iteration get
//! their own environment; everything else costs nothing at run time.
//!
//! The capture analysis is by name and errs on the safe side: a function's
//! variable lives in an environment if any function nested in it mentions
//! that name without declaring it itself.

use crate::interp::{num_to_string, Value};
use crate::parse::{Export, Expr, Func, Member, Prop, Stmt};
use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

/// One instruction. The operand stack effect is noted as [before → after].
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Op {
    Undef,
    Null,
    True,
    False,
    Int(i32),
    /// consts[i]
    Const(u32),
    Pop,
    /// [a → a a]
    Dup,
    /// [a b → a b a b]
    Dup2,
    /// [a b → b a]
    Swap,
    /// [a b → a b a]
    Over,
    /// [a b c → b c a]
    Rot3,
    /// [a b → b]
    Nip,
    /// `i++;` / `i--;` as a statement, on a local: (slot, +1 or -1)
    IncLocal(u32, i32),
    /// A script's top-level `var`: the global property exists (undefined if new).
    DeclareVar(u32),
    GetLocal(u32),
    /// Stores (keeping the value) / puts (popping it).
    SetLocal(u32),
    PutLocal(u32),
    /// (hops up the environment chain, slot)
    GetEnv(u32, u32),
    SetEnv(u32, u32),
    PutEnv(u32, u32),
    /// (name, inline cache)
    GetGlobal(u32, u32),
    /// A global assignment: the name, and an inline cache of where it was.
    SetGlobal(u32, u32),
    PutGlobal(u32, u32),
    TypeofGlobal(u32),
    /// A top-level `let`/`const`/`class` of a script: [v →] (name, const?)
    DeclareLex(u32, bool),
    /// An assignment to a `const`: throws.
    ConstAssign(u32),
    /// A new environment of n slots (for captured block-scoped names).
    PushEnv(u32),
    PopEnv,
    /// A fresh copy of the current environment (each loop iteration's own `let`s).
    CloneEnv,
    /// `with`: [w → v] if w has the name, jumping over the fallback (skip ops); else [w →].
    WithGet(u32, u32),
    /// [v w → v] likewise, for assignment.
    WithSet(u32, u32),
    /// (name, inline cache): [o → v]
    GetProp(u32, u32),
    /// [o v → v]
    SetProp(u32, u32),
    /// [o k → v]
    GetElem,
    /// [o k v → v]
    SetElem,
    /// [o → o f]: a method, keeping its object (for a call)
    GetMethod(u32, u32),
    /// [o k → o f]
    GetElemMethod,
    /// [o → bool]
    DelProp(u32),
    DelElem,
    DelGlobal(u32),
    /// [k o → bool]
    In,
    /// [v f → bool]
    InstanceOf,
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Exp,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    UShr,
    Eq,
    Ne,
    StrictEq,
    StrictNe,
    Lt,
    Gt,
    Le,
    Ge,
    Not,
    Neg,
    ToNum,
    BitNot,
    Typeof,
    Inc,
    Dec,
    Jump(u32),
    JumpIfFalse(u32),
    JumpIfTrue(u32),
    /// Jumps keeping the value when it's falsy (`&&`) / truthy (`||`), else pops it.
    JumpIfFalseKeep(u32),
    JumpIfTrueKeep(u32),
    /// `??`: jumps keeping the value unless it's null or undefined, else pops it.
    JumpIfNotNullishKeep(u32),
    /// `?.`: [v → undefined] and jumps if v is null or undefined.
    JumpIfNullish(u32),
    /// `f?.()`: [this f → undefined] and jumps if f is null or undefined.
    JumpIfNullishCall(u32),
    Return,
    Throw,
    /// A `try` region: exceptions jump to the handler with the exception pushed.
    TryStart(u32),
    TryEnd,
    /// [this f args… → r]
    Call(u32),
    /// [this f args-array → r]
    CallSpread,
    /// [f args… → o]
    New(u32),
    NewSpread,
    /// `super(...)` in a derived constructor: [args… → undefined]
    SuperCall(u32),
    SuperCallSpread,
    /// A closure over funcs[i].
    Closure(u32),
    This,
    /// The function `new` called, or undefined.
    NewTarget,
    /// The function being run (a named function expression's own name).
    Callee,
    Arguments,
    /// The parent class (for `super`).
    LoadSuper,
    /// The i'th argument / the arguments from i on, as an array.
    Arg(u32),
    ArgsRest(u32),
    NewObject,
    /// [items… → array]
    NewArray(u32),
    /// [arr v → arr]
    ArrayPush,
    /// [arr iterable → arr]
    ArraySpread,
    /// [o v → o]
    InitProp(u32),
    /// [o k v → o]
    InitElem,
    /// [o f → o]: (name, getter?)
    InitAccessor(u32, bool),
    /// [o k f → o]
    InitAccessorElem(bool),
    /// [o src → o]
    ObjSpread,
    /// [o p → o]: `__proto__: p`
    InitProto,
    /// [sup? ctor → ctor proto] (has a parent class?)
    MakeClass(bool),
    /// [ctor proto f → ctor proto]: (name, flags: 1 static, 2 getter, 4 setter)
    DefMethod(u32, u8),
    /// [ctor proto k f → ctor proto]
    DefMethodElem(u8),
    /// [ctor proto f → ctor proto]: the instance-field initialiser
    SetFields,
    /// [ctor proto f → ctor proto]: runs f with `this` = ctor (static fields and blocks)
    RunStatic,
    /// [v → iter]
    GetIter,
    /// [iter → iter v], or [iter] and a jump when it's done
    IterNext(u32),
    /// [iter → iter v] (undefined once done)
    IterValue,
    /// [iter → iter array-of-the-rest]
    IterRest,
    /// [iter →], telling the iterator it wasn't run to the end
    IterClose,
    /// [o → iter over its enumerable keys]
    ForInKeys,
    /// [o → o rest] without the names in consts[i] (an array)
    ObjRest(u32),
    /// [v → v], throwing for null and undefined (destructuring)
    RequireObject,
    /// [n parts → string]
    Template(u32),
    /// consts[i] is the pattern, consts[i + 1] the flags
    Regex(u32),
    /// [v → sent]
    Yield,
    /// [v → result]
    Await,
    /// A module's hoisting is done: it waits here until it's evaluated.
    InitDone,
    /// [spec → promise]
    Import,
    ImportMeta,
    /// [v → v] names an anonymous function or class: consts[i]
    SetName(u32),
    Nop,
}

/// A compiled function (or script, or module).
pub struct Code {
    pub name: Option<Rc<str>>,
    pub ops: Vec<Op>,
    pub consts: Vec<Value>,
    pub funcs: Vec<Rc<Code>>,
    /// Inline caches for property and global lookups: where the name was found last.
    pub ics: Vec<Cell<u32>>,
    pub nlocals: u32,
    /// Slots in the function's own environment (0: it needs none).
    pub env_size: u32,
    /// Declared parameters before any default or rest (`length`).
    pub nparams: u32,
    pub arrow: bool,
    pub is_async: bool,
    pub is_gen: bool,
    pub strict: bool,
    /// A method or class member: no `prototype` of its own.
    pub method: bool,
    /// A class constructor.
    pub class_ctor: bool,
    /// It makes its own environment on entry (env_size slots).
    pub has_env: bool,
    pub line: u32,
    /// The module (or page) URL it came from, for `import()` and `import.meta`.
    pub url: Option<Rc<str>>,
    /// What each call site calls, for "x is not a function": (pc, description).
    pub call_names: Vec<(u32, Rc<str>)>,
}

impl std::fmt::Debug for Code {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "function {:?} (locals {}, env {})", self.name, self.nlocals, self.env_size)?;
        for (i, op) in self.ops.iter().enumerate() {
            writeln!(f, "  {i:4} {op:?}")?;
        }
        Ok(())
    }
}

/// What a module declares, for linking: its imports and exports by slot.
#[derive(Default, Debug)]
pub struct ModuleInfo {
    /// (specifier, env slot holding that module's namespace)
    pub imports: Vec<(Rc<str>, u32)>,
    /// (exported name, env slot)
    pub exports: Vec<(Rc<str>, u32)>,
    /// (specifier, [(imported, exported)]) re-exports; "*" imported = the namespace
    pub reexports: Vec<(Rc<str>, Vec<(Rc<str>, Rc<str>)>)>,
    /// `export * from`
    pub star: Vec<Rc<str>>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Loc {
    Local(u32),
    /// (environment level within its function, slot)
    Env(u32, u32),
    /// `this`, `super`, `arguments` read straight from the frame
    This,
    Super,
    Arguments,
    /// A module import: (env level, slot of the namespace, name const)
    Import(u32, u32, u32),
}

struct Binding {
    name: Rc<str>,
    loc: Loc,
    constant: bool,
}

struct Scope {
    names: Vec<Binding>,
    /// This scope pushed an environment (at this level).
    env: Option<u32>,
    env_slots: u32,
    /// The hidden variable holding a `with` statement's object.
    with: Option<Loc>,
    locals_at_start: u32,
}

enum CtxKind {
    Loop,
    Breakable,
    Try { finally: Option<Rc<Vec<Stmt>>>, active: bool },
}

struct Ctx {
    kind: CtxKind,
    labels: Vec<Rc<str>>,
    breaks: Vec<usize>,
    continues: Vec<usize>,
    env_count: u32,
    /// Operand stack values held by enclosing statements, outside and inside this one.
    pins_outside: u32,
    pins_inside: u32,
}

#[derive(PartialEq, Clone, Copy)]
enum Top {
    Function,
    Script,
    Module,
}

struct FnState {
    ops: Vec<Op>,
    consts: Vec<Value>,
    const_strs: HashMap<Rc<str>, u32>,
    funcs: Vec<Rc<Code>>,
    nics: u32,
    scopes: Vec<Scope>,
    captured: HashSet<Rc<str>>,
    env_count: u32,
    nlocals: u32,
    max_locals: u32,
    arrow: bool,
    is_async: bool,
    is_gen: bool,
    strict: bool,
    top: Top,
    ctxs: Vec<Ctx>,
    pins: u32,
    /// A script's completion value (the last expression statement's).
    completion: Option<u32>,
    /// Names of the function's own environment's slots in use.
    fn_env_slots: u32,
    /// A class field initialiser or static block: `this` is the frame's.
    label_next: Vec<Rc<str>>,
    call_names: Vec<(u32, Rc<str>)>,
    /// The latest position something jumps to (no peephole may merge across it).
    barrier: Cell<usize>,
}

type R<T> = Result<T, String>;

pub struct Compiler {
    fns: Vec<FnState>,
    free_cache: FreeCache,
    pub module: ModuleInfo,
    url: Option<Rc<str>>,
    /// The function being compiled is a declaration (its name isn't bound inside it).
    declaring: bool,
    /// The names imported bindings read from their modules' namespaces.
    import_names: Vec<Rc<str>>,
    /// Functions the compiler made itself (default constructors, field initialisers).
    keep: Vec<Rc<Func>>,
}

type FreeCache = HashMap<*const Func, Rc<FreeNames>>;

/// What `f` mentions without declaring at its own top level (nested
/// functions included), and whether it or its arrows use `this`,
/// `arguments` or `super`. Computed once per function.
fn free_of(cache: &mut FreeCache, f: &Func) -> Rc<FreeNames> {
    let key = f as *const Func;
    if let Some(c) = cache.get(&key) {
        return c.clone();
    }
    let mut names = HashSet::new();
    let (this, arguments, sup) = {
        let mut m = Mentions { names: &mut names, cache, this: false, arguments: false, sup: false };
        m.params_and_body(f);
        (m.this, m.arguments, m.sup)
    };
    let mut declared = Vec::new();
    for p in &f.params {
        crate::parse::pattern_names(p, &mut declared);
    }
    var_names(&f.body, &mut declared);
    for d in function_decls(&f.body) {
        if let Some(n) = &d.name {
            declared.push(n.clone());
        }
    }
    let mut lex = Vec::new();
    lexical_names(&f.body, &mut lex);
    declared.extend(lex.into_iter().map(|(n, _)| n));
    for d in declared {
        names.remove(&d);
    }
    let fr = Rc::new(FreeNames { names, this, arguments, sup });
    cache.insert(key, fr.clone());
    fr
}

/// What a function mentions without declaring.
#[derive(Default)]
struct FreeNames {
    names: HashSet<Rc<str>>,
    this: bool,
    arguments: bool,
    sup: bool,
}

// ------------------------------------------------------------------ analysis

/// Every name mentioned anywhere in these statements (nested functions too),
/// and whether `this`/`arguments`/`super` are used outside non-arrow functions.
struct Mentions<'a> {
    names: &'a mut HashSet<Rc<str>>,
    /// What each nested function leaves free, worked out once per function.
    cache: &'a mut FreeCache,
    this: bool,
    arguments: bool,
    sup: bool,
}

impl Mentions<'_> {
    fn stmts(&mut self, ss: &[Stmt]) {
        ss.iter().for_each(|s| self.stmt(s));
    }
    fn stmt(&mut self, s: &Stmt) {
        match s {
            Stmt::Expr(e) | Stmt::Throw(e) => self.expr(e),
            Stmt::Var(_, decls) => {
                for (p, i) in decls {
                    self.expr(p);
                    if let Some(i) = i {
                        self.expr(i);
                    }
                }
            }
            Stmt::Func(f) => self.func(f),
            Stmt::Return(e) => {
                if let Some(e) = e {
                    self.expr(e)
                }
            }
            Stmt::If(c, a, b) => {
                self.expr(c);
                self.stmt(a);
                if let Some(b) = b {
                    self.stmt(b);
                }
            }
            Stmt::Block(b) => self.stmts(b),
            Stmt::For(i, t, u, b) => {
                if let Some(i) = i {
                    self.stmt(i);
                }
                if let Some(t) = t {
                    self.expr(t);
                }
                if let Some(u) = u {
                    self.expr(u);
                }
                self.stmt(b);
            }
            Stmt::ForIn(_, p, _, it, b) => {
                self.expr(p);
                self.expr(it);
                self.stmt(b);
            }
            Stmt::While(c, b) | Stmt::DoWhile(b, c) | Stmt::With(c, b) => {
                self.expr(c);
                self.stmt(b);
            }
            Stmt::Try(a, c, f) => {
                self.stmts(a);
                if let Some((p, b)) = c {
                    if let Some(p) = p {
                        self.expr(p);
                    }
                    self.stmts(b);
                }
                if let Some(f) = f {
                    self.stmts(f);
                }
            }
            Stmt::Switch(d, cases) => {
                self.expr(d);
                for (t, b) in cases {
                    if let Some(t) = t {
                        self.expr(t);
                    }
                    self.stmts(b);
                }
            }
            Stmt::Labeled(_, b) => self.stmt(b),
            Stmt::Class(n, sup, members) => {
                self.names.insert(n.clone());
                self.class(sup.as_ref(), members);
            }
            Stmt::Export(e) => match &**e {
                Export::Decl(s, _) => self.stmt(s),
                Export::Default(e) => self.expr(e),
                Export::Names(names) => names.iter().for_each(|(l, _)| {
                    self.names.insert(l.clone());
                }),
                _ => {}
            },
            Stmt::Break(_) | Stmt::Continue(_) | Stmt::Empty | Stmt::Import(..) => {}
        }
    }
    fn class(&mut self, sup: Option<&Expr>, members: &[(Expr, Member, bool)]) {
        if let Some(s) = sup {
            self.expr(s);
        }
        for (k, m, _) in members {
            self.expr(k);
            match m {
                Member::Method(f) | Member::Get(f) | Member::Set(f) => self.func(f),
                // Field initialisers and static blocks run as methods: their own `this`.
                Member::Field(Some(e)) => {
                    let mut inner = Mentions { names: self.names, cache: self.cache, this: false, arguments: false, sup: false };
                    inner.expr(e);
                }
                Member::StaticBlock(b) => {
                    let mut inner = Mentions { names: self.names, cache: self.cache, this: false, arguments: false, sup: false };
                    inner.stmts(b);
                }
                Member::Field(None) => {}
            }
        }
    }
    /// A nested function contributes what it leaves free (and, for an arrow, its use of `this`).
    fn func(&mut self, f: &Func) {
        let fr = free_of(self.cache, f);
        self.names.extend(fr.names.iter().cloned());
        if f.arrow {
            self.this |= fr.this;
            self.arguments |= fr.arguments;
            self.sup |= fr.sup;
        }
    }
    fn params_and_body(&mut self, f: &Func) {
        for p in &f.params {
            self.expr(p);
        }
        self.stmts(&f.body);
    }
    fn expr(&mut self, e: &Expr) {
        match e {
            Expr::Ident(n) => {
                if &**n == "arguments" {
                    self.arguments = true;
                } else if &**n == "super" {
                    self.sup = true;
                    self.this = true;
                }
                self.names.insert(n.clone());
            }
            Expr::This => self.this = true,
            Expr::Template(_, es) | Expr::Array(es) | Expr::Seq(es) => es.iter().for_each(|x| self.expr(x)),
            Expr::Object(props) => {
                for p in props {
                    match p {
                        Prop::KeyValue(k, v) => {
                            self.expr(k);
                            self.expr(v);
                        }
                        Prop::Spread(v) => self.expr(v),
                        Prop::Getter(k, f) | Prop::Setter(k, f) => {
                            self.expr(k);
                            self.func(f);
                        }
                    }
                }
            }
            Expr::Func(f) => self.func(f),
            Expr::Unary(_, x) | Expr::Update(_, _, x) | Expr::Spread(x) | Expr::Await(x) | Expr::Rest(x) | Expr::Import(x) => self.expr(x),
            Expr::Binary(_, a, b) | Expr::Logical(_, a, b) | Expr::Assign(_, a, b) | Expr::Member(a, b, _) | Expr::Default(a, b) => {
                self.expr(a);
                self.expr(b);
            }
            Expr::Cond(a, b, c) => {
                self.expr(a);
                self.expr(b);
                self.expr(c);
            }
            Expr::Call(f, args, _) | Expr::New(f, args) => {
                self.expr(f);
                args.iter().for_each(|x| self.expr(x));
            }
            Expr::ArrayPat(items) => items.iter().flatten().for_each(|x| self.expr(x)),
            Expr::ObjectPat(props) => props.iter().for_each(|(k, v)| {
                self.expr(k);
                self.expr(v);
            }),
            Expr::Yield(x, _) => {
                if let Some(x) = x {
                    self.expr(x);
                }
            }
            _ => {}
        }
    }
}

/// The `var` names declared anywhere in a function body (not in nested functions).
fn var_names(ss: &[Stmt], out: &mut Vec<Rc<str>>) {
    for s in ss {
        var_names_stmt(s, out);
    }
}

fn var_names_stmt(s: &Stmt, out: &mut Vec<Rc<str>>) {
    match s {
        Stmt::Var("var", decls) => decls.iter().for_each(|(p, _)| crate::parse::pattern_names(p, out)),
        Stmt::ForIn("var", p, _, _, b) => {
            crate::parse::pattern_names(p, out);
            var_names_stmt(b, out);
        }
        Stmt::If(_, a, b) => {
            var_names_stmt(a, out);
            if let Some(b) = b {
                var_names_stmt(b, out);
            }
        }
        Stmt::Block(b) => var_names(b, out),
        Stmt::For(i, _, _, b) => {
            if let Some(i) = i {
                var_names_stmt(i, out);
            }
            var_names_stmt(b, out);
        }
        Stmt::ForIn(_, _, _, _, b) | Stmt::While(_, b) | Stmt::DoWhile(b, _) | Stmt::Labeled(_, b) | Stmt::With(_, b) => var_names_stmt(b, out),
        Stmt::Try(a, c, f) => {
            var_names(a, out);
            if let Some((_, c)) = c {
                var_names(c, out);
            }
            if let Some(f) = f {
                var_names(f, out);
            }
        }
        Stmt::Switch(_, cases) => cases.iter().for_each(|(_, b)| var_names(b, out)),
        Stmt::Export(e) => {
            if let Export::Decl(d, _) = &**e {
                var_names_stmt(d, out);
            }
        }
        _ => {}
    }
}

/// The block-scoped names a statement list declares directly: (name, const?).
fn lexical_names(ss: &[Stmt], out: &mut Vec<(Rc<str>, bool)>) {
    for s in ss {
        let s = match s {
            Stmt::Export(e) => match &**e {
                Export::Decl(d, _) => d,
                _ => continue,
            },
            s => s,
        };
        match s {
            Stmt::Var(k @ ("let" | "const"), decls) => {
                for (p, _) in decls {
                    let mut ns = Vec::new();
                    crate::parse::pattern_names(p, &mut ns);
                    out.extend(ns.into_iter().map(|n| (n, *k == "const")));
                }
            }
            Stmt::Class(n, ..) => out.push((n.clone(), false)),
            _ => {}
        }
    }
}

/// The function declarations directly in a statement list.
fn function_decls(ss: &[Stmt]) -> Vec<Rc<Func>> {
    let mut out = Vec::new();
    for s in ss {
        match s {
            Stmt::Func(f) => out.push(f.clone()),
            Stmt::Export(e) => {
                if let Export::Decl(Stmt::Func(f), _) = &**e {
                    out.push(f.clone());
                }
            }
            _ => {}
        }
    }
    out
}

fn has_use_strict(body: &[Stmt]) -> bool {
    for s in body {
        match s {
            Stmt::Expr(Expr::Str(d)) if &**d == "use strict" => return true,
            Stmt::Expr(Expr::Str(_)) => {}
            _ => return false,
        }
    }
    false
}

// ------------------------------------------------------------------ compiler

impl Default for Compiler {
    fn default() -> Self {
        Self::new()
    }
}

impl Compiler {
    pub fn new() -> Self {
        Compiler { fns: Vec::new(), free_cache: HashMap::new(), module: ModuleInfo::default(), url: None, declaring: false, import_names: Vec::new(), keep: Vec::new() }
    }

    /// What a nested function mentions without declaring at its own top level.
    fn free_names(&mut self, f: &Rc<Func>) -> Rc<FreeNames> {
        free_of(&mut self.free_cache, f)
    }

    /// The names functions nested in this body capture from it.
    fn captures(&mut self, params: &[Expr], body: &[Stmt], extra: &[&[Stmt]]) -> (HashSet<Rc<str>>, bool, bool, bool) {
        let mut funcs = Vec::new();
        let mut classes = Vec::new();
        collect_funcs_exprs(params, &mut funcs, &mut classes);
        collect_funcs(body, &mut funcs, &mut classes);
        for e in extra {
            collect_funcs(e, &mut funcs, &mut classes);
        }
        let mut out = HashSet::new();
        let (mut this, mut args, mut sup) = (false, false, false);
        for f in funcs {
            let fr = self.free_names(&f);
            out.extend(fr.names.iter().cloned());
            this |= f.arrow && fr.this;
            args |= f.arrow && fr.arguments;
            sup |= f.arrow && fr.sup;
        }
        // Class field initialisers and static blocks see the enclosing scope too.
        for names in classes {
            out.extend(names);
        }
        (out, this, args, sup)
    }

    // -------------------------------------------------------------- entry points

    /// A classic script: its top-level declarations are globals.
    pub fn script(src_prog: &[Stmt]) -> R<Rc<Code>> {
        let mut c = Compiler::new();
        c.top_level(src_prog, Top::Script)
    }

    /// A module: everything at its top level lives in its environment.
    pub fn module(prog: &[Stmt], url: &str) -> R<(Rc<Code>, ModuleInfo)> {
        let mut c = Compiler::new();
        c.url = Some(url.into());
        let code = c.top_level(prog, Top::Module)?;
        Ok((code, std::mem::take(&mut c.module)))
    }

    fn top_level(&mut self, prog: &[Stmt], top: Top) -> R<Rc<Code>> {
        let strict = top == Top::Module || has_use_strict(prog);
        let (mut captured, this_cap, _, _) = self.captures(&[], prog, &[]);
        if top == Top::Module {
            // Everything a module declares can be read by importers.
            let mut all = Vec::new();
            var_names(prog, &mut all);
            let mut lex = Vec::new();
            lexical_names(prog, &mut lex);
            all.extend(lex.into_iter().map(|(n, _)| n));
            all.extend(function_decls(prog).into_iter().filter_map(|f| f.name.clone()));
            captured.extend(all);
            captured.insert("default".into());
        }
        if this_cap {
            captured.insert("this".into());
        }
        self.push_fn(captured, false, top == Top::Module, false, strict, top);
        let fi = self.fns.len() - 1;
        if top == Top::Script {
            let slot = self.alloc_local();
            self.fns[fi].completion = Some(slot);
        }
        // `this` at the top: the global object (script) or undefined (module).
        if this_cap {
            let loc = self.declare_in_fn_scope("this", false);
            self.emit(Op::This);
            self.store_loc(loc, true);
        } else {
            self.cur_scope().names.push(Binding { name: "this".into(), loc: Loc::This, constant: true });
        }
        if top == Top::Module {
            let mut names = Vec::new();
            var_names(prog, &mut names);
            for n in names {
                self.declare_in_fn_scope(&n, false);
            }
            let mut lex = Vec::new();
            lexical_names(prog, &mut lex);
            for (n, c) in lex {
                let loc = self.declare_in_fn_scope(&n, false);
                let _ = c;
                let _ = loc;
            }
            self.declare_in_fn_scope("default", false);
            self.module_imports(prog)?;
            for f in function_decls(prog) {
                self.declare_in_fn_scope(f.name.as_deref().unwrap_or("default"), false);
            }
            self.hoist_functions(prog)?;
            self.module_exports(prog)?;
            self.emit(Op::InitDone);
        } else {
            // Script top level: `var` and functions are global object properties.
            for f in function_decls(prog) {
                self.declaring = true;
                let idx = self.func(&f, None)?;
                self.emit(Op::Closure(idx));
                let n = self.name_const(f.name.as_deref().unwrap_or(""));
                let ic = self.ic();
                self.emit(Op::PutGlobal(n, ic));
            }
            let mut vars = Vec::new();
            var_names(prog, &mut vars);
            for v in vars {
                // `var x;` creates the property if it isn't there already.
                let n = self.name_const(&v);
                self.emit(Op::DeclareVar(n));
            }
        }
        for s in prog {
            if let (Stmt::Expr(e), Some(slot)) = (s, self.fns[fi].completion) {
                self.expr(e)?;
                self.emit(Op::PutLocal(slot));
            } else {
                self.stmt(s)?;
            }
        }
        match self.fns[fi].completion {
            Some(slot) => self.emit(Op::GetLocal(slot)),
            None => self.emit(Op::Undef),
        };
        self.emit(Op::Return);
        let st = self.fns.pop().expect("the top level");
        Ok(Rc::new(finish(st, None, 0, false, false, 0, self.url.clone())))
    }

    fn module_imports(&mut self, prog: &[Stmt]) -> R<()> {
        for s in prog {
            if let Stmt::Import(spec, names) = s {
                let ns_name: Rc<str> = format!("\0ns{}", self.module.imports.len()).into();
                let loc = self.declare_in_fn_scope(&ns_name, true);
                let Loc::Env(level, slot) = loc else {
                    return Err("module namespace not in the environment".into());
                };
                self.module.imports.push((spec.clone(), slot));
                for (imported, local) in names {
                    if &**imported == "*" {
                        // The namespace itself.
                        let scope = self.cur_scope();
                        scope.names.push(Binding { name: local.clone(), loc: Loc::Env(level, slot), constant: true });
                    } else {
                        self.import_names.push(imported.clone());
                        let k = self.import_names.len() as u32 - 1;
                        self.cur_scope().names.push(Binding { name: local.clone(), loc: Loc::Import(level, slot, k), constant: true });
                    }
                }
            }
        }
        Ok(())
    }

    fn module_exports(&mut self, prog: &[Stmt]) -> R<()> {
        for s in prog {
            let Stmt::Export(e) = s else { continue };
            match &**e {
                Export::Decl(_, names) | Export::Names(names) => {
                    for (local, exported) in names {
                        match self.lookup(local) {
                            Some((Loc::Env(_, slot), _)) => self.module.exports.push((exported.clone(), slot)),
                            Some((Loc::Import(_, ns, k), _)) => {
                                // Re-exporting an imported name: through its module's namespace.
                                let name = self.import_names[k as usize].clone();
                                let spec = self.module.imports.iter().find(|(_, s)| *s == ns).map(|(sp, _)| sp.clone());
                                if let Some(spec) = spec {
                                    self.module.reexports.push((spec, vec![(name, exported.clone())]));
                                }
                            }
                            _ => return Err(format!("export of undeclared '{local}'")),
                        }
                    }
                }
                Export::Default(_) => {
                    if let Some((Loc::Env(_, slot), _)) = self.lookup("default") {
                        self.module.exports.push(("default".into(), slot));
                    }
                }
                Export::From(spec, names) => self.module.reexports.push((spec.clone(), names.clone())),
                Export::All(spec) => self.module.star.push(spec.clone()),
            }
        }
        Ok(())
    }

    // -------------------------------------------------------------- functions

    fn push_fn(&mut self, captured: HashSet<Rc<str>>, arrow: bool, is_async: bool, is_gen: bool, strict: bool, top: Top) {
        let mut st = FnState {
            ops: Vec::new(),
            consts: Vec::new(),
            const_strs: HashMap::new(),
            funcs: Vec::new(),
            nics: 0,
            scopes: Vec::new(),
            captured,
            env_count: 0,
            nlocals: 0,
            max_locals: 0,
            arrow,
            is_async,
            is_gen,
            strict,
            top,
            ctxs: Vec::new(),
            pins: 0,
            completion: None,
            fn_env_slots: 0,
            label_next: Vec::new(),
            call_names: Vec::new(),
            barrier: Cell::new(0),
        };
        // The function's own environment, if anything in it is captured (made by the VM on entry).
        let needs_env = !st.captured.is_empty() || top == Top::Module;
        if needs_env {
            st.env_count = 1;
        }
        st.scopes.push(Scope { names: Vec::new(), env: needs_env.then_some(1), env_slots: 0, with: None, locals_at_start: 0 });
        self.fns.push(st);
    }

    /// Compiles a nested function; its index in the current function's `funcs`.
    fn func(&mut self, f: &Rc<Func>, name: Option<&str>) -> R<u32> {
        self.func_with(f, name, false, false)
    }

    fn func_with(&mut self, f: &Rc<Func>, name: Option<&str>, method: bool, class_ctor: bool) -> R<u32> {
        let strict = self.fns.last().map(|s| s.strict).unwrap_or(false) || has_use_strict(&f.body) || class_ctor || method;
        let (captured, this_cap, args_cap, sup_cap) = self.captures(&f.params, &f.body, &[]);
        let mut cap = captured;
        // The function's own name and its parameters are part of its scope.
        let own = self.free_names(f);
        let (uses_args, uses_super) = (own.arguments, own.sup);
        if this_cap && !f.arrow {
            cap.insert("this".into());
        }
        if args_cap && !f.arrow {
            cap.insert("arguments".into());
        }
        if sup_cap && !f.arrow {
            cap.insert("super".into());
        }
        let declaring = std::mem::take(&mut self.declaring);
        self.push_fn(cap, f.arrow, f.is_async, f.is_gen, strict, Top::Function);
        let r = self.func_body(f, uses_args, uses_super, declaring || method);
        let st = self.fns.pop().expect("a function");
        r?;
        let nparams = f.params.iter().take_while(|p| !matches!(p, Expr::Default(..) | Expr::Rest(_))).count() as u32;
        let name: Option<Rc<str>> = f.name.clone().or_else(|| name.map(Rc::from));
        let code = finish(st, name, nparams, method, class_ctor, f.line, self.url.clone());
        let cur = self.fns.last_mut().expect("an enclosing function");
        cur.funcs.push(Rc::new(code));
        Ok(cur.funcs.len() as u32 - 1)
    }

    fn func_body(&mut self, f: &Rc<Func>, uses_args: bool, uses_super: bool, declaring: bool) -> R<()> {
        // Pseudo-variables of non-arrow functions.
        if !f.arrow {
            for (name, loc, op) in [("this", Loc::This, Op::This), ("arguments", Loc::Arguments, Op::Arguments), ("super", Loc::Super, Op::LoadSuper)] {
                let used = match name {
                    "arguments" => uses_args,
                    "super" => uses_super,
                    _ => true,
                };
                if !used && name != "this" {
                    continue;
                }
                if self.fns.last().expect("fn").captured.contains(name) {
                    let l = self.declare_in_fn_scope(name, false);
                    self.emit(op);
                    self.store_loc(l, true);
                } else if name == "arguments" {
                    // A local, made once on entry.
                    let l = self.declare_in_fn_scope(name, false);
                    self.emit(op);
                    self.store_loc(l, true);
                } else {
                    self.cur_scope().names.push(Binding { name: name.into(), loc, constant: true });
                }
            }
        }
        // Parameters, then the body's declarations.
        let mut params = Vec::new();
        for p in &f.params {
            crate::parse::pattern_names(p, &mut params);
        }
        for p in &params {
            self.declare_in_fn_scope(p, false);
        }
        let mut vars = Vec::new();
        var_names(&f.body, &mut vars);
        for v in &vars {
            if self.lookup_here(v).is_none() {
                self.declare_in_fn_scope(v, false);
            }
        }
        let mut lex = Vec::new();
        lexical_names(&f.body, &mut lex);
        for (n, c) in &lex {
            if self.lookup_here(n).is_none() {
                let loc = self.declare_in_fn_scope(n, false);
                if *c {
                    self.set_const(n, loc);
                }
            }
        }
        let fdecls = function_decls(&f.body);
        for d in &fdecls {
            if let Some(n) = &d.name {
                if self.lookup_here(n).is_none() {
                    self.declare_in_fn_scope(n, false);
                }
            }
        }
        // A named function expression sees its own name (unless something shadows it); a method doesn't.
        if let (Some(n), false) = (&f.name, declaring) {
            if !f.arrow && self.lookup_here(n).is_none() {
                let loc = self.declare_in_fn_scope(n, false);
                self.emit(Op::Callee);
                self.store_loc(loc, true);
            }
        }
        for (i, p) in f.params.iter().enumerate() {
            match p {
                Expr::Ident(n) => {
                    self.emit(Op::Arg(i as u32));
                    let (loc, _) = self.lookup(n).ok_or("parameter not declared")?;
                    self.store_loc(loc, true);
                }
                Expr::Rest(t) => {
                    self.emit(Op::ArgsRest(i as u32));
                    self.bind(t, true)?;
                }
                p => {
                    self.emit(Op::Arg(i as u32));
                    self.bind(p, true)?;
                }
            }
        }
        self.hoist_functions(&f.body)?;
        for s in &f.body {
            self.stmt(s)?;
        }
        self.emit(Op::Undef);
        self.emit(Op::Return);
        Ok(())
    }

    /// Function declarations in a body exist before it runs.
    fn hoist_functions(&mut self, body: &[Stmt]) -> R<()> {
        for d in function_decls(body) {
            self.declaring = true;
            let idx = self.func(&d, None)?;
            self.emit(Op::Closure(idx));
            let name = d.name.clone().unwrap_or_else(|| "default".into());
            match self.lookup(&name) {
                Some((loc, _)) => self.store_loc(loc, true),
                None => {
                    let n = self.name_const(&name);
                    let ic = self.ic();
                    self.emit(Op::PutGlobal(n, ic));
                }
            }
        }
        Ok(())
    }

    // -------------------------------------------------------------- scopes

    fn st(&mut self) -> &mut FnState {
        self.fns.last_mut().expect("a function")
    }

    fn cur_scope(&mut self) -> &mut Scope {
        self.st().scopes.last_mut().expect("a scope")
    }

    fn alloc_local(&mut self) -> u32 {
        let st = self.st();
        let n = st.nlocals;
        st.nlocals += 1;
        st.max_locals = st.max_locals.max(st.nlocals);
        n
    }

    /// Declares a name in the function's own scope (its first).
    fn declare_in_fn_scope(&mut self, name: &str, force_env: bool) -> Loc {
        let st = self.st();
        let name: Rc<str> = name.into();
        if let Some(b) = st.scopes[0].names.iter().find(|b| b.name == name) {
            return b.loc;
        }
        let loc = if force_env || st.captured.contains(&name) {
            let slot = st.fn_env_slots;
            st.fn_env_slots += 1;
            st.scopes[0].env_slots += 1;
            Loc::Env(1, slot)
        } else {
            let n = st.nlocals;
            st.nlocals += 1;
            st.max_locals = st.max_locals.max(st.nlocals);
            Loc::Local(n)
        };
        st.scopes[0].names.push(Binding { name, loc, constant: false });
        loc
    }

    fn set_const(&mut self, name: &str, loc: Loc) {
        for sc in self.st().scopes.iter_mut().rev() {
            if let Some(b) = sc.names.iter_mut().find(|b| &*b.name == name && b.loc == loc) {
                b.constant = true;
                return;
            }
        }
    }

    /// A name in the current function's scopes only.
    fn lookup_here(&self, name: &str) -> Option<Loc> {
        let st = self.fns.last()?;
        for sc in st.scopes.iter().rev() {
            if let Some(b) = sc.names.iter().rev().find(|b| &*b.name == name) {
                return Some(b.loc);
            }
        }
        None
    }

    /// Opens a block scope declaring these names (in an environment if captured).
    fn enter_block(&mut self, names: &[(Rc<str>, bool)]) {
        let st = self.st();
        let locals_at_start = st.nlocals;
        let any_env = names.iter().any(|(n, _)| st.captured.contains(n));
        let env = if any_env {
            st.env_count += 1;
            Some(st.env_count)
        } else {
            None
        };
        let mut scope = Scope { names: Vec::new(), env, env_slots: 0, with: None, locals_at_start };
        for (n, c) in names {
            if scope.names.iter().any(|b| b.name == *n) {
                continue;
            }
            let loc = if let (true, Some(level)) = (st.captured.contains(n), env) {
                scope.env_slots += 1;
                Loc::Env(level, scope.env_slots - 1)
            } else {
                let l = st.nlocals;
                st.nlocals += 1;
                st.max_locals = st.max_locals.max(st.nlocals);
                Loc::Local(l)
            };
            scope.names.push(Binding { name: n.clone(), loc, constant: *c });
        }
        let slots = scope.env_slots;
        st.scopes.push(scope);
        if env.is_some() {
            self.emit(Op::PushEnv(slots));
        }
        // Locals start undefined in each block (a loop body runs many times).
        let locals: Vec<u32> = self.st().scopes.last().expect("scope").names.iter().filter_map(|b| if let Loc::Local(l) = b.loc { Some(l) } else { None }).collect();
        for l in locals {
            self.emit(Op::Undef);
            self.emit(Op::PutLocal(l));
        }
    }

    fn leave_block(&mut self) {
        let sc = self.st().scopes.pop().expect("a block scope");
        if sc.env.is_some() {
            self.emit(Op::PopEnv);
            self.st().env_count -= 1;
        }
        self.st().nlocals = sc.locals_at_start;
    }

    /// Where a name lives, from the innermost function out; None for a global.
    /// Also the `with` objects passed on the way (innermost first).
    fn resolve(&self, name: &str) -> (Option<(Loc, u32, bool)>, Vec<(Loc, u32)>) {
        let mut hops = 0u32;
        let mut withs = Vec::new();
        for fi in (0..self.fns.len()).rev() {
            let st = &self.fns[fi];
            let here = fi == self.fns.len() - 1;
            for sc in st.scopes.iter().rev() {
                if let Some(b) = sc.names.iter().rev().find(|b| &*b.name == name) {
                    let loc = b.loc;
                    let depth = match loc {
                        Loc::Env(level, _) | Loc::Import(level, ..) => hops + (st.env_count - level),
                        _ => 0,
                    };
                    if !here && matches!(loc, Loc::Local(_) | Loc::This | Loc::Super | Loc::Arguments) {
                        // Not captured after all (shouldn't happen): treat as global.
                        return (None, withs);
                    }
                    return (Some((loc, depth, b.constant)), withs);
                }
                if let Some(w) = sc.with {
                    if let Loc::Env(level, slot) = w {
                        withs.push((Loc::Env(level, slot), hops + (st.env_count - level)));
                    }
                }
            }
            hops += st.env_count;
            if st.top != Top::Function {
                break;
            }
        }
        (None, withs)
    }

    fn lookup(&self, name: &str) -> Option<(Loc, u32)> {
        self.resolve(name).0.map(|(l, d, _)| (l, d))
    }

    fn store_loc(&mut self, loc: Loc, pop: bool) {
        match loc {
            Loc::Local(i) => self.emit(if pop { Op::PutLocal(i) } else { Op::SetLocal(i) }),
            Loc::Env(level, slot) => {
                let depth = self.st().env_count - level;
                self.emit(if pop { Op::PutEnv(depth, slot) } else { Op::SetEnv(depth, slot) })
            }
            _ => {
                if pop {
                    self.emit(Op::Pop);
                }
                0
            }
        };
    }

    // -------------------------------------------------------------- emitting

    fn emit(&mut self, op: Op) -> usize {
        let st = self.st();
        // A store followed by a pop is a store that pops (unless something jumps in between).
        if op == Op::Pop && st.ops.len() > st.barrier.get() {
            let merged = match st.ops.last() {
                Some(Op::SetLocal(i)) => Some(Op::PutLocal(*i)),
                Some(Op::SetEnv(d, i)) => Some(Op::PutEnv(*d, *i)),
                Some(Op::SetGlobal(k, ic)) => Some(Op::PutGlobal(*k, *ic)),
                _ => None,
            };
            if let Some(m) = merged {
                *st.ops.last_mut().expect("an op") = m;
                return st.ops.len() - 1;
            }
        }
        st.ops.push(op);
        st.ops.len() - 1
    }

    /// The current position, as a jump target.
    fn here(&self) -> u32 {
        let st = self.fns.last().expect("fn");
        st.barrier.set(st.ops.len());
        st.ops.len() as u32
    }

    /// An expression whose value isn't used.
    fn expr_discard(&mut self, e: &Expr) -> R<()> {
        if let Expr::Update(op, _, target) = e {
            if let Expr::Ident(n) = &**target {
                if let (Some((Loc::Local(i), _, false)), true) = (self.resolve(n).0, self.resolve(n).1.is_empty()) {
                    self.emit(Op::IncLocal(i, if *op == "++" { 1 } else { -1 }));
                    return Ok(());
                }
            }
        }
        self.expr(e)?;
        self.emit(Op::Pop);
        Ok(())
    }

    fn patch(&mut self, at: usize) {
        let to = self.here();
        let op = &mut self.st().ops[at];
        *op = match *op {
            Op::Jump(_) => Op::Jump(to),
            Op::JumpIfFalse(_) => Op::JumpIfFalse(to),
            Op::JumpIfTrue(_) => Op::JumpIfTrue(to),
            Op::JumpIfFalseKeep(_) => Op::JumpIfFalseKeep(to),
            Op::JumpIfTrueKeep(_) => Op::JumpIfTrueKeep(to),
            Op::JumpIfNotNullishKeep(_) => Op::JumpIfNotNullishKeep(to),
            Op::JumpIfNullish(_) => Op::JumpIfNullish(to),
            Op::JumpIfNullishCall(_) => Op::JumpIfNullishCall(to),
            Op::TryStart(_) => Op::TryStart(to),
            Op::IterNext(_) => Op::IterNext(to),
            other => other,
        };
    }

    fn name_const(&mut self, s: &str) -> u32 {
        let st = self.st();
        if let Some(&i) = st.const_strs.get(s) {
            return i;
        }
        let rc: Rc<str> = s.into();
        st.consts.push(Value::Str(rc.clone()));
        let i = st.consts.len() as u32 - 1;
        st.const_strs.insert(rc, i);
        i
    }

    fn num_const(&mut self, n: f64) -> u32 {
        let st = self.st();
        st.consts.push(Value::Num(n));
        st.consts.len() as u32 - 1
    }

    fn ic(&mut self) -> u32 {
        let st = self.st();
        st.nics += 1;
        st.nics - 1
    }

    fn get_prop(&mut self, name: &str) {
        let n = self.name_const(name);
        let ic = self.ic();
        self.emit(Op::GetProp(n, ic));
    }

    // -------------------------------------------------------------- statements

    fn stmts(&mut self, ss: &[Stmt]) -> R<()> {
        for s in ss {
            self.stmt(s)?;
        }
        Ok(())
    }

    /// A block: its own scope for `let`, `const`, `class` and functions.
    fn block(&mut self, ss: &[Stmt]) -> R<()> {
        let mut lex = Vec::new();
        lexical_names(ss, &mut lex);
        let fdecls = function_decls(ss);
        lex.extend(fdecls.iter().filter_map(|f| f.name.clone()).map(|n| (n, false)));
        if lex.is_empty() {
            return self.stmts(ss);
        }
        self.enter_block(&lex);
        self.hoist_functions(ss)?;
        let r = self.stmts(ss);
        self.leave_block();
        r
    }

    fn stmt(&mut self, s: &Stmt) -> R<()> {
        let labels = std::mem::take(&mut self.st().label_next);
        match s {
            Stmt::Expr(e) => self.expr_discard(e)?,
            Stmt::Var(kind, decls) => {
                for (p, init) in decls {
                    match init {
                        Some(e) => {
                            let name = if let Expr::Ident(n) = p { Some(n.clone()) } else { None };
                            self.expr_named(e, name.as_deref())?;
                        }
                        None if *kind == "var" => continue,
                        None => {
                            self.emit(Op::Undef);
                        }
                    }
                    self.declare_bind(p, kind)?;
                }
            }
            Stmt::Func(_) | Stmt::Empty | Stmt::Import(..) => {}
            Stmt::Return(e) => {
                match e {
                    Some(e) => self.expr(e)?,
                    None => {
                        self.emit(Op::Undef);
                    }
                }
                self.ret()?;
            }
            Stmt::If(c, a, b) => {
                self.cond(c)?;
                let j = self.emit(Op::JumpIfFalse(0));
                self.stmt(a)?;
                if let Some(b) = b {
                    let j2 = self.emit(Op::Jump(0));
                    self.patch(j);
                    self.stmt(b)?;
                    self.patch(j2);
                } else {
                    self.patch(j);
                }
            }
            Stmt::Block(b) => {
                if labels.is_empty() {
                    self.block(b)?;
                } else {
                    self.push_ctx(CtxKind::Breakable, labels);
                    self.block(b)?;
                    self.pop_ctx_breakable();
                }
            }
            Stmt::While(c, body) => {
                let top = self.here();
                self.cond(c)?;
                let exit = self.emit(Op::JumpIfFalse(0));
                self.push_ctx(CtxKind::Loop, labels);
                self.stmt(body)?;
                self.patch_continues(top);
                self.emit(Op::Jump(top));
                self.patch(exit);
                self.pop_ctx_breakable();
            }
            Stmt::DoWhile(body, c) => {
                let top = self.here();
                self.push_ctx(CtxKind::Loop, labels);
                self.stmt(body)?;
                let cont = self.here();
                self.patch_continues(cont);
                self.cond(c)?;
                self.emit(Op::JumpIfTrue(top));
                self.pop_ctx_breakable();
            }
            Stmt::For(init, test, update, body) => self.for_loop(init.as_deref(), test.as_ref(), update.as_ref(), body, labels)?,
            Stmt::ForIn(kind, pat, of, iter, body) => self.for_in(kind, pat, *of, iter, body, labels)?,
            Stmt::Break(l) => self.jump_out(l.as_ref(), false)?,
            Stmt::Continue(l) => self.jump_out(l.as_ref(), true)?,
            Stmt::Throw(e) => {
                self.expr(e)?;
                self.emit(Op::Throw);
            }
            Stmt::Try(body, catch, finally) => self.try_stmt(body, catch.as_ref(), finally.as_ref())?,
            Stmt::Switch(d, cases) => self.switch(d, cases, labels)?,
            Stmt::Labeled(l, body) => {
                let mut ls = labels;
                ls.push(l.clone());
                let is_loop = matches!(&**body, Stmt::For(..) | Stmt::ForIn(..) | Stmt::While(..) | Stmt::DoWhile(..) | Stmt::Labeled(..));
                if is_loop {
                    self.st().label_next = ls;
                    self.stmt(body)?;
                } else {
                    self.push_ctx(CtxKind::Breakable, ls);
                    self.stmt(body)?;
                    self.pop_ctx_breakable();
                }
            }
            Stmt::With(o, body) => {
                self.expr(o)?;
                self.emit(Op::RequireObject);
                let name: Rc<str> = format!("\0with{}", self.here()).into();
                // The object lives in an environment, so nested functions see it too.
                let st = self.st();
                st.env_count += 1;
                let level = st.env_count;
                let locals_at_start = st.nlocals;
                st.scopes.push(Scope { names: vec![Binding { name, loc: Loc::Env(level, 0), constant: true }], env: Some(level), env_slots: 1, with: Some(Loc::Env(level, 0)), locals_at_start });
                self.emit(Op::PushEnv(1));
                self.emit(Op::PutEnv(0, 0));
                self.stmt(body)?;
                self.leave_block();
            }
            Stmt::Class(name, sup, members) => {
                self.class(Some(name), sup.as_ref(), members)?;
                self.declare_bind(&Expr::Ident(name.clone()), "let")?;
            }
            Stmt::Export(e) => match &**e {
                Export::Decl(d, _) => self.stmt(d)?,
                Export::Default(e) => {
                    self.expr_named(e, Some("default"))?;
                    let (loc, _) = self.lookup("default").ok_or("no default binding")?;
                    self.store_loc(loc, true);
                }
                _ => {}
            },
        }
        Ok(())
    }

    /// Stores the value on the stack into a declaration's pattern.
    fn declare_bind(&mut self, p: &Expr, kind: &str) -> R<()> {
        let top_script = self.fns.len() == 1 && self.fns[0].top == Top::Script && self.st().scopes.len() == 1;
        if top_script {
            if let Expr::Ident(n) = p {
                let c = self.name_const(n);
                if kind == "var" {
                    let ic = self.ic();
                    self.emit(Op::PutGlobal(c, ic));
                } else {
                    self.emit(Op::DeclareLex(c, kind == "const"));
                }
                return Ok(());
            }
            if kind != "var" {
                // A destructuring `let` at the top of a script: each name a global lexical.
                return self.bind_with(p, &mut |c: &mut Compiler, n: &Rc<str>| {
                    let k = c.name_const(n);
                    c.emit(Op::DeclareLex(k, kind == "const"));
                    Ok(())
                });
            }
        }
        self.bind(p, true)
    }

    fn ret(&mut self) -> R<()> {
        // Through any finally blocks on the way out.
        let has_finally = self.st().ctxs.iter().any(|c| matches!(c.kind, CtxKind::Try { .. }));
        if !has_finally {
            self.emit(Op::Return);
            return Ok(());
        }
        let tmp = self.alloc_local();
        self.emit(Op::PutLocal(tmp));
        let n = self.st().ctxs.len();
        for k in (0..n).rev() {
            self.leave_try_ctx(k)?;
        }
        self.emit(Op::GetLocal(tmp));
        self.emit(Op::Return);
        self.st().nlocals -= 1;
        Ok(())
    }

    /// Leaving a try context by a jump: ends its handler and runs its finally.
    fn leave_try_ctx(&mut self, k: usize) -> R<()> {
        let (active, finally) = match &self.st().ctxs[k].kind {
            CtxKind::Try { finally, active } => (*active, finally.clone()),
            _ => return Ok(()),
        };
        if active {
            self.emit(Op::TryEnd);
        }
        if let Some(f) = finally {
            // The finally block runs outside this try (its own jumps go further out).
            let saved: Vec<Ctx> = self.st().ctxs.drain(k..).collect();
            let r = self.block(&f);
            self.st().ctxs.extend(saved);
            r?;
        }
        Ok(())
    }

    fn push_ctx(&mut self, kind: CtxKind, labels: Vec<Rc<str>>) {
        let st = self.st();
        let (env_count, pins) = (st.env_count, st.pins);
        st.ctxs.push(Ctx { kind, labels, breaks: Vec::new(), continues: Vec::new(), env_count, pins_outside: pins, pins_inside: pins });
    }

    fn patch_continues(&mut self, to: u32) {
        let conts = std::mem::take(&mut self.st().ctxs.last_mut().expect("ctx").continues);
        for c in conts {
            if let Op::Jump(_) = self.st().ops[c] {
                self.st().ops[c] = Op::Jump(to);
            }
        }
    }

    fn pop_ctx_breakable(&mut self) {
        let ctx = self.st().ctxs.pop().expect("ctx");
        for b in ctx.breaks {
            self.patch(b);
        }
    }

    /// break / continue: out through finally blocks, environments and pinned values.
    fn jump_out(&mut self, label: Option<&Rc<str>>, cont: bool) -> R<()> {
        let n = self.st().ctxs.len();
        let mut target = None;
        for k in (0..n).rev() {
            let c = &self.st().ctxs[k];
            let fits = match label {
                Some(l) => c.labels.contains(l) && (!cont || matches!(c.kind, CtxKind::Loop)),
                None => matches!(c.kind, CtxKind::Loop) || (!cont && c.labels.iter().any(|l| &**l == "\0switch")),
            };
            if fits && !matches!(c.kind, CtxKind::Try { .. }) {
                target = Some(k);
                break;
            }
        }
        let Some(t) = target else {
            return Err(format!("{} outside a loop", if cont { "continue" } else { "break" }));
        };
        for k in (t + 1..n).rev() {
            self.leave_try_ctx(k)?;
        }
        let (env_target, pins_target) = {
            let c = &self.st().ctxs[t];
            (c.env_count, if cont { c.pins_inside } else { c.pins_outside })
        };
        let st = self.st();
        let envs = st.env_count - env_target;
        let pins = st.pins.saturating_sub(pins_target);
        for _ in 0..envs {
            self.emit(Op::PopEnv);
        }
        // A for-of's iterator is closed when a break leaves its loop.
        for k in 0..pins {
            let iter_of_loop = !cont && k + 1 == pins && matches!(self.st().ctxs[t].kind, CtxKind::Loop) && self.st().ctxs[t].pins_inside > self.st().ctxs[t].pins_outside;
            self.emit(if iter_of_loop { Op::IterClose } else { Op::Pop });
        }
        let j = self.emit(Op::Jump(0));
        let c = &mut self.st().ctxs[t];
        if cont {
            c.continues.push(j);
        } else {
            c.breaks.push(j);
        }
        Ok(())
    }

    fn for_loop(&mut self, init: Option<&Stmt>, test: Option<&Expr>, update: Option<&Expr>, body: &Stmt, labels: Vec<Rc<str>>) -> R<()> {
        // `for (let ...)`: a scope around the loop; a fresh copy per iteration if captured.
        let mut lex = Vec::new();
        if let Some(Stmt::Var(k @ ("let" | "const"), decls)) = init {
            for (p, _) in decls {
                let mut ns = Vec::new();
                crate::parse::pattern_names(p, &mut ns);
                lex.extend(ns.into_iter().map(|n| (n, *k == "const")));
            }
        }
        let scoped = !lex.is_empty();
        if scoped {
            self.enter_block(&lex);
        }
        let per_iteration = scoped && self.st().scopes.last().map(|s| s.env.is_some()).unwrap_or(false);
        if let Some(i) = init {
            self.stmt(i)?;
        }
        let top = self.here();
        let exit = match test {
            Some(t) => {
                self.cond(t)?;
                Some(self.emit(Op::JumpIfFalse(0)))
            }
            None => None,
        };
        self.push_ctx(CtxKind::Loop, labels);
        self.stmt(body)?;
        let cont = self.here();
        self.patch_continues(cont);
        if per_iteration {
            self.emit(Op::CloneEnv);
        }
        if let Some(u) = update {
            self.expr_discard(u)?;
        }
        self.emit(Op::Jump(top));
        if let Some(e) = exit {
            self.patch(e);
        }
        self.pop_ctx_breakable();
        if scoped {
            self.leave_block();
        }
        Ok(())
    }

    fn for_in(&mut self, kind: &str, pat: &Expr, of: bool, iter: &Expr, body: &Stmt, labels: Vec<Rc<str>>) -> R<()> {
        self.expr(iter)?;
        self.emit(if of { Op::GetIter } else { Op::ForInKeys });
        self.st().pins += 1;
        let top = self.here();
        let next = self.emit(Op::IterNext(0));
        // Each iteration binds afresh.
        let mut lex = Vec::new();
        if kind == "let" || kind == "const" {
            let mut ns = Vec::new();
            crate::parse::pattern_names(pat, &mut ns);
            lex.extend(ns.into_iter().map(|n| (n, kind == "const")));
        }
        let scoped = !lex.is_empty();
        self.push_ctx(CtxKind::Loop, labels);
        {
            let c = self.st().ctxs.last_mut().expect("ctx");
            c.pins_outside -= 1;
        }
        if scoped {
            self.enter_block(&lex);
            self.bind(pat, true)?;
        } else if kind == "var" {
            self.bind(pat, true)?;
        } else {
            self.bind(pat, false)?;
        }
        self.stmt(body)?;
        if scoped {
            self.leave_block();
        }
        let cont = self.here();
        self.patch_continues(cont);
        self.emit(Op::Jump(top));
        self.patch(next);
        self.st().pins -= 1;
        self.emit(Op::Pop);
        self.pop_ctx_breakable();
        Ok(())
    }

    fn switch(&mut self, d: &Expr, cases: &[(Option<Expr>, Vec<Stmt>)], labels: Vec<Rc<str>>) -> R<()> {
        self.expr(d)?;
        self.st().pins += 1;
        let mut lex = Vec::new();
        let mut fdecls = Vec::new();
        for (_, b) in cases {
            lexical_names(b, &mut lex);
            fdecls.extend(function_decls(b));
        }
        lex.extend(fdecls.iter().filter_map(|f| f.name.clone()).map(|n| (n, false)));
        let scoped = !lex.is_empty();
        if scoped {
            self.enter_block(&lex);
            for (_, b) in cases {
                self.hoist_functions(b)?;
            }
        }
        let mut jumps = Vec::new();
        for (t, _) in cases {
            match t {
                Some(t) => {
                    self.emit(Op::Dup);
                    self.expr(t)?;
                    self.emit(Op::StrictEq);
                    jumps.push(Some(self.emit(Op::JumpIfTrue(0))));
                }
                None => jumps.push(None),
            }
        }
        let to_default = self.emit(Op::Jump(0));
        self.push_ctx(CtxKind::Breakable, labels);
        {
            let c = self.st().ctxs.last_mut().expect("ctx");
            c.pins_outside -= 1;
            // A plain break leaves the switch.
            c.labels.push("\0switch".into());
        }
        let mut default_at = None;
        for (k, (_, body)) in cases.iter().enumerate() {
            match jumps[k] {
                Some(j) => self.patch(j),
                None => default_at = Some(self.here()),
            }
            self.stmts(body)?;
        }
        // Breaks land here, before the discriminant is popped… so they pop it themselves.
        let end_normal = self.emit(Op::Jump(0));
        match default_at {
            Some(at) => self.st().ops[to_default] = Op::Jump(at),
            None => self.patch(to_default),
        }
        // No case matched (and no default): fall out.
        self.patch(end_normal);
        self.emit(Op::Pop);
        self.st().pins -= 1;
        // A break pops the discriminant on its way (pins_outside), so it lands after the Pop.
        let ctx = self.st().ctxs.pop().expect("ctx");
        for b in ctx.breaks {
            self.patch(b);
        }
        if scoped {
            self.leave_block();
        }
        Ok(())
    }

    fn try_stmt(&mut self, body: &[Stmt], catch: Option<&(Option<Expr>, Vec<Stmt>)>, finally: Option<&Vec<Stmt>>) -> R<()> {
        let fin: Option<Rc<Vec<Stmt>>> = finally.map(|f| Rc::new(f.clone()));
        let start = self.emit(Op::TryStart(0));
        self.push_ctx(CtxKind::Try { finally: fin.clone(), active: true }, Vec::new());
        self.block(body)?;
        self.st().ctxs.pop();
        self.emit(Op::TryEnd);
        if let Some(f) = &fin {
            self.block(f)?;
        }
        let done = self.emit(Op::Jump(0));
        self.patch(start);
        // The exception is on the stack.
        let mut done2 = None;
        if let Some((param, cbody)) = catch {
            let finally_start = if fin.is_some() {
                let s = self.emit(Op::TryStart(0));
                self.push_ctx(CtxKind::Try { finally: fin.clone(), active: true }, Vec::new());
                Some(s)
            } else {
                None
            };
            match param {
                Some(p) => {
                    let mut names = Vec::new();
                    crate::parse::pattern_names(p, &mut names);
                    self.enter_block(&names.into_iter().map(|n| (n, false)).collect::<Vec<_>>());
                    self.bind(p, true)?;
                    self.block(cbody)?;
                    self.leave_block();
                }
                None => {
                    self.emit(Op::Pop);
                    self.block(cbody)?;
                }
            }
            if let (Some(s), Some(f)) = (finally_start, &fin) {
                self.st().ctxs.pop();
                self.emit(Op::TryEnd);
                self.block(f)?;
                done2 = Some(self.emit(Op::Jump(0)));
                self.patch(s);
            }
        }
        if let Some(f) = &fin {
            // An exception passing through: run finally, then rethrow it.
            if catch.is_none() || done2.is_some() {
                let tmp = self.alloc_local();
                self.emit(Op::PutLocal(tmp));
                self.block(f)?;
                self.emit(Op::GetLocal(tmp));
                self.emit(Op::Throw);
                self.st().nlocals -= 1;
            }
        }
        self.patch(done);
        if let Some(d) = done2 {
            self.patch(d);
        }
        Ok(())
    }

    // -------------------------------------------------------------- classes

    /// Leaves the class constructor on the stack.
    fn class(&mut self, name: Option<&Rc<str>>, sup: Option<&Expr>, members: &[(Expr, Member, bool)]) -> R<()> {
        // The class's own name is visible inside it.
        let inner_name = name.filter(|n| !n.is_empty() && &***n != "_class").cloned();
        if let Some(n) = &inner_name {
            self.enter_block(&[(n.clone(), true)]);
        }
        if let Some(s) = sup {
            self.expr(s)?;
        }
        let ctor = members.iter().find_map(|(k, m, st)| match m {
            Member::Method(f) if !st && matches!(k, Expr::Str(n) if &**n == "constructor") => Some(f.clone()),
            _ => None,
        });
        let ctor = ctor.unwrap_or_else(|| {
            let body =
                if sup.is_some() { vec![Stmt::Expr(Expr::Call(Box::new(Expr::Ident("super".into())), vec![Expr::Spread(Box::new(Expr::Ident("arguments".into())))], false))] } else { Vec::new() };
            Rc::new(Func { name: None, params: Vec::new(), body, arrow: false, expr_body: false, is_async: false, is_gen: false, line: 0 })
        });
        // Functions made here stay alive while compiling (the analysis cache is keyed by address).
        self.keep.push(ctor.clone());
        let cname = name.filter(|n| &***n != "_class").map(|n| n.to_string());
        let idx = self.func_with(&ctor, cname.as_deref(), false, true)?;
        self.emit(Op::Closure(idx));
        self.emit(Op::MakeClass(sup.is_some()));
        if let Some(n) = &inner_name {
            // [ctor proto]: bind the name for the methods and static initialisers.
            self.emit(Op::Over);
            let (loc, _) = self.lookup(n).ok_or("class name")?;
            self.store_loc(loc, true);
        }
        let mut fields = Vec::new();
        let mut statics = Vec::new();
        for (k, m, is_static) in members {
            if !is_static && matches!(k, Expr::Str(n) if &**n == "constructor") {
                continue;
            }
            let flags = if *is_static { 1 } else { 0 };
            match m {
                Member::Method(f) | Member::Get(f) | Member::Set(f) => {
                    let flags = flags
                        | match m {
                            Member::Get(_) => 2,
                            Member::Set(_) => 4,
                            _ => 0,
                        };
                    match k {
                        Expr::Str(n) => {
                            let idx = self.func_with(f, Some(n), true, false)?;
                            self.emit(Op::Closure(idx));
                            let c = self.name_const(n);
                            self.emit(Op::DefMethod(c, flags));
                        }
                        k => {
                            self.expr(k)?;
                            let idx = self.func_with(f, None, true, false)?;
                            self.emit(Op::Closure(idx));
                            self.emit(Op::DefMethodElem(flags));
                        }
                    }
                }
                Member::Field(init) => {
                    let target = Expr::Member(Box::new(Expr::This), Box::new(k.clone()), false);
                    let assign = Stmt::Expr(Expr::Assign("=", Box::new(target), Box::new(init.clone().unwrap_or(Expr::Undefined))));
                    if *is_static {
                        statics.push(assign);
                    } else {
                        fields.push(assign);
                    }
                }
                Member::StaticBlock(body) => statics.push(Stmt::Block(body.clone())),
            }
        }
        for (list, op) in [(fields, Op::SetFields), (statics, Op::RunStatic)] {
            if list.is_empty() {
                continue;
            }
            let f = Rc::new(Func { name: None, params: Vec::new(), body: list, arrow: false, expr_body: false, is_async: false, is_gen: false, line: 0 });
            self.keep.push(f.clone());
            let idx = self.func_with(&f, None, true, false)?;
            self.emit(Op::Closure(idx));
            self.emit(op);
        }
        self.emit(Op::Pop);
        if inner_name.is_some() {
            self.leave_block();
        }
        Ok(())
    }

    // -------------------------------------------------------------- patterns

    /// Stores the value on the stack into a target (popping it): a name, a
    /// member, or a destructuring pattern. `declare`: names are declarations
    /// (already in scope), so `const` is allowed.
    fn bind(&mut self, p: &Expr, declare: bool) -> R<()> {
        self.bind_with(p, &mut |c: &mut Compiler, n: &Rc<str>| c.store_name(n, true, declare))
    }

    fn bind_with(&mut self, p: &Expr, store: &mut dyn FnMut(&mut Compiler, &Rc<str>) -> R<()>) -> R<()> {
        match p {
            Expr::Ident(n) => store(self, n),
            Expr::Member(o, k, _) => {
                // [v] → set o[k] = v
                self.expr(o)?;
                match &**k {
                    Expr::Str(name) => {
                        self.emit(Op::Swap);
                        let c = self.name_const(name);
                        let ic = self.ic();
                        self.emit(Op::SetProp(c, ic));
                    }
                    k => {
                        self.expr(k)?;
                        self.emit(Op::Rot3);
                        self.emit(Op::SetElem);
                    }
                }
                self.emit(Op::Pop);
                Ok(())
            }
            Expr::Default(t, d) => {
                self.emit(Op::Dup);
                self.emit(Op::Undef);
                self.emit(Op::StrictEq);
                let j = self.emit(Op::JumpIfFalse(0));
                self.emit(Op::Pop);
                let name = if let Expr::Ident(n) = &**t { Some(n.clone()) } else { None };
                self.expr_named(d, name.as_deref())?;
                self.patch(j);
                self.bind_with(t, store)
            }
            Expr::ArrayPat(items) => {
                self.emit(Op::GetIter);
                for it in items {
                    match it {
                        None => {
                            self.emit(Op::IterValue);
                            self.emit(Op::Pop);
                        }
                        Some(Expr::Rest(t)) => {
                            self.emit(Op::IterRest);
                            self.bind_with(t, store)?;
                        }
                        Some(p) => {
                            self.emit(Op::IterValue);
                            self.bind_with(p, store)?;
                        }
                    }
                }
                self.emit(Op::Pop);
                Ok(())
            }
            Expr::ObjectPat(props) => {
                self.emit(Op::RequireObject);
                let mut used: Vec<Value> = Vec::new();
                for (k, v) in props {
                    if let Expr::Rest(t) = v {
                        let list = Value::Undefined;
                        let _ = list;
                        let st = self.st();
                        st.consts.push(Value::Undefined);
                        let ci = st.consts.len() as u32 - 1;
                        self.pending_rest_names(ci, used.clone());
                        self.emit(Op::ObjRest(ci));
                        self.bind_with(t, store)?;
                        continue;
                    }
                    self.emit(Op::Dup);
                    match k {
                        Expr::Str(name) => {
                            used.push(Value::Str(name.clone()));
                            self.get_prop(name);
                        }
                        k => {
                            self.expr(k)?;
                            self.emit(Op::GetElem);
                        }
                    }
                    self.bind_with(v, store)?;
                }
                self.emit(Op::Pop);
                Ok(())
            }
            _ => Err("invalid destructuring target".into()),
        }
    }

    /// The names an object rest pattern leaves out, kept as a constant list.
    fn pending_rest_names(&mut self, ci: u32, names: Vec<Value>) {
        // Stored as an array-like marker: a string of NUL-separated names.
        let joined: String = names.iter().filter_map(|v| if let Value::Str(s) = v { Some(s.to_string()) } else { None }).collect::<Vec<_>>().join("\u{0}");
        self.st().consts[ci as usize] = Value::Str(joined.into());
    }

    /// Stores the top of the stack into a name (`pop`) or keeps it (`!pop`).
    fn store_name(&mut self, n: &Rc<str>, pop: bool, declare: bool) -> R<()> {
        let (found, withs) = self.resolve(n);
        let mut skips = Vec::new();
        for (wloc, depth) in withs {
            // [v] → if the with object has it, set it there.
            if let Loc::Env(_, slot) = wloc {
                self.emit(Op::GetEnv(depth, slot));
            }
            let c = self.name_const(n);
            skips.push(self.emit(Op::WithSet(c, 0)));
        }
        match found {
            Some((loc, depth, constant)) => {
                if constant && !declare {
                    let c = self.name_const(n);
                    self.emit(Op::ConstAssign(c));
                    return Ok(());
                }
                match loc {
                    Loc::Local(i) => {
                        self.emit(if pop { Op::PutLocal(i) } else { Op::SetLocal(i) });
                    }
                    Loc::Env(_, slot) => {
                        self.emit(if pop { Op::PutEnv(depth, slot) } else { Op::SetEnv(depth, slot) });
                    }
                    _ => {
                        let c = self.name_const(n);
                        self.emit(Op::ConstAssign(c));
                    }
                }
            }
            None => {
                let c = self.name_const(n);
                let ic = self.ic();
                self.emit(if pop { Op::PutGlobal(c, ic) } else { Op::SetGlobal(c, ic) });
            }
        }
        // A `with` hit skips the ordinary store (and leaves the value as the store would).
        let end = self.here();
        for s in skips {
            if let Op::WithSet(c, _) = self.st().ops[s] {
                let skip = end - s as u32 - 1;
                self.st().ops[s] = Op::WithSet(c, skip | if pop { 0x8000_0000 } else { 0 });
            }
        }
        Ok(())
    }

    fn load_name(&mut self, n: &Rc<str>) -> R<()> {
        let (found, withs) = self.resolve(n);
        let mut skips = Vec::new();
        for (wloc, depth) in withs {
            if let Loc::Env(_, slot) = wloc {
                self.emit(Op::GetEnv(depth, slot));
            }
            let c = self.name_const(n);
            skips.push(self.emit(Op::WithGet(c, 0)));
        }
        match found {
            Some((Loc::Local(i), _, _)) => {
                self.emit(Op::GetLocal(i));
            }
            Some((Loc::Env(_, slot), depth, _)) => {
                self.emit(Op::GetEnv(depth, slot));
            }
            Some((Loc::Import(_, slot, k), depth, _)) => {
                self.emit(Op::GetEnv(depth, slot));
                let name = self.import_names[k as usize].clone();
                self.get_prop(&name);
            }
            Some((Loc::This, ..)) => {
                self.emit(Op::This);
            }
            Some((Loc::Super, ..)) => {
                self.emit(Op::LoadSuper);
            }
            Some((Loc::Arguments, ..)) => {
                self.emit(Op::Arguments);
            }
            None => {
                let c = self.name_const(n);
                let ic = self.ic();
                self.emit(Op::GetGlobal(c, ic));
            }
        }
        let end = self.here();
        for s in skips {
            if let Op::WithGet(c, _) = self.st().ops[s] {
                self.st().ops[s] = Op::WithGet(c, end - s as u32 - 1);
            }
        }
        Ok(())
    }

    // -------------------------------------------------------------- expressions

    /// A condition (the value is only tested).
    fn cond(&mut self, e: &Expr) -> R<()> {
        self.expr(e)
    }

    /// An expression whose anonymous function or class takes `name`.
    fn expr_named(&mut self, e: &Expr, name: Option<&str>) -> R<()> {
        match (e, name) {
            (Expr::Func(f), Some(n)) if f.name.is_none() => {
                let idx = self.func(f, Some(n))?;
                self.emit(Op::Closure(idx));
                Ok(())
            }
            (Expr::Call(callee, args, false), Some(n))
                if args.is_empty() && matches!(&**callee, Expr::Func(f) if f.arrow && matches!(f.body.first(), Some(Stmt::Class(c, ..)) if &**c == "_class")) =>
            {
                // An anonymous class expression.
                self.expr(e)?;
                let c = self.name_const(n);
                self.emit(Op::SetName(c));
                Ok(())
            }
            _ => self.expr(e),
        }
    }

    fn expr(&mut self, e: &Expr) -> R<()> {
        match e {
            Expr::Num(n) => {
                if n.fract() == 0.0 && n.abs() < 1e9 && !(*n == 0.0 && n.is_sign_negative()) {
                    self.emit(Op::Int(*n as i32));
                } else {
                    let c = self.num_const(*n);
                    self.emit(Op::Const(c));
                }
            }
            Expr::Str(s) => {
                let c = self.name_const(s);
                self.emit(Op::Const(c));
            }
            Expr::Bool(b) => {
                self.emit(if *b { Op::True } else { Op::False });
            }
            Expr::Null => {
                self.emit(Op::Null);
            }
            Expr::Undefined => {
                self.emit(Op::Undef);
            }
            Expr::Template(strs, exprs) => {
                let mut n = 0;
                for (i, s) in strs.iter().enumerate() {
                    if !s.is_empty() {
                        let c = self.name_const(s);
                        self.emit(Op::Const(c));
                        n += 1;
                    }
                    if let Some(e) = exprs.get(i) {
                        self.expr(e)?;
                        n += 1;
                    }
                }
                self.emit(Op::Template(n));
            }
            Expr::Ident(n) => self.load_name(n)?,
            Expr::This => self.load_name(&"this".into())?,
            Expr::NewTarget => {
                self.emit(Op::NewTarget);
            }
            Expr::Array(items) => {
                if items.iter().any(|x| matches!(x, Expr::Spread(_))) {
                    self.emit(Op::NewArray(0));
                    for it in items {
                        match it {
                            Expr::Spread(x) => {
                                self.expr(x)?;
                                self.emit(Op::ArraySpread);
                            }
                            x => {
                                self.expr(x)?;
                                self.emit(Op::ArrayPush);
                            }
                        }
                    }
                } else {
                    for it in items {
                        self.expr(it)?;
                    }
                    self.emit(Op::NewArray(items.len() as u32));
                }
            }
            Expr::Object(props) => {
                self.emit(Op::NewObject);
                for p in props {
                    match p {
                        Prop::KeyValue(Expr::Str(k), v) => {
                            self.expr_named(v, Some(k))?;
                            if &**k == "__proto__" {
                                self.emit(Op::InitProto);
                            } else {
                                let c = self.name_const(k);
                                self.emit(Op::InitProp(c));
                            }
                        }
                        Prop::KeyValue(k, v) => {
                            self.expr(k)?;
                            match (k, v) {
                                // `{ a() {} }`, `{ a: function () {} }`: named after the key.
                                (Expr::Str(name), Expr::Func(f)) if f.name.is_none() => {
                                    let idx = self.func_with(f, Some(name), false, false)?;
                                    self.emit(Op::Closure(idx));
                                }
                                _ => self.expr(v)?,
                            }
                            self.emit(Op::InitElem);
                        }
                        Prop::Spread(v) => {
                            self.expr(v)?;
                            self.emit(Op::ObjSpread);
                        }
                        Prop::Getter(k, f) | Prop::Setter(k, f) => {
                            let getter = matches!(p, Prop::Getter(..));
                            match k {
                                Expr::Str(name) => {
                                    let idx = self.func_with(f, Some(name), true, false)?;
                                    self.emit(Op::Closure(idx));
                                    let c = self.name_const(name);
                                    self.emit(Op::InitAccessor(c, getter));
                                }
                                k => {
                                    self.expr(k)?;
                                    let idx = self.func_with(f, None, true, false)?;
                                    self.emit(Op::Closure(idx));
                                    self.emit(Op::InitAccessorElem(getter));
                                }
                            }
                        }
                    }
                }
            }
            Expr::Func(f) => {
                let idx = self.func(f, None)?;
                self.emit(Op::Closure(idx));
            }
            Expr::Regex(body, flags) => {
                let st = self.st();
                st.consts.push(Value::Str(body.clone()));
                st.consts.push(Value::Str(flags.clone()));
                let c = st.consts.len() as u32 - 2;
                self.emit(Op::Regex(c));
            }
            Expr::Unary(op, x) => self.unary(op, x)?,
            Expr::Update(op, prefix, target) => self.update(op, *prefix, target)?,
            Expr::Binary(op, a, b) => {
                self.expr(a)?;
                self.expr(b)?;
                self.emit(binary_op(op).ok_or_else(|| format!("unknown operator {op}"))?);
            }
            Expr::Logical(op, a, b) => {
                self.expr(a)?;
                let j = self.emit(match *op {
                    "&&" => Op::JumpIfFalseKeep(0),
                    "||" => Op::JumpIfTrueKeep(0),
                    _ => Op::JumpIfNotNullishKeep(0),
                });
                self.expr(b)?;
                self.patch(j);
            }
            Expr::Assign(op, target, value) => self.assign(op, target, value)?,
            Expr::Cond(c, a, b) => {
                self.cond(c)?;
                let j = self.emit(Op::JumpIfFalse(0));
                self.expr(a)?;
                let j2 = self.emit(Op::Jump(0));
                self.patch(j);
                self.expr(b)?;
                self.patch(j2);
            }
            Expr::Member(..) | Expr::Call(..) => {
                let mut exits = Vec::new();
                self.chain(e, &mut exits)?;
                for x in exits {
                    self.patch(x);
                }
            }
            Expr::New(callee, args) => {
                self.expr(callee)?;
                let at = if args.iter().any(|a| matches!(a, Expr::Spread(_))) {
                    self.spread_args(args)?;
                    self.emit(Op::NewSpread)
                } else {
                    for a in args {
                        self.expr(a)?;
                    }
                    self.emit(Op::New(args.len() as u32))
                };
                let d = describe_callee(callee).into();
                self.st().call_names.push((at as u32, d));
            }
            Expr::Seq(list) => {
                for (i, x) in list.iter().enumerate() {
                    self.expr(x)?;
                    if i + 1 < list.len() {
                        self.emit(Op::Pop);
                    }
                }
            }
            Expr::Yield(arg, delegate) => {
                match arg {
                    Some(x) => self.expr(x)?,
                    None => {
                        self.emit(Op::Undef);
                    }
                }
                if *delegate {
                    // yield* : yield each of the inner iterator's values.
                    self.emit(Op::GetIter);
                    let top = self.here();
                    let next = self.emit(Op::IterNext(0));
                    self.emit(Op::Yield);
                    let cont = self.emit(Op::JumpIfFalse(0));
                    // return(): close the inner iterator, then return from here.
                    self.emit(Op::Swap);
                    self.emit(Op::IterClose);
                    self.ret()?;
                    self.patch(cont);
                    self.emit(Op::Pop);
                    self.emit(Op::Jump(top));
                    self.patch(next);
                    self.emit(Op::Pop);
                    self.emit(Op::Undef);
                } else {
                    self.emit(Op::Yield);
                    // Resumed by return(): leave through any finally blocks.
                    let cont = self.emit(Op::JumpIfFalse(0));
                    self.ret()?;
                    self.patch(cont);
                }
            }
            Expr::Await(x) => {
                self.expr(x)?;
                self.emit(Op::Await);
            }
            Expr::Import(spec) => {
                self.expr(spec)?;
                self.emit(Op::Import);
            }
            Expr::ImportMeta => {
                self.emit(Op::ImportMeta);
            }
            Expr::Spread(_) | Expr::ArrayPat(_) | Expr::ObjectPat(_) | Expr::Default(..) | Expr::Rest(_) => return Err("unexpected pattern".into()),
        }
        Ok(())
    }

    fn spread_args(&mut self, args: &[Expr]) -> R<()> {
        self.emit(Op::NewArray(0));
        for a in args {
            match a {
                Expr::Spread(x) => {
                    self.expr(x)?;
                    self.emit(Op::ArraySpread);
                }
                x => {
                    self.expr(x)?;
                    self.emit(Op::ArrayPush);
                }
            }
        }
        Ok(())
    }

    /// A member/call chain; `?.` jumps to the chain's end with undefined.
    fn chain(&mut self, e: &Expr, exits: &mut Vec<usize>) -> R<()> {
        match e {
            Expr::Member(o, k, optional) => {
                if let Expr::Ident(n) = &**o {
                    if &**n == "super" {
                        // super.x: from the parent class's prototype.
                        self.load_name(n)?;
                        self.get_prop("prototype");
                        return self.member_key(k);
                    }
                }
                self.chain_part(o, exits)?;
                if *optional {
                    exits.push(self.emit(Op::JumpIfNullish(0)));
                }
                self.member_key(k)
            }
            Expr::Call(callee, args, optional) => {
                if let Expr::Ident(n) = &**callee {
                    if &**n == "super" {
                        if args.iter().any(|a| matches!(a, Expr::Spread(_))) {
                            self.spread_args(args)?;
                            self.emit(Op::SuperCallSpread);
                        } else {
                            for a in args {
                                self.expr(a)?;
                            }
                            self.emit(Op::SuperCall(args.len() as u32));
                        }
                        return Ok(());
                    }
                }
                // [this f]
                match &**callee {
                    Expr::Member(o, k, opt_member) => {
                        if matches!(&**o, Expr::Ident(n) if &**n == "super") {
                            self.load_name(&"this".into())?;
                            self.load_name(&"super".into())?;
                            self.get_prop("prototype");
                            self.member_key(k)?;
                        } else {
                            self.chain_part(o, exits)?;
                            if *opt_member {
                                exits.push(self.emit(Op::JumpIfNullish(0)));
                            }
                            match &**k {
                                Expr::Str(name) => {
                                    let c = self.name_const(name);
                                    let ic = self.ic();
                                    self.emit(Op::GetMethod(c, ic));
                                }
                                k => {
                                    self.expr(k)?;
                                    self.emit(Op::GetElemMethod);
                                }
                            }
                        }
                    }
                    other => {
                        self.emit(Op::Undef);
                        self.chain_part(other, exits)?;
                    }
                }
                if *optional {
                    exits.push(self.emit(Op::JumpIfNullishCall(0)));
                }
                let at = if args.iter().any(|a| matches!(a, Expr::Spread(_))) {
                    self.spread_args(args)?;
                    self.emit(Op::CallSpread)
                } else {
                    for a in args {
                        self.expr(a)?;
                    }
                    self.emit(Op::Call(args.len() as u32))
                };
                let d = describe_callee(callee).into();
                self.st().call_names.push((at as u32, d));
                Ok(())
            }
            other => self.expr(other),
        }
    }

    /// A link in a chain: members and calls continue the same chain.
    fn chain_part(&mut self, e: &Expr, exits: &mut Vec<usize>) -> R<()> {
        match e {
            Expr::Member(..) | Expr::Call(..) => self.chain(e, exits),
            other => self.expr(other),
        }
    }

    fn member_key(&mut self, k: &Expr) -> R<()> {
        match k {
            Expr::Str(name) => self.get_prop(name),
            k => {
                self.expr(k)?;
                self.emit(Op::GetElem);
            }
        }
        Ok(())
    }

    fn unary(&mut self, op: &str, x: &Expr) -> R<()> {
        match op {
            "typeof" => {
                if let Expr::Ident(n) = x {
                    let (found, withs) = self.resolve(n);
                    if found.is_none() && withs.is_empty() {
                        let c = self.name_const(n);
                        self.emit(Op::TypeofGlobal(c));
                        return Ok(());
                    }
                }
                self.expr(x)?;
                self.emit(Op::Typeof);
            }
            "delete" => match x {
                Expr::Member(o, k, _) => {
                    self.expr(o)?;
                    match &**k {
                        Expr::Str(name) => {
                            let c = self.name_const(name);
                            self.emit(Op::DelProp(c));
                        }
                        k => {
                            self.expr(k)?;
                            self.emit(Op::DelElem);
                        }
                    }
                }
                Expr::Ident(n) if self.lookup(n).is_none() => {
                    let c = self.name_const(n);
                    self.emit(Op::DelGlobal(c));
                }
                other => {
                    self.expr(other)?;
                    self.emit(Op::Pop);
                    self.emit(Op::True);
                }
            },
            _ => {
                self.expr(x)?;
                match op {
                    "!" => self.emit(Op::Not),
                    "-" => self.emit(Op::Neg),
                    "+" => self.emit(Op::ToNum),
                    "~" => self.emit(Op::BitNot),
                    "void" => {
                        self.emit(Op::Pop);
                        self.emit(Op::Undef)
                    }
                    _ => return Err(format!("unknown operator {op}")),
                };
            }
        }
        Ok(())
    }

    fn update(&mut self, op: &str, prefix: bool, target: &Expr) -> R<()> {
        let step = if op == "++" { Op::Inc } else { Op::Dec };
        match target {
            Expr::Ident(n) => {
                self.load_name(n)?;
                self.emit(Op::ToNum);
                if prefix {
                    self.emit(step);
                    self.store_name(n, false, false)?;
                } else {
                    self.emit(Op::Dup);
                    self.emit(step);
                    self.store_name(n, true, false)?;
                }
            }
            Expr::Member(o, k, _) => {
                self.expr(o)?;
                match &**k {
                    Expr::Str(name) => {
                        // [o] → [o o.k]
                        self.emit(Op::Dup);
                        self.get_prop(name);
                        self.emit(Op::ToNum);
                        let c = self.name_const(name);
                        let ic = self.ic();
                        if prefix {
                            self.emit(step);
                            self.emit(Op::SetProp(c, ic));
                        } else {
                            // [o old] → [old o new] → set → [old new] → pop
                            self.emit(Op::Dup);
                            self.emit(Op::Rot3);
                            self.emit(Op::Swap);
                            self.emit(step);
                            self.emit(Op::SetProp(c, ic));
                            self.emit(Op::Pop);
                        }
                    }
                    k => {
                        // [o k] → [o k o k] → [o k old]
                        self.expr(k)?;
                        self.emit(Op::Dup2);
                        self.emit(Op::GetElem);
                        self.emit(Op::ToNum);
                        if prefix {
                            self.emit(step);
                            self.emit(Op::SetElem);
                        } else {
                            // keep old: stash it in a temp.
                            let tmp = self.alloc_local();
                            self.emit(Op::SetLocal(tmp));
                            self.emit(step);
                            self.emit(Op::SetElem);
                            self.emit(Op::Pop);
                            self.emit(Op::GetLocal(tmp));
                            self.st().nlocals -= 1;
                        }
                    }
                }
            }
            _ => return Err("invalid update target".into()),
        }
        Ok(())
    }

    fn assign(&mut self, op: &str, target: &Expr, value: &Expr) -> R<()> {
        let logical = matches!(op, "&&=" | "||=" | "??=");
        // `super.x = v` sets x on this object (the parent's prototype is only where reads look).
        if let Expr::Member(o, k, opt) = target {
            if matches!(&**o, Expr::Ident(s) if &**s == "super") {
                return self.assign(op, &Expr::Member(Box::new(Expr::Ident("this".into())), k.clone(), *opt), value);
            }
        }
        match target {
            Expr::Ident(n) => {
                if op == "=" {
                    self.expr_named(value, Some(n))?;
                } else if logical {
                    self.load_name(n)?;
                    let j = self.emit(logical_jump(op));
                    self.expr_named(value, Some(n))?;
                    self.store_name(n, false, false)?;
                    self.patch(j);
                    return Ok(());
                } else {
                    self.load_name(n)?;
                    self.expr(value)?;
                    self.emit(binary_op(&op[..op.len() - 1]).ok_or("unknown operator")?);
                }
                self.store_name(n, false, false)
            }
            Expr::Member(o, k, _) if !matches!(&**o, Expr::Ident(s) if &**s == "super") => {
                self.expr(o)?;
                match &**k {
                    Expr::Str(name) => {
                        let c = self.name_const(name);
                        let ic = self.ic();
                        if op == "=" {
                            self.expr(value)?;
                        } else if logical {
                            // [o] → [o o.k]; keep if decided, else assign.
                            self.emit(Op::Dup);
                            self.get_prop(name);
                            let j = self.emit(logical_jump(op));
                            self.expr(value)?;
                            self.emit(Op::SetProp(c, ic));
                            let end = self.emit(Op::Jump(0));
                            self.patch(j);
                            // [o cur] → [cur]
                            self.emit(Op::Nip);
                            self.patch(end);
                            return Ok(());
                        } else {
                            self.emit(Op::Dup);
                            self.get_prop(name);
                            self.expr(value)?;
                            self.emit(binary_op(&op[..op.len() - 1]).ok_or("unknown operator")?);
                        }
                        self.emit(Op::SetProp(c, ic));
                    }
                    k => {
                        self.expr(k)?;
                        if op == "=" {
                            self.expr(value)?;
                        } else if logical {
                            self.emit(Op::Dup2);
                            self.emit(Op::GetElem);
                            let j = self.emit(logical_jump(op));
                            self.expr(value)?;
                            self.emit(Op::SetElem);
                            let end = self.emit(Op::Jump(0));
                            self.patch(j);
                            // [o k cur] → [cur]
                            self.emit(Op::Nip);
                            self.emit(Op::Nip);
                            self.patch(end);
                            return Ok(());
                        } else {
                            self.emit(Op::Dup2);
                            self.emit(Op::GetElem);
                            self.expr(value)?;
                            self.emit(binary_op(&op[..op.len() - 1]).ok_or("unknown operator")?);
                        }
                        self.emit(Op::SetElem);
                    }
                }
                Ok(())
            }
            Expr::ArrayPat(_) | Expr::ObjectPat(_) if op == "=" => {
                self.expr(value)?;
                self.emit(Op::Dup);
                self.bind(target, false)
            }
            t => Err(format!("invalid assignment target {}", format!("{t:?}").chars().take(120).collect::<String>())),
        }
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
        Expr::Call(f, ..) => format!("{}(...)", describe_callee(f)),
        _ => "expression".into(),
    }
}

fn logical_jump(op: &str) -> Op {
    match op {
        "&&=" => Op::JumpIfFalseKeep(0),
        "||=" => Op::JumpIfTrueKeep(0),
        _ => Op::JumpIfNotNullishKeep(0),
    }
}

fn binary_op(op: &str) -> Option<Op> {
    Some(match op {
        "+" => Op::Add,
        "-" => Op::Sub,
        "*" => Op::Mul,
        "/" => Op::Div,
        "%" => Op::Mod,
        "**" => Op::Exp,
        "&" => Op::BitAnd,
        "|" => Op::BitOr,
        "^" => Op::BitXor,
        "<<" => Op::Shl,
        ">>" => Op::Shr,
        ">>>" => Op::UShr,
        "==" => Op::Eq,
        "!=" => Op::Ne,
        "===" => Op::StrictEq,
        "!==" => Op::StrictNe,
        "<" => Op::Lt,
        ">" => Op::Gt,
        "<=" => Op::Le,
        ">=" => Op::Ge,
        "instanceof" => Op::InstanceOf,
        "in" => Op::In,
        _ => return None,
    })
}

/// Functions nested directly in these statements (not inside other functions),
/// and the names class bodies' field initialisers mention.
fn collect_funcs(ss: &[Stmt], out: &mut Vec<Rc<Func>>, classes: &mut Vec<HashSet<Rc<str>>>) {
    for s in ss {
        collect_funcs_stmt(s, out, classes);
    }
}

fn collect_funcs_exprs(es: &[Expr], out: &mut Vec<Rc<Func>>, classes: &mut Vec<HashSet<Rc<str>>>) {
    for e in es {
        collect_funcs_expr(e, out, classes);
    }
}

fn collect_funcs_stmt(s: &Stmt, out: &mut Vec<Rc<Func>>, classes: &mut Vec<HashSet<Rc<str>>>) {
    match s {
        Stmt::Expr(e) | Stmt::Throw(e) => collect_funcs_expr(e, out, classes),
        Stmt::Var(_, decls) => {
            for (p, i) in decls {
                collect_funcs_expr(p, out, classes);
                if let Some(i) = i {
                    collect_funcs_expr(i, out, classes);
                }
            }
        }
        Stmt::Func(f) => out.push(f.clone()),
        Stmt::Return(Some(e)) => collect_funcs_expr(e, out, classes),
        Stmt::If(c, a, b) => {
            collect_funcs_expr(c, out, classes);
            collect_funcs_stmt(a, out, classes);
            if let Some(b) = b {
                collect_funcs_stmt(b, out, classes);
            }
        }
        Stmt::Block(b) => collect_funcs(b, out, classes),
        Stmt::For(i, t, u, b) => {
            if let Some(i) = i {
                collect_funcs_stmt(i, out, classes);
            }
            if let Some(t) = t {
                collect_funcs_expr(t, out, classes);
            }
            if let Some(u) = u {
                collect_funcs_expr(u, out, classes);
            }
            collect_funcs_stmt(b, out, classes);
        }
        Stmt::ForIn(_, p, _, it, b) => {
            collect_funcs_expr(p, out, classes);
            collect_funcs_expr(it, out, classes);
            collect_funcs_stmt(b, out, classes);
        }
        Stmt::While(c, b) | Stmt::DoWhile(b, c) | Stmt::With(c, b) => {
            collect_funcs_expr(c, out, classes);
            collect_funcs_stmt(b, out, classes);
        }
        Stmt::Try(a, c, f) => {
            collect_funcs(a, out, classes);
            if let Some((p, b)) = c {
                if let Some(p) = p {
                    collect_funcs_expr(p, out, classes);
                }
                collect_funcs(b, out, classes);
            }
            if let Some(f) = f {
                collect_funcs(f, out, classes);
            }
        }
        Stmt::Switch(d, cases) => {
            collect_funcs_expr(d, out, classes);
            for (t, b) in cases {
                if let Some(t) = t {
                    collect_funcs_expr(t, out, classes);
                }
                collect_funcs(b, out, classes);
            }
        }
        Stmt::Labeled(_, b) => collect_funcs_stmt(b, out, classes),
        Stmt::Class(_, sup, members) => collect_class(sup.as_ref(), members, out, classes),
        Stmt::Export(e) => match &**e {
            Export::Decl(d, _) => collect_funcs_stmt(d, out, classes),
            Export::Default(e) => collect_funcs_expr(e, out, classes),
            _ => {}
        },
        _ => {}
    }
}

fn collect_class(sup: Option<&Expr>, members: &[(Expr, Member, bool)], out: &mut Vec<Rc<Func>>, classes: &mut Vec<HashSet<Rc<str>>>) {
    if let Some(s) = sup {
        collect_funcs_expr(s, out, classes);
    }
    for (k, m, _) in members {
        collect_funcs_expr(k, out, classes);
        match m {
            Member::Method(f) | Member::Get(f) | Member::Set(f) => out.push(f.clone()),
            Member::Field(Some(e)) => {
                let mut names = HashSet::new();
                let mut cache = FreeCache::new();
                let mut m = Mentions { names: &mut names, cache: &mut cache, this: false, arguments: false, sup: false };
                m.expr(e);
                classes.push(names);
            }
            Member::StaticBlock(b) => {
                let mut names = HashSet::new();
                let mut cache = FreeCache::new();
                let mut m = Mentions { names: &mut names, cache: &mut cache, this: false, arguments: false, sup: false };
                m.stmts(b);
                classes.push(names);
            }
            Member::Field(None) => {}
        }
    }
}

fn collect_funcs_expr(e: &Expr, out: &mut Vec<Rc<Func>>, classes: &mut Vec<HashSet<Rc<str>>>) {
    match e {
        Expr::Func(f) => out.push(f.clone()),
        Expr::Template(_, es) | Expr::Array(es) | Expr::Seq(es) => collect_funcs_exprs(es, out, classes),
        Expr::Object(props) => {
            for p in props {
                match p {
                    Prop::KeyValue(k, v) => {
                        collect_funcs_expr(k, out, classes);
                        collect_funcs_expr(v, out, classes);
                    }
                    Prop::Spread(v) => collect_funcs_expr(v, out, classes),
                    Prop::Getter(k, f) | Prop::Setter(k, f) => {
                        collect_funcs_expr(k, out, classes);
                        out.push(f.clone());
                    }
                }
            }
        }
        Expr::Unary(_, x) | Expr::Update(_, _, x) | Expr::Spread(x) | Expr::Await(x) | Expr::Rest(x) | Expr::Import(x) => collect_funcs_expr(x, out, classes),
        Expr::Binary(_, a, b) | Expr::Logical(_, a, b) | Expr::Assign(_, a, b) | Expr::Member(a, b, _) | Expr::Default(a, b) => {
            collect_funcs_expr(a, out, classes);
            collect_funcs_expr(b, out, classes);
        }
        Expr::Cond(a, b, c) => {
            collect_funcs_expr(a, out, classes);
            collect_funcs_expr(b, out, classes);
            collect_funcs_expr(c, out, classes);
        }
        Expr::Call(f, args, _) | Expr::New(f, args) => {
            collect_funcs_expr(f, out, classes);
            collect_funcs_exprs(args, out, classes);
        }
        Expr::ArrayPat(items) => items.iter().flatten().for_each(|x| collect_funcs_expr(x, out, classes)),
        Expr::ObjectPat(props) => props.iter().for_each(|(k, v)| {
            collect_funcs_expr(k, out, classes);
            collect_funcs_expr(v, out, classes);
        }),
        Expr::Yield(Some(x), _) => collect_funcs_expr(x, out, classes),
        _ => {}
    }
}

fn finish(st: FnState, name: Option<Rc<str>>, nparams: u32, method: bool, class_ctor: bool, line: u32, url: Option<Rc<str>>) -> Code {
    let has_env = st.top == Top::Module || !st.captured.is_empty();
    Code {
        has_env,
        url,
        call_names: st.call_names,
        name,
        ops: st.ops,
        consts: st.consts,
        funcs: st.funcs,
        ics: (0..st.nics).map(|_| Cell::new(u32::MAX)).collect(),
        nlocals: st.max_locals,
        env_size: if has_env { st.fn_env_slots.max(st.scopes.first().map(|s| s.env_slots).unwrap_or(0)) } else { 0 },
        nparams,
        arrow: st.arrow,
        is_async: st.is_async,
        is_gen: st.is_gen,
        strict: st.strict,
        method,
        class_ctor,
        line,
    }
}

/// A number as a property key (for constant keys).
pub fn key_of_num(n: f64) -> Rc<str> {
    num_to_string(n).into()
}
