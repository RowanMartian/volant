//! ES modules: `import` / `export` declarations, `import()` and `import.meta`.
//!
//! A module graph is linked before any of it runs: each module is compiled
//! with its top level in one environment, and gets a namespace object whose
//! properties are getters over that environment, so imports are live bindings
//! (a module sees later changes to what it imported, and cycles work as long
//! as nothing is read too early). Linking runs a module's frame up to the end
//! of its hoisting; then the graph runs depth first, dependencies before the
//! modules importing them. Each URL is loaded and run once.

use crate::interp::{Interp, Kind, Obj, Object, Res, Slot, Value};
use crate::vm::{Env, Suspended};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

/// Where modules come from (the page's network, or files in tests).
pub trait Loader {
    /// The absolute URL `spec` names, imported from `base`.
    fn resolve(&self, spec: &str, base: &str) -> String;
    /// The source of the module at `url`.
    fn load(&self, url: &str) -> Result<String, String>;
}

#[derive(Clone, Copy, PartialEq)]
enum State {
    Linked,
    Running,
    Done,
}

pub struct Module {
    pub url: Rc<str>,
    pub ns: Obj,
    /// Its top-level frame, waiting to run once linked.
    body: Option<Suspended>,
    deps: Vec<ModuleRef>,
    state: State,
    error: Option<Value>,
}

pub type ModuleRef = Rc<RefCell<Module>>;

impl Interp {
    /// Runs `src` as the module at `url` (an inline `<script type="module">`,
    /// or one fetched already), with everything it imports.
    pub fn run_module(&mut self, src: &str, url: &str) -> Result<(), String> {
        let r = self.link(url, src).and_then(|m| self.evaluate(&m));
        self.run_microtasks();
        r.map_err(|e| self.describe(&e))
    }

    /// Loads, links and runs the module at `url` (once); its namespace.
    pub fn import_url(&mut self, url: &str) -> Res<Obj> {
        let m = self.load_module(url)?;
        self.evaluate(&m)?;
        let ns = m.borrow().ns.clone();
        Ok(ns)
    }

    fn resolve_spec(&mut self, spec: &str, base: &str) -> Res<String> {
        match &self.loader {
            Some(l) => Ok(l.resolve(spec, base)),
            None => self.throw("TypeError", &format!("can't import {spec}: modules can't be loaded here")),
        }
    }

    fn load_module(&mut self, url: &str) -> Res<ModuleRef> {
        if let Some(m) = self.modules.get(url) {
            return Ok(m.clone());
        }
        let Some(loader) = self.loader.clone() else {
            return self.throw("TypeError", &format!("can't load {url}"));
        };
        match self.waiting(|| loader.load(url)) {
            Ok(src) => self.link(url, &src),
            Err(e) => self.throw("TypeError", &format!("couldn't load module {url}: {e}")),
        }
    }

    /// Compiles a module and links it (and, first, what it imports): its
    /// environment, hoisted functions, namespace and imported bindings.
    /// Runs none of its statements.
    fn link(&mut self, url: &str, src: &str) -> Res<ModuleRef> {
        let prog = match crate::parse::parse_module(src) {
            Ok(p) => p,
            Err(e) => return self.throw("SyntaxError", &format!("{e} (in {url})")),
        };
        let (code, info) = match crate::compile::Compiler::module(&prog, url) {
            Ok(c) => c,
            Err(e) => return self.throw("SyntaxError", &format!("{e} (in {url})")),
        };
        let ns = Rc::new(RefCell::new(Object::new(None, Kind::Plain)));
        ns.borrow_mut().set_hidden("\0@toStringTag", Value::Str("Module".into()));
        let (body, env) = self.module_frame(code)?;
        for (name, slot) in &info.exports {
            self.export_binding(&ns, name, &env, *slot);
        }
        let m = Rc::new(RefCell::new(Module { url: url.into(), ns: ns.clone(), body: Some(body), deps: Vec::new(), state: State::Linked, error: None }));
        // In the cache before its imports, so a cycle finds it.
        self.modules.insert(url.into(), m.clone());
        let mut deps = Vec::new();
        for (spec, slot) in &info.imports {
            let dep = self.dependency(spec, url)?;
            let dep_ns = dep.borrow().ns.clone();
            env.vars.borrow_mut()[*slot as usize] = Value::Obj(dep_ns);
            deps.push(dep);
        }
        for (spec, names) in &info.reexports {
            let dep = self.dependency(spec, url)?;
            let dep_ns = dep.borrow().ns.clone();
            for (imported, exported) in names {
                if &**imported == "*" {
                    ns.borrow_mut().set_own(exported, Value::Obj(dep_ns.clone()));
                } else {
                    self.reexport(&ns, exported, &dep_ns, imported);
                }
            }
            deps.push(dep);
        }
        for spec in &info.star {
            let dep = self.dependency(spec, url)?;
            let dep_ns = dep.borrow().ns.clone();
            let keys = dep_ns.borrow().own_keys();
            for k in keys.iter().filter(|k| &***k != "default") {
                if ns.borrow().get_own(k).is_none() {
                    self.reexport(&ns, k, &dep_ns, k);
                }
            }
            deps.push(dep);
        }
        m.borrow_mut().deps = deps;
        Ok(m)
    }

