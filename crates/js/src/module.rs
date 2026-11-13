//! ES modules: `import` / `export` declarations, `import()` and `import.meta`.
//!
//! A module graph is linked before any of it runs: each module gets its own
//! scope and a namespace object whose properties are getters over that scope,
//! so imports are live bindings (a module sees later changes to what it
//! imported, and cycles work as long as nothing is read too early). Then the
//! graph runs depth first, dependencies before the modules importing them.
//! Each URL is loaded and run once.

use crate::interp::{scope, Interp, Kind, Obj, Object, Res, Scope, Slot, Value};
use crate::parse::{Export, Stmt};
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
    scope: Scope,
    prog: Rc<Vec<Stmt>>,
    deps: Vec<ModuleRef>,
    state: State,
    error: Option<Value>,
}

pub type ModuleRef = Rc<RefCell<Module>>;

/// The hidden variable holding a module's URL (for `import()` and `import.meta`).
const URL_VAR: &str = "\0url";

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
        let Some(loader) = self.loader.clone() else { return self.throw("TypeError", &format!("can't load {url}")) };
        match loader.load(url) {
            Ok(src) => self.link(url, &src),
            Err(e) => self.throw("TypeError", &format!("couldn't load module {url}: {e}")),
        }
    }

    /// Parses a module and links it (and, first, what it imports): its scope,
    /// hoisted declarations, namespace and imported bindings. Runs nothing.
    fn link(&mut self, url: &str, src: &str) -> Res<ModuleRef> {
        let prog = match crate::parse::parse_module(src) {
            Ok(p) => Rc::new(p),
            Err(e) => return self.throw("SyntaxError", &format!("{e} (in {url})")),
        };
        let ns = Rc::new(RefCell::new(Object::new(None, Kind::Plain)));
        ns.borrow_mut().set_own("\0@toStringTag", Value::Str("Module".into()));
        let sc = scope(Some(&self.global), true, Some(Value::Undefined));
        sc.borrow_mut().vars.insert(URL_VAR.into(), (Value::Str(url.into()), false));
        let m = Rc::new(RefCell::new(Module { url: url.into(), ns: ns.clone(), scope: sc.clone(), prog: prog.clone(), deps: Vec::new(), state: State::Linked, error: None }));
        // In the cache before its imports, so a cycle finds it.
        self.modules.insert(url.into(), m.clone());
        self.hoist_module(&prog, &sc);
        let mut deps = Vec::new();
        for s in prog.iter() {
            match s {
                Stmt::Import(spec, names) => {
                    let dep = self.dependency(spec, url)?;
                    let dep_ns = dep.borrow().ns.clone();
                    for (imported, local) in names {
                        if &**imported == "*" {
                            sc.borrow_mut().vars.insert(local.clone(), (Value::Obj(dep_ns.clone()), false));
                        } else {
                            sc.borrow_mut().imports.get_or_insert_with(Default::default).insert(local.clone(), (dep_ns.clone(), imported.clone()));
                        }
                    }
                    deps.push(dep);
                }
                Stmt::Export(e) => match &**e {
                    Export::Decl(_, names) | Export::Names(names) => {
                        for (local, exported) in names {
                            self.export_binding(&ns, exported, &sc, local);
                        }
                    }
                    Export::Default(_) => self.export_binding(&ns, "default", &sc, "default"),
                    Export::From(spec, names) => {
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
                    Export::All(spec) => {
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
                },
                _ => {}
            }
        }
        m.borrow_mut().deps = deps;
        Ok(m)
    }

    fn dependency(&mut self, spec: &str, base: &str) -> Res<ModuleRef> {
        let url = self.resolve_spec(spec, base)?;
        self.load_module(&url)
    }

    /// The module's `var`s and functions exist before anything runs (so a
    /// module importing it in a cycle can call its functions).
    fn hoist_module(&mut self, prog: &[Stmt], sc: &Scope) {
        self.hoist(prog, sc, true);
    }

    /// `ns[exported]` reads `local` in a module's scope, live.
    fn export_binding(&mut self, ns: &Obj, exported: &str, sc: &Scope, local: &str) {
        let (sc, local): (Scope, Rc<str>) = (sc.clone(), local.into());
        let getter = self.native(move |i, _this, _args| module_var(i, &sc, &local));
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
            let (prog, sc) = {
                let b = m.borrow();
                (b.prog.clone(), b.scope.clone())
            };
            for s in prog.iter() {
                if let Err(e) = self.exec(s, &sc) {
                    r = Err(e);
                    break;
                }
            }
        }
        let mut b = m.borrow_mut();
        b.state = State::Done;
        b.error = r.as_ref().err().cloned();
        r
    }

    /// The URL code in this scope was loaded from: its module's, or the page's.
    fn current_url(&mut self, sc: &Scope) -> Rc<str> {
        match self.lookup(sc, URL_VAR) {
            Ok(Value::Str(u)) => u,
            _ => self.base_url.clone(),
        }
    }

    /// `import(spec)`: a promise of the module's namespace.
    pub(crate) fn dynamic_import(&mut self, spec: &str, sc: &Scope) -> Value {
        let base = self.current_url(sc);
        let (p, _) = self.new_promise();
        let r = self.resolve_spec(spec, &base).and_then(|url| self.import_url(&url));
        match r {
            Ok(ns) => self.resolve_promise(&p, Value::Obj(ns)),
            Err(e) => self.reject_promise(&p, e),
        }
        p
    }

    /// `import.meta`: the module's URL, and `resolve()` against it.
    pub(crate) fn import_meta(&mut self, sc: &Scope) -> Value {
        let url = self.current_url(sc);
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

/// A variable of a module's own scope (an imported one is read through), or
/// undefined before it's declared.
fn module_var(i: &mut Interp, sc: &Scope, name: &str) -> Res<Value> {
    if let Some((v, _)) = sc.borrow().vars.get(name) {
        return Ok(v.clone());
    }
    let imported = sc.borrow().imports.as_ref().and_then(|m| m.get(name).cloned());
    match imported {
        Some((ns, k)) => i.get(&Value::Obj(ns), &k),
        None => Ok(Value::Undefined),
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
