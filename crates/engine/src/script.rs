//! Page scripts: runs a document's JavaScript with volant-js against a live
//! DOM, the way a browser's content process does.
//!
//! The DOM is volant-core's [`Dom`] (a vector of nodes), mutated in place;
//! detached nodes are their own parent. JavaScript sees nodes through wrapper
//! objects (one per node, so `===` works) whose prototypes carry the DOM API:
//! traversal, mutation, attributes, `innerHTML`, `querySelector` (with the
//! engine's own selector matching), `classList`, `style`, `dataset`, events
//! with capture and bubbling, and form controls. The rest of the platform
//! (URL, fetch, XMLHttpRequest, storage, observers, …) is written in
//! JavaScript in `prelude.js` on top of a few native hooks.
//!
//! [`Runtime`] owns the interpreter and the document. The app feeds it user
//! input (clicks, typing), advances its clock, and takes [`Snapshot`]s of the
//! document to lay out. Scripts run as if `defer`red: after the whole
//! document is parsed.

use crate::css::{self, Env};
use crate::style;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use volant_core::html::{self, Data, Dom, Node, NodeId, ROOT};
use volant_js::interp::{Interp, Kind, Obj, Res, Slot, Value};

/// The network, as scripts see it (also used to load `<script src>`).
pub trait Host {
    /// Performs a request: (status, content type, body), or None if it failed.
    fn fetch(&self, method: &str, url: &str, body: Option<&str>) -> Option<(u16, String, Vec<u8>)>;
}

/// No network: every request fails.
pub struct NoHost;

impl Host for NoHost {
    fn fetch(&self, _: &str, _: &str, _: Option<&str>) -> Option<(u16, String, Vec<u8>)> {
        None
    }
}

/// Where window-level listeners are kept.
const WINDOW: NodeId = NodeId::MAX;
const FRAGMENT: &str = "#document-fragment";
const COMMENT: &str = "#comment";
/// The step budget for each script and each event (about a second of work).
const BUDGET: u64 = 30_000_000;

struct Listener {
    ty: String,
    f: Value,
    capture: bool,
    once: bool,
}

struct Protos {
    node: Obj,
    element: Obj,
    text: Obj,
    document: Obj,
    class_list: Obj,
    style: Obj,
    event: Obj,
}

pub struct State {
    pub dom: Dom,
    pub url: String,
    pub env: Env,
    /// Style sheets from outside the document (linked sheets), for getComputedStyle.
    pub sheets: Vec<String>,
    listeners: HashMap<NodeId, Vec<Listener>>,
    wrappers: HashMap<NodeId, Value>,
    styles: HashMap<NodeId, Value>,
    datasets: HashMap<NodeId, Value>,
    handlers: HashMap<(NodeId, String), Value>,
    pub focus: Option<NodeId>,
    pub navigate: Option<String>,
    cookies: Vec<(String, String)>,
    current_script: Option<NodeId>,
    host: Rc<dyn Host>,
    protos: Option<Protos>,
    /// Bumped on every change to the document (so the app knows to lay it out again).
    pub generation: u64,
    computed: Option<(u64, style::Styled)>,
}

type S = Rc<RefCell<State>>;

// ------------------------------------------------------------------ URLs

fn scheme_len(s: &str) -> Option<usize> {
    let colon = s.find(':')?;
    let scheme = &s[..colon];
    (!scheme.is_empty() && scheme.chars().next()?.is_ascii_alphabetic() && scheme.chars().all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c))).then_some(colon)
}

fn remove_dots(path: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    let segs: Vec<&str> = path.split('/').collect();
    for (k, seg) in segs.iter().enumerate() {
        let last = k + 1 == segs.len();
        match *seg {
            "." => {
                if last {
                    out.push("");
                }
            }
            ".." => {
                if out.len() > 1 {
                    out.pop();
                }
                if last {
                    out.push("");
                }
            }
            s => out.push(s),
        }
    }
    let j = out.join("/");
    if j.starts_with('/') {
        j
    } else {
        format!("/{j}")
    }
}

/// Resolves `href` against `base` (RFC 3986), keeping fragments.
pub fn resolve_url(base: &str, href: &str) -> String {
    let h = href.trim();
    if scheme_len(h).is_some() {
        return h.to_string();
    }
    let base_nofrag = base.split('#').next().unwrap_or(base);
    if h.is_empty() {
        return base_nofrag.to_string();
    }
    let Some(sl) = scheme_len(base) else { return h.to_string() };
    let scheme = &base[..sl];
    let after = &base_nofrag[sl + 1..];
    if let Some(rest) = h.strip_prefix("//") {
        return format!("{scheme}://{rest}");
    }
    let (auth, path_q) = match after.strip_prefix("//") {
        Some(a) => {
            let end = a.find(['/', '?']).unwrap_or(a.len());
            (format!("//{}", &a[..end]), &a[end..])
        }
        None => (String::new(), after),
    };
    let (bpath, bquery) = match path_q.find('?') {
        Some(q) => (&path_q[..q], &path_q[q..]),
        None => (path_q, ""),
    };
    if h.starts_with('#') {
        return format!("{scheme}:{auth}{bpath}{bquery}{h}");
    }
    if h.starts_with('?') {
        return format!("{scheme}:{auth}{bpath}{h}");
    }
    let (hpath, tail) = match h.find(['?', '#']) {
        Some(p) => (&h[..p], &h[p..]),
        None => (h, ""),
    };
    let merged = if hpath.starts_with('/') {
        hpath.to_string()
    } else {
        let dir = match bpath.rfind('/') {
            Some(p) => &bpath[..=p],
            None => "/",
        };
        format!("{dir}{hpath}")
    };
    format!("{scheme}:{auth}{}{tail}", remove_dots(&merged))
}

// ------------------------------------------------------------------ DOM helpers

fn is_element(dom: &Dom, id: NodeId) -> bool {
    matches!(&dom.nodes.get(id as usize).map(|n| &n.data), Some(Data::Element { name, .. }) if !name.starts_with('#'))
}

fn tag_of(dom: &Dom, id: NodeId) -> String {
    dom.tag(id).unwrap_or("").to_string()
}

fn attrs_mut(dom: &mut Dom, id: NodeId) -> Option<&mut Vec<(Box<str>, String)>> {
    match &mut dom.nodes.get_mut(id as usize)?.data {
        Data::Element { attrs, .. } => Some(attrs),
        _ => None,
    }
}

fn set_attr(dom: &mut Dom, id: NodeId, k: &str, v: &str) {
    if let Some(attrs) = attrs_mut(dom, id) {
        match attrs.iter_mut().find(|(a, _)| &**a == k) {
            Some(slot) => slot.1 = v.to_string(),
            None => attrs.push((k.into(), v.to_string())),
        }
    }
}

fn remove_attr(dom: &mut Dom, id: NodeId, k: &str) {
    if let Some(attrs) = attrs_mut(dom, id) {
        attrs.retain(|(a, _)| &**a != k);
    }
}

fn new_node(dom: &mut Dom, data: Data) -> NodeId {
    let id = dom.nodes.len() as NodeId;
    dom.nodes.push(Node { parent: id, children: Vec::new(), data });
    id
}

fn detach(dom: &mut Dom, id: NodeId) {
    let p = dom.nodes[id as usize].parent;
    if p != id {
        dom.nodes[p as usize].children.retain(|&c| c != id);
        dom.nodes[id as usize].parent = id;
    }
}

fn is_inclusive_ancestor(dom: &Dom, anc: NodeId, mut id: NodeId) -> bool {
    loop {
        if id == anc {
            return true;
        }
        let p = dom.nodes[id as usize].parent;
        if p == id {
            return false;
        }
        id = p;
    }
}

fn connected(dom: &Dom, id: NodeId) -> bool {
    is_inclusive_ancestor(dom, ROOT, id)
}

/// Inserts `child` into `parent` before `before` (or at the end); fragments insert their children.
fn insert(dom: &mut Dom, parent: NodeId, child: NodeId, before: Option<NodeId>) -> Result<(), &'static str> {
    if is_inclusive_ancestor(dom, child, parent) {
        return Err("the new child contains the parent");
    }
    let moving: Vec<NodeId> = if dom.tag(child) == Some(FRAGMENT) { dom.nodes[child as usize].children.clone() } else { vec![child] };
    for c in moving {
        detach(dom, c);
        let kids = &mut dom.nodes[parent as usize].children;
        let at = before.and_then(|b| kids.iter().position(|&k| k == b)).unwrap_or(kids.len());
        kids.insert(at, c);
        dom.nodes[c as usize].parent = parent;
    }
    Ok(())
}

fn set_text(dom: &mut Dom, id: NodeId, text: &str) {
    if let Data::Text(t) = &mut dom.nodes[id as usize].data {
        *t = text.to_string();
        return;
    }
    for c in dom.nodes[id as usize].children.clone() {
        detach(dom, c);
    }
    if !text.is_empty() {
        let t = new_node(dom, Data::Text(text.to_string()));
        let _ = insert(dom, id, t, None);
    }
}

/// Copies a node (and its subtree when `deep`) within the document.
fn clone_node(dom: &mut Dom, id: NodeId, deep: bool) -> NodeId {
    let data = match &dom.nodes[id as usize].data {
        Data::Document => Data::Element { name: FRAGMENT.into(), attrs: Vec::new() },
        Data::Element { name, attrs } => Data::Element { name: name.clone(), attrs: attrs.clone() },
        Data::Text(t) => Data::Text(t.clone()),
    };
    let copy = new_node(dom, data);
    if deep {
        for c in dom.nodes[id as usize].children.clone() {
            let cc = clone_node(dom, c, true);
            let _ = insert(dom, copy, cc, None);
        }
    }
    copy
}

/// Copies a parsed fragment into the document under a new fragment node.
fn import(dom: &mut Dom, src: &Dom) -> NodeId {
    fn copy(dom: &mut Dom, src: &Dom, sid: NodeId, parent: NodeId) {
        let data = match &src.nodes[sid as usize].data {
            Data::Element { name, attrs } => Data::Element { name: name.clone(), attrs: attrs.clone() },
            Data::Text(t) => Data::Text(t.clone()),
            Data::Document => return,
        };
        let id = new_node(dom, data);
        let _ = insert(dom, parent, id, None);
        for &c in src.children(sid) {
            copy(dom, src, c, id);
        }
    }
    let frag = new_node(dom, Data::Element { name: FRAGMENT.into(), attrs: Vec::new() });
    for &c in src.children(ROOT) {
        copy(dom, src, c, frag);
    }
    frag
}

const VOID: &[&str] = &["area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "source", "track", "wbr"];

fn serialize(dom: &Dom, id: NodeId, out: &mut String) {
    match &dom.nodes[id as usize].data {
        Data::Text(t) => {
            let raw = matches!(dom.tag(dom.parent(id)), Some("script" | "style"));
            if raw {
                out.push_str(t);
            } else {
                html::escape_into(t, out, false);
            }
        }
        Data::Element { name, .. } if &**name == COMMENT => out.push_str("<!---->"),
        Data::Element { name, attrs } if &**name != FRAGMENT => {
            out.push('<');
            out.push_str(name);
            for (k, v) in attrs {
                out.push(' ');
                out.push_str(k);
                out.push_str("=\"");
                html::escape_into(v, out, true);
                out.push('"');
            }
            out.push('>');
            if VOID.contains(&&**name) {
                return;
            }
            for &c in dom.children(id) {
                serialize(dom, c, out);
            }
            out.push_str("</");
            out.push_str(name);
            out.push('>');
        }
        _ => {
            for &c in dom.children(id) {
                serialize(dom, c, out);
            }
        }
    }
}

fn inner_html(dom: &Dom, id: NodeId) -> String {
    let mut out = String::new();
    for &c in dom.children(id) {
        serialize(dom, c, &mut out);
    }
    out
}

/// Descendants of `id` in document order (not `id` itself).
fn descendants(dom: &Dom, id: NodeId) -> Vec<NodeId> {
    let mut out = Vec::new();
    let mut stack: Vec<NodeId> = dom.children(id).iter().rev().copied().collect();
    while let Some(n) = stack.pop() {
        out.push(n);
        stack.extend(dom.children(n).iter().rev());
    }
    out
}

/// Splits a selector list at top-level commas.
fn selector_list(text: &str) -> Option<Vec<css::Selector>> {
    let mut parts = Vec::new();
    let (mut depth, mut start, mut quote) = (0i32, 0usize, None);
    for (k, c) in text.char_indices() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => quote = Some(c),
            (None, '(' | '[') => depth += 1,
            (None, ')' | ']') => depth -= 1,
            (None, ',') if depth == 0 => {
                parts.push(&text[start..k]);
                start = k + 1;
            }
            _ => {}
        }
    }
    parts.push(&text[start..]);
    parts.iter().map(|p| css::parse_selector(p.trim().trim_start_matches(":scope").trim())).collect()
}

fn matches_any(dom: &Dom, id: NodeId, sels: &[css::Selector]) -> bool {
    is_element(dom, id) && sels.iter().any(|s| style::matches(dom, id, s))
}

fn camel(kebab: &str) -> String {
    let mut out = String::new();
    let mut up = false;
    for c in kebab.trim_start_matches('-').chars() {
        if c == '-' {
            up = true;
        } else if up {
            out.extend(c.to_uppercase());
            up = false;
        } else {
            out.push(c);
        }
    }
    if kebab.starts_with("--") {
        return kebab.to_string();
    }
    out
}