    fn dependency(&mut self, spec: &str, base: &str) -> Res<ModuleRef> {
        let url = self.resolve_spec(spec, base)?;
        self.load_module(&url)
    }

    /// `ns[exported]` reads a slot of the module's environment, live.
    fn export_binding(&mut self, ns: &Obj, exported: &str, env: &Rc<Env>, slot: u32) {
        let env = env.clone();
        let getter = self.native(move |_i, _this, _args| Ok(env.vars.borrow().get(slot as usize).cloned().unwrap_or(Value::Undefined)));
        ns.borrow_mut().set_slot(exported, Slot::Accessor(Some(getter), None));
    }

    /// `ns[exported]` reads `name` of another module's namespace, live.
    fn reexport(&mut self, ns: &Obj, exported: &str, from: &Obj, name: &str) {
        let (from, name): (Obj, Rc<str>) = (from.clone(), name.into());
        let getter = self.native(move |i, _this, _args| i.get(&Value::Obj(from.clone()), &name));
        ns.borrow_mut().set_slot(exported, Slot::Accessor(Some(getter), None));
    }

    /// Runs a linked module after what it imports (once; a cycle doesn't wait for itself).
    fn evaluate(&mut self, m: &ModuleRef) -> Res<()> {
        let state = m.borrow().state;
        match state {
            State::Running => return Ok(()),
            State::Done => {
                let err = m.borrow().error.clone();
                return match err {
                    Some(e) => Err(e),
                    None => Ok(()),
                };
            }
            State::Linked => {}
        }
        m.borrow_mut().state = State::Running;
        let deps = m.borrow().deps.clone();
        let mut r = Ok(());
        for d in &deps {
            r = self.evaluate(d);
            if r.is_err() {
                break;
            }
        }
        if r.is_ok() {
            let body = m.borrow_mut().body.take();
            if let Some(b) = body {
                r = self.module_run(b);
            }
        }
        let mut b = m.borrow_mut();
        b.state = State::Done;
        b.error = r.as_ref().err().cloned();
        r
    }

    /// `import(spec)`: a promise of the module's namespace.
    pub(crate) fn dynamic_import(&mut self, spec: &str, base: &str) -> Value {
        let (p, _) = self.new_promise();
        let r = self.resolve_spec(spec, base).and_then(|url| self.import_url(&url));
        match r {
            Ok(ns) => self.resolve_promise(&p, Value::Obj(ns)),
            Err(e) => self.reject_promise(&p, e),
        }
        p
    }

    /// `import.meta`: the module's URL, and `resolve()` against it.
    pub(crate) fn import_meta(&mut self, url: Rc<str>) -> Value {
        let meta = self.obj(Kind::Plain);
        let base = url.clone();
        let resolve = self.native(move |i, _this, args| {
            let spec = i.to_str(args.first().unwrap_or(&Value::Undefined))?;
            let r = i.resolve_spec(&spec, &base)?;
            Ok(Value::Str(r.into()))
        });
        if let Value::Obj(o) = &meta {
            let mut b = o.borrow_mut();
            b.set_own("url", Value::Str(url));
            b.set_own("resolve", resolve);
        }
        meta
    }
}

/// Modules held in memory by URL (for tests and tools).
pub struct MapLoader(pub HashMap<String, String>);

impl Loader for MapLoader {
    fn resolve(&self, spec: &str, base: &str) -> String {
        resolve_path(spec, base)
    }
    fn load(&self, url: &str) -> Result<String, String> {
        self.0.get(url).cloned().ok_or_else(|| "not found".into())
    }
}

/// Resolves a relative specifier (`./a.js`, `../b.js`, `/c.js`) against a
/// path-like base; anything else is returned as it is.
pub fn resolve_path(spec: &str, base: &str) -> String {
    if !(spec.starts_with("./") || spec.starts_with("../") || spec.starts_with('/')) {
        return spec.to_string();
    }
    let mut parts: Vec<&str> = if spec.starts_with('/') { Vec::new() } else { base.split('/').collect() };
    if !spec.starts_with('/') {
        parts.pop(); // the base's file name
    }
    for seg in spec.split('/') {
        match seg {
            "." | "" => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    let joined = parts.join("/");
    if spec.starts_with('/') {
        format!("/{joined}")
    } else {
        joined
    }
}