fn kebab(camel: &str) -> String {
    if camel.starts_with("--") {
        return camel.to_string();
    }
    let mut out = String::new();
    for c in camel.chars() {
        if c.is_ascii_uppercase() {
            out.push('-');
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    if out.starts_with("webkit-") || out.starts_with("moz-") || out.starts_with("ms-") {
        out.insert(0, '-');
    }
    out
}

// ------------------------------------------------------------------ JS glue

fn arg(a: &[Value], i: usize) -> Value {
    a.get(i).cloned().unwrap_or(Value::Undefined)
}

fn str_val(s: &str) -> Value {
    Value::Str(s.into())
}

fn hidden(o: &Obj, k: &str) -> Option<Value> {
    match o.borrow().get_own(k) {
        Some(Slot::Data(v)) => Some(v.clone()),
        _ => None,
    }
}

/// The node behind a wrapper (or the document for `window` calls).
pub fn node_of(v: &Value) -> Option<NodeId> {
    match v {
        Value::Obj(o) => match hidden(o, "\0node") {
            Some(Value::Num(n)) => Some(n as NodeId),
            _ => None,
        },
        _ => None,
    }
}

fn protos(st: &S) -> (Obj, Obj, Obj, Obj) {
    let s = st.borrow();
    let p = s.protos.as_ref().expect("protos");
    (p.node.clone(), p.element.clone(), p.text.clone(), p.document.clone())
}

/// The wrapper object for a node (made on first use).
fn wrap(i: &Interp, st: &S, id: NodeId) -> Value {
    if let Some(v) = st.borrow().wrappers.get(&id) {
        return v.clone();
    }
    let (node_p, element_p, text_p, document_p) = protos(st);
    let proto = {
        let s = st.borrow();
        match &s.dom.nodes[id as usize].data {
            Data::Document => document_p,
            Data::Text(_) => text_p,
            Data::Element { name, .. } if name.starts_with('#') => node_p,
            Data::Element { .. } => element_p,
        }
    };
    let o = i.obj(Kind::Plain);
    if let Value::Obj(oo) = &o {
        oo.borrow_mut().proto = Some(proto);
        oo.borrow_mut().set_own("\0node", Value::Num(id as f64));
    }
    st.borrow_mut().wrappers.insert(id, o.clone());
    o
}

fn wrap_opt(i: &Interp, st: &S, id: Option<NodeId>) -> Value {
    match id {
        Some(id) => wrap(i, st, id),
        None => Value::Null,
    }
}

fn wrap_list(i: &Interp, st: &S, ids: Vec<NodeId>) -> Value {
    let items = ids.into_iter().map(|id| wrap(i, st, id)).collect();
    let arr = i.array(items);
    // `item(k)` as on NodeList / HTMLCollection.
    arr
}

fn this_node(i: &Interp, this: &Value) -> Res<NodeId> {
    match node_of(this) {
        Some(n) => Ok(n),
        None => i.throw("TypeError", "Illegal invocation"),
    }
}

fn def(i: &Interp, o: &Obj, name: &str, f: impl Fn(&mut Interp, Value, Vec<Value>) -> Res<Value> + 'static) {
    let fv = i.native(f);
    if let Value::Obj(fo) = &fv {
        fo.borrow_mut().set_own("name", str_val(name));
    }
    o.borrow_mut().set_own(name, fv);
}

fn method(i: &Interp, st: &S, o: &Obj, name: &str, f: impl Fn(&mut Interp, &S, NodeId, Vec<Value>) -> Res<Value> + 'static) {
    let st = st.clone();
    def(i, o, name, move |i, t, a| {
        let id = this_node(i, &t)?;
        f(i, &st, id, a)
    });
}

fn getter(i: &Interp, st: &S, o: &Obj, name: &str, g: impl Fn(&mut Interp, &S, NodeId) -> Res<Value> + 'static) {
    let st = st.clone();
    let gv = i.native(move |i, t, _a| {
        let id = this_node(i, &t)?;
        g(i, &st, id)
    });
    o.borrow_mut().set_slot(name, Slot::Accessor(Some(gv), None));
}

fn accessor(
    i: &Interp,
    st: &S,
    o: &Obj,
    name: &str,
    g: impl Fn(&mut Interp, &S, NodeId) -> Res<Value> + 'static,
    s: impl Fn(&mut Interp, &S, NodeId, Value) -> Res<()> + 'static,
) {
    let st1 = st.clone();
    let gv = i.native(move |i, t, _a| {
        let id = this_node(i, &t)?;
        g(i, &st1, id)
    });
    let st2 = st.clone();
    let sv = i.native(move |i, t, a| {
        let id = this_node(i, &t)?;
        s(i, &st2, id, arg(&a, 0))?;
        Ok(Value::Undefined)
    });
    o.borrow_mut().set_slot(name, Slot::Accessor(Some(gv), Some(sv)));
}

fn changed(st: &S) {
    let mut s = st.borrow_mut();
    s.generation += 1;
}

/// Writes `el.style` and `el.dataset` objects back into the attributes.
fn flush(i: &mut Interp, st: &S) {
    let styles: Vec<(NodeId, Value)> = st.borrow().styles.iter().map(|(k, v)| (*k, v.clone())).collect();
    for (id, obj) in styles {
        let Value::Obj(o) = &obj else { continue };
        let keys = o.borrow().own_keys();
        let mut decls = Vec::new();
        for k in keys {
            let v = i.get(&obj, &k).unwrap_or(Value::Undefined);
            let v = match v {
                Value::Str(s) => s.to_string(),
                Value::Num(n) => volant_js::interp::num_to_string(n),
                _ => continue,
            };
            if !v.trim().is_empty() {
                decls.push(format!("{}: {v}", kebab(&k)));
            }
        }
        let text = decls.join("; ");
        let mut s = st.borrow_mut();
        let old = s.dom.attr(id, "style").unwrap_or("").to_string();
        if old != text {
            if text.is_empty() {
                remove_attr(&mut s.dom, id, "style");
            } else {
                set_attr(&mut s.dom, id, "style", &text);
            }
            s.generation += 1;
        }
    }
    let sets: Vec<(NodeId, Value)> = st.borrow().datasets.iter().map(|(k, v)| (*k, v.clone())).collect();
    for (id, obj) in sets {
        let Value::Obj(o) = &obj else { continue };
        let keys = o.borrow().own_keys();
        let mut want = Vec::new();
        for k in keys {
            let v = i.get(&obj, &k).unwrap_or(Value::Undefined);
            if matches!(v, Value::Undefined) {
                continue;
            }
            let v = i.to_str(&v).map(|s| s.to_string()).unwrap_or_default();
            want.push((format!("data-{}", kebab(&k)), v));
        }
        let mut s = st.borrow_mut();
        let have: Vec<(String, String)> = match &s.dom.nodes[id as usize].data {
            Data::Element { attrs, .. } => attrs.iter().filter(|(k, _)| k.starts_with("data-")).map(|(k, v)| (k.to_string(), v.clone())).collect(),
            _ => continue,
        };
        if have != want {
            if let Some(attrs) = attrs_mut(&mut s.dom, id) {
                attrs.retain(|(k, _)| !k.starts_with("data-"));
                attrs.extend(want.into_iter().map(|(k, v)| (k.into_boxed_str(), v)));
            }
            s.generation += 1;
        }
    }
}

// ------------------------------------------------------------------ events

/// A new event object.
pub fn make_event(i: &mut Interp, st: &S, ty: &str, bubbles: bool, cancelable: bool) -> Value {
    let e = i.obj(Kind::Plain);
    let proto = st.borrow().protos.as_ref().map(|p| p.event.clone());
    if let Value::Obj(o) = &e {
        let mut b = o.borrow_mut();
        b.proto = proto;
        b.set_own("type", str_val(ty));
        b.set_own("bubbles", Value::Bool(bubbles));
        b.set_own("cancelable", Value::Bool(cancelable));
        b.set_own("defaultPrevented", Value::Bool(false));
        b.set_own("isTrusted", Value::Bool(true));
        b.set_own("timeStamp", Value::Num(i.now));
        b.set_own("eventPhase", Value::Num(0.0));
    }
    e
}

/// The handler from an `on<type>` property or attribute.
fn inline_handler(i: &mut Interp, st: &S, node: NodeId, ty: &str) -> Option<Value> {
    let key = format!("on{ty}");
    let target = if node == WINDOW { Some(Value::Obj(i.global_obj.clone())) } else { st.borrow().wrappers.get(&node).cloned() };
    if let Some(Value::Obj(o)) = &target {
        if let Some(Slot::Data(f)) = o.borrow().get_own(&key) {
            if Interp::is_callable(f) {
                return Some(f.clone());
            }
        }
    }
    if node == WINDOW {
        return None;
    }
    let code = st.borrow().dom.attr(node, &key).map(str::to_string)?;
    if let Some(f) = st.borrow().handlers.get(&(node, code.clone())) {
        return Some(f.clone());
    }
    let f = i.run(&format!("(function(event) {{\n{code}\n}})")).ok()?;
    st.borrow_mut().handlers.insert((node, code), f.clone());
    Some(f)
}

fn call_listeners(i: &mut Interp, st: &S, node: NodeId, ev: &Value, phase: u8) {
    let ty = match i.get(ev, "type") {
        Ok(Value::Str(s)) => s.to_string(),
        _ => return,
    };
    let current = if node == WINDOW { Value::Obj(i.global_obj.clone()) } else { wrap(i, st, node) };
    let _ = i.set(ev, "currentTarget", current.clone());
    let _ = i.set(ev, "eventPhase", Value::Num(phase as f64));
    let list: Vec<(Value, bool)> = st
        .borrow()
        .listeners
        .get(&node)
        .map(|l| l.iter().filter(|x| x.ty == ty && (phase == 2 || x.capture == (phase == 1))).map(|x| (x.f.clone(), x.once)).collect())
        .unwrap_or_default();
    let mut fns: Vec<(Value, bool)> = list;
    if phase != 1 {
        if let Some(h) = inline_handler(i, st, node, &ty) {
            fns.push((h, false));
        }
    }
    for (f, once) in fns {
        if once {
            if let Some(l) = st.borrow_mut().listeners.get_mut(&node) {
                l.retain(|x| !(x.ty == ty && Interp::strict_eq(&x.f, &f)));
            }
        }
        let r = if Interp::is_callable(&f) {
            i.call(&f, current.clone(), vec![ev.clone()])
        } else {
            match i.get(&f, "handleEvent") {
                Ok(h) if Interp::is_callable(&h) => i.call(&h, f.clone(), vec![ev.clone()]),
                _ => Ok(Value::Undefined),
            }
        };
        match r {
            // `return false` from an inline handler cancels the event.
            Ok(Value::Bool(false)) => {
                let _ = i.set(ev, "defaultPrevented", Value::Bool(true));
            }
            Err(e) => {
                let d = i.describe(&e);
                i.errors.push(d);
            }
            _ => {}
        }
        if matches!(i.get(ev, "\0stopNow"), Ok(Value::Bool(true))) {
            break;
        }
    }
}

/// Dispatches `ev` at `target` (capture, target, bubble). Returns false if it was cancelled.
pub fn dispatch(i: &mut Interp, st: &S, target: NodeId, ev: &Value) -> bool {
    let mut path = vec![target];
    if target != WINDOW {
        let dom_path = {
            let s = st.borrow();
            let mut p = Vec::new();
            let mut id = target;
            loop {
                let parent = s.dom.nodes[id as usize].parent;
                if parent == id {
                    break;
                }
                p.push(parent);
                id = parent;
            }
            (p, id == ROOT)
        };
        path.extend(dom_path.0);
        if dom_path.1 && !matches!(i.get(ev, "type"), Ok(Value::Str(t)) if &*t == "load") {
            path.push(WINDOW);
        }
    }
    let tv = if target == WINDOW { Value::Obj(i.global_obj.clone()) } else { wrap(i, st, target) };
    let _ = i.set(ev, "target", tv.clone());
    let _ = i.set(ev, "srcElement", tv);
    let stopped = |i: &mut Interp| matches!(i.get(ev, "\0stop"), Ok(Value::Bool(true)));
    for &n in path.iter().skip(1).rev() {
        call_listeners(i, st, n, ev, 1);
        if stopped(i) {
            return !matches!(i.get(ev, "defaultPrevented"), Ok(Value::Bool(true)));
        }
    }
    call_listeners(i, st, target, ev, 2);
    let bubbles = matches!(i.get(ev, "bubbles"), Ok(Value::Bool(true)));
    if bubbles {
        for &n in path.iter().skip(1) {
            if stopped(i) {
                break;
            }
            call_listeners(i, st, n, ev, 3);
        }
    }
    let _ = i.set(ev, "currentTarget", Value::Null);
    !matches!(i.get(ev, "defaultPrevented"), Ok(Value::Bool(true)))
}

fn fire(i: &mut Interp, st: &S, target: NodeId, ty: &str, bubbles: bool) -> bool {
    let ev = make_event(i, st, ty, bubbles, true);
    dispatch(i, st, target, &ev)
}

// ------------------------------------------------------------------ forms

fn closest(dom: &Dom, mut id: NodeId, pred: impl Fn(&Dom, NodeId) -> bool) -> Option<NodeId> {
    loop {
        if pred(dom, id) {
            return Some(id);
        }
        let p = dom.nodes[id as usize].parent;
        if p == id {
            return None;
        }
        id = p;
    }
}

fn input_type(dom: &Dom, id: NodeId) -> String {
    dom.attr(id, "type").unwrap_or("text").to_ascii_lowercase()
}

fn is_text_field(dom: &Dom, id: NodeId) -> bool {
    match dom.tag(id) {
        Some("textarea") => true,
        Some("input") => !matches!(input_type(dom, id).as_str(), "checkbox" | "radio" | "submit" | "button" | "reset" | "image" | "hidden" | "file" | "range" | "color"),
        _ => false,
    }
}

fn field_value(dom: &Dom, id: NodeId) -> String {
    match dom.tag(id) {
        Some("textarea") => dom.text(id),
        Some("select") => crate::forms::selected_option(dom, id).map(|o| option_value(dom, o)).unwrap_or_default(),
        Some("option") => option_value(dom, id),
        _ => dom.attr(id, "value").unwrap_or("").to_string(),
    }
}

fn option_value(dom: &Dom, o: NodeId) -> String {
    dom.attr(o, "value").map(str::to_string).unwrap_or_else(|| dom.text(o).trim().to_string())
}

fn set_field_value(dom: &mut Dom, id: NodeId, v: &str) {
    match dom.tag(id) {
        Some("textarea") => set_text(dom, id, v),
        Some("select") => {
            let options: Vec<NodeId> = descendants(dom, id).into_iter().filter(|&o| dom.tag(o) == Some("option")).collect();
            for o in options {
                if option_value(dom, o) == v {
                    set_attr(dom, o, "selected", "");
                } else {
                    remove_attr(dom, o, "selected");
                }
            }
        }
        _ => set_attr(dom, id, "value", v),
    }
}

fn url_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'*' => out.push(b as char),
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Submits a form (GET): navigates to its action with the fields as the query.
fn submit(i: &mut Interp, st: &S, form: NodeId, submitter: Option<NodeId>) {
    if !fire(i, st, form, "submit", true) {
        return;
    }
    flush(i, st);
    let mut s = st.borrow_mut();
    let dom = &s.dom;
    let mut pairs = Vec::new();
    for f in descendants(dom, form) {
        let Some(tag) = dom.tag(f) else { continue };
        if !matches!(tag, "input" | "select" | "textarea" | "button") || dom.attr(f, "disabled").is_some() {
            continue;
        }
        let Some(name) = dom.attr(f, "name").filter(|n| !n.is_empty()) else { continue };
        let ty = input_type(dom, f);
        if tag == "button" || matches!(ty.as_str(), "submit" | "image" | "reset" | "button") {
            if Some(f) != submitter || ty == "reset" {
                continue;
            }
        }
        if matches!(ty.as_str(), "checkbox" | "radio") && tag == "input" {
            if dom.attr(f, "checked").is_none() {
                continue;
            }
            pairs.push((name.to_string(), dom.attr(f, "value").unwrap_or("on").to_string()));
            continue;
        }
        pairs.push((name.to_string(), field_value(dom, f)));
    }
    let query: Vec<String> = pairs.iter().map(|(k, v)| format!("{}={}", url_encode(k), url_encode(v))).collect();
    let action = dom.attr(form, "action").filter(|a| !a.trim().is_empty()).map(|a| resolve_url(&s.url, a)).unwrap_or_else(|| s.url.clone());
    let base = action.split(['?', '#']).next().unwrap_or(&action).to_string();
    s.navigate = Some(format!("{base}?{}", query.join("&")));
}

/// What a click does when no script cancelled it.
fn activate(i: &mut Interp, st: &S, target: NodeId) {
    enum Act {
        Link(String),
        Script(String),
        Toggle(NodeId, bool),
        Submit(NodeId, NodeId),
        Focus(NodeId),
        Select(NodeId),
        Details(NodeId),
        Label(NodeId),
        None,
    }
    let act = {
        let s = st.borrow();
        let dom = &s.dom;
        let el = closest(dom, target, |d, n| {
            matches!(d.tag(n), Some("a" | "area")) && d.attr(n, "href").is_some()
                || matches!(d.tag(n), Some("button" | "input" | "select" | "textarea" | "summary" | "label"))
        });
        match el {
            None => Act::None,
            Some(n) => match dom.tag(n).unwrap_or("") {
                "a" | "area" => {
                    let href = dom.attr(n, "href").unwrap_or("");
                    match href.trim().strip_prefix("javascript:") {
                        Some(code) => Act::Script(code.to_string()),
                        None if href.trim().starts_with('#') => Act::None,
                        None => Act::Link(resolve_url(&s.url, href)),
                    }
                }
                "summary" => dom.parent(n).ne(&n).then(|| Act::Details(dom.parent(n))).unwrap_or(Act::None),
                "label" => match dom.attr(n, "for") {
                    Some(f) => (0..dom.nodes.len() as NodeId).find(|&x| dom.attr(x, "id") == Some(f)).map(Act::Label).unwrap_or(Act::None),
                    None => descendants(dom, n).into_iter().find(|&x| matches!(dom.tag(x), Some("input" | "select" | "textarea"))).map(Act::Label).unwrap_or(Act::None),
                },
                "select" => Act::Select(n),
                "textarea" => Act::Focus(n),
                "button" | "input" => {
                    let ty = if dom.tag(n) == Some("button") { dom.attr(n, "type").unwrap_or("submit").to_ascii_lowercase() } else { input_type(dom, n) };
                    let form = closest(dom, n, |d, x| d.tag(x) == Some("form"));
                    match ty.as_str() {
                        "checkbox" => Act::Toggle(n, false),
                        "radio" => Act::Toggle(n, true),
                        "submit" | "image" => form.map(|f| Act::Submit(f, n)).unwrap_or(Act::None),
                        _ if dom.tag(n) == Some("input") && is_text_field(dom, n) => Act::Focus(n),
                        _ => Act::None,
                    }
                }
                _ => Act::None,
            },
        }
    };
    match act {
        Act::Link(url) => st.borrow_mut().navigate = Some(url),
        Act::Script(code) => {
            let code = code.replace("%20", " ");
            if let Err(e) = i.run(&code) {
                i.errors.push(e);
            }
        }
        Act::Toggle(n, radio) => {
            {
                let mut s = st.borrow_mut();
                let on = s.dom.attr(n, "checked").is_some();
                if radio {
                    let name = s.dom.attr(n, "name").unwrap_or("").to_string();
                    if !name.is_empty() {
                        let all: Vec<NodeId> = (0..s.dom.nodes.len() as NodeId).filter(|&x| s.dom.tag(x) == Some("input") && s.dom.attr(x, "name") == Some(&name)).collect();
                        for x in all {
                            remove_attr(&mut s.dom, x, "checked");
                        }
                    }
                    set_attr(&mut s.dom, n, "checked", "");
                } else if on {
                    remove_attr(&mut s.dom, n, "checked");
                } else {
                    set_attr(&mut s.dom, n, "checked", "");
                }
                s.generation += 1;
            }
            fire(i, st, n, "input", true);
            fire(i, st, n, "change", true);
        }
        Act::Submit(form, by) => submit(i, st, form, Some(by)),
        Act::Focus(n) => focus(i, st, Some(n)),
        Act::Label(n) => {
            let is_check = matches!(input_type(&st.borrow().dom, n).as_str(), "checkbox" | "radio");
            if is_check {
                let ev = make_event(i, st, "click", true, true);
                if dispatch(i, st, n, &ev) {
                    activate(i, st, n);
                }
            } else {
                focus(i, st, Some(n));
            }
        }
        Act::Select(n) => {
            // A click steps to the next option (there's no drop-down menu yet).
            {
                let mut s = st.borrow_mut();
                let options: Vec<NodeId> = descendants(&s.dom, n).into_iter().filter(|&o| s.dom.tag(o) == Some("option") && s.dom.attr(o, "disabled").is_none()).collect();
                if !options.is_empty() {
                    let cur = crate::forms::selected_option(&s.dom, n).and_then(|c| options.iter().position(|&o| o == c)).unwrap_or(0);
                    let next = options[(cur + 1) % options.len()];
                    for &o in &options {
                        remove_attr(&mut s.dom, o, "selected");
                    }
                    set_attr(&mut s.dom, next, "selected", "");
                    s.generation += 1;
                }
            }
            focus(i, st, Some(n));
            fire(i, st, n, "input", true);
            fire(i, st, n, "change", true);
        }
        Act::Details(d) => {
            let mut s = st.borrow_mut();
            if s.dom.attr(d, "open").is_some() {
                remove_attr(&mut s.dom, d, "open");
            } else {
                set_attr(&mut s.dom, d, "open", "");
            }
            s.generation += 1;
        }
        Act::None => {}
    }
}

fn focus(i: &mut Interp, st: &S, n: Option<NodeId>) {
    let old = st.borrow().focus;
    if old == n {
        return;
    }
    if let Some(o) = old {
        fire(i, st, o, "blur", false);
        fire(i, st, o, "focusout", true);
    }
    st.borrow_mut().focus = n;
    if let Some(n) = n {
        fire(i, st, n, "focus", false);
        fire(i, st, n, "focusin", true);
    }
}

// ------------------------------------------------------------------ the API

fn install(i: &mut Interp, st: &S) {
    let mk = |i: &Interp, proto: Option<Obj>| -> Obj {
        let o = i.obj(Kind::Plain);
        let Value::Obj(o) = o else { unreachable!() };
        o.borrow_mut().proto = proto;
        o
    };
    let node = mk(i, Some(i.object_proto.clone()));
    let element = mk(i, Some(node.clone()));
    let text = mk(i, Some(node.clone()));
    let document = mk(i, Some(node.clone()));
    let class_list = mk(i, Some(i.object_proto.clone()));
    let style_p = mk(i, Some(i.object_proto.clone()));
    let event = mk(i, Some(i.object_proto.clone()));
    st.borrow_mut().protos = Some(Protos {
        node: node.clone(),
        element: element.clone(),
        text: text.clone(),
        document: document.clone(),
        class_list: class_list.clone(),
        style: style_p.clone(),
        event: event.clone(),
    });

    // Constructors, for instanceof.
    let ctor = |i: &mut Interp, name: &str, proto: &Obj| {
        let f = i.native(|i, _t, _a| i.throw("TypeError", "Illegal constructor"));
        if let Value::Obj(fo) = &f {
            fo.borrow_mut().set_own("prototype", Value::Obj(proto.clone()));
            fo.borrow_mut().set_own("name", str_val(name));
        }
        proto.borrow_mut().set_own("constructor", f.clone());
        i.global_obj.borrow_mut().set_own(name, f.clone());
        f
    };
    let node_ctor = ctor(i, "Node", &node);
    let element_ctor = ctor(i, "Element", &element);
    ctor(i, "Text", &text);
    ctor(i, "Document", &document);
    ctor(i, "Event", &event);
    for (k, v) in [("ELEMENT_NODE", 1.0), ("TEXT_NODE", 3.0), ("COMMENT_NODE", 8.0), ("DOCUMENT_NODE", 9.0), ("DOCUMENT_FRAGMENT_NODE", 11.0)] {
        if let Value::Obj(n) = &node_ctor {
            n.borrow_mut().set_own(k, Value::Num(v));
        }
        node.borrow_mut().set_own(k, Value::Num(v));
    }
    for name in [
        "HTMLElement", "HTMLDivElement", "HTMLSpanElement", "HTMLAnchorElement", "HTMLImageElement", "HTMLInputElement", "HTMLButtonElement",
        "HTMLFormElement", "HTMLSelectElement", "HTMLOptionElement", "HTMLTextAreaElement", "HTMLCanvasElement", "HTMLIFrameElement",
        "HTMLScriptElement", "HTMLStyleElement", "HTMLLinkElement", "HTMLMetaElement", "HTMLBodyElement", "HTMLHeadElement", "HTMLHtmlElement",
        "HTMLParagraphElement", "HTMLHeadingElement", "HTMLUListElement", "HTMLOListElement", "HTMLLIElement", "HTMLTableElement",
        "HTMLTableRowElement", "HTMLTableCellElement", "HTMLVideoElement", "HTMLAudioElement", "HTMLMediaElement", "HTMLTemplateElement",
        "HTMLLabelElement", "HTMLDetailsElement", "HTMLDialogElement", "HTMLPictureElement", "HTMLSourceElement", "SVGElement",
        "SVGSVGElement", "HTMLUnknownElement", "HTMLBRElement", "HTMLHRElement", "HTMLPreElement",
    ] {
        i.global_obj.borrow_mut().set_own(name, element_ctor.clone());
    }
    for name in ["CharacterData", "Comment", "DocumentFragment", "ShadowRoot", "HTMLDocument"] {
        let f = i.native(|i, _t, _a| i.throw("TypeError", "Illegal constructor"));
        if let Value::Obj(fo) = &f {
            fo.borrow_mut().set_own("prototype", Value::Obj(node.clone()));
        }
        i.global_obj.borrow_mut().set_own(name, f);
    }

    // ---------------------------------------------------------- events
    def(i, &event, "preventDefault", |i, t, _a| {
        if matches!(i.get(&t, "cancelable"), Ok(Value::Bool(true))) {
            i.set(&t, "defaultPrevented", Value::Bool(true))?;
        }
        Ok(Value::Undefined)
    });
    def(i, &event, "stopPropagation", |i, t, _a| {
        i.set(&t, "\0stop", Value::Bool(true))?;
        i.set(&t, "cancelBubble", Value::Bool(true))?;
        Ok(Value::Undefined)
    });
    def(i, &event, "stopImmediatePropagation", |i, t, _a| {
        i.set(&t, "\0stop", Value::Bool(true))?;
        i.set(&t, "\0stopNow", Value::Bool(true))?;
        Ok(Value::Undefined)
    });
    def(i, &event, "composedPath", |i, t, _a| {
        let target = i.get(&t, "target")?;
        Ok(i.array(vec![target]))
    });
    let st2 = st.clone();
    let make = i.native(move |i, _t, a| {
        // __makeEvent(type, init): what the Event constructors in the prelude build on.
        let ty = i.to_str(&arg(&a, 0))?;
        let init = arg(&a, 1);
        let flag = |i: &mut Interp, k: &str| -> Res<bool> { Ok(matches!(init, Value::Obj(_)) && Interp::truthy(&i.get(&init, k)?)) };
        let (b, c) = (flag(i, "bubbles")?, flag(i, "cancelable")?);
        let e = make_event(i, &st2, &ty, b, c);
        i.set(&e, "isTrusted", Value::Bool(false))?;
        if let Value::Obj(io) = &init {
            for k in io.borrow().own_keys() {
                let v = i.get(&init, &k)?;
                i.set(&e, &k, v)?;
            }
        }
        Ok(e)
    });
    i.global_obj.borrow_mut().set_own("__makeEvent", make);

    // EventTarget methods on nodes (and on window, below).
    fn add_listener(i: &mut Interp, st: &S, node: NodeId, a: &[Value]) -> Res<Value> {
        let ty = i.to_str(&arg(a, 0))?.to_string();
        let f = arg(a, 1);
        if matches!(f, Value::Undefined | Value::Null) {
            return Ok(Value::Undefined);
        }
        let opts = arg(a, 2);
        let (capture, once) = match &opts {
            Value::Bool(b) => (*b, false),
            Value::Obj(_) => (Interp::truthy(&i.get(&opts, "capture")?), Interp::truthy(&i.get(&opts, "once")?)),
            _ => (false, false),
        };
        let mut s = st.borrow_mut();
        let list = s.listeners.entry(node).or_default();
        if !list.iter().any(|l| l.ty == ty && l.capture == capture && Interp::strict_eq(&l.f, &f)) {
            list.push(Listener { ty, f, capture, once });
        }
        Ok(Value::Undefined)
    }
    fn remove_listener(i: &mut Interp, st: &S, node: NodeId, a: &[Value]) -> Res<Value> {
        let ty = i.to_str(&arg(a, 0))?.to_string();
        let f = arg(a, 1);
        let capture = match arg(a, 2) {
            Value::Bool(b) => b,
            o @ Value::Obj(_) => Interp::truthy(&i.get(&o, "capture")?),
            _ => false,
        };
        if let Some(l) = st.borrow_mut().listeners.get_mut(&node) {
            l.retain(|x| !(x.ty == ty && x.capture == capture && Interp::strict_eq(&x.f, &f)));
        }
        Ok(Value::Undefined)
    }
    method(i, st, &node, "addEventListener", |i, st, id, a| add_listener(i, st, id, &a));
    method(i, st, &node, "removeEventListener", |i, st, id, a| remove_listener(i, st, id, &a));
    method(i, st, &node, "dispatchEvent", |i, st, id, a| {
        let ev = arg(&a, 0);
        let _ = i.set(&ev, "isTrusted", Value::Bool(false));
        Ok(Value::Bool(dispatch(i, st, id, &ev)))
    });
    let g = Value::Obj(i.global_obj.clone());
    let st2 = st.clone();
    let wadd = i.native(move |i, _t, a| add_listener(i, &st2, WINDOW, &a));
    let st2 = st.clone();
    let wrem = i.native(move |i, _t, a| remove_listener(i, &st2, WINDOW, &a));
    let st2 = st.clone();
    let wdispatch = i.native(move |i, _t, a| {
        let ev = arg(&a, 0);
        Ok(Value::Bool(dispatch(i, &st2, WINDOW, &ev)))
    });
    i.set(&g, "addEventListener", wadd).ok();
    i.set(&g, "removeEventListener", wrem).ok();
    i.set(&g, "dispatchEvent", wdispatch).ok();

    // ---------------------------------------------------------- Node
    getter(i, st, &node, "nodeType", |_i, st, id| {
        let s = st.borrow();
        Ok(Value::Num(match &s.dom.nodes[id as usize].data {
            Data::Document => 9.0,
            Data::Text(_) => 3.0,
            Data::Element { name, .. } if &**name == COMMENT => 8.0,
            Data::Element { name, .. } if &**name == FRAGMENT => 11.0,
            _ => 1.0,
        }))
    });
    getter(i, st, &node, "nodeName", |_i, st, id| {
        let s = st.borrow();
        Ok(str_val(&match &s.dom.nodes[id as usize].data {
            Data::Document => "#document".to_string(),
            Data::Text(_) => "#text".to_string(),
            Data::Element { name, .. } if name.starts_with('#') => name.to_string(),
            Data::Element { name, .. } => name.to_ascii_uppercase(),
        }))
    });
    let parent_of = |st: &S, id: NodeId| -> Option<NodeId> {
        let s = st.borrow();
        let p = s.dom.nodes[id as usize].parent;
        (p != id).then_some(p)
    };
    getter(i, st, &node, "parentNode", move |i, st, id| Ok(wrap_opt(i, st, parent_of(st, id))));
    getter(i, st, &node, "parentElement", move |i, st, id| {
        let p = parent_of(st, id).filter(|&p| is_element(&st.borrow().dom, p));
        Ok(wrap_opt(i, st, p))
    });
    getter(i, st, &node, "childNodes", |i, st, id| {
        let kids = st.borrow().dom.children(id).to_vec();
        Ok(wrap_list(i, st, kids))
    });
    getter(i, st, &node, "children", |i, st, id| {
        let s = st.borrow();
        let kids: Vec<NodeId> = s.dom.children(id).iter().copied().filter(|&c| is_element(&s.dom, c)).collect();
        drop(s);
        Ok(wrap_list(i, st, kids))
    });
    getter(i, st, &node, "childElementCount", |_i, st, id| {
        let s = st.borrow();
        Ok(Value::Num(s.dom.children(id).iter().filter(|&&c| is_element(&s.dom, c)).count() as f64))
    });
    for (name, last, elements) in [("firstChild", false, false), ("lastChild", true, false), ("firstElementChild", false, true), ("lastElementChild", true, true)] {
        getter(i, st, &node, name, move |i, st, id| {
            let found = {
                let s = st.borrow();
                let kids = s.dom.children(id);
                let ok = |c: &&NodeId| !elements || is_element(&s.dom, **c);
                if last { kids.iter().rev().find(ok).copied() } else { kids.iter().find(ok).copied() }
            };
            Ok(wrap_opt(i, st, found))
        });
    }
    for (name, forward, elements) in [("nextSibling", true, false), ("previousSibling", false, false), ("nextElementSibling", true, true), ("previousElementSibling", false, true)] {
        getter(i, st, &node, name, move |i, st, id| {
            let found = {
                let s = st.borrow();
                let p = s.dom.nodes[id as usize].parent;
                if p == id {
                    None
                } else {
                    let kids = s.dom.children(p);
                    let at = kids.iter().position(|&k| k == id).unwrap_or(0);
                    let ok = |c: &NodeId| !elements || is_element(&s.dom, *c);
                    if forward { kids[at + 1..].iter().find(|c| ok(c)).copied() } else { kids[..at].iter().rev().find(|c| ok(c)).copied() }
                }
            };
            Ok(wrap_opt(i, st, found))
        });
    }
    accessor(
        i,
        st,
        &node,
        "textContent",
        |_i, st, id| {
            let s = st.borrow();
            Ok(match &s.dom.nodes[id as usize].data {
                Data::Document => Value::Null,
                _ => str_val(&s.dom.text(id)),
            })
        },
        |i, st, id, v| {
            let t = if matches!(v, Value::Null | Value::Undefined) { "".into() } else { i.to_str(&v)? };
            set_text(&mut st.borrow_mut().dom, id, &t);
            changed(st);
            Ok(())
        },
    );
    for name in ["nodeValue", "data", "wholeText"] {
        accessor(
            i,
            st,
            &node,
            name,
            |_i, st, id| {
                let s = st.borrow();
                Ok(match &s.dom.nodes[id as usize].data {
                    Data::Text(t) => str_val(t),
                    _ => Value::Null,
                })
            },
            |i, st, id, v| {
                let t = i.to_str(&v)?;
                let is_text = matches!(st.borrow().dom.nodes[id as usize].data, Data::Text(_));
                if is_text {
                    set_text(&mut st.borrow_mut().dom, id, &t);
                    changed(st);
                }
                Ok(())
            },
        );
    }
    getter(i, st, &node, "length", |_i, st, id| {
        let s = st.borrow();
        Ok(match &s.dom.nodes[id as usize].data {
            Data::Text(t) => Value::Num(t.encode_utf16().count() as f64),
            _ => Value::Undefined,
        })
    });
    getter(i, st, &node, "ownerDocument", |i, st, id| Ok(if id == ROOT { Value::Null } else { wrap(i, st, ROOT) }));
    getter(i, st, &node, "isConnected", |_i, st, id| Ok(Value::Bool(connected(&st.borrow().dom, id))));
    method(i, st, &node, "getRootNode", |i, st, id, _a| {
        let mut r = id;
        {
            let s = st.borrow();
            loop {
                let p = s.dom.nodes[r as usize].parent;
                if p == r {
                    break;
                }
                r = p;
            }
        }
        Ok(wrap(i, st, r))
    });
    method(i, st, &node, "hasChildNodes", |_i, st, id, _a| Ok(Value::Bool(!st.borrow().dom.children(id).is_empty())));
    method(i, st, &node, "contains", |_i, st, id, a| {
        Ok(Value::Bool(match node_of(&arg(&a, 0)) {
            Some(other) => is_inclusive_ancestor(&st.borrow().dom, id, other),
            None => false,
        }))
    });
    method(i, st, &node, "isSameNode", |_i, _st, id, a| Ok(Value::Bool(node_of(&arg(&a, 0)) == Some(id))));
    method(i, st, &node, "compareDocumentPosition", |_i, st, id, a| {
        let Some(other) = node_of(&arg(&a, 0)) else { return Ok(Value::Num(0.0)) };
        let s = st.borrow();
        if other == id {
            return Ok(Value::Num(0.0));
        }
        if is_inclusive_ancestor(&s.dom, id, other) {
            return Ok(Value::Num(16.0 + 4.0)); // contained by, following
        }
        if is_inclusive_ancestor(&s.dom, other, id) {
            return Ok(Value::Num(8.0 + 2.0)); // contains, preceding
        }
        let order = descendants(&s.dom, ROOT);
        let (a, b) = (order.iter().position(|&n| n == id), order.iter().position(|&n| n == other));
        Ok(Value::Num(match (a, b) {
            (Some(a), Some(b)) if b > a => 4.0,
            (Some(_), Some(_)) => 2.0,
            _ => 1.0,
        }))
    });
    fn insert_js(i: &mut Interp, st: &S, parent: NodeId, child: &Value, before: Option<NodeId>) -> Res<Value> {
        let Some(c) = node_of(child) else { return i.throw("TypeError", "the argument is not a Node") };
        let r = insert(&mut st.borrow_mut().dom, parent, c, before);
        if let Err(e) = r {
            return i.throw("HierarchyRequestError", e);
        }
        changed(st);
        Ok(child.clone())
    }
    method(i, st, &node, "appendChild", |i, st, id, a| insert_js(i, st, id, &arg(&a, 0), None));
    method(i, st, &node, "insertBefore", |i, st, id, a| {
        let before = node_of(&arg(&a, 1));
        insert_js(i, st, id, &arg(&a, 0), before)
    });
    method(i, st, &node, "removeChild", |i, st, id, a| {
        let child = arg(&a, 0);
        let Some(c) = node_of(&child) else { return i.throw("TypeError", "the argument is not a Node") };
        if st.borrow().dom.nodes[c as usize].parent != id || c == id {
            return i.throw("NotFoundError", "the node to remove isn't a child of this node");
        }
        detach(&mut st.borrow_mut().dom, c);
        changed(st);
        Ok(child)
    });
    method(i, st, &node, "replaceChild", |i, st, id, a| {
        let (new, old) = (arg(&a, 0), arg(&a, 1));
        let Some(o) = node_of(&old) else { return i.throw("TypeError", "the argument is not a Node") };
        insert_js(i, st, id, &new, Some(o))?;
        detach(&mut st.borrow_mut().dom, o);
        changed(st);
        Ok(old)
    });
    method(i, st, &node, "cloneNode", |i, st, id, a| {
        let deep = Interp::truthy(&arg(&a, 0));
        let c = clone_node(&mut st.borrow_mut().dom, id, deep);
        Ok(wrap(i, st, c))
    });
    method(i, st, &node, "normalize", |_i, _st, _id, _a| Ok(Value::Undefined));
    // ChildNode / ParentNode conveniences (strings become text nodes).
    fn to_nodes(i: &mut Interp, st: &S, a: &[Value]) -> Res<Vec<NodeId>> {
        let mut out = Vec::new();
        for v in a {
            match node_of(v) {
                Some(n) => out.push(n),
                None => {
                    let t = i.to_str(v)?;
                    out.push(new_node(&mut st.borrow_mut().dom, Data::Text(t.to_string())));
                }
            }
        }
        Ok(out)
    }
    fn place(i: &mut Interp, st: &S, parent: NodeId, nodes: Vec<NodeId>, before: Option<NodeId>) -> Res<Value> {
        for n in nodes {
            if let Err(e) = insert(&mut st.borrow_mut().dom, parent, n, before) {
                return i.throw("HierarchyRequestError", e);
            }
        }
        changed(st);
        Ok(Value::Undefined)
    }
    method(i, st, &node, "append", |i, st, id, a| {
        let ns = to_nodes(i, st, &a)?;
        place(i, st, id, ns, None)
    });
    method(i, st, &node, "prepend", |i, st, id, a| {
        let ns = to_nodes(i, st, &a)?;
        let first = st.borrow().dom.children(id).first().copied();
        place(i, st, id, ns, first)
    });
    method(i, st, &node, "replaceChildren", |i, st, id, a| {
        let ns = to_nodes(i, st, &a)?;
        for c in st.borrow().dom.children(id).to_vec() {
            detach(&mut st.borrow_mut().dom, c);
        }
        place(i, st, id, ns, None)
    });
    method(i, st, &node, "remove", |_i, st, id, _a| {
        detach(&mut st.borrow_mut().dom, id);
        changed(st);
        Ok(Value::Undefined)
    });
    method(i, st, &node, "before", |i, st, id, a| {
        let ns = to_nodes(i, st, &a)?;
        let p = st.borrow().dom.nodes[id as usize].parent;
        if p == id {
            return Ok(Value::Undefined);
        }
        place(i, st, p, ns, Some(id))
    });
    method(i, st, &node, "after", |i, st, id, a| {
        let ns = to_nodes(i, st, &a)?;
        let (p, next) = {
            let s = st.borrow();
            let p = s.dom.nodes[id as usize].parent;
            let kids = s.dom.children(p);
            let at = kids.iter().position(|&k| k == id).unwrap_or(0);
            (p, kids.get(at + 1).copied())
        };
        if p == id {
            return Ok(Value::Undefined);
        }
        place(i, st, p, ns, next)
    });
    method(i, st, &node, "replaceWith", |i, st, id, a| {
        let ns = to_nodes(i, st, &a)?;
        let p = st.borrow().dom.nodes[id as usize].parent;
        if p == id {
            return Ok(Value::Undefined);
        }
        place(i, st, p, ns, Some(id))?;
        detach(&mut st.borrow_mut().dom, id);
        changed(st);
        Ok(Value::Undefined)
    });
    // Queries (on documents, fragments and elements).
    fn query(i: &mut Interp, st: &S, scope: NodeId, sel: &Value, all: bool) -> Res<Value> {
        let text = i.to_str(sel)?;
        let Some(sels) = selector_list(&text) else {
            // A selector we can't parse matches nothing (rather than throwing, which breaks more pages).
            return Ok(if all { i.array(Vec::new()) } else { Value::Null });
        };
        flush(i, st);
        let found: Vec<NodeId> = {
            let s = st.borrow();
            let d = descendants(&s.dom, scope);
            if all {
                d.into_iter().filter(|&n| matches_any(&s.dom, n, &sels)).collect()
            } else {
                d.into_iter().find(|&n| matches_any(&s.dom, n, &sels)).into_iter().collect()
            }
        };
        Ok(if all { wrap_list(i, st, found) } else { wrap_opt(i, st, found.first().copied()) })
    }
    method(i, st, &node, "querySelector", |i, st, id, a| query(i, st, id, &arg(&a, 0), false));
    method(i, st, &node, "querySelectorAll", |i, st, id, a| query(i, st, id, &arg(&a, 0), true));
    method(i, st, &node, "getElementsByTagName", |i, st, id, a| {
        let t = i.to_str(&arg(&a, 0))?.to_ascii_lowercase();
        let found: Vec<NodeId> = {
            let s = st.borrow();
            descendants(&s.dom, id).into_iter().filter(|&n| is_element(&s.dom, n) && (t == "*" || s.dom.tag(n) == Some(t.as_str()))).collect()
        };
        Ok(wrap_list(i, st, found))
    });
    method(i, st, &node, "getElementsByClassName", |i, st, id, a| {
        let want: Vec<String> = i.to_str(&arg(&a, 0))?.split_whitespace().map(str::to_string).collect();
        let found: Vec<NodeId> = {
            let s = st.borrow();
            descendants(&s.dom, id)
                .into_iter()
                .filter(|&n| {
                    let c = s.dom.attr(n, "class").unwrap_or("");
                    !want.is_empty() && want.iter().all(|w| c.split_whitespace().any(|x| x == w))
                })
                .collect()
        };
        Ok(wrap_list(i, st, found))
    });
    method(i, st, &node, "getElementById", |i, st, id, a| {
        let want = i.to_str(&arg(&a, 0))?;
        let found = {
            let s = st.borrow();
            descendants(&s.dom, id).into_iter().find(|&n| s.dom.attr(n, "id") == Some(&*want))
        };
        Ok(wrap_opt(i, st, found))
    });

    // ---------------------------------------------------------- Element
    getter(i, st, &element, "tagName", |_i, st, id| Ok(str_val(&tag_of(&st.borrow().dom, id).to_ascii_uppercase())));
    getter(i, st, &element, "localName", |_i, st, id| Ok(str_val(&tag_of(&st.borrow().dom, id))));
    getter(i, st, &element, "namespaceURI", |_i, _st, _id| Ok(str_val("http://www.w3.org/1999/xhtml")));
    method(i, st, &element, "getAttribute", |i, st, id, a| {
        let k = i.to_str(&arg(&a, 0))?.to_ascii_lowercase();
        if k == "style" {
            flush(i, st);
        }
        Ok(st.borrow().dom.attr(id, &k).map(str_val).unwrap_or(Value::Null))
    });
    method(i, st, &element, "setAttribute", |i, st, id, a| {
        let k = i.to_str(&arg(&a, 0))?.to_ascii_lowercase();
        let v = i.to_str(&arg(&a, 1))?;
        {
            let mut s = st.borrow_mut();
            set_attr(&mut s.dom, id, &k, &v);
            if k == "style" {
                s.styles.remove(&id);
            }
            if k.starts_with("data-") {
                s.datasets.remove(&id);
            }
        }
        changed(st);
        Ok(Value::Undefined)
    });
    method(i, st, &element, "removeAttribute", |i, st, id, a| {
        let k = i.to_str(&arg(&a, 0))?.to_ascii_lowercase();
        {
            let mut s = st.borrow_mut();
            remove_attr(&mut s.dom, id, &k);
            if k == "style" {
                s.styles.remove(&id);
            }
        }
        changed(st);
        Ok(Value::Undefined)
    });
    method(i, st, &element, "hasAttribute", |i, st, id, a| {
        let k = i.to_str(&arg(&a, 0))?.to_ascii_lowercase();
        Ok(Value::Bool(st.borrow().dom.attr(id, &k).is_some()))
    });
    method(i, st, &element, "toggleAttribute", |i, st, id, a| {
        let k = i.to_str(&arg(&a, 0))?.to_ascii_lowercase();
        let has = st.borrow().dom.attr(id, &k).is_some();
        let want = match arg(&a, 1) {
            Value::Undefined => !has,
            f => Interp::truthy(&f),
        };
        {
            let mut s = st.borrow_mut();
            if want {
                set_attr(&mut s.dom, id, &k, "");
            } else {
                remove_attr(&mut s.dom, id, &k);
            }
        }
        changed(st);
        Ok(Value::Bool(want))
    });
    method(i, st, &element, "getAttributeNames", |i, st, id, _a| {
        let names: Vec<Value> = match &st.borrow().dom.nodes[id as usize].data {
            Data::Element { attrs, .. } => attrs.iter().map(|(k, _)| str_val(k)).collect(),
            _ => Vec::new(),
        };
        Ok(i.array(names))
    });
    getter(i, st, &element, "attributes", |i, st, id| {
        let pairs: Vec<(String, String)> = match &st.borrow().dom.nodes[id as usize].data {
            Data::Element { attrs, .. } => attrs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect(),
            _ => Vec::new(),
        };
        let mut list = Vec::new();
        for (k, v) in pairs {
            let o = i.obj(Kind::Plain);
            i.set(&o, "name", str_val(&k))?;
            i.set(&o, "value", str_val(&v))?;
            list.push(o);
        }
        Ok(i.array(list))
    });
    // Reflected string attributes.
    for (prop, attr) in [
        ("id", "id"), ("className", "class"), ("title", "title"), ("lang", "lang"), ("dir", "dir"), ("name", "name"), ("type", "type"),
        ("placeholder", "placeholder"), ("alt", "alt"), ("rel", "rel"), ("target", "target"), ("method", "method"), ("htmlFor", "for"),
        ("role", "role"), ("slot", "slot"), ("download", "download"), ("accept", "accept"), ("autocomplete", "autocomplete"), ("pattern", "pattern"),
        ("min", "min"), ("max", "max"), ("step", "step"), ("charset", "charset"), ("content", "content"), ("media", "media"), ("sizes", "sizes"), ("srcset", "srcset"),
    ] {
        accessor(
            i,
            st,
            &element,
            prop,
            move |_i, st, id| Ok(str_val(st.borrow().dom.attr(id, attr).unwrap_or(""))),
            move |i, st, id, v| {
                let v = i.to_str(&v)?;
                set_attr(&mut st.borrow_mut().dom, id, attr, &v);
                changed(st);
                Ok(())
            },
        );
    }
    // Boolean attributes.
    for (prop, attr) in [
        ("hidden", "hidden"), ("disabled", "disabled"), ("checked", "checked"), ("selected", "selected"), ("required", "required"),
        ("readOnly", "readonly"), ("multiple", "multiple"), ("autofocus", "autofocus"), ("open", "open"), ("defer", "defer"), ("async", "async"),
        ("noValidate", "novalidate"), ("defaultChecked", "checked"), ("inert", "inert"), ("draggable", "draggable"),
    ] {
        accessor(
            i,
            st,
            &element,
            prop,
            move |_i, st, id| Ok(Value::Bool(st.borrow().dom.attr(id, attr).is_some())),
            move |_i, st, id, v| {
                {
                    let mut s = st.borrow_mut();
                    if Interp::truthy(&v) {
                        set_attr(&mut s.dom, id, attr, "");
                    } else {
                        remove_attr(&mut s.dom, id, attr);
                    }
                }
                changed(st);
                Ok(())
            },
        );
    }
    // URL attributes come back absolute.
    for (prop, attr) in [("href", "href"), ("src", "src"), ("action", "action"), ("poster", "poster"), ("cite", "cite")] {
        accessor(
            i,
            st,
            &element,
            prop,
            move |_i, st, id| {
                let s = st.borrow();
                Ok(str_val(&match s.dom.attr(id, attr) {
                    Some(v) => resolve_url(&s.url, v),
                    None if attr == "action" => s.url.clone(),
                    None => String::new(),
                }))
            },
            move |i, st, id, v| {
                let v = i.to_str(&v)?;
                set_attr(&mut st.borrow_mut().dom, id, attr, &v);
                changed(st);
                Ok(())
            },
        );
    }
    for (prop, attr, default) in [("tabIndex", "tabindex", -1.0), ("width", "width", 0.0), ("height", "height", 0.0), ("colSpan", "colspan", 1.0), ("rowSpan", "rowspan", 1.0), ("maxLength", "maxlength", -1.0)] {
        accessor(
            i,
            st,
            &element,
            prop,
            move |_i, st, id| Ok(Value::Num(st.borrow().dom.attr(id, attr).and_then(|v| v.trim().parse().ok()).unwrap_or(default))),
            move |i, st, id, v| {
                let n = i.to_number(&v)?;
                set_attr(&mut st.borrow_mut().dom, id, attr, &volant_js::interp::num_to_string(n));
                changed(st);
                Ok(())
            },
        );
    }
    accessor(
        i,
        st,
        &element,
        "innerHTML",
        |_i, st, id| Ok(str_val(&inner_html(&st.borrow().dom, id))),
        |i, st, id, v| {
            let h = if matches!(v, Value::Null) { "".into() } else { i.to_str(&v)? };
            let parsed = html::parse_with_scripts(&h);
            let mut s = st.borrow_mut();
            for c in s.dom.children(id).to_vec() {
                detach(&mut s.dom, c);
            }
            let frag = import(&mut s.dom, &parsed);
            let _ = insert(&mut s.dom, id, frag, None);
            s.generation += 1;
            Ok(())
        },
    );
    accessor(
        i,
        st,
        &element,
        "outerHTML",
        |_i, st, id| {
            let mut out = String::new();
            serialize(&st.borrow().dom, id, &mut out);
            Ok(str_val(&out))
        },
        |i, st, id, v| {
            let h = i.to_str(&v)?;
            let parsed = html::parse_with_scripts(&h);
            let mut s = st.borrow_mut();
            let p = s.dom.nodes[id as usize].parent;
            if p != id {
                let frag = import(&mut s.dom, &parsed);
                let _ = insert(&mut s.dom, p, frag, Some(id));
                detach(&mut s.dom, id);
                s.generation += 1;
            }
            Ok(())
        },
    );
    accessor(
        i,
        st,
        &element,
        "innerText",
        |_i, st, id| Ok(str_val(&st.borrow().dom.text(id))),
        |i, st, id, v| {
            let t = i.to_str(&v)?;
            set_text(&mut st.borrow_mut().dom, id, &t);
            changed(st);
            Ok(())
        },
    );
    method(i, st, &element, "insertAdjacentHTML", |i, st, id, a| {
        let pos = i.to_str(&arg(&a, 0))?.to_ascii_lowercase();
        let h = i.to_str(&arg(&a, 1))?;
        let parsed = html::parse_with_scripts(&h);
        let mut s = st.borrow_mut();
        let frag = import(&mut s.dom, &parsed);
        adjacent(&mut s.dom, id, &pos, frag);
        s.generation += 1;
        Ok(Value::Undefined)
    });
    method(i, st, &element, "insertAdjacentElement", |i, st, id, a| {
        let pos = i.to_str(&arg(&a, 0))?.to_ascii_lowercase();
        let el = arg(&a, 1);
        if let Some(n) = node_of(&el) {
            adjacent(&mut st.borrow_mut().dom, id, &pos, n);
            changed(st);
        }
        Ok(el)
    });
    method(i, st, &element, "insertAdjacentText", |i, st, id, a| {
        let pos = i.to_str(&arg(&a, 0))?.to_ascii_lowercase();
        let t = i.to_str(&arg(&a, 1))?;
        let mut s = st.borrow_mut();
        let n = new_node(&mut s.dom, Data::Text(t.to_string()));
        adjacent(&mut s.dom, id, &pos, n);
        s.generation += 1;
        Ok(Value::Undefined)
    });
    method(i, st, &element, "matches", |i, st, id, a| {
        let text = i.to_str(&arg(&a, 0))?;
        flush(i, st);
        let s = st.borrow();
        Ok(Value::Bool(selector_list(&text).map(|sels| matches_any(&s.dom, id, &sels)).unwrap_or(false)))
    });
    method(i, st, &element, "closest", |i, st, id, a| {
        let text = i.to_str(&arg(&a, 0))?;
        flush(i, st);
        let found = {
            let s = st.borrow();
            selector_list(&text).and_then(|sels| closest(&s.dom, id, |d, n| matches_any(d, n, &sels)))
        };
        Ok(wrap_opt(i, st, found))
    });
    // classList
    getter(i, st, &element, "classList", |i, st, id| {
        let proto = st.borrow().protos.as_ref().map(|p| p.class_list.clone());
        let o = i.obj(Kind::Plain);
        if let Value::Obj(oo) = &o {
            oo.borrow_mut().proto = proto;
            oo.borrow_mut().set_own("\0node", Value::Num(id as f64));
        }
        Ok(o)
    });
    fn classes(st: &S, id: NodeId) -> Vec<String> {
        st.borrow().dom.attr(id, "class").unwrap_or("").split_whitespace().map(str::to_string).collect()
    }
    fn set_classes(st: &S, id: NodeId, c: Vec<String>) {
        set_attr(&mut st.borrow_mut().dom, id, "class", &c.join(" "));
        changed(st);
    }
    method(i, st, &class_list, "add", |i, st, id, a| {
        let mut c = classes(st, id);
        for v in &a {
            let t = i.to_str(v)?.to_string();
            if !c.contains(&t) {
                c.push(t);
            }
        }
        set_classes(st, id, c);
        Ok(Value::Undefined)
    });
    method(i, st, &class_list, "remove", |i, st, id, a| {
        let mut c = classes(st, id);
        for v in &a {
            let t = i.to_str(v)?.to_string();
            c.retain(|x| *x != t);
        }
        set_classes(st, id, c);
        Ok(Value::Undefined)
    });
    method(i, st, &class_list, "toggle", |i, st, id, a| {
        let mut c = classes(st, id);
        let t = i.to_str(&arg(&a, 0))?.to_string();
        let want = match arg(&a, 1) {
            Value::Undefined => !c.contains(&t),
            f => Interp::truthy(&f),
        };
        c.retain(|x| *x != t);
        if want {
            c.push(t);
        }
        set_classes(st, id, c);
        Ok(Value::Bool(want))
    });
    method(i, st, &class_list, "contains", |i, st, id, a| {
        let t = i.to_str(&arg(&a, 0))?.to_string();
        Ok(Value::Bool(classes(st, id).contains(&t)))
    });
    method(i, st, &class_list, "replace", |i, st, id, a| {
        let (old, new) = (i.to_str(&arg(&a, 0))?.to_string(), i.to_str(&arg(&a, 1))?.to_string());
        let mut c = classes(st, id);
        let found = c.iter().position(|x| *x == old);
        if let Some(p) = found {
            c[p] = new;
            set_classes(st, id, c);
        }
        Ok(Value::Bool(found.is_some()))
    });
    method(i, st, &class_list, "item", |_i, st, id, a| {
        let k = match arg(&a, 0) {
            Value::Num(n) => n as usize,
            _ => 0,
        };
        Ok(classes(st, id).get(k).map(|s| str_val(s)).unwrap_or(Value::Null))
    });
    method(i, st, &class_list, "forEach", |i, st, id, a| {
        let f = arg(&a, 0);
        for (k, c) in classes(st, id).into_iter().enumerate() {
            i.call(&f, Value::Undefined, vec![str_val(&c), Value::Num(k as f64)])?;
        }
        Ok(Value::Undefined)
    });
    method(i, st, &class_list, "toString", |_i, st, id, _a| Ok(str_val(&classes(st, id).join(" "))));
    getter(i, st, &class_list, "length", |_i, st, id| Ok(Value::Num(classes(st, id).len() as f64)));
    accessor(i, st, &class_list, "value", |_i, st, id| Ok(str_val(&classes(st, id).join(" "))), |i, st, id, v| {
        let t = i.to_str(&v)?;
        set_classes(st, id, t.split_whitespace().map(str::to_string).collect());
        Ok(())
    });
    // style
    getter(i, st, &element, "style", |i, st, id| {
        if let Some(v) = st.borrow().styles.get(&id) {
            return Ok(v.clone());
        }
        let decls = st.borrow().dom.attr(id, "style").map(css::parse_declarations).unwrap_or_default();
        let proto = st.borrow().protos.as_ref().map(|p| p.style.clone());
        let o = i.obj(Kind::Plain);
        if let Value::Obj(oo) = &o {
            oo.borrow_mut().proto = proto;
            for d in decls {
                oo.borrow_mut().set_own(&camel(&d.name), str_val(&d.value));
            }
        }
        st.borrow_mut().styles.insert(id, o.clone());
        Ok(o)
    });
    def(i, &style_p, "setProperty", |i, t, a| {
        let k = camel(&i.to_str(&arg(&a, 0))?);
        let v = arg(&a, 1);
        i.set(&t, &k, if matches!(v, Value::Null) { str_val("") } else { v })?;
        Ok(Value::Undefined)
    });
    def(i, &style_p, "getPropertyValue", |i, t, a| {
        let k = camel(&i.to_str(&arg(&a, 0))?);
        match i.get(&t, &k)? {
            Value::Undefined => Ok(str_val("")),
            v => Ok(v),
        }
    });
    def(i, &style_p, "removeProperty", |i, t, a| {
        let k = camel(&i.to_str(&arg(&a, 0))?);
        let old = i.get(&t, &k)?;
        if let Value::Obj(o) = &t {
            o.borrow_mut().delete(&k);
        }
        Ok(old)
    });
    let css_text_get = i.native(|i, t, _a| {
        let Value::Obj(o) = &t else { return Ok(str_val("")) };
        let keys = o.borrow().own_keys();
        let mut parts = Vec::new();
        for k in keys {
            let v = i.get(&t, &k)?;
            let v = i.to_str(&v)?;
            if !v.is_empty() {
                parts.push(format!("{}: {v};", kebab(&k)));
            }
        }
        Ok(str_val(&parts.join(" ")))
    });
    let css_text_set = i.native(|i, t, a| {
        let text = i.to_str(&arg(&a, 0))?;
        if let Value::Obj(o) = &t {
            for k in o.borrow().own_keys() {
                o.borrow_mut().delete(&k);
            }
            for d in css::parse_declarations(&text) {
                o.borrow_mut().set_own(&camel(&d.name), str_val(&d.value));
            }
        }
        Ok(Value::Undefined)
    });
    style_p.borrow_mut().set_slot("cssText", Slot::Accessor(Some(css_text_get), Some(css_text_set)));
    // dataset
    getter(i, st, &element, "dataset", |i, st, id| {
        if let Some(v) = st.borrow().datasets.get(&id) {
            return Ok(v.clone());
        }
        let pairs: Vec<(String, String)> = match &st.borrow().dom.nodes[id as usize].data {
            Data::Element { attrs, .. } => attrs.iter().filter_map(|(k, v)| k.strip_prefix("data-").map(|k| (camel(k), v.clone()))).collect(),
            _ => Vec::new(),
        };
        let o = i.obj(Kind::Plain);
        for (k, v) in pairs {
            i.set(&o, &k, str_val(&v))?;
        }
        st.borrow_mut().datasets.insert(id, o.clone());
        Ok(o)
    });
    // Form controls.
    accessor(
        i,
        st,
        &element,
        "value",
        |i, st, id| {
            flush(i, st);
            Ok(str_val(&field_value(&st.borrow().dom, id)))
        },
        |i, st, id, v| {
            let v = if matches!(v, Value::Null) { "".into() } else { i.to_str(&v)? };
            set_field_value(&mut st.borrow_mut().dom, id, &v);
            changed(st);
            Ok(())
        },
    );
    accessor(
        i,
        st,
        &element,
        "defaultValue",
        |_i, st, id| Ok(str_val(st.borrow().dom.attr(id, "value").unwrap_or(""))),
        |i, st, id, v| {
            let v = i.to_str(&v)?;
            set_attr(&mut st.borrow_mut().dom, id, "value", &v);
            changed(st);
            Ok(())
        },
    );
    getter(i, st, &element, "valueAsNumber", |_i, st, id| Ok(Value::Num(field_value(&st.borrow().dom, id).trim().parse().unwrap_or(f64::NAN))));
    getter(i, st, &element, "options", |i, st, id| {
        let found: Vec<NodeId> = {
            let s = st.borrow();
            descendants(&s.dom, id).into_iter().filter(|&o| s.dom.tag(o) == Some("option")).collect()
        };
        Ok(wrap_list(i, st, found))
    });
    accessor(
        i,
        st,
        &element,
        "selectedIndex",
        |_i, st, id| {
            let s = st.borrow();
            let options: Vec<NodeId> = descendants(&s.dom, id).into_iter().filter(|&o| s.dom.tag(o) == Some("option")).collect();
            let sel = crate::forms::selected_option(&s.dom, id);
            Ok(Value::Num(sel.and_then(|x| options.iter().position(|&o| o == x)).map(|p| p as f64).unwrap_or(-1.0)))
        },
        |i, st, id, v| {
            let k = i.to_number(&v)? as i64;
            let mut s = st.borrow_mut();
            let options: Vec<NodeId> = descendants(&s.dom, id).into_iter().filter(|&o| s.dom.tag(o) == Some("option")).collect();
            for (n, &o) in options.iter().enumerate() {
                if n as i64 == k {
                    set_attr(&mut s.dom, o, "selected", "");
                } else {
                    remove_attr(&mut s.dom, o, "selected");
                }
            }
            s.generation += 1;
            Ok(())
        },
    );
    getter(i, st, &element, "text", |_i, st, id| Ok(str_val(st.borrow().dom.text(id).trim())));
    getter(i, st, &element, "form", |i, st, id| {
        let f = closest(&st.borrow().dom, id, |d, n| d.tag(n) == Some("form"));
        Ok(wrap_opt(i, st, f))
    });
    getter(i, st, &element, "elements", |i, st, id| {
        let found: Vec<NodeId> = {
            let s = st.borrow();
            descendants(&s.dom, id).into_iter().filter(|&n| matches!(s.dom.tag(n), Some("input" | "select" | "textarea" | "button"))).collect()
        };
        Ok(wrap_list(i, st, found))
    });
    method(i, st, &element, "submit", |i, st, id, _a| {
        submit(i, st, id, None);
        Ok(Value::Undefined)
    });
    method(i, st, &element, "requestSubmit", |i, st, id, _a| {
        submit(i, st, id, None);
        Ok(Value::Undefined)
    });
    method(i, st, &element, "reset", |_i, _st, _id, _a| Ok(Value::Undefined));
    method(i, st, &element, "checkValidity", |_i, _st, _id, _a| Ok(Value::Bool(true)));
    method(i, st, &element, "reportValidity", |_i, _st, _id, _a| Ok(Value::Bool(true)));
    method(i, st, &element, "setCustomValidity", |_i, _st, _id, _a| Ok(Value::Undefined));
    method(i, st, &element, "select", |_i, _st, _id, _a| Ok(Value::Undefined));
    method(i, st, &element, "setSelectionRange", |_i, _st, _id, _a| Ok(Value::Undefined));
    method(i, st, &element, "click", |i, st, id, _a| {
        let ev = make_event(i, st, "click", true, true);
        if dispatch(i, st, id, &ev) {
            activate(i, st, id);
        }
        Ok(Value::Undefined)
    });
    method(i, st, &element, "focus", |i, st, id, _a| {
        focus(i, st, Some(id));
        Ok(Value::Undefined)
    });
    method(i, st, &element, "blur", |i, st, id, _a| {
        if st.borrow().focus == Some(id) {
            focus(i, st, None);
        }
        Ok(Value::Undefined)
    });
    method(i, st, &element, "showModal", |_i, st, id, _a| {
        set_attr(&mut st.borrow_mut().dom, id, "open", "");
        changed(st);
        Ok(Value::Undefined)
    });
    method(i, st, &element, "show", |_i, st, id, _a| {
        set_attr(&mut st.borrow_mut().dom, id, "open", "");
        changed(st);
        Ok(Value::Undefined)
    });
    method(i, st, &element, "close", |_i, st, id, _a| {
        remove_attr(&mut st.borrow_mut().dom, id, "open");
        changed(st);
        Ok(Value::Undefined)
    });
    // Geometry: the engine lays out elsewhere, so these are estimates.
    let rect = |i: &mut Interp, st: &S, id: NodeId| -> Res<Value> {
        let (w, h) = {
            let s = st.borrow();
            let hidden = closest(&s.dom, id, |d, n| d.attr(n, "hidden").is_some()).is_some();
            if hidden || !connected(&s.dom, id) {
                (0.0, 0.0)
            } else {
                (s.env.width as f64, 20.0)
            }
        };
        let o = i.obj(Kind::Plain);
        for (k, v) in [("x", 0.0), ("y", 0.0), ("left", 0.0), ("top", 0.0), ("width", w), ("height", h), ("right", w), ("bottom", h)] {
            i.set(&o, k, Value::Num(v))?;
        }
        Ok(o)
    };
    method(i, st, &element, "getBoundingClientRect", move |i, st, id, _a| rect(i, st, id));
    method(i, st, &element, "getClientRects", move |i, st, id, _a| {
        let r = rect(i, st, id)?;
        Ok(i.array(vec![r]))
    });
    for (prop, which) in [("offsetWidth", 0), ("clientWidth", 0), ("scrollWidth", 0), ("offsetHeight", 1), ("clientHeight", 1), ("scrollHeight", 1)] {
        getter(i, st, &element, prop, move |_i, st, id| {
            let s = st.borrow();
            let hidden = closest(&s.dom, id, |d, n| d.attr(n, "hidden").is_some()).is_some() || !connected(&s.dom, id);
            Ok(Value::Num(if hidden { 0.0 } else if which == 0 { s.env.width as f64 } else { 20.0 }))
        });
    }
    for prop in ["offsetTop", "offsetLeft", "clientTop", "clientLeft", "scrollTop", "scrollLeft"] {
        accessor(i, st, &element, prop, |_i, _st, _id| Ok(Value::Num(0.0)), |_i, _st, _id, _v| Ok(()));
    }
    getter(i, st, &element, "offsetParent", |i, st, _id| {
        let body = st.borrow().dom.find("body");
        Ok(wrap_opt(i, st, body))
    });
    for name in ["scrollIntoView", "scrollTo", "scrollBy", "scroll", "setPointerCapture", "releasePointerCapture", "requestFullscreen"] {
        method(i, st, &element, name, |_i, _st, _id, _a| Ok(Value::Undefined));
    }
    method(i, st, &element, "animate", |i, _st, _id, _a| {
        let o = i.obj(Kind::Plain);
        let p = i.promise_resolve(Value::Undefined);
        i.set(&o, "finished", p)?;
        let noop = i.native(|_i, _t, _a| Ok(Value::Undefined));
        for k in ["cancel", "play", "pause", "finish", "reverse"] {
            i.set(&o, k, noop.clone())?;
        }
        Ok(o)
    });
    method(i, st, &element, "getAnimations", |i, _st, _id, _a| Ok(i.array(Vec::new())));
    method(i, st, &element, "attachShadow", |i, st, id, _a| Ok(wrap(i, st, id)));
    getter(i, st, &element, "shadowRoot", |_i, _st, _id| Ok(Value::Null));
    getter(i, st, &element, "content", |i, st, id| Ok(wrap(i, st, id)));
    getter(i, st, &element, "isContentEditable", |_i, _st, _id| Ok(Value::Bool(false)));
    getter(i, st, &element, "complete", |_i, _st, _id| Ok(Value::Bool(true)));
    for prop in ["naturalWidth", "naturalHeight"] {
        getter(i, st, &element, prop, |_i, _st, _id| Ok(Value::Num(0.0)));
    }
    method(i, st, &element, "getContext", |_i, _st, _id, _a| Ok(Value::Null));
    for name in ["play", "load"] {
        method(i, st, &element, name, |i, _st, _id, _a| Ok(i.promise_resolve(Value::Undefined)));
    }
    method(i, st, &element, "pause", |_i, _st, _id, _a| Ok(Value::Undefined));

    // ---------------------------------------------------------- Document
    let find_tag = |st: &S, t: &str| st.borrow().dom.find(t);
    getter(i, st, &document, "documentElement", move |i, st, _id| Ok(wrap_opt(i, st, find_tag(st, "html"))));
    getter(i, st, &document, "head", move |i, st, _id| Ok(wrap_opt(i, st, find_tag(st, "head"))));
    getter(i, st, &document, "body", move |i, st, _id| Ok(wrap_opt(i, st, find_tag(st, "body"))));
    getter(i, st, &document, "scrollingElement", move |i, st, _id| Ok(wrap_opt(i, st, find_tag(st, "html"))));
    accessor(
        i,
        st,
        &document,
        "title",
        |_i, st, _id| {
            let s = st.borrow();
            Ok(str_val(&s.dom.find("title").map(|t| s.dom.text(t).split_whitespace().collect::<Vec<_>>().join(" ")).unwrap_or_default()))
        },
        |i, st, _id, v| {
            let t = i.to_str(&v)?;
            let mut s = st.borrow_mut();
            let title = match s.dom.find("title") {
                Some(t) => t,
                None => {
                    let t = new_node(&mut s.dom, Data::Element { name: "title".into(), attrs: Vec::new() });
                    let head = s.dom.find("head").unwrap_or(ROOT);
                    let _ = insert(&mut s.dom, head, t, None);
                    t
                }
            };
            set_text(&mut s.dom, title, &t);
            s.generation += 1;
            Ok(())
        },
    );
    getter(i, st, &document, "URL", |_i, st, _id| Ok(str_val(&st.borrow().url)));
    getter(i, st, &document, "documentURI", |_i, st, _id| Ok(str_val(&st.borrow().url)));
    getter(i, st, &document, "baseURI", |_i, st, _id| Ok(str_val(&st.borrow().url)));
    getter(i, st, &document, "referrer", |_i, _st, _id| Ok(str_val("")));
    getter(i, st, &document, "characterSet", |_i, _st, _id| Ok(str_val("UTF-8")));
    getter(i, st, &document, "compatMode", |_i, _st, _id| Ok(str_val("CSS1Compat")));
    getter(i, st, &document, "contentType", |_i, _st, _id| Ok(str_val("text/html")));
    getter(i, st, &document, "visibilityState", |_i, _st, _id| Ok(str_val("visible")));
    getter(i, st, &document, "hidden", |_i, _st, _id| Ok(Value::Bool(false)));
    getter(i, st, &document, "defaultView", |i, _st, _id| Ok(Value::Obj(i.global_obj.clone())));
    getter(i, st, &document, "activeElement", |i, st, _id| {
        let f = st.borrow().focus;
        let body = st.borrow().dom.find("body");
        Ok(wrap_opt(i, st, f.or(body)))
    });
    getter(i, st, &document, "currentScript", |i, st, _id| {
        let c = st.borrow().current_script;
        Ok(wrap_opt(i, st, c))
    });
    getter(i, st, &document, "domain", |_i, st, _id| {
        let u = st.borrow().url.clone();
        Ok(str_val(u.split("://").nth(1).and_then(|r| r.split(['/', ':', '?']).next()).unwrap_or("")))
    });
    accessor(
        i,
        st,
        &document,
        "cookie",
        |_i, st, _id| Ok(str_val(&st.borrow().cookies.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("; "))),
        |i, st, _id, v| {
            let c = i.to_str(&v)?;
            let first = c.split(';').next().unwrap_or("");
            if let Some((k, v)) = first.split_once('=') {
                let (k, v) = (k.trim().to_string(), v.trim().to_string());
                let mut s = st.borrow_mut();
                s.cookies.retain(|(x, _)| *x != k);
                let expired = c.to_ascii_lowercase().contains("max-age=0") || c.to_ascii_lowercase().contains("expires=thu, 01 jan 1970");
                if !expired {
                    s.cookies.push((k, v));
                }
            }
            Ok(())
        },
    );
    for (name, tag) in [("forms", "form"), ("images", "img"), ("scripts", "script"), ("links", "a"), ("embeds", "embed")] {
        getter(i, st, &document, name, move |i, st, _id| {
            let found: Vec<NodeId> = {
                let s = st.borrow();
                descendants(&s.dom, ROOT).into_iter().filter(|&n| s.dom.tag(n) == Some(tag)).collect()
            };
            Ok(wrap_list(i, st, found))
        });
    }
    method(i, st, &document, "createElement", |i, st, _id, a| {
        let t = i.to_str(&arg(&a, 0))?.to_ascii_lowercase();
        let n = new_node(&mut st.borrow_mut().dom, Data::Element { name: t.into(), attrs: Vec::new() });
        Ok(wrap(i, st, n))
    });
    method(i, st, &document, "createElementNS", |i, st, _id, a| {
        let t = i.to_str(&arg(&a, 1))?.to_ascii_lowercase();
        let n = new_node(&mut st.borrow_mut().dom, Data::Element { name: t.into(), attrs: Vec::new() });
        Ok(wrap(i, st, n))
    });
    method(i, st, &document, "createTextNode", |i, st, _id, a| {
        let t = i.to_str(&arg(&a, 0))?;
        let n = new_node(&mut st.borrow_mut().dom, Data::Text(t.to_string()));
        Ok(wrap(i, st, n))
    });
    method(i, st, &document, "createComment", |i, st, _id, _a| {
        let n = new_node(&mut st.borrow_mut().dom, Data::Element { name: COMMENT.into(), attrs: Vec::new() });
        Ok(wrap(i, st, n))
    });
    method(i, st, &document, "createDocumentFragment", |i, st, _id, _a| {
        let n = new_node(&mut st.borrow_mut().dom, Data::Element { name: FRAGMENT.into(), attrs: Vec::new() });
        Ok(wrap(i, st, n))
    });
    method(i, st, &document, "createEvent", |i, st, _id, _a| Ok(make_event(i, st, "", false, false)));
    method(i, st, &document, "importNode", |i, st, _id, a| {
        let Some(n) = node_of(&arg(&a, 0)) else { return Ok(Value::Null) };
        let c = clone_node(&mut st.borrow_mut().dom, n, Interp::truthy(&arg(&a, 1)));
        Ok(wrap(i, st, c))
    });
    method(i, st, &document, "adoptNode", |_i, st, _id, a| {
        if let Some(n) = node_of(&arg(&a, 0)) {
            detach(&mut st.borrow_mut().dom, n);
        }
        Ok(arg(&a, 0))
    });
    method(i, st, &document, "hasFocus", |_i, _st, _id, _a| Ok(Value::Bool(true)));
    method(i, st, &document, "write", |i, st, _id, a| {
        let mut h = String::new();
        for v in &a {
            h.push_str(&i.to_str(v)?);
        }
        let parsed = html::parse_with_scripts(&h);
        let mut s = st.borrow_mut();
        let frag = import(&mut s.dom, &parsed);
        // After the running script, or at the end of the body.
        match s.current_script {
            Some(sc) => adjacent(&mut s.dom, sc, "afterend", frag),
            None => {
                let body = s.dom.find("body").unwrap_or(ROOT);
                let _ = insert(&mut s.dom, body, frag, None);
            }
        }
        s.generation += 1;
        Ok(Value::Undefined)
    });
    method(i, st, &document, "writeln", |i, _st, _id, a| {
        let doc = i.get(&Value::Obj(i.global_obj.clone()), "document")?;
        let w = i.get(&doc, "write")?;
        let mut a = a;
        a.push(str_val("\n"));
        i.call(&w, doc, a)
    });
    method(i, st, &document, "open", |_i, _st, _id, _a| Ok(Value::Undefined));
    method(i, st, &document, "close", |_i, _st, _id, _a| Ok(Value::Undefined));
    method(i, st, &document, "getSelection", |i, _st, _id, _a| {
        let g = Value::Obj(i.global_obj.clone());
        let f = i.get(&g, "getSelection")?;
        i.call(&f, g, vec![])
    });
    accessor(i, st, &document, "readyState", |_i, st, _id| Ok(str_val(if st.borrow().current_script.is_some() { "loading" } else { "complete" })), |_i, _st, _id, _v| Ok(()));

    // ---------------------------------------------------------- window
    let doc = wrap(i, st, ROOT);
    let g = Value::Obj(i.global_obj.clone());
    i.set(&g, "document", doc).ok();
    let hook = |i: &mut Interp, name: &str, f: Value| {
        i.global_obj.borrow_mut().set_slot(name, Slot::Data(f));
    };
    let st2 = st.clone();
    let f = i.native(move |i, _t, a| {
        // __fetch(method, url, body) → [status, contentType, text, finalUrl] or null
        let method = i.to_str(&arg(&a, 0))?.to_ascii_uppercase();
        let url = i.to_str(&arg(&a, 1))?;
        let body = match arg(&a, 2) {
            Value::Undefined | Value::Null => None,
            b => Some(i.to_str(&b)?.to_string()),
        };
        let (host, abs) = {
            let s = st2.borrow();
            (s.host.clone(), resolve_url(&s.url, &url))
        };
        Ok(match host.fetch(&method, &abs, body.as_deref()) {
            Some((status, ty, bytes)) => {
                let text = String::from_utf8_lossy(&bytes).into_owned();
                i.array(vec![Value::Num(status as f64), str_val(&ty), str_val(&text), str_val(&abs)])
            }
            None => Value::Null,
        })
    });
    hook(i, "__fetch", f);
    let st2 = st.clone();
    let f = i.native(move |i, _t, a| {
        let q = i.to_str(&arg(&a, 0))?;
        let env = st2.borrow().env;
        Ok(Value::Bool(css::media_applies(&q, &env)))
    });
    hook(i, "__media", f);
    let st2 = st.clone();
    let f = i.native(move |i, _t, a| {
        let u = i.to_str(&arg(&a, 0))?;
        let base = match arg(&a, 1) {
            Value::Undefined => st2.borrow().url.clone(),
            b => i.to_str(&b)?.to_string(),
        };
        Ok(str_val(&resolve_url(&base, &u)))
    });
    hook(i, "__resolve", f);
    let st2 = st.clone();
    let f = i.native(move |i, _t, a| {
        let u = i.to_str(&arg(&a, 0))?;
        let mut s = st2.borrow_mut();
        let abs = resolve_url(&s.url, &u);
        let same_doc = abs.split('#').next() == s.url.split('#').next() && abs.contains('#');
        if same_doc {
            s.url = abs;
        } else {
            s.navigate = Some(abs);
        }
        Ok(Value::Undefined)
    });
    hook(i, "__navigate", f);
    let st2 = st.clone();
    let f = i.native(move |i, _t, a| {
        // __setURL(url): history.pushState / replaceState
        if let Value::Str(u) = arg(&a, 0) {
            let mut s = st2.borrow_mut();
            s.url = resolve_url(&s.url, &u);
        }
        let _ = i;
        Ok(str_val(&st2.borrow().url))
    });
    hook(i, "__setURL", f);
    let st2 = st.clone();
    let f = i.native(move |_i, _t, _a| Ok(str_val(&st2.borrow().url)));
    hook(i, "__url", f);
    let st2 = st.clone();
    let f = i.native(move |_i, _t, _a| Ok(Value::Num(st2.borrow().env.width as f64)));
    hook(i, "__width", f);
    let st2 = st.clone();
    let f = i.native(move |_i, _t, _a| Ok(Value::Bool(st2.borrow().env.dark)));
    hook(i, "__dark", f);
    let st2 = st.clone();
    let f = i.native(move |i, _t, a| {
        // __computed(el, prop): a computed value, from the engine's own cascade.
        let Some(id) = node_of(&arg(&a, 0)) else { return Ok(str_val("")) };
        let prop = i.to_str(&arg(&a, 1))?.to_string();
        flush(i, &st2);
        Ok(str_val(&computed(&st2, id, &prop)))
    });
    hook(i, "__computed", f);
}

/// insertAdjacent*: beforebegin, afterbegin, beforeend, afterend.
fn adjacent(dom: &mut Dom, id: NodeId, pos: &str, n: NodeId) {
    let parent = dom.nodes[id as usize].parent;
    match pos {
        "beforebegin" if parent != id => {
            let _ = insert(dom, parent, n, Some(id));
        }
        "afterend" if parent != id => {
            let next = {
                let kids = dom.children(parent);
                let at = kids.iter().position(|&k| k == id).unwrap_or(0);
                kids.get(at + 1).copied()
            };
            let _ = insert(dom, parent, n, next);
        }
        "afterbegin" => {
            let first = dom.children(id).first().copied();
            let _ = insert(dom, id, n, first);
        }
        _ => {
            let _ = insert(dom, id, n, None);
        }
    }
}

fn computed(st: &S, id: NodeId, prop: &str) -> String {
    let mut s = st.borrow_mut();
    let generation = s.generation;
    if s.computed.as_ref().map(|c| c.0) != Some(generation) {
        let mut all = crate::style_sheets(&s.dom);
        all.extend(s.sheets.iter().cloned());
        let styled = style::compute(&s.dom, &all, &s.env);
        s.computed = Some((generation, styled));
    }
    let Some((_, styled)) = &s.computed else { return String::new() };
    let Some(st) = styled.styles.get(id as usize) else { return String::new() };
    let color = |c: style::Color| if c.a == 255 { format!("rgb({}, {}, {})", c.r, c.g, c.b) } else { format!("rgba({}, {}, {}, {})", c.r, c.g, c.b, c.a as f32 / 255.0) };
    let len = |l: style::Len| match l {
        style::Len::Px(v) => format!("{v}px"),
        style::Len::Pct(p) => format!("{p}%"),
        _ => "auto".into(),
    };
    match prop {
        "display" => format!("{:?}", st.display).to_ascii_lowercase().replace("listitem", "list-item").replace("inlineblock", "inline-block"),
        "visibility" => (if st.hidden { "hidden" } else { "visible" }).into(),
        "opacity" => format!("{}", st.opacity),
        "color" => color(st.color),
        "background-color" | "backgroundColor" => color(st.background),
        "font-size" | "fontSize" => format!("{}px", st.font_size),
        "font-weight" | "fontWeight" => format!("{}", st.weight),
        "line-height" | "lineHeight" => format!("{}px", st.line_height),
        "position" => format!("{:?}", st.position).to_ascii_lowercase(),
        "width" => len(st.width),
        "height" => len(st.height),
        "overflow" => (if st.overflow_clip { "hidden" } else { "visible" }).into(),
        _ => String::new(),
    }
}

// ------------------------------------------------------------------ the runtime

/// What the app lays out: the document as it is now, in document order.
pub struct Snapshot {
    pub dom: Dom,
    /// For each node of `dom`, the runtime's node (to send clicks back).
    pub map: Vec<NodeId>,
    pub title: String,
    pub url: String,
    pub generation: u64,
}

pub struct Runtime {
    pub interp: Interp,
    pub state: S,
    started: std::time::Instant,
}

const PRELUDE: &str = include_str!("prelude.js");

impl Runtime {
    /// Parses `html` (keeping scripts) and sets up the page's JavaScript world; runs nothing yet.
    pub fn new(html: &str, url: &str, env: Env, host: Rc<dyn Host>) -> Runtime {
        let dom = html::parse_with_scripts(html);
        let state = Rc::new(RefCell::new(State {
            dom,
            url: url.to_string(),
            env,
            sheets: Vec::new(),
            listeners: HashMap::new(),
            wrappers: HashMap::new(),
            styles: HashMap::new(),
            datasets: HashMap::new(),
            handlers: HashMap::new(),
            focus: None,
            navigate: None,
            cookies: Vec::new(),
            current_script: None,
            host,
            protos: None,
            generation: 1,
            computed: None,
        }));
        let mut interp = Interp::new();
        install(&mut interp, &state);
        interp.reset_steps(BUDGET);
        if let Err(e) = interp.run(PRELUDE) {
            interp.errors.push(format!("prelude: {e}"));
        }
        Runtime { interp, state, started: std::time::Instant::now() }
    }

    /// The scripts to run, in order: (node, source text, where it came from).
    fn scripts(&self) -> Vec<(NodeId, Option<String>, String)> {
        let s = self.state.borrow();
        let mut normal = Vec::new();
        let mut deferred = Vec::new();
        for id in descendants(&s.dom, ROOT) {
            if s.dom.tag(id) != Some("script") {
                continue;
            }
            let ty = s.dom.attr(id, "type").unwrap_or("").trim().to_ascii_lowercase();
            let js = ty.is_empty() || ty == "module" || ty.contains("javascript") || ty.contains("ecmascript");
            if !js || s.dom.attr(id, "nomodule").is_some() {
                continue;
            }
            let entry = match s.dom.attr(id, "src") {
                Some(src) => {
                    let abs = resolve_url(&s.url, src);
                    (id, None, abs)
                }
                None => (id, Some(s.dom.text(id)), format!("inline script {}", normal.len() + deferred.len() + 1)),
            };
            if s.dom.attr(id, "defer").is_some() && s.dom.attr(id, "src").is_some() {
                deferred.push(entry);
            } else {
                normal.push(entry);
            }
        }
        normal.extend(deferred);
        normal
    }

    /// Runs the page's scripts, then DOMContentLoaded and load, then what they scheduled (about a second's worth).
    pub fn run_scripts(&mut self) {
        let host = self.state.borrow().host.clone();
        for (id, text, origin) in self.scripts() {
            let src = match text {
                Some(t) => t,
                None => match host.fetch("GET", &origin, None) {
                    Some((200..=299, _, body)) => String::from_utf8_lossy(&body).into_owned(),
                    _ => {
                        self.interp.errors.push(format!("couldn't load {origin}"));
                        continue;
                    }
                },
            };
            if src.contains("\nimport ") || src.trim_start().starts_with("import ") || src.contains("\nexport ") {
                self.interp.errors.push(format!("{origin}: modules (import/export) aren't supported yet"));
                continue;
            }
            self.state.borrow_mut().current_script = Some(id);
            self.interp.reset_steps(BUDGET);
            if let Err(e) = self.interp.run(&src) {
                self.interp.errors.push(format!("{origin}: {e}"));
            }
            self.state.borrow_mut().current_script = None;
        }
        self.state.borrow_mut().current_script = None;
        self.interp.reset_steps(BUDGET);
        let i = &mut self.interp;
        fire(i, &self.state, ROOT, "readystatechange", false);
        fire(i, &self.state, ROOT, "DOMContentLoaded", true);
        fire(i, &self.state, WINDOW, "load", false);
        fire(i, &self.state, WINDOW, "pageshow", false);
        self.interp.run_microtasks();
        self.settle(1000.0);
    }

    /// Runs timers due within the next `ms` of virtual time.
    fn settle(&mut self, ms: f64) {
        self.interp.reset_steps(BUDGET);
        let until = self.interp.now + ms;
        self.interp.run_jobs(until, 20_000);
        self.interp.now = self.interp.now.max(until);
        flush(&mut self.interp, &self.state);
    }

    /// Lets the page's clock catch up with real time (timers, animations).
    pub fn tick(&mut self) {
        let real = self.started.elapsed().as_secs_f64() * 1000.0 + 1000.0;
        if real > self.interp.now {
            let ms = real - self.interp.now;
            self.settle(ms);
        }
    }

    pub fn has_timers(&self) -> bool {
        !self.interp.timers.is_empty()
    }

    /// A click on a node (from hit testing a snapshot).
    pub fn click(&mut self, node: NodeId) {
        self.interp.reset_steps(BUDGET);
        let target = {
            let s = self.state.borrow();
            if node as usize >= s.dom.nodes.len() {
                return;
            }
            // Text isn't an event target: its element is.
            if matches!(s.dom.nodes[node as usize].data, Data::Text(_)) { s.dom.parent(node) } else { node }
        };
        let i = &mut self.interp;
        for ty in ["pointerdown", "mousedown", "pointerup", "mouseup"] {
            fire(i, &self.state, target, ty, true);
        }
        let ev = make_event(i, &self.state, "click", true, true);
        let _ = i.set(&ev, "button", Value::Num(0.0));
        let _ = i.set(&ev, "detail", Value::Num(1.0));
        if dispatch(i, &self.state, target, &ev) {
            activate(i, &self.state, target);
        }
        let focusable = {
            let s = self.state.borrow();
            closest(&s.dom, target, |d, n| matches!(d.tag(n), Some("input" | "textarea" | "select" | "button" | "a")))
        };
        if focusable.is_none() {
            focus(&mut self.interp, &self.state, None);
        }
        self.interp.run_microtasks();
        self.settle(50.0);
    }

    /// Typed text goes into the focused field.
    pub fn type_text(&mut self, text: &str) {
        self.interp.reset_steps(BUDGET);
        let Some(f) = self.state.borrow().focus else { return };
        if !is_text_field(&self.state.borrow().dom, f) {
            return;
        }
        for c in text.chars() {
            let i = &mut self.interp;
            let ev = make_event(i, &self.state, "keydown", true, true);
            let _ = i.set(&ev, "key", str_val(&c.to_string()));
            if !dispatch(i, &self.state, f, &ev) {
                continue;
            }
            {
                let mut s = self.state.borrow_mut();
                let v = field_value(&s.dom, f);
                set_field_value(&mut s.dom, f, &format!("{v}{c}"));
                s.generation += 1;
            }
            let ev = make_event(i, &self.state, "input", true, false);
            let _ = i.set(&ev, "data", str_val(&c.to_string()));
            let _ = i.set(&ev, "inputType", str_val("insertText"));
            dispatch(i, &self.state, f, &ev);
            let ev = make_event(i, &self.state, "keyup", true, true);
            let _ = i.set(&ev, "key", str_val(&c.to_string()));
            dispatch(i, &self.state, f, &ev);
        }
        self.settle(50.0);
    }

    /// A special key: "Backspace", "Enter", "Tab" or "Escape".
    pub fn key(&mut self, key: &str) {
        self.interp.reset_steps(BUDGET);
        let target = {
            let s = self.state.borrow();
            s.focus.or_else(|| s.dom.find("body")).unwrap_or(ROOT)
        };
        let i = &mut self.interp;
        let ev = make_event(i, &self.state, "keydown", true, true);
        let _ = i.set(&ev, "key", str_val(key));
        let _ = i.set(&ev, "code", str_val(key));
        let _ = i.set(&ev, "keyCode", Value::Num(match key {
            "Enter" => 13.0,
            "Backspace" => 8.0,
            "Tab" => 9.0,
            "Escape" => 27.0,
            _ => 0.0,
        }));
        if dispatch(i, &self.state, target, &ev) {
            let field = self.state.borrow().focus.filter(|&f| is_text_field(&self.state.borrow().dom, f));
            match (key, field) {
                ("Backspace", Some(f)) => {
                    {
                        let mut s = self.state.borrow_mut();
                        let mut v = field_value(&s.dom, f);
                        v.pop();
                        set_field_value(&mut s.dom, f, &v);
                        s.generation += 1;
                    }
                    let ev = make_event(i, &self.state, "input", true, false);
                    let _ = i.set(&ev, "inputType", str_val("deleteContentBackward"));
                    dispatch(i, &self.state, f, &ev);
                }
                ("Enter", Some(f)) => {
                    let (is_area, form) = {
                        let s = self.state.borrow();
                        (s.dom.tag(f) == Some("textarea"), closest(&s.dom, f, |d, n| d.tag(n) == Some("form")))
                    };
                    if is_area {
                        {
                            let mut s = self.state.borrow_mut();
                            let v = field_value(&s.dom, f);
                            set_field_value(&mut s.dom, f, &format!("{v}\n"));
                            s.generation += 1;
                        }
                        fire(i, &self.state, f, "input", true);
                    } else {
                        fire(i, &self.state, f, "change", true);
                        if let Some(form) = form {
                            submit(i, &self.state, form, None);
                        }
                    }
                }
                ("Tab", _) => {
                    let next = {
                        let s = self.state.borrow();
                        let fields: Vec<NodeId> = descendants(&s.dom, ROOT).into_iter().filter(|&n| is_text_field(&s.dom, n)).collect();
                        let at = s.focus.and_then(|f| fields.iter().position(|&x| x == f)).map(|p| p + 1).unwrap_or(0);
                        fields.get(at % fields.len().max(1)).copied()
                    };
                    focus(i, &self.state, next);
                }
                ("Escape", _) => focus(i, &self.state, None),
                _ => {}
            }
        }
        let ev = make_event(i, &self.state, "keyup", true, true);
        let _ = i.set(&ev, "key", str_val(key));
        dispatch(i, &self.state, target, &ev);
        self.settle(50.0);
    }

    /// Where the page asked to go (a link, a form, `location.href = …`), once.
    pub fn take_navigation(&mut self) -> Option<String> {
        self.state.borrow_mut().navigate.take()
    }

    /// What scripts printed, and their uncaught errors, since the last call.
    pub fn take_console(&mut self) -> Vec<String> {
        let mut out = std::mem::take(&mut self.interp.output);
        out.extend(self.interp.errors.drain(..).map(|e| format!("Uncaught {e}")));
        out
    }

    pub fn generation(&self) -> u64 {
        self.state.borrow().generation
    }

    pub fn focused(&self) -> Option<NodeId> {
        self.state.borrow().focus
    }

    /// The document, laid out-ready: in document order, form controls filled in.
    pub fn snapshot(&mut self) -> Snapshot {
        flush(&mut self.interp, &self.state);
        let s = self.state.borrow();
        let mut dom = Dom { nodes: vec![Node { parent: ROOT, children: Vec::new(), data: Data::Document }] };
        let mut map = vec![ROOT];
        fn copy(src: &Dom, sid: NodeId, dst: &mut Dom, parent: NodeId, map: &mut Vec<NodeId>) {
            let data = match &src.nodes[sid as usize].data {
                Data::Element { name, .. } if name.starts_with('#') => return,
                Data::Element { name, attrs } => Data::Element { name: name.clone(), attrs: attrs.clone() },
                Data::Text(t) => Data::Text(t.clone()),
                Data::Document => return,
            };
            let id = dst.nodes.len() as NodeId;
            dst.nodes.push(Node { parent, children: Vec::new(), data });
            dst.nodes[parent as usize].children.push(id);
            map.push(sid);
            for &c in src.children(sid) {
                copy(src, c, dst, id, map);
            }
        }
        for &c in s.dom.children(ROOT) {
            copy(&s.dom, c, &mut dom, ROOT, &mut map);
        }
        let before = dom.nodes.len();
        crate::forms::prepare(&mut dom);
        // What form controls added maps to the control.
        for k in before..dom.nodes.len() {
            let mut p = dom.nodes[k].parent as usize;
            while p >= before {
                p = dom.nodes[p].parent as usize;
            }
            map.push(map[p]);
        }
        let title = dom.find("title").map(|t| dom.text(t).split_whitespace().collect::<Vec<_>>().join(" ")).unwrap_or_default();
        Snapshot { dom, map, title, url: s.url.clone(), generation: s.generation }
    }
}

/// Renders a snapshot: the document's own `<style>`s plus `sheets`.
pub fn render_snapshot(snap: &Snapshot, sheets: &[String], env: Env, fonts: &dyn crate::Fonts, images: &dyn crate::Images) -> crate::Page {
    let mut all = crate::style_sheets(&snap.dom);
    all.extend(sheets.iter().filter(|s| !s.is_empty()).cloned());
    let styled = style::compute(&snap.dom, &all, &env);
    crate::layout::layout(&snap.dom, &styled, env.width, fonts, images)
}

/// The `<link rel=stylesheet>` URLs of a document, absolute, in order, with their media.
pub fn stylesheet_links(dom: &Dom, base: &str) -> Vec<(String, String)> {
    descendants(dom, ROOT)
        .into_iter()
        .filter(|&n| dom.tag(n) == Some("link") && dom.attr(n, "rel").map(|r| r.to_ascii_lowercase().split_whitespace().any(|w| w == "stylesheet")).unwrap_or(false))
        .filter(|&n| dom.attr(n, "disabled").is_none())
        .filter_map(|n| dom.attr(n, "href").map(|h| (resolve_url(base, h), dom.attr(n, "media").unwrap_or("").to_string())))
        .collect()
}

#[cfg(test)]
mod tests;
