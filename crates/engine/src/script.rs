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

/// A request a page makes (a script, a style sheet, fetch(), a form).
#[derive(Debug, Clone, Copy)]
pub struct Request<'a> {
    pub method: &'a str,
    pub url: &'a str,
    /// Request headers the page set (`Content-Type`, …); cookies are the host's to add.
    pub headers: &'a [(String, String)],
    pub body: Option<&'a [u8]>,
    /// Send and keep cookies (a cross-origin `fetch` only does with `credentials: 'include'`).
    pub credentials: bool,
    /// The page that made the request, which decides the `SameSite` cookies
    /// that go with it; `None` when the person did (the address bar).
    pub initiator: Option<&'a str>,
    /// A top-level navigation (a link, a form), not a page's own request.
    pub navigation: bool,
}

impl<'a> Request<'a> {
    /// A GET the person or the browser makes, with all its cookies.
    pub fn get(url: &'a str) -> Request<'a> {
        Request { method: "GET", url, headers: &[], body: None, credentials: true, initiator: None, navigation: false }
    }
}

/// What came back.
#[derive(Debug, Clone, Default)]
pub struct Reply {
    pub status: u16,
    pub content_type: String,
    pub body: Vec<u8>,
    /// Where it finally came from (after redirects); empty means the address asked for.
    pub url: String,
    /// Response headers, lower-case names.
    pub headers: Vec<(String, String)>,
}

impl Reply {
    pub fn ok(status: u16, content_type: &str, body: Vec<u8>) -> Reply {
        Reply { status, content_type: content_type.into(), body, ..Default::default() }
    }
    pub fn success(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// The network, as scripts see it (also used to load `<script src>`), and its cookies.
pub trait Host {
    /// Performs a request, or None if it failed.
    fn request(&self, req: &Request) -> Option<Reply>;
    /// A plain GET.
    fn get(&self, url: &str) -> Option<Reply> {
        self.request(&Request::get(url))
    }
    /// A GET for `page` (its scripts, style sheets, pictures).
    fn get_for(&self, url: &str, page: &str) -> Option<Reply> {
        self.request(&Request { initiator: Some(page), ..Request::get(url) })
    }
    /// `document.cookie` for a page (None: the host keeps no cookies, so the page keeps its own).
    fn cookie(&self, _url: &str) -> Option<String> {
        None
    }
    /// `document.cookie = …`; false if the host keeps no cookies.
    fn set_cookie(&self, _url: &str, _cookie: &str) -> bool {
        false
    }
    /// A way to GET a page's files (`url`, for `page`) on other threads, so they
    /// can all be on their way at once; None if the host can't.
    fn background(&self) -> Option<Fetch> {
        None
    }
    /// Starts `urls` (for `page`) on their way, to be asked for soon; does nothing if the host can't.
    fn prefetch(&self, _urls: Vec<String>, _page: &str) {}
}

/// GETs `url` for `page` from any thread (see [`Host::background`]).
pub type Fetch = std::sync::Arc<dyn Fn(&str, &str) -> Option<Reply> + Send + Sync>;

/// How many of a page's files are fetched at once.
const PRELOAD_THREADS: usize = 6;

/// A page's scripts and style sheets, fetched ahead and in parallel as soon as
/// the page is parsed (they still run in order; only the waiting overlaps).
#[derive(Default)]
pub struct Preload {
    /// By URL: None while on its way, then the reply (None if it failed).
    slots: std::sync::Mutex<HashMap<String, Option<Option<Reply>>>>,
    ready: std::sync::Condvar,
    /// Milliseconds the page spent waiting for preloaded files.
    waited: std::sync::Mutex<f64>,
}

impl Preload {
    fn start(self: &std::sync::Arc<Self>, urls: Vec<String>, page: &str, fetch: Fetch) {
        let mut queue = std::collections::VecDeque::new();
        {
            let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
            for u in urls {
                if !slots.contains_key(&u) {
                    slots.insert(u.clone(), None);
                    queue.push_back(u);
                }
            }
        }
        let n = queue.len().min(PRELOAD_THREADS);
        let queue = std::sync::Arc::new(std::sync::Mutex::new(queue));
        let mut started = 0;
        for _ in 0..n {
            let (me, queue, fetch, page) = (self.clone(), queue.clone(), fetch.clone(), page.to_string());
            let spawned = std::thread::Builder::new().name("volant-preload".into()).spawn(move || loop {
                let next = queue.lock().unwrap_or_else(|e| e.into_inner()).pop_front();
                let Some(url) = next else { break };
                let reply = fetch(&url, &page);
                if let Some(slot) = me.slots.lock().unwrap_or_else(|e| e.into_inner()).get_mut(&url) {
                    *slot = Some(reply);
                }
                me.ready.notify_all();
            });
            if spawned.is_ok() {
                started += 1;
            }
        }
        if started == 0 {
            // No thread to fetch them: the page asks for each itself.
            let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
            for u in queue.lock().unwrap_or_else(|e| e.into_inner()).drain(..) {
                slots.remove(&u);
            }
        }
    }

    /// The preloaded reply for `url` (waiting if it's on its way); None if it wasn't preloaded.
    fn take(&self, url: &str) -> Option<Option<Reply>> {
        let started = std::time::Instant::now();
        let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            match slots.get(url) {
                None => return None,
                Some(Some(_)) => {
                    *self.waited.lock().unwrap_or_else(|e| e.into_inner()) += started.elapsed().as_secs_f64() * 1000.0;
                    return slots.remove(url).flatten();
                }
                Some(None) if started.elapsed() > std::time::Duration::from_secs(60) => {
                    slots.remove(url);
                    return None;
                }
                Some(None) => {
                    slots = self.ready.wait_timeout(slots, std::time::Duration::from_secs(1)).map(|r| r.0).unwrap_or_else(|e| e.into_inner().0);
                }
            }
        }
    }
}

/// The host as the page sees it: preloaded files come from the preload.
struct Preloading {
    inner: Rc<dyn Host>,
    preload: std::sync::Arc<Preload>,
}

impl Host for Preloading {
    fn request(&self, req: &Request) -> Option<Reply> {
        if req.method == "GET" && req.body.is_none() {
            if let Some(r) = self.preload.take(req.url) {
                return r;
            }
        }
        self.inner.request(req)
    }
    fn cookie(&self, url: &str) -> Option<String> {
        self.inner.cookie(url)
    }
    fn set_cookie(&self, url: &str, cookie: &str) -> bool {
        self.inner.set_cookie(url, cookie)
    }
    fn background(&self) -> Option<Fetch> {
        self.inner.background()
    }
    fn prefetch(&self, urls: Vec<String>, page: &str) {
        if let Some(fetch) = self.inner.background() {
            self.preload.start(urls, page, fetch);
        }
    }
}

/// The files a document will ask for first: its scripts, style sheets and preloads, in order.
fn subresources(dom: &Dom, base: &str) -> Vec<String> {
    let mut out = Vec::new();
    for id in descendants(dom, ROOT) {
        match dom.tag(id) {
            Some("script") if is_js(dom, id) => {
                if let Some(src) = dom.attr(id, "src").filter(|s| !s.trim().is_empty()) {
                    out.push(resolve_url(base, src));
                }
            }
            Some("link") => {
                let rel = dom.attr(id, "rel").unwrap_or("").to_ascii_lowercase();
                let kind = dom.attr(id, "as").unwrap_or("").to_ascii_lowercase();
                let wanted = rel.split_whitespace().any(|r| r == "stylesheet" || r == "modulepreload") || (rel.split_whitespace().any(|r| r == "preload") && (kind == "script" || kind == "style"));
                if let Some(href) = dom.attr(id, "href").filter(|h| wanted && !h.trim().is_empty()) {
                    if !rel.contains("alternate") {
                        out.push(resolve_url(base, href));
                    }
                }
            }
            _ => {}
        }
    }
    out.retain(|u| u.starts_with("http://") || u.starts_with("https://"));
    let mut seen = std::collections::HashSet::new();
    out.retain(|u| seen.insert(u.clone()));
    out
}

/// One recorded canvas operation (see the context in `prelude.js`).
fn decode_canvas_op(v: &Value) -> Option<crate::canvas::Op> {
    use crate::canvas::{color, font, Align, Op};
    let Value::Obj(o) = v else { return None };
    let o = o.borrow();
    let Kind::Array(a) = &o.kind else { return None };
    let num = |k: usize| match a.get(k) {
        Some(Value::Num(n)) if n.is_finite() => *n as f32,
        _ => 0.0,
    };
    let text = |k: usize| match a.get(k) {
        Some(Value::Str(s)) => s.to_string(),
        _ => String::new(),
    };
    let points = |v: &[Value]| -> Vec<(f32, f32)> {
        v.chunks(2).filter_map(|p| match p {
            [Value::Num(x), Value::Num(y)] if x.is_finite() && y.is_finite() => Some((*x as f32, *y as f32)),
            _ => None,
        }).collect()
    };
    match text(0).as_str() {
        "p" => {
            let alpha = num(4);
            let paint = |k: usize| match a.get(k) {
                Some(Value::Str(s)) => color(s, alpha),
                _ => None,
            };
            let (fill, stroke) = (paint(1), paint(2));
            let Some(Value::Obj(list)) = a.get(5) else { return None };
            let list = list.borrow();
            let Kind::Array(subs) = &list.kind else { return None };
            let mut subpaths = Vec::new();
            for sp in subs {
                let Value::Obj(sp) = sp else { continue };
                let sp = sp.borrow();
                let Kind::Array(v) = &sp.kind else { continue };
                let closed = matches!(v.first(), Some(Value::Bool(true)));
                let pts = points(v.get(1..).unwrap_or(&[]));
                if pts.len() >= 2 {
                    subpaths.push((closed, pts));
                }
            }
            (!subpaths.is_empty() && (fill.is_some() || stroke.is_some())).then(|| Op::Path { subpaths, fill, stroke, line_width: num(3).max(0.1) })
        }
        "c" => Some(Op::Clear { points: points(a.get(1..).unwrap_or(&[])) }),
        "t" => {
            let (size, bold, serif, mono) = font(&text(4));
            let size = size * num(9).max(0.01);
            let color = color(&text(5), num(8))?;
            let align = match text(6).as_str() {
                "center" => Align::Center,
                "right" | "end" => Align::Right,
                _ => Align::Left,
            };
            // Baselines other than the alphabetic one, moved to it.
            let y = num(2)
                + match text(7).as_str() {
                    "top" | "hanging" => size * 0.8,
                    "middle" => size * 0.32,
                    "bottom" | "ideographic" => -size * 0.2,
                    _ => 0.0,
                };
            Some(Op::Text { x: num(1), y, text: text(3), size, bold, serif, mono, color, align })
        }
        "i" => Some(Op::Image { src: text(1), x: num(2), y: num(3), w: num(4), h: num(5), alpha: num(6) }),
        _ => None,
    }
}

/// The element a `#fragment` points at: the one with that id, else an `<a name>`.
pub fn anchor_target(dom: &Dom, frag: &str) -> Option<NodeId> {
    let all = descendants(dom, ROOT);
    all.iter().copied().find(|&n| dom.attr(n, "id") == Some(frag)).or_else(|| all.iter().copied().find(|&n| dom.tag(n) == Some("a") && dom.attr(n, "name") == Some(frag)))
}

/// `%XX` escapes decoded (as in a URL's fragment).
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Some(v) = std::str::from_utf8(&b[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Where a `#fragment` is on a laid-out snapshot: the top of its target's first box.
pub fn anchor_y(snap: &Snapshot, page: &crate::Page, frag: &str) -> Option<f32> {
    let frag = percent_decode(frag);
    if frag.is_empty() || frag == "top" {
        return Some(0.0);
    }
    let t = anchor_target(&snap.dom, &frag)?;
    let inside: std::collections::HashSet<NodeId> = descendants(&snap.dom, t).into_iter().chain([t]).collect();
    page.hits.iter().filter(|h| inside.contains(&h.node)).map(|h| h.y).fold(None, |m: Option<f32>, y| Some(m.map_or(y, |m| m.min(y))))
}

/// One line of a `<select>`'s menu.
#[derive(Debug, Clone, PartialEq)]
pub struct MenuItem {
    pub label: String,
    pub disabled: bool,
    pub selected: bool,
    /// An `<optgroup>`'s label, not an option.
    pub header: bool,
}

/// An element and its ancestors (up to the document).
fn element_chain(dom: &Dom, mut n: NodeId) -> Vec<NodeId> {
    let mut out = Vec::new();
    loop {
        if dom.tag(n).is_some() {
            out.push(n);
        }
        let p = dom.parent(n);
        if p == n || n == ROOT {
            break;
        }
        n = p;
    }
    out
}

/// No network: every request fails.
pub struct NoHost;

impl Host for NoHost {
    fn request(&self, _: &Request) -> Option<Reply> {
        None
    }
}

/// Where a page asked to go: a link or a GET form (no body), or a POST form.
#[derive(Debug, Clone, PartialEq)]
pub struct Navigation {
    pub url: String,
    pub method: String,
    pub body: Option<Vec<u8>>,
    /// The body's type (`application/x-www-form-urlencoded`, `multipart/form-data; boundary=…`).
    pub content_type: Option<String>,
    /// The page it came from (for `Referer` and `Origin`); the app fills it in.
    pub referrer: Option<String>,
}

impl Navigation {
    pub fn get(url: String) -> Navigation {
        Navigation { url, method: "GET".into(), body: None, content_type: None, referrer: None }
    }
}

/// Where window-level listeners are kept.
const WINDOW: NodeId = NodeId::MAX;
const FRAGMENT: &str = "#document-fragment";
/// XML's Name production, near enough: a letter, `_` or `:` (or any non-ASCII), then those, digits, `-` and `.`.
fn is_xml_name(s: &str) -> bool {
    let mut c = s.chars();
    let start = |ch: char| ch.is_ascii_alphabetic() || ch == '_' || ch == ':' || (ch as u32) >= 0xC0;
    matches!(c.next(), Some(f) if start(f)) && c.all(|ch| start(ch) || ch.is_ascii_digit() || ch == '-' || ch == '.' || ch == '\u{B7}')
}

/// DOM's "valid element local name".
fn is_element_local_name(s: &str) -> bool {
    let mut c = s.chars();
    match c.next() {
        None => false,
        Some(f) if f.is_ascii_alphabetic() => !s.contains(|ch: char| matches!(ch, '\t' | '\n' | '\x0c' | '\r' | ' ' | '\0' | '/' | '>')),
        Some(f) if f == ':' || f == '_' || f >= '\u{80}' => c.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '.' | ':' | '_') || ch >= '\u{80}'),
        _ => false,
    }
}

/// DOM's "valid namespace prefix", "valid attribute local name" and "valid doctype name".
fn no_ws_or(s: &str, extra: &[char]) -> bool {
    !s.contains(|ch: char| matches!(ch, '\t' | '\n' | '\x0c' | '\r' | ' ' | '\0') || extra.contains(&ch))
}
fn is_doctype_name(s: &str) -> bool {
    no_ws_or(s, &['>'])
}
/// The native stack a thread running a [`Runtime`] needs.
pub const STACK: usize = 512 << 20;
/// The step budget for each script and each event (about three seconds of
/// work), unless [`Runtime::budget`] says otherwise.
const BUDGET: u64 = 100_000_000;

struct Listener {
    ty: String,
    f: Value,
    capture: bool,
    once: bool,
}

struct Protos {
    node: Obj,
    /// HTMLElement.prototype (under Element's), and each element interface's own
    /// prototype by tag (HTMLScriptElement.prototype for `script`, …).
    html_element: Obj,
    /// Element interfaces by local name, for HTML's and SVG's namespaces.
    by_tag: HashMap<&'static str, Obj>,
    svg_tags: HashMap<&'static str, Obj>,
    element: Obj,
    svg: Obj,
    mathml: Obj,
    unknown: Obj,
    html_document: Obj,
    xml_document: Obj,
    text: Obj,
    comment: Obj,
    doctype: Obj,
    pi: Obj,
    fragment: Obj,
    class_list: Obj,
    style: Obj,
    event: Obj,
    /// NodeList.prototype: lists are arrays underneath, with `item()` on top.
    node_list: Obj,
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
    /// Where the caret is in the focused field (characters from the start).
    pub caret: usize,
    /// The element under the pointer.
    pub hover: Option<NodeId>,
    /// The `:hover` selectors of the page's style sheets, and what they were read from.
    hover_rules: Option<(usize, Rc<Vec<css::Selector>>)>,
    /// A `<select>` that was clicked: the app shows its menu.
    menu: Option<NodeId>,
    /// Decoded canvas drawings, by canvas: (the JS list they came from, its length, the drawing).
    canvases: HashMap<NodeId, (usize, usize, std::sync::Arc<crate::canvas::Canvas>)>,
    /// The page moved to a `#fragment` (a link, `location.hash`): scroll there.
    pub scroll_to_anchor: bool,
    pub navigate: Option<Navigation>,
    /// Cookies when the host keeps none (tests, previews).
    cookies: volant_core::cookies::Jar,
    current_script: Option<NodeId>,
    /// Scripts already run (or scheduled), so moving one doesn't run it again.
    ran: std::collections::HashSet<NodeId>,
    host: Rc<dyn Host>,
    loader: Rc<PageLoader>,
    protos: Option<Protos>,
    /// Bumped on every change to the document (so the app knows to lay it out again).
    pub generation: u64,
    computed: Option<Computed>,
    /// The document's base URL, for the generation and URL it was worked out at.
    base_cache: RefCell<Option<(u64, String, String)>>,
    /// The document each node made by (or adopted into) a document other than
    /// the page's belongs to, where its tree doesn't say.
    owners: HashMap<NodeId, NodeId>,
    /// The URLs of documents other than the page's (those frames loaded).
    doc_urls: HashMap<NodeId, String>,
    /// Documents other than the page's, and their content types.
    documents: HashMap<NodeId, String>,
}

/// The document a node belongs to: the document at the root of its tree, or
/// (out of the tree) the one that made or last adopted it.
fn owner_doc(s: &State, id: NodeId) -> NodeId {
    let mut r = id;
    loop {
        let p = s.dom.nodes[r as usize].parent;
        if p == r {
            break;
        }
        r = p;
    }
    if matches!(s.dom.nodes[r as usize].data, Data::Document) {
        return r;
    }
    s.owners.get(&r).or_else(|| s.owners.get(&id)).copied().unwrap_or(ROOT)
}

/// Moves a subtree into `doc` (as inserting it into another document's tree does).
fn adopt(st: &S, doc: NodeId, n: NodeId) {
    let mut s = st.borrow_mut();
    let doc = owner_doc(&s, doc);
    let mut all = descendants(&s.dom, n);
    all.push(n);
    for k in all {
        if doc == ROOT {
            s.owners.remove(&k);
        } else {
            s.owners.insert(k, doc);
        }
    }
}

/// Records that `doc` made node `n` (nothing to record for the page's own document).
fn made_by(st: &S, doc: NodeId, n: NodeId) {
    let doc = owner_doc(&st.borrow(), doc);
    if doc != ROOT {
        st.borrow_mut().owners.insert(n, doc);
    }
}

/// The document element: the document's first element child.
fn document_element(dom: &Dom, doc: NodeId) -> Option<NodeId> {
    dom.children(doc).iter().copied().find(|&c| dom.tag(c).is_some_and(|t| !t.starts_with('#')))
}

/// The document element's first child that is one of `tags`.
fn html_child(dom: &Dom, doc: NodeId, tags: &[&str]) -> Option<NodeId> {
    let html = document_element(dom, doc).filter(|&h| dom.tag(h) == Some("html"))?;
    dom.children(html).iter().copied().find(|&c| dom.tag(c).is_some_and(|t| tags.contains(&t)))
}

/// What `getComputedStyle` keeps between calls: the parsed sheets (until
/// they change) and the styles worked out since the document last changed.
struct Computed {
    generation: u64,
    sheets: u64,
    cascade: Rc<style::Cascade>,
    memo: HashMap<NodeId, style::Style>,
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
    if let Some(u) = volant_core::url::resolve(base, href) {
        return u;
    }
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

pub const HTML_NS: &str = "http://www.w3.org/1999/xhtml";
const SVG_NS: &str = "http://www.w3.org/2000/svg";
const MATHML_NS: &str = "http://www.w3.org/1998/Math/MathML";
const XML_NS: &str = "http://www.w3.org/XML/1998/namespace";
const XMLNS_NS: &str = "http://www.w3.org/2000/xmlns/";

/// An element's namespace and prefix.
fn ns_of(dom: &Dom, id: NodeId) -> (Option<&str>, Option<&str>) {
    match dom.ns.get(&id) {
        Some((n, p)) => (n.as_deref(), p.as_deref()),
        None => match dom.tag(id) {
            Some("svg") => (Some(SVG_NS), None),
            Some("math") => (Some(MATHML_NS), None),
            _ => (Some(HTML_NS), None),
        },
    }
}

/// Whether the document `id` belongs to is an HTML document (not XML).
fn in_html_doc(s: &State, id: NodeId) -> bool {
    s.documents.get(&owner_doc(s, id)).is_none_or(|t| t == "text/html")
}

/// An element's `tagName`: its qualified name, upper-cased for HTML elements in HTML documents.
fn tag_name(s: &State, id: NodeId) -> String {
    let name = tag_of(&s.dom, id);
    if ns_of(&s.dom, id).0 == Some(HTML_NS) && in_html_doc(s, id) {
        name.to_ascii_uppercase()
    } else {
        name
    }
}

/// Validates and splits a qualified name for `namespace`, as `createElementNS` does.
fn validate_and_extract(namespace: Option<&str>, qname: &str) -> Result<(Option<String>, String), (&'static str, &'static str)> {
    let (prefix, local) = match qname.split_once(':') {
        Some((p, l)) => (Some(p.to_string()), l.to_string()),
        None => (None, qname.to_string()),
    };
    if prefix.as_deref().is_some_and(|p| p.is_empty() || !no_ws_or(p, &['/', '>'])) || !is_element_local_name(&local) {
        return Err(("InvalidCharacterError", "not a valid qualified name"));
    }
    let bad = (prefix.is_some() && namespace.is_none())
        || (prefix.as_deref() == Some("xml") && namespace != Some(XML_NS))
        || ((qname == "xmlns" || prefix.as_deref() == Some("xmlns")) && namespace != Some(XMLNS_NS))
        || (namespace == Some(XMLNS_NS) && qname != "xmlns" && prefix.as_deref() != Some("xmlns"));
    if bad {
        return Err(("NamespaceError", "the name doesn't fit the namespace"));
    }
    Ok((prefix, local))
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
    if let Some(list) = dom.attr_ns.get_mut(&id) {
        list.retain(|(a, _, _)| &**a != k);
    }
}

/// An attribute's (namespace, prefix, local name), from its qualified name.
fn attr_name_parts<'a>(dom: &'a Dom, id: NodeId, k: &'a str) -> (Option<&'a str>, Option<&'a str>, &'a str) {
    match dom.attr_ns.get(&id).and_then(|l| l.iter().find(|(a, _, _)| &**a == k)) {
        Some((_, ns, Some(p))) => (Some(&**ns), Some(&**p), k.strip_prefix(&**p).and_then(|r| r.strip_prefix(':')).unwrap_or(k)),
        Some((_, ns, None)) => (Some(&**ns), None, k),
        None => (None, None, k),
    }
}

/// The qualified name of `id`'s attribute in `ns` named `local`, if it has one.
fn attr_by_ns(dom: &Dom, id: NodeId, ns: Option<&str>, local: &str) -> Option<String> {
    let Data::Element { attrs, .. } = &dom.nodes[id as usize].data else { return None };
    attrs.iter().map(|(k, _)| &**k).find(|k| {
        let (n, _, l) = attr_name_parts(dom, id, k);
        n == ns && l == local
    }).map(str::to_string)
}

/// `setAttribute` and friends lower-case names only on HTML elements in HTML documents.
fn attr_key(s: &State, id: NodeId, k: std::rc::Rc<str>) -> String {
    let html = match s.dom.ns.get(&id) {
        None => true,
        Some((ns, _)) => ns.as_deref() == Some(HTML_NS) && in_html_doc(s, id),
    };
    if html { k.to_ascii_lowercase() } else { k.to_string() }
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
/// DOM's "ensure pre-insert validity" (and the replace variant, when `replacing`):
/// the DOMException name and message for an insertion the DOM can't make.
fn check_insert(dom: &Dom, parent: NodeId, node: NodeId, before: Option<NodeId>, replacing: bool) -> Result<(), (&'static str, &'static str)> {
    let kind = |id: NodeId| match &dom.nodes[id as usize].data {
        Data::Document => 'D',
        Data::Text(_) => 'T',
        Data::Comment(_) | Data::Pi { .. } => 'C',
        Data::Doctype { .. } => 'Y',
        Data::Element { name, .. } if &**name == FRAGMENT => 'F',
        Data::Element { .. } => 'E',
    };
    let hierarchy = |m| Err(("HierarchyRequestError", m));
    let pk = kind(parent);
    if matches!(pk, 'T' | 'C' | 'Y') {
        return hierarchy("this node can't have children");
    }
    if is_inclusive_ancestor(dom, node, parent) {
        return hierarchy("the new child contains the parent");
    }
    if let Some(b) = before {
        if dom.nodes[b as usize].parent != parent || b == parent {
            return Err(("NotFoundError", if replacing { "the node to replace isn't a child of this node" } else { "the node to insert before isn't a child of this node" }));
        }
    }
    let nk = kind(node);
    if nk == 'D' {
        return hierarchy("a document can't be inserted");
    }
    if nk == 'Y' && pk != 'D' {
        return hierarchy("a doctype can only be in a document");
    }
    if pk == 'D' {
        let elements = |id: NodeId| dom.children(id).iter().filter(|&&c| kind(c) == 'E').count();
        // Element children of the document other than the one being replaced.
        let others = dom.children(parent).iter().filter(|&&c| kind(c) == 'E' && !(replacing && Some(c) == before)).count();
        let kids = dom.children(parent);
        let at = before.and_then(|b| kids.iter().position(|&k| k == b));
        // Is there a doctype at or after the reference child (skipping it when it's being replaced)?
        let doctype_after = at.map(|p| kids[p + replacing as usize..].iter().any(|&k| kind(k) == 'Y')).unwrap_or(false);
        let element_before = match at {
            Some(p) => kids[..p].iter().any(|&k| kind(k) == 'E'),
            None => others > 0,
        };
        match nk {
            'T' => return hierarchy("a document can't hold text"),
            'F' => {
                if dom.children(node).iter().any(|&c| kind(c) == 'T') || elements(node) > 1 {
                    return hierarchy("a document can only have one element");
                }
                if elements(node) == 1 && (others > 0 || doctype_after) {
                    return hierarchy("the document already has an element");
                }
            }
            'E' if others > 0 || doctype_after => return hierarchy("the document already has an element"),
            'Y' => {
                if kids.iter().any(|&k| kind(k) == 'Y' && !(replacing && Some(k) == before)) || element_before {
                    return hierarchy("the document can't take a doctype there");
                }
            }
            _ => {}
        }
    }
    Ok(())
}

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
    match &mut dom.nodes[id as usize].data {
        Data::Text(t) | Data::Comment(t) | Data::Pi { data: t, .. } => {
            *t = text.to_string();
            return;
        }
        Data::Doctype { .. } => return,
        _ => {}
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
        other => other.clone(),
    };
    let copy = new_node(dom, data);
    if let Some(ns) = dom.ns.get(&id).cloned() {
        dom.ns.insert(copy, ns);
    }
    if let Some(a) = dom.attr_ns.get(&id).cloned() {
        dom.attr_ns.insert(copy, a);
    }
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
            other => other.clone(),
        };
        let id = new_node(dom, data);
        if let Some(ns) = src.ns.get(&sid) {
            dom.ns.insert(id, ns.clone());
        }
        if let Some(a) = src.attr_ns.get(&sid) {
            dom.attr_ns.insert(id, a.clone());
        }
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

/// Makes sure there's an `html` with a `head` and a `body`, as browsers always build.
fn ensure_structure(dom: &mut Dom) {
    let el = |name: &str| Data::Element { name: name.into(), attrs: Vec::new() };
    let html = match dom.find("html") {
        Some(h) => h,
        None => {
            let h = new_node(dom, el("html"));
            // The doctype (and comments before the first element) stay with the document.
            let kids = dom.children(ROOT).to_vec();
            let lead = kids.iter().take_while(|&&c| matches!(dom.nodes[c as usize].data, Data::Doctype { .. } | Data::Comment(_))).count();
            for &c in &kids[lead..] {
                let _ = insert(dom, h, c, None);
            }
            let _ = insert(dom, ROOT, h, None);
            h
        }
    };
    let head = match dom.find("head") {
        Some(h) => h,
        None => {
            let h = new_node(dom, el("head"));
            let first = dom.children(html).first().copied();
            let _ = insert(dom, html, h, first);
            h
        }
    };
    if dom.find("body").is_none() {
        let b = new_node(dom, el("body"));
        let head_like = |d: &Dom, n: NodeId| matches!(d.tag(n), Some("title" | "meta" | "link" | "base")) || matches!(&d.nodes[n as usize].data, Data::Text(t) if t.trim().is_empty());
        for c in dom.children(html).to_vec() {
            if c == head {
                continue;
            }
            if head_like(dom, c) {
                let _ = insert(dom, head, c, None);
            } else {
                let _ = insert(dom, b, c, None);
            }
        }
        let _ = insert(dom, html, b, None);
    }
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
        Data::Comment(t) => {
            out.push_str("<!--");
            out.push_str(t);
            out.push_str("-->");
        }
        Data::Doctype { name, .. } => {
            out.push_str("<!DOCTYPE ");
            out.push_str(name);
            out.push('>');
        }
        Data::Pi { target, data } => {
            out.push_str("<?");
            out.push_str(target);
            out.push(' ');
            out.push_str(data);
            out.push('>');
        }
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

/// A document's URL: the page's, a frame's, or about:blank.
fn doc_url(s: &State, doc: NodeId) -> String {
    if doc == ROOT {
        s.url.clone()
    } else {
        s.doc_urls.get(&doc).cloned().unwrap_or_else(|| "about:blank".into())
    }
}

/// The document's base URL: its first `<base href>`, resolved against its URL.
pub fn base_url(dom: &Dom, url: &str) -> String {
    base_url_in(dom, ROOT, url)
}

fn base_url_in(dom: &Dom, doc: NodeId, url: &str) -> String {
    let mut stack: Vec<NodeId> = dom.children(doc).iter().rev().copied().collect();
    while let Some(n) = stack.pop() {
        if dom.tag(n) == Some("base") {
            if let Some(h) = dom.attr(n, "href") {
                return volant_core::url::resolve(url, h).unwrap_or_else(|| url.to_string());
            }
        }
        stack.extend(dom.children(n).iter().rev());
    }
    url.to_string()
}

impl State {
    /// [`base_url`] for the page, worked out again only after a change.
    fn base(&self) -> String {
        let mut c = self.base_cache.borrow_mut();
        if let Some((g, u, b)) = &*c {
            if *g == self.generation && *u == self.url {
                return b.clone();
            }
        }
        let b = base_url(&self.dom, &self.url);
        *c = Some((self.generation, self.url.clone(), b.clone()));
        b
    }
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

/// The prototype a node's wrapper starts with: its interface's, as in browsers.
fn proto_for(st: &S, id: NodeId) -> Obj {
    let s = st.borrow();
    let p = s.protos.as_ref().expect("protos");
    match &s.dom.nodes[id as usize].data {
        Data::Document if s.documents.get(&id).is_some_and(|t| t != "text/html") => p.xml_document.clone(),
        Data::Document => p.html_document.clone(),
        Data::Text(_) => p.text.clone(),
        Data::Comment(_) => p.comment.clone(),
        Data::Doctype { .. } => p.doctype.clone(),
        Data::Pi { .. } => p.pi.clone(),
        Data::Element { name, .. } if &**name == FRAGMENT => p.fragment.clone(),
        Data::Element { name, .. } if name.starts_with('#') => p.node.clone(),
        Data::Element { name, .. } => {
            let (ns, prefix) = ns_of(&s.dom, id);
            let local = if prefix.is_some() { name.split_once(':').map_or(&**name, |(_, l)| l) } else { name };
            match ns {
                // Unknown HTML elements are HTMLUnknownElements, unless they could be custom elements.
                Some(HTML_NS) => p.by_tag.get(local).unwrap_or(if local.contains('-') { &p.html_element } else { &p.unknown }).clone(),
                Some(SVG_NS) => p.svg_tags.get(local).unwrap_or(&p.svg).clone(),
                Some(MATHML_NS) => p.mathml.clone(),
                _ => p.element.clone(),
            }
        }
    }
}

/// The wrapper object for a node (made on first use).
fn wrap(i: &Interp, st: &S, id: NodeId) -> Value {
    if let Some(v) = st.borrow().wrappers.get(&id) {
        return v.clone();
    }
    let proto = proto_for(st, id);
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
    if let (Value::Obj(o), Some(p)) = (&arr, st.borrow().protos.as_ref().map(|p| p.node_list.clone())) {
        o.borrow_mut().proto = Some(p);
    }
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
        let o = o.borrow();
        for k in [key.clone(), format!("\0{key}")] {
            if let Some(Slot::Data(f)) = o.get_own(&k) {
                if Interp::is_callable(f) {
                    return Some(f.clone());
                }
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

// ------------------------------------------------------------------ scripts

#[derive(Clone, Copy, PartialEq)]
enum ScriptKind {
    Classic,
    Module,
    ImportMap,
}

/// What a `<script>` holds, if it's something to run.
fn script_kind(dom: &Dom, id: NodeId) -> Option<ScriptKind> {
    let ty = dom.attr(id, "type").unwrap_or("").trim().to_ascii_lowercase();
    match ty.as_str() {
        "module" => Some(ScriptKind::Module),
        "importmap" => Some(ScriptKind::ImportMap),
        // A `nomodule` script is the fallback for browsers without modules.
        _ if dom.attr(id, "nomodule").is_some() => None,
        t if t.is_empty() || t.contains("javascript") || t.contains("ecmascript") => Some(ScriptKind::Classic),
        _ => None,
    }
}

fn is_js(dom: &Dom, id: NodeId) -> bool {
    script_kind(dom, id).is_some()
}

/// A script's source and where it came from: inline, or fetched from its `src`.
fn script_source(st: &S, id: NodeId) -> Result<(String, String), String> {
    let (src, text, url, host) = {
        let s = st.borrow();
        (s.dom.attr(id, "src").map(|x| resolve_url(&s.base(), x)), s.dom.text(id), s.url.clone(), s.host.clone())
    };
    match src {
        Some(abs) => match host.get_for(&abs, &url) {
            Some(r) if r.success() => Ok((String::from_utf8_lossy(&r.body).into_owned(), abs)),
            _ => Err(format!("couldn't load {abs}")),
        },
        None => Ok((text, url)),
    }
}

/// Runs one script element now (fetching it if needed), then fires load or error at it.
fn execute(i: &mut Interp, st: &S, id: NodeId) {
    let (kind, external) = {
        let s = st.borrow();
        (script_kind(&s.dom, id), s.dom.attr(id, "src").is_some())
    };
    let Some(kind) = kind else { return };
    let ok = match script_source(st, id) {
        Ok((code, origin)) => {
            let r = match kind {
                ScriptKind::ImportMap => {
                    st.borrow().loader.add_map(&code, &origin);
                    return;
                }
                ScriptKind::Module => {
                    // An inline module is its own module, at the document's URL.
                    let key = if external { origin.clone() } else { format!("{origin}#inline-module-{id}") };
                    i.run_module(&code, &key)
                }
                ScriptKind::Classic => {
                    let prev = st.borrow_mut().current_script.replace(id);
                    let r = i.run(&code).map(|_| ());
                    st.borrow_mut().current_script = prev;
                    r
                }
            };
            if let Err(e) = r {
                let place = if external { origin } else { format!("inline script in {origin}") };
                i.errors.push(format!("{place}: {e}"));
            }
            true
        }
        Err(e) => {
            i.errors.push(e);
            false
        }
    };
    fire(i, st, id, if ok { "load" } else { "error" }, false);
}

/// Loads a page's modules over its network, after its import maps.
pub struct PageLoader {
    host: Rc<dyn Host>,
    /// (specifier or prefix ending in '/', absolute URL), from `<script type="importmap">`.
    imports: RefCell<Vec<(String, String)>>,
}

impl PageLoader {
    fn add_map(&self, json: &str, base: &str) {
        let Some(map) = volant_core::json::parse(json) else { return };
        if let Some(volant_core::json::Value::Obj(entries)) = map.get("imports") {
            let mut imports = self.imports.borrow_mut();
            for (k, v) in entries {
                if let Some(v) = v.as_str() {
                    imports.push((k.clone(), resolve_url(base, v)));
                }
            }
        }
    }
}

impl volant_js::module::Loader for PageLoader {
    fn resolve(&self, spec: &str, base: &str) -> String {
        let imports = self.imports.borrow();
        if let Some((_, target)) = imports.iter().find(|(k, _)| k == spec) {
            return target.clone();
        }
        // The longest matching prefix ("lib/" → "https://cdn/lib/").
        let prefix = imports.iter().filter(|(k, _)| k.ends_with('/') && spec.starts_with(k.as_str())).max_by_key(|(k, _)| k.len());
        if let Some((k, target)) = prefix {
            return format!("{target}{}", &spec[k.len()..]);
        }
        let relative = spec.starts_with('/') || spec.starts_with("./") || spec.starts_with("../");
        if relative || spec.contains("://") {
            resolve_url(base, spec)
        } else {
            format!("bare:{spec}") // a bare name no import map maps: it fails to load
        }
    }
    fn load(&self, url: &str) -> Result<String, String> {
        if url.starts_with("bare:") {
            return Err("bare module names need an import map".into());
        }
        match self.host.get(url) {
            Some(r) if r.success() => Ok(String::from_utf8_lossy(&r.body).into_owned()),
            Some(r) => Err(format!("HTTP {}", r.status)),
            None => Err("network error".into()),
        }
    }
}

/// Scripts that just entered the document run soon after (as browsers do for inserted scripts).
fn inserted(i: &mut Interp, st: &S, node: NodeId) {
    let new: Vec<NodeId> = {
        let s = st.borrow();
        if !connected(&s.dom, node) {
            return;
        }
        let mut all = vec![node];
        all.extend(descendants(&s.dom, node));
        all.into_iter().filter(|&n| matches!(s.dom.tag(n), Some("script" | "iframe")) && !s.ran.contains(&n)).collect()
    };
    // Frames get their documents as they're connected.
    let (frames, new): (Vec<NodeId>, Vec<NodeId>) = new.into_iter().partition(|&n| st.borrow().dom.tag(n) == Some("iframe"));
    let new: Vec<NodeId> = new.into_iter().filter(|&n| is_js(&st.borrow().dom, n)).collect();
    if !frames.is_empty() {
        let hook = i.global_obj.borrow().get_own("__frameInserted").cloned();
        if let Some(Slot::Data(hook)) = hook {
            for f in frames {
                let el = wrap(i, st, f);
                let _ = i.call(&hook, Value::Undefined, vec![el]);
            }
        }
    }
    // Their files all start downloading now, side by side, as browsers do; each
    // script runs when its turn comes (an inserted script is async).
    let (urls, page, host) = {
        let s = st.borrow();
        let urls: Vec<String> = new.iter().filter_map(|&n| s.dom.attr(n, "src")).map(|x| resolve_url(&s.base(), x)).filter(|u| u.starts_with("http://") || u.starts_with("https://")).collect();
        (urls, s.url.clone(), s.host.clone())
    };
    if !urls.is_empty() {
        host.prefetch(urls, &page);
    }
    for n in new {
        let empty = {
            let s = st.borrow();
            s.dom.attr(n, "src").is_none() && s.dom.text(n).trim().is_empty()
        };
        if empty {
            continue; // its text may come later
        }
        st.borrow_mut().ran.insert(n);
        let st2 = st.clone();
        let job = i.native(move |i, _t, _a| {
            execute(i, &st2, n);
            Ok(Value::Undefined)
        });
        i.add_timer(job, 0.0, Vec::new(), false);
    }
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

/// Submits a form: navigates to its action, GET with the fields as the query or
/// POST with them as the body (url-encoded, multipart or plain text).
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
    // The submit button can override the form's action, method and encoding.
    let attr = |name: &str| {
        let over = submitter.and_then(|b| dom.attr(b, &format!("form{name}"))).filter(|v| !v.trim().is_empty());
        over.or_else(|| dom.attr(form, name)).map(|v| v.trim().to_string())
    };
    let action = attr("action").filter(|a| !a.is_empty()).map(|a| resolve_url(&s.base(), &a)).unwrap_or_else(|| s.url.clone());
    let method = attr("method").map(|m| m.to_ascii_uppercase()).filter(|m| m == "POST" || m == "DIALOG").unwrap_or_else(|| "GET".into());
    if method == "DIALOG" {
        return;
    }
    let query = form_urlencode(&pairs);
    s.navigate = Some(if method == "POST" {
        let enctype = attr("enctype").unwrap_or_default().to_ascii_lowercase();
        if enctype == "multipart/form-data" {
            let boundary = format!("----VolantFormBoundary{:x}", s.generation.wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ pairs.len() as u64);
            let mut body = String::new();
            for (k, v) in &pairs {
                body.push_str(&format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{}\"\r\n\r\n{v}\r\n", k.replace('"', "%22")));
            }
            body.push_str(&format!("--{boundary}--\r\n"));
            Navigation { url: action, method, body: Some(body.into_bytes()), content_type: Some(format!("multipart/form-data; boundary={boundary}")), referrer: None }
        } else if enctype == "text/plain" {
            let body: String = pairs.iter().map(|(k, v)| format!("{k}={v}\r\n")).collect();
            Navigation { url: action, method, body: Some(body.into_bytes()), content_type: Some("text/plain".into()), referrer: None }
        } else {
            Navigation { url: action, method, body: Some(query.into_bytes()), content_type: Some("application/x-www-form-urlencoded".into()), referrer: None }
        }
    } else {
        // A GET form replaces the action's query; its fragment goes.
        let base = action.split(['?', '#']).next().unwrap_or(&action).to_string();
        Navigation::get(format!("{base}?{query}"))
    });
}

/// The headers a browser adds to a page's request: `Referer`, and `Origin` when it isn't a GET.
pub fn request_context(headers: &mut Vec<(String, String)>, page: &str, method: &str) {
    let has = |h: &[(String, String)], n: &str| h.iter().any(|(k, _)| k.eq_ignore_ascii_case(n));
    if !page.starts_with("http") {
        return;
    }
    if !has(headers, "referer") {
        headers.push(("Referer".into(), page.split('#').next().unwrap_or(page).to_string()));
    }
    if method != "GET" && method != "HEAD" && !has(headers, "origin") {
        if let Some(u) = volant_core::http::Url::parse(page) {
            headers.push(("Origin".into(), u.origin()));
        }
    }
}

/// The origin of a web address (`https://example.com:8443`), or `null`.
pub fn origin_of(url: &str) -> String {
    volant_core::http::Url::parse(url).map(|u| u.origin()).unwrap_or_else(|| "null".into())
}

/// Request headers a page may send anywhere without asking first (CORS "safelisted").
fn safelisted(name: &str, value: &str) -> bool {
    match name.to_ascii_lowercase().as_str() {
        "accept" | "accept-language" | "content-language" => value.len() <= 128,
        "content-type" => {
            let mime = value.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
            matches!(mime.as_str(), "application/x-www-form-urlencoded" | "multipart/form-data" | "text/plain")
        }
        _ => false,
    }
}

/// Did the server let `origin` read this answer?
fn cors_allows(headers: &[(String, String)], origin: &str, include: bool) -> bool {
    let h = |n: &str| headers.iter().find(|(k, _)| k == n).map(|(_, v)| v.trim());
    let allowed = match h("access-control-allow-origin") {
        Some("*") => !include,
        Some(o) => o == origin,
        None => false,
    };
    allowed && (!include || h("access-control-allow-credentials") == Some("true"))
}

/// A page's `fetch`/XHR, under the same-origin policy: another origin's answer
/// is only readable if it allows it (CORS, with a preflight when the request
/// isn't a simple one), and cookies go to other origins only with
/// `credentials: 'include'`. `Err` says why it was refused, as the console shows it.
#[allow(clippy::too_many_arguments)]
pub fn cors_fetch(
    host: &dyn Host,
    page: &str,
    method: &str,
    url: &str,
    mut headers: Vec<(String, String)>,
    body: Option<&[u8]>,
    credentials: &str,
    mode: &str,
) -> Result<Reply, String> {
    let fail = || format!("TypeError: Failed to fetch {url}");
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(fail());
    }
    // Pages can't set the headers that carry the browser's own word.
    headers.retain(|(k, _)| {
        let k = k.to_ascii_lowercase();
        !(matches!(k.as_str(), "cookie" | "cookie2" | "host" | "origin" | "referer" | "connection" | "content-length" | "transfer-encoding" | "keep-alive" | "te" | "trailer" | "upgrade" | "via" | "expect" | "date" | "dnt") || k.starts_with("proxy-") || k.starts_with("sec-"))
    });
    let origin = origin_of(page);
    let same = origin_of(url) == origin;
    let include = credentials == "include";
    let cookies = match credentials {
        "omit" => false,
        "include" => true,
        _ => same,
    };
    let simple_method = matches!(method, "GET" | "HEAD" | "POST");
    if !same {
        match mode {
            "same-origin" => return Err(format!("Blocked: {url} isn't on this page's origin (mode: same-origin)")),
            "no-cors" => {
                // Sent, but nothing about the answer reaches the page.
                if !simple_method {
                    return Err(fail());
                }
                headers.retain(|(k, v)| safelisted(k, v));
                request_context(&mut headers, page, method);
                let req = Request { method, url, headers: &headers, body, credentials: cookies, initiator: Some(page), navigation: false };
                host.request(&req).ok_or_else(fail)?;
                return Ok(Reply::ok(0, "", Vec::new()));
            }
            _ => {}
        }
        // Anything beyond a simple request needs the server's permission first.
        let mut extra: Vec<String> = headers.iter().filter(|(k, v)| !safelisted(k, v)).map(|(k, _)| k.to_ascii_lowercase()).collect();
        extra.sort();
        extra.dedup();
        if !simple_method || !extra.is_empty() {
            let mut pre = vec![("Origin".to_string(), origin.clone()), ("Access-Control-Request-Method".to_string(), method.to_string())];
            if !extra.is_empty() {
                pre.push(("Access-Control-Request-Headers".into(), extra.join(",")));
            }
            request_context(&mut pre, page, "OPTIONS");
            let req = Request { method: "OPTIONS", url, headers: &pre, body: None, credentials: false, initiator: Some(page), navigation: false };
            let r = host.request(&req).ok_or_else(fail)?;
            let list = |n: &str| -> Vec<String> {
                r.headers.iter().filter(|(k, _)| k == n).flat_map(|(_, v)| v.split(',').map(|x| x.trim().to_ascii_lowercase()).collect::<Vec<_>>()).collect()
            };
            let methods = list("access-control-allow-methods");
            let allowed_headers = list("access-control-allow-headers");
            let ok = r.success()
                && cors_allows(&r.headers, &origin, include)
                && (simple_method || methods.iter().any(|m| m.eq_ignore_ascii_case(method)) || (!include && methods.iter().any(|m| m == "*")))
                && extra.iter().all(|h| allowed_headers.contains(h) || (!include && h != "authorization" && allowed_headers.iter().any(|a| a == "*")));
            if !ok {
                return Err(format!("Blocked by CORS: {origin} may not {method} {url} (the server's preflight didn't allow it)"));
            }
        }
        headers.push(("Origin".into(), origin.clone()));
    }
    request_context(&mut headers, page, method);
    let req = Request { method, url, headers: &headers, body, credentials: cookies, initiator: Some(page), navigation: false };
    let mut r = host.request(&req).ok_or_else(fail)?;
    // Where it ended up (a same-origin request may have been redirected away).
    let landed = if r.url.is_empty() { url.to_string() } else { r.url.clone() };
    if origin_of(&landed) != origin {
        if !cors_allows(&r.headers, &origin, include) {
            return Err(format!("Blocked by CORS: {landed} didn't allow {origin} to read it"));
        }
        // Only the headers the server chose to show.
        let expose: Vec<String> = r.headers.iter().filter(|(k, _)| k == "access-control-expose-headers").flat_map(|(_, v)| v.split(',').map(|x| x.trim().to_ascii_lowercase()).collect::<Vec<_>>()).collect();
        let all = !include && expose.iter().any(|e| e == "*");
        r.headers.retain(|(k, _)| {
            matches!(k.as_str(), "cache-control" | "content-language" | "content-length" | "content-type" | "expires" | "last-modified" | "pragma") || (all && k != "set-cookie") || expose.contains(k)
        });
    }
    Ok(r)
}

/// `application/x-www-form-urlencoded` (spaces as `+`).
fn form_urlencode(pairs: &[(String, String)]) -> String {
    pairs.iter().map(|(k, v)| format!("{}={}", url_encode(k), url_encode(v))).collect::<Vec<_>>().join("&")
}

/// What a click does when no script cancelled it.
fn activate(i: &mut Interp, st: &S, target: NodeId) {
    enum Act {
        Link(String),
        Hash(String),
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
                        None => {
                            let abs = resolve_url(&s.base(), href);
                            // Another place on this page: no new page, just the address and a scroll.
                            let same_doc = abs.split('#').next() == s.url.split('#').next() && href.contains('#');
                            if same_doc { Act::Hash(abs) } else { Act::Link(abs) }
                        }
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
        Act::Link(url) => st.borrow_mut().navigate = Some(Navigation::get(url)),
        Act::Hash(url) => {
            let changed = {
                let mut s = st.borrow_mut();
                let changed = s.url != url;
                s.url = url;
                s.generation += 1; // the app scrolls to the target
                s.scroll_to_anchor = true;
                changed
            };
            if changed {
                fire(i, st, WINDOW, "hashchange", false);
            }
        }
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
            // The app shows the options as a menu (see `Runtime::take_menu`).
            focus(i, st, Some(n));
            if st.borrow().dom.attr(n, "disabled").is_none() {
                st.borrow_mut().menu = Some(n);
            }
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
    {
        let mut s = st.borrow_mut();
        s.focus = n;
        s.caret = n.map(|f| field_value(&s.dom, f).chars().count()).unwrap_or(0);
        s.generation += 1; // the caret and :focus styles move
    }
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
    let character_data = mk(i, Some(node.clone()));
    let text = mk(i, Some(character_data.clone()));
    let comment = mk(i, Some(character_data.clone()));
    let pi = mk(i, Some(character_data.clone()));
    let doctype = mk(i, Some(node.clone()));
    let fragment = mk(i, Some(node.clone()));
    let document = mk(i, Some(node.clone()));
    let class_list = mk(i, Some(i.object_proto.clone()));
    let style_p = mk(i, Some(i.object_proto.clone()));
    let event = mk(i, Some(i.object_proto.clone()));
    let html_element = mk(i, Some(element.clone()));
    let html_document = mk(i, Some(document.clone()));
    let node_list = mk(i, Some(i.array_proto.clone()));
    // Each element interface: (name, its tags, the interface it extends).
    let mut by_tag: HashMap<&'static str, Obj> = HashMap::new();
    let mut svg_tags: HashMap<&'static str, Obj> = HashMap::new();
    let mut ifaces: Vec<(&'static str, Obj)> = Vec::new();
    let media = mk(i, Some(html_element.clone()));
    let svg = mk(i, Some(element.clone()));
    let svg_graphics = mk(i, Some(svg.clone()));
    let mathml = mk(i, Some(element.clone()));
    let unknown = mk(i, Some(html_element.clone()));
    ifaces.extend([("HTMLMediaElement", media.clone()), ("SVGElement", svg.clone()), ("SVGGraphicsElement", svg_graphics.clone()), ("MathMLElement", mathml.clone()), ("HTMLUnknownElement", unknown.clone())]);
    for (name, tags, parent) in ELEMENT_INTERFACES {
        let parent = match *parent {
            "media" => &media,
            "svg" => &svg,
            "svg-graphics" => &svg_graphics,
            "mathml" => &mathml,
            _ => &html_element,
        };
        let is_svg = parent.as_ptr() == svg.as_ptr() || parent.as_ptr() == svg_graphics.as_ptr();
        let proto = mk(i, Some(parent.clone()));
        for t in tags.iter() {
            if is_svg {
                svg_tags.entry(t).or_insert_with(|| proto.clone());
            } else {
                by_tag.entry(t).or_insert_with(|| proto.clone());
            }
        }
        ifaces.push((name, proto));
    }
    // HTML's elements that are plain HTMLElements.
    for t in HTML_PLAIN {
        by_tag.entry(t).or_insert_with(|| html_element.clone());
    }
    let xml_document = mk(i, Some(document.clone()));
    st.borrow_mut().protos = Some(Protos {
        html_element: html_element.clone(),
        by_tag,
        svg_tags,
        element: element.clone(),
        svg: svg.clone(),
        mathml: mathml.clone(),
        unknown: unknown.clone(),
        html_document: html_document.clone(),
        xml_document: xml_document.clone(),
        node: node.clone(),
        text: text.clone(),
        comment: comment.clone(),
        doctype: doctype.clone(),
        pi: pi.clone(),
        fragment: fragment.clone(),
        class_list: class_list.clone(),
        style: style_p.clone(),
        event: event.clone(),
        node_list: node_list.clone(),
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
    ctor(i, "CharacterData", &character_data);
    ctor(i, "Text", &text);
    ctor(i, "Comment", &comment);
    ctor(i, "ProcessingInstruction", &pi);
    ctor(i, "DocumentType", &doctype);
    ctor(i, "DocumentFragment", &fragment);
    ctor(i, "Document", &document);
    ctor(i, "Event", &event);
    #[rustfmt::skip]
    let node_types = [("ELEMENT_NODE", 1.0), ("ATTRIBUTE_NODE", 2.0), ("TEXT_NODE", 3.0), ("CDATA_SECTION_NODE", 4.0), ("ENTITY_REFERENCE_NODE", 5.0),
        ("ENTITY_NODE", 6.0), ("PROCESSING_INSTRUCTION_NODE", 7.0), ("COMMENT_NODE", 8.0), ("DOCUMENT_NODE", 9.0), ("DOCUMENT_TYPE_NODE", 10.0),
        ("DOCUMENT_FRAGMENT_NODE", 11.0), ("NOTATION_NODE", 12.0), ("DOCUMENT_POSITION_DISCONNECTED", 1.0), ("DOCUMENT_POSITION_PRECEDING", 2.0),
        ("DOCUMENT_POSITION_FOLLOWING", 4.0), ("DOCUMENT_POSITION_CONTAINS", 8.0), ("DOCUMENT_POSITION_CONTAINED_BY", 16.0),
        ("DOCUMENT_POSITION_IMPLEMENTATION_SPECIFIC", 32.0)];
    for (k, v) in node_types {
        if let Value::Obj(n) = &node_ctor {
            n.borrow_mut().set_own(k, Value::Num(v));
        }
        node.borrow_mut().set_own(k, Value::Num(v));
    }
    let _ = element_ctor;
    ctor(i, "HTMLElement", &html_element);
    ctor(i, "HTMLDocument", &html_document);
    ctor(i, "XMLDocument", &xml_document);
    ctor(i, "NodeList", &node_list);
    node_list.borrow_mut().set_own("\0@toStringTag", str_val("NodeList"));
    def(i, &node_list, "item", |i, t, a| {
        let n = i.to_number(&arg(&a, 0))?;
        let v = i.get(&t, &(if n.is_finite() && n >= 0.0 { n as usize } else { usize::MAX }).to_string())?;
        Ok(if matches!(v, Value::Undefined) { Value::Null } else { v })
    });
    def(i, &node_list, "namedItem", |i, t, a| {
        let name = i.to_str(&arg(&a, 0))?;
        for el in volant_js::builtins::items(i, &t)? {
            for k in ["id", "name"] {
                if let Value::Str(v) = i.get(&el, k)? {
                    if *v == *name {
                        return Ok(el);
                    }
                }
            }
        }
        Ok(Value::Null)
    });
    for (name, proto) in &ifaces {
        ctor(i, name, proto);
        proto.borrow_mut().set_own("\0@toStringTag", str_val(name));
    }
    for name in ["ShadowRoot"] {
        let f = i.native(|i, _t, _a| i.throw("TypeError", "Illegal constructor"));
        if let Value::Obj(fo) = &f {
            fo.borrow_mut().set_own("prototype", Value::Obj(node.clone()));
            fo.borrow_mut().set_own("name", str_val(name));
        }
        i.global_obj.borrow_mut().set_own(name, f);
    }
    // Event handler properties (`el.onclick = f`), on HTMLElement's prototype as in browsers.
    for ev in EVENT_HANDLERS {
        let key = format!("\0on{ev}");
        let k2 = key.clone();
        let get = i.native(move |_i, t, _a| {
            Ok(match &t {
                Value::Obj(o) => match o.borrow().get_own(&k2) {
                    Some(Slot::Data(v)) => v.clone(),
                    _ => Value::Null,
                },
                _ => Value::Undefined,
            })
        });
        let set = i.native(move |_i, t, a| {
            if let Value::Obj(o) = &t {
                let v = arg(&a, 0);
                o.borrow_mut().set_own(&key, if Interp::is_callable(&v) { v } else { Value::Null });
            }
            Ok(Value::Undefined)
        });
        html_element.borrow_mut().set_slot(&format!("on{ev}"), Slot::Accessor(Some(get.clone()), Some(set.clone())));
        html_document.borrow_mut().set_slot(&format!("on{ev}"), Slot::Accessor(Some(get), Some(set)));
        // On window they're plain properties, so `onload = f` works as a global too.
        let mut g = i.global_obj.borrow_mut();
        if g.get_own(&format!("on{ev}")).is_none() {
            g.set_own(&format!("on{ev}"), Value::Null);
        }
    }
    for ev in ["message", "messageerror", "popstate", "hashchange", "beforeunload", "unload", "pageshow", "pagehide", "online", "offline", "storage", "languagechange", "unhandledrejection", "rejectionhandled", "orientationchange", "devicemotion", "deviceorientation"] {
        let mut g = i.global_obj.borrow_mut();
        if g.get_own(&format!("on{ev}")).is_none() {
            g.set_own(&format!("on{ev}"), Value::Null);
        }
    }

    // Object.prototype.toString tags (libraries tell plain objects from host objects by these).
    for (o, tag) in [(&comment, "Comment"), (&doctype, "DocumentType"), (&pi, "ProcessingInstruction"), (&fragment, "DocumentFragment"), (&character_data, "CharacterData"), (&element, "Element"), (&html_element, "HTMLElement"), (&text, "Text"), (&document, "Document"), (&html_document, "HTMLDocument"), (&node, "Node"), (&event, "Event"), (&class_list, "DOMTokenList"), (&style_p, "CSSStyleDeclaration")] {
        o.borrow_mut().set_own("\0@toStringTag", str_val(tag));
    }
    i.global_obj.borrow_mut().set_own("\0@toStringTag", str_val("Window"));

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
            let keys = io.borrow().own_keys();
            for k in keys {
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
            Data::Comment(_) => 8.0,
            Data::Doctype { .. } => 10.0,
            Data::Pi { .. } => 7.0,
            Data::Element { name, .. } if &**name == FRAGMENT => 11.0,
            Data::Element { .. } => 1.0,
        }))
    });
    getter(i, st, &node, "nodeName", |_i, st, id| {
        let s = st.borrow();
        Ok(str_val(&match &s.dom.nodes[id as usize].data {
            Data::Document => "#document".to_string(),
            Data::Text(_) => "#text".to_string(),
            Data::Comment(_) => "#comment".to_string(),
            Data::Doctype { name, .. } => name.to_string(),
            Data::Pi { target, .. } => target.to_string(),
            Data::Element { name, .. } if name.starts_with('#') => name.to_string(),
            Data::Element { .. } => tag_name(&s, id),
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
                Data::Document | Data::Doctype { .. } => Value::Null,
                Data::Comment(t) | Data::Pi { data: t, .. } => str_val(t),
                _ => str_val(&s.dom.text(id)),
            })
        },
        |i, st, id, v| {
            let t = if matches!(v, Value::Null | Value::Undefined) { "".into() } else { i.to_str(&v)? };
            if matches!(st.borrow().dom.nodes[id as usize].data, Data::Document | Data::Doctype { .. }) {
                return Ok(());
            }
            set_text(&mut st.borrow_mut().dom, id, &t);
            changed(st);
            Ok(())
        },
    );
    for (name, proto) in [("nodeValue", &node), ("data", &character_data)] {
        accessor(
            i,
            st,
            proto,
            name,
            |_i, st, id| {
                let s = st.borrow();
                Ok(match &s.dom.nodes[id as usize].data {
                    Data::Text(t) | Data::Comment(t) | Data::Pi { data: t, .. } => str_val(t),
                    _ => Value::Null,
                })
            },
            |i, st, id, v| {
                // Both are [LegacyNullToEmptyString].
                let t = if matches!(v, Value::Null) { "".into() } else { i.to_str(&v)? };
                set_text(&mut st.borrow_mut().dom, id, &t);
                changed(st);
                Ok(())
            },
        );
    }
    getter(i, st, &text, "wholeText", |_i, st, id| {
        // This text node's run of adjacent text nodes, joined.
        let s = st.borrow();
        let p = s.dom.parent(id);
        let run: Vec<NodeId> = if p == id { vec![id] } else { s.dom.children(p).to_vec() };
        let at = run.iter().position(|&k| k == id).unwrap_or(0);
        let is_text = |k: &NodeId| matches!(s.dom.nodes[*k as usize].data, Data::Text(_));
        let start = run[..at].iter().rposition(|k| !is_text(k)).map(|x| x + 1).unwrap_or(0);
        let end = run[at..].iter().position(|k| !is_text(k)).map(|x| x + at).unwrap_or(run.len());
        let mut out = String::new();
        for k in &run[start..end] {
            if let Data::Text(t) = &s.dom.nodes[*k as usize].data {
                out.push_str(t);
            }
        }
        Ok(str_val(&out))
    });
    getter(i, st, &character_data, "length", |_i, st, id| {
        let s = st.borrow();
        Ok(match &s.dom.nodes[id as usize].data {
            Data::Text(t) | Data::Comment(t) | Data::Pi { data: t, .. } => Value::Num(t.encode_utf16().count() as f64),
            _ => Value::Undefined,
        })
    });
    getter(i, st, &pi, "target", |_i, st, id| {
        Ok(match &st.borrow().dom.nodes[id as usize].data {
            Data::Pi { target, .. } => str_val(target),
            _ => Value::Undefined,
        })
    });
    for (k, which) in [("name", 0), ("publicId", 1), ("systemId", 2)] {
        getter(i, st, &doctype, k, move |_i, st, id| {
            Ok(match &st.borrow().dom.nodes[id as usize].data {
                Data::Doctype { name, public_id, system_id } => str_val([&**name, public_id.as_str(), system_id.as_str()][which]),
                _ => Value::Undefined,
            })
        });
    }
    getter(i, st, &node, "ownerDocument", |i, st, id| {
        if matches!(st.borrow().dom.nodes[id as usize].data, Data::Document) {
            return Ok(Value::Null);
        }
        let d = owner_doc(&st.borrow(), id);
        Ok(wrap(i, st, d))
    });
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
    fn insert_js(i: &mut Interp, st: &S, parent: NodeId, child: &Value, before: Option<NodeId>, replacing: bool) -> Res<Value> {
        let Some(c) = node_of(child) else { return i.throw("TypeError", "the argument is not a Node") };
        if let Err((name, msg)) = check_insert(&st.borrow().dom, parent, c, before, replacing) {
            return i.throw(name, msg);
        }
        let is_frag = st.borrow().dom.tag(c) == Some(FRAGMENT);
        let moved: Vec<NodeId> = if is_frag { st.borrow().dom.children(c).to_vec() } else { vec![c] };
        let r = insert(&mut st.borrow_mut().dom, parent, c, before);
        if let Err(e) = r {
            return i.throw("HierarchyRequestError", e);
        }
        changed(st);
        // Into another document's tree: the nodes are that document's now.
        let other_docs = !st.borrow().owners.is_empty() || owner_doc(&st.borrow(), parent) != ROOT;
        if other_docs {
            for &n in &moved {
                adopt(st, parent, n);
            }
        }
        for n in moved {
            inserted(i, st, n);
        }
        Ok(child.clone())
    }
    method(i, st, &node, "appendChild", |i, st, id, a| insert_js(i, st, id, &arg(&a, 0), None, false));
    method(i, st, &node, "insertBefore", |i, st, id, a| {
        if a.len() < 2 {
            return i.throw("TypeError", "insertBefore needs 2 arguments");
        }
        let before = node_of(&arg(&a, 1));
        if before.is_none() && !matches!(arg(&a, 1), Value::Null | Value::Undefined) {
            return i.throw("TypeError", "the second argument is not a Node");
        }
        // Inserting a node before itself inserts it before its next sibling.
        let before = match (before, node_of(&arg(&a, 0))) {
            (Some(b), Some(n)) if b == n => {
                let d = &st.borrow().dom;
                let kids = d.children(id);
                kids.iter().position(|&k| k == b).and_then(|p| kids.get(p + 1).copied())
            }
            (b, _) => b,
        };
        insert_js(i, st, id, &arg(&a, 0), before, false)
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
        let Some(n) = node_of(&new) else { return i.throw("TypeError", "the argument is not a Node") };
        if let Err((name, msg)) = check_insert(&st.borrow().dom, id, n, Some(o), true) {
            return i.throw(name, msg);
        }
        if n == o {
            return Ok(old);
        }
        insert_js(i, st, id, &new, Some(o), true)?;
        detach(&mut st.borrow_mut().dom, o);
        changed(st);
        Ok(old)
    });
    method(i, st, &node, "cloneNode", |i, st, id, a| {
        let deep = Interp::truthy(&arg(&a, 0));
        let c = clone_node(&mut st.borrow_mut().dom, id, deep);
        let is_doc = matches!(st.borrow().dom.nodes[id as usize].data, Data::Document);
        if is_doc {
            let ty = st.borrow().documents.get(&id).cloned().unwrap_or_else(|| "text/html".into());
            st.borrow_mut().documents.insert(c, ty);
        } else {
            let doc = owner_doc(&st.borrow(), id);
            adopt(st, doc, c);
        }
        Ok(wrap(i, st, c))
    });
    // Joins adjacent text nodes and drops empty ones, all the way down.
    method(i, st, &node, "normalize", |_i, st, id, _a| {
        fn norm(dom: &mut Dom, id: NodeId) {
            let kids = dom.children(id).to_vec();
            let mut prev_text: Option<NodeId> = None;
            for k in kids {
                let text = match &dom.nodes[k as usize].data {
                    Data::Text(t) => Some(t.clone()),
                    _ => None,
                };
                match (text, prev_text) {
                    (Some(t), _) if t.is_empty() => detach(dom, k),
                    (Some(t), Some(p)) => {
                        if let Data::Text(pt) = &mut dom.nodes[p as usize].data {
                            pt.push_str(&t);
                        }
                        detach(dom, k);
                    }
                    (Some(_), None) => prev_text = Some(k),
                    (None, _) => {
                        prev_text = None;
                        norm(dom, k);
                    }
                }
            }
        }
        norm(&mut st.borrow_mut().dom, id);
        changed(st);
        Ok(Value::Undefined)
    });
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
        let mut moved = Vec::new();
        for n in nodes {
            if st.borrow().dom.tag(n) == Some(FRAGMENT) {
                moved.extend(st.borrow().dom.children(n).to_vec());
            } else {
                moved.push(n);
            }
            if let Err(e) = insert(&mut st.borrow_mut().dom, parent, n, before) {
                return i.throw("HierarchyRequestError", e);
            }
        }
        changed(st);
        for n in moved {
            inserted(i, st, n);
        }
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
        let old = st.borrow().dom.children(id).to_vec();
        for c in old {
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
    // In HTML documents HTML elements match the name lower-cased, others as given.
    method(i, st, &node, "getElementsByTagName", |i, st, id, a| {
        let t = i.to_str(&arg(&a, 0))?.to_string();
        let lower = t.to_ascii_lowercase();
        let found: Vec<NodeId> = {
            let s = st.borrow();
            let html_doc = in_html_doc(&s, id);
            descendants(&s.dom, id)
                .into_iter()
                .filter(|&n| {
                    is_element(&s.dom, n)
                        && (t == "*" || s.dom.tag(n) == Some(if html_doc && ns_of(&s.dom, n).0 == Some(HTML_NS) { lower.as_str() } else { t.as_str() }))
                })
                .collect()
        };
        Ok(wrap_list(i, st, found))
    });
    method(i, st, &node, "getElementsByTagNameNS", |i, st, id, a| {
        let ns = match arg(&a, 0) {
            Value::Null | Value::Undefined => None,
            v => Some(i.to_str(&v)?.to_string()).filter(|n| !n.is_empty()),
        };
        let local = i.to_str(&arg(&a, 1))?.to_string();
        let found: Vec<NodeId> = {
            let s = st.borrow();
            descendants(&s.dom, id)
                .into_iter()
                .filter(|&n| {
                    if !is_element(&s.dom, n) {
                        return false;
                    }
                    let (en, ep) = ns_of(&s.dom, n);
                    let tag = s.dom.tag(n).unwrap_or("");
                    let l = ep.and_then(|p| tag.strip_prefix(p)).and_then(|r| r.strip_prefix(':')).unwrap_or(tag);
                    (ns.as_deref() == Some("*") || ns.as_deref() == en) && (local == "*" || local == l)
                })
                .collect()
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
    getter(i, st, &element, "tagName", |_i, st, id| Ok(str_val(&tag_name(&st.borrow(), id))));
    getter(i, st, &element, "localName", |_i, st, id| {
        let s = st.borrow();
        let name = tag_of(&s.dom, id);
        Ok(str_val(match ns_of(&s.dom, id).1 {
            Some(_) => name.split_once(':').map_or(name.as_str(), |(_, l)| l),
            None => &name,
        }))
    });
    getter(i, st, &element, "namespaceURI", |_i, st, id| Ok(ns_of(&st.borrow().dom, id).0.map(str_val).unwrap_or(Value::Null)));
    getter(i, st, &element, "prefix", |_i, st, id| Ok(ns_of(&st.borrow().dom, id).1.map(str_val).unwrap_or(Value::Null)));
    method(i, st, &element, "getAttribute", |i, st, id, a| {
        let k = i.to_str(&arg(&a, 0))?;
        let k = attr_key(&st.borrow(), id, k);
        if k == "style" {
            flush(i, st);
        }
        Ok(st.borrow().dom.attr(id, &k).map(str_val).unwrap_or(Value::Null))
    });
    method(i, st, &element, "setAttribute", |i, st, id, a| {
        let k = i.to_str(&arg(&a, 0))?;
        if k.is_empty() || !no_ws_or(&k, &['/', '=', '>']) {
            return i.throw("InvalidCharacterError", "not a valid attribute name");
        }
        let k = attr_key(&st.borrow(), id, k);
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
        let k = i.to_str(&arg(&a, 0))?;
        let k = attr_key(&st.borrow(), id, k);
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
        let k = i.to_str(&arg(&a, 0))?;
        let k = attr_key(&st.borrow(), id, k);
        Ok(Value::Bool(st.borrow().dom.attr(id, &k).is_some()))
    });
    method(i, st, &element, "toggleAttribute", |i, st, id, a| {
        let k = i.to_str(&arg(&a, 0))?;
        if k.is_empty() || !no_ws_or(&k, &['/', '=', '>']) {
            return i.throw("InvalidCharacterError", "not a valid attribute name");
        }
        let k = attr_key(&st.borrow(), id, k);
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
        type Parts = (String, String, Option<String>, Option<String>, String);
        let pairs: Vec<Parts> = {
            let s = st.borrow();
            match &s.dom.nodes[id as usize].data {
                Data::Element { attrs, .. } => attrs
                    .iter()
                    .map(|(k, v)| {
                        let (ns, p, l) = attr_name_parts(&s.dom, id, k);
                        (k.to_string(), v.clone(), ns.map(str::to_string), p.map(str::to_string), l.to_string())
                    })
                    .collect(),
                _ => Vec::new(),
            }
        };
        let mut list = Vec::new();
        for (k, v, ns, p, l) in pairs {
            let o = i.obj(Kind::Plain);
            i.set(&o, "name", str_val(&k))?;
            i.set(&o, "value", str_val(&v))?;
            i.set(&o, "namespaceURI", ns.as_deref().map(str_val).unwrap_or(Value::Null))?;
            i.set(&o, "prefix", p.as_deref().map(str_val).unwrap_or(Value::Null))?;
            i.set(&o, "localName", str_val(&l))?;
            list.push(o);
        }
        Ok(i.array(list))
    });
    // The namespace-aware attribute methods.
    fn ns_arg(i: &mut Interp, v: &Value) -> Result<Option<String>, Value> {
        Ok(match v {
            Value::Null | Value::Undefined => None,
            v => Some(i.to_str(v)?.to_string()).filter(|s| !s.is_empty()),
        })
    }
    method(i, st, &element, "setAttributeNS", |i, st, id, a| {
        let ns = ns_arg(i, &arg(&a, 0))?;
        let qname = i.to_str(&arg(&a, 1))?.to_string();
        let v = i.to_str(&arg(&a, 2))?;
        let (prefix, local) = match validate_and_extract(ns.as_deref(), &qname) {
            Ok(x) => x,
            Err((name, msg)) => return i.throw(name, msg),
        };
        {
            let mut s = st.borrow_mut();
            match attr_by_ns(&s.dom, id, ns.as_deref(), &local) {
                Some(k) => set_attr(&mut s.dom, id, &k, &v),
                None => {
                    set_attr(&mut s.dom, id, &qname, &v);
                    let list = s.dom.attr_ns.entry(id).or_default();
                    list.retain(|(a, _, _)| **a != *qname);
                    if let Some(ns) = &ns {
                        list.push((Box::from(qname.as_str()), Box::from(ns.as_str()), prefix.map(Into::into)));
                    }
                }
            }
            if qname == "style" {
                s.styles.remove(&id);
            }
            if qname.starts_with("data-") {
                s.datasets.remove(&id);
            }
        }
        changed(st);
        Ok(Value::Undefined)
    });
    method(i, st, &element, "getAttributeNS", |i, st, id, a| {
        let ns = ns_arg(i, &arg(&a, 0))?;
        let local = i.to_str(&arg(&a, 1))?;
        let s = st.borrow();
        Ok(attr_by_ns(&s.dom, id, ns.as_deref(), &local).and_then(|k| s.dom.attr(id, &k)).map(str_val).unwrap_or(Value::Null))
    });
    method(i, st, &element, "hasAttributeNS", |i, st, id, a| {
        let ns = ns_arg(i, &arg(&a, 0))?;
        let local = i.to_str(&arg(&a, 1))?;
        Ok(Value::Bool(attr_by_ns(&st.borrow().dom, id, ns.as_deref(), &local).is_some()))
    });
    method(i, st, &element, "removeAttributeNS", |i, st, id, a| {
        let ns = ns_arg(i, &arg(&a, 0))?;
        let local = i.to_str(&arg(&a, 1))?;
        let k = attr_by_ns(&st.borrow().dom, id, ns.as_deref(), &local);
        if let Some(k) = k {
            {
                let mut s = st.borrow_mut();
                remove_attr(&mut s.dom, id, &k);
                if k == "style" {
                    s.styles.remove(&id);
                }
            }
            changed(st);
        }
        Ok(Value::Undefined)
    });
    // Reflected string attributes: Element's own, then HTML's (not on SVG or MathML elements).
    for (proto, prop, attr) in [("id", "id"), ("className", "class"), ("role", "role"), ("slot", "slot")].map(|(p, a)| (&element, p, a)).into_iter().chain([
        ("title", "title"), ("lang", "lang"), ("dir", "dir"), ("name", "name"), ("type", "type"),
        ("placeholder", "placeholder"), ("alt", "alt"), ("rel", "rel"), ("target", "target"), ("method", "method"), ("htmlFor", "for"),
        ("download", "download"), ("accept", "accept"), ("autocomplete", "autocomplete"), ("pattern", "pattern"),
        ("min", "min"), ("max", "max"), ("step", "step"), ("charset", "charset"), ("content", "content"), ("media", "media"), ("sizes", "sizes"), ("srcset", "srcset"),
    ].map(|(p, a)| (&html_element, p, a))) {
        accessor(
            i,
            st,
            proto,
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
            &html_element,
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
                    // A URL that doesn't parse comes back as written.
                    Some(v) => volant_core::url::resolve(&s.base(), v).unwrap_or_else(|| v.to_string()),
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
            move |_i, st, id| {
                let s = st.borrow();
                // A canvas is 300 x 150 unless told otherwise.
                let default = match (s.dom.tag(id), attr) {
                    (Some("canvas"), "width") => 300.0,
                    (Some("canvas"), "height") => 150.0,
                    _ => default,
                };
                Ok(Value::Num(s.dom.attr(id, attr).and_then(|v| v.trim().parse().ok()).unwrap_or(default)))
            },
            move |i, st, id, v| {
                let n = i.to_number(&v)?;
                set_attr(&mut st.borrow_mut().dom, id, attr, &volant_js::interp::num_to_string(n));
                changed(st);
                // Resizing a canvas clears it.
                if matches!(attr, "width" | "height") && st.borrow().dom.tag(id) == Some("canvas") {
                    let w = wrap(i, st, id);
                    if let Ok(ctx @ Value::Obj(_)) = i.get(&w, "__ctx") {
                        let reset = i.get(&ctx, "_reset")?;
                        i.call(&reset, ctx, vec![])?;
                    }
                }
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
            inserted(i, st, n);
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
    // classList, and the DOMTokenList behind every token-list attribute (relList, sandbox, …).
    fn token_list(i: &Interp, st: &S, el: &Value, id: NodeId, attr: &str) -> Value {
        let slot = format!("\0tl:{attr}");
        if let Value::Obj(w) = el {
            if let Some(v) = hidden(w, &slot) {
                return v;
            }
        }
        let proto = st.borrow().protos.as_ref().map(|p| p.class_list.clone());
        let o = i.obj(Kind::Plain);
        if let Value::Obj(oo) = &o {
            oo.borrow_mut().proto = proto;
            oo.borrow_mut().set_own("\0node", Value::Num(id as f64));
            oo.borrow_mut().set_own("\0attr", str_val(attr));
        }
        if let Value::Obj(w) = el {
            w.borrow_mut().set_own(&slot, o.clone());
        }
        o
    }
    getter(i, st, &element, "classList", |i, st, id| {
        let el = wrap(i, st, id);
        Ok(token_list(i, st, &el, id, "class"))
    });
    {
        let st2 = st.clone();
        let f = i.native(move |i, _t, a| {
            let el = arg(&a, 0);
            let Some(id) = node_of(&el) else { return i.throw("TypeError", "not a node") };
            let attr = i.to_str(&arg(&a, 1))?.to_string();
            Ok(token_list(i, &st2, &el, id, &attr))
        });
        i.global_obj.borrow_mut().set_own("__tokenList", f);
    }
    {
        // URL Standard natives behind URL and URLSearchParams.
        fn parts(i: &mut Interp, u: &volant_core::url::Url) -> Value {
            let opt = |p: &str, v: &Option<String>| match v.as_deref() {
                None | Some("") => String::new(),
                Some(x) => format!("{p}{x}"),
            };
            let items = [
                u.href(),
                u.origin(),
                format!("{}:", u.scheme),
                u.username.clone(),
                u.password.clone(),
                u.host_port(),
                u.host.clone().unwrap_or_default(),
                u.port.map(|p| p.to_string()).unwrap_or_default(),
                u.pathname(),
                opt("?", &u.query),
                opt("#", &u.fragment),
                u.query.clone().unwrap_or_default(),
            ];
            let v = items.iter().map(|x| str_val(x)).collect();
            i.array(v)
        }
        let f = i.native(|i, _t, a| {
            let input = i.to_str(&arg(&a, 0))?.to_string();
            let base = match arg(&a, 1) {
                Value::Undefined => None,
                b => match volant_core::url::parse(&i.to_str(&b)?, None) {
                    Some(u) => Some(u),
                    None => return Ok(Value::Null),
                },
            };
            Ok(match volant_core::url::parse(&input, base.as_ref()) {
                Some(u) => parts(i, &u),
                None => Value::Null,
            })
        });
        i.global_obj.borrow_mut().set_own("__urlParse", f);
        let f = i.native(|i, _t, a| {
            let href = i.to_str(&arg(&a, 0))?.to_string();
            let which = i.to_str(&arg(&a, 1))?.to_string();
            let value = i.to_str(&arg(&a, 2))?.to_string();
            let Some(mut u) = volant_core::url::parse(&href, None) else { return Ok(Value::Null) };
            u.set(&which, &value);
            Ok(parts(i, &u))
        });
        i.global_obj.borrow_mut().set_own("__urlSet", f);
        let f = i.native(|i, _t, a| {
            let input = i.to_str(&arg(&a, 0))?.to_string();
            let pairs = volant_core::url::form_parse(&input).into_iter().map(|(k, v)| i.array(vec![str_val(&k), str_val(&v)])).collect();
            Ok(i.array(pairs))
        });
        i.global_obj.borrow_mut().set_own("__formParse", f);
        let f = i.native(|i, _t, a| {
            let input = i.to_str(&arg(&a, 0))?.to_string();
            Ok(str_val(&volant_core::url::form_encode(&input)))
        });
        i.global_obj.borrow_mut().set_own("__formEncode", f);
    }
    {
        // Encoding Standard natives behind TextDecoder and TextEncoder.
        let f = i.native(|i, _t, a| {
            let l = i.to_str(&arg(&a, 0))?;
            Ok(volant_core::encoding::for_label(&l).map(str_val).unwrap_or(Value::Null))
        });
        i.global_obj.borrow_mut().set_own("__encodingFor", f);
        let f = i.native(|i, _t, a| {
            let enc = i.to_str(&arg(&a, 0))?;
            let bytes = volant_js::typed::bytes_of(&arg(&a, 1)).unwrap_or_default();
            match volant_core::encoding::decode(&enc, &bytes, Interp::truthy(&arg(&a, 2)), Interp::truthy(&arg(&a, 3))) {
                Ok((text, used)) => Ok(i.array(vec![str_val(&text), Value::Num(used as f64)])),
                Err(()) => i.throw("TypeError", "The encoded data was not valid"),
            }
        });
        i.global_obj.borrow_mut().set_own("__decode", f);
        let f = i.native(|i, _t, a| {
            let t = i.to_str(&arg(&a, 0))?;
            Ok(volant_js::typed::new_bytes(i, t.as_bytes().to_vec()))
        });
        i.global_obj.borrow_mut().set_own("__encode", f);
    }
    /// The element and attribute a DOMTokenList stands for.
    fn list_of(i: &Interp, this: &Value) -> Res<(NodeId, String)> {
        let id = this_node(i, this)?;
        let attr = match this {
            Value::Obj(o) => match hidden(o, "\0attr") {
                Some(Value::Str(s)) => s.to_string(),
                _ => "class".into(),
            },
            _ => "class".into(),
        };
        Ok((id, attr))
    }
    /// The token set: the attribute split on ASCII whitespace, duplicates dropped.
    fn tokens(st: &S, id: NodeId, attr: &str) -> Vec<String> {
        let s = st.borrow();
        let mut out: Vec<String> = Vec::new();
        for t in s.dom.attr(id, attr).unwrap_or("").split(|c: char| matches!(c, ' ' | '\t' | '\n' | '\x0c' | '\r')).filter(|t| !t.is_empty()) {
            if !out.iter().any(|x| x == t) {
                out.push(t.to_string());
            }
        }
        out
    }
    /// DOMTokenList's update steps.
    fn set_tokens(st: &S, id: NodeId, attr: &str, c: Vec<String>) {
        if c.is_empty() && st.borrow().dom.attr(id, attr).is_none() {
            return;
        }
        set_attr(&mut st.borrow_mut().dom, id, attr, &c.join(" "));
        changed(st);
    }
    fn token(i: &mut Interp, v: &Value) -> Res<String> {
        let t = i.to_str(v)?.to_string();
        if t.is_empty() {
            return i.throw("DOM:SyntaxError", "a token can't be empty");
        }
        if t.contains([' ', '\t', '\n', '\x0c', '\r']) {
            return i.throw("InvalidCharacterError", "a token can't contain whitespace");
        }
        Ok(t)
    }
    let st_tl = st.clone();
    let tl_proto = class_list.clone();
    let tl_method = move |i: &Interp, name: &str, f: fn(&mut Interp, &S, NodeId, &str, Vec<Value>) -> Res<Value>| {
        let st = st_tl.clone();
        def(i, &tl_proto, name, move |i, t, a| {
            let (id, attr) = list_of(i, &t)?;
            f(i, &st, id, &attr, a)
        });
    };
    tl_method(i, "add", |i, st, id, attr, a| {
        let mut want = Vec::new();
        for v in &a {
            want.push(token(i, v)?);
        }
        let mut c = tokens(st, id, attr);
        for t in want {
            if !c.contains(&t) {
                c.push(t);
            }
        }
        set_tokens(st, id, attr, c);
        Ok(Value::Undefined)
    });
    tl_method(i, "remove", |i, st, id, attr, a| {
        let mut gone = Vec::new();
        for v in &a {
            gone.push(token(i, v)?);
        }
        let mut c = tokens(st, id, attr);
        c.retain(|x| !gone.contains(x));
        set_tokens(st, id, attr, c);
        Ok(Value::Undefined)
    });
    tl_method(i, "toggle", |i, st, id, attr, a| {
        let t = token(i, &arg(&a, 0))?;
        let mut c = tokens(st, id, attr);
        let has = c.contains(&t);
        let force = match arg(&a, 1) {
            Value::Undefined => None,
            f => Some(Interp::truthy(&f)),
        };
        let want = force.unwrap_or(!has);
        if has == want {
            return Ok(Value::Bool(want));
        }
        if want {
            c.push(t);
        } else {
            c.retain(|x| *x != t);
        }
        set_tokens(st, id, attr, c);
        Ok(Value::Bool(want))
    });
    tl_method(i, "contains", |i, st, id, attr, a| {
        let t = i.to_str(&arg(&a, 0))?.to_string();
        Ok(Value::Bool(tokens(st, id, attr).contains(&t)))
    });
    tl_method(i, "replace", |i, st, id, attr, a| {
        let old = token(i, &arg(&a, 0))?;
        let new = token(i, &arg(&a, 1))?;
        let mut c = tokens(st, id, attr);
        let Some(p) = c.iter().position(|x| *x == old) else { return Ok(Value::Bool(false)) };
        if c.contains(&new) {
            c.remove(p);
            let q = c.iter().position(|x| *x == new).unwrap_or(0);
            // The new token takes the earlier of the two places.
            if q > p {
                let n = c.remove(q);
                c.insert(p, n);
            }
        } else {
            c[p] = new;
        }
        set_tokens(st, id, attr, c);
        Ok(Value::Bool(true))
    });
    tl_method(i, "supports", |i, _st, _id, attr, a| {
        let t = i.to_str(&arg(&a, 0))?.to_ascii_lowercase();
        let supported: &[&str] = match attr {
            "rel" => &["alternate", "stylesheet", "icon", "preload", "prefetch", "preconnect", "dns-prefetch", "modulepreload", "noopener", "noreferrer", "opener", "manifest", "next", "search", "author", "help", "license", "bookmark", "external", "nofollow", "tag"],
            "sandbox" => &["allow-downloads", "allow-forms", "allow-modals", "allow-orientation-lock", "allow-pointer-lock", "allow-popups", "allow-popups-to-escape-sandbox", "allow-presentation", "allow-same-origin", "allow-scripts", "allow-top-navigation", "allow-top-navigation-by-user-activation", "allow-top-navigation-to-custom-protocols"],
            _ => return i.throw("TypeError", &format!("{attr} has no supported tokens")),
        };
        Ok(Value::Bool(supported.contains(&t.as_str())))
    });
    tl_method(i, "item", |i, st, id, attr, a| {
        let k = i.to_number(&arg(&a, 0))?;
        let k = if k.is_finite() { (k.trunc() as i64).rem_euclid(1 << 32) as usize } else { 0 };
        Ok(tokens(st, id, attr).get(k).map(|s| str_val(s)).unwrap_or(Value::Null))
    });
    tl_method(i, "forEach", |i, st, id, attr, a| {
        let f = arg(&a, 0);
        let this = arg(&a, 1);
        for (k, c) in tokens(st, id, attr).into_iter().enumerate() {
            i.call(&f, this.clone(), vec![str_val(&c), Value::Num(k as f64)])?;
        }
        Ok(Value::Undefined)
    });
    tl_method(i, "toString", |_i, st, id, attr, _a| Ok(str_val(st.borrow().dom.attr(id, attr).unwrap_or(""))));
    for name in ["\0@iterator", "values", "keys", "entries"] {
        let st = st.clone();
        def(i, &class_list, name, move |i, t, _a| {
            let (id, attr) = list_of(i, &t)?;
            let list: Vec<Value> = tokens(&st, id, &attr).iter().map(|c| str_val(c)).collect();
            let arr = i.array(list);
            let f = i.get(&arr, if name == "\0@iterator" { "values" } else { name })?;
            i.call(&f, arr, vec![])
        });
    }
    {
        let st1 = st.clone();
        let len = i.native(move |i, t, _a| {
            let (id, attr) = list_of(i, &t)?;
            Ok(Value::Num(tokens(&st1, id, &attr).len() as f64))
        });
        class_list.borrow_mut().set_slot("length", Slot::Accessor(Some(len), None));
        let st2 = st.clone();
        let get = i.native(move |i, t, _a| {
            let (id, attr) = list_of(i, &t)?;
            Ok(str_val(st2.borrow().dom.attr(id, &attr).unwrap_or("")))
        });
        let st3 = st.clone();
        let set = i.native(move |i, t, a| {
            let (id, attr) = list_of(i, &t)?;
            let v = i.to_str(&arg(&a, 0))?;
            set_attr(&mut st3.borrow_mut().dom, id, &attr, &v);
            changed(&st3);
            Ok(Value::Undefined)
        });
        class_list.borrow_mut().set_slot("value", Slot::Accessor(Some(get), Some(set)));
    }
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
            let keys = o.borrow().own_keys();
            for k in keys {
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
    method(i, st, &element, "setSelectionRange", |i, st, id, a| {
        let end = i.to_number(&arg(&a, 1))?;
        let mut s = st.borrow_mut();
        if s.focus == Some(id) && end.is_finite() {
            let len = field_value(&s.dom, id).chars().count();
            s.caret = (end.max(0.0) as usize).min(len);
            s.generation += 1;
        }
        Ok(Value::Undefined)
    });
    for name in ["selectionStart", "selectionEnd"] {
        accessor(
            i,
            st,
            &element,
            name,
            |_i, st, id| {
                let s = st.borrow();
                let len = field_value(&s.dom, id).chars().count();
                Ok(Value::Num(if s.focus == Some(id) { s.caret.min(len) } else { len } as f64))
            },
            |i, st, id, v| {
                let n = i.to_number(&v)?;
                let mut s = st.borrow_mut();
                if s.focus == Some(id) && n.is_finite() {
                    let len = field_value(&s.dom, id).chars().count();
                    s.caret = (n.max(0.0) as usize).min(len);
                    s.generation += 1;
                }
                Ok(())
            },
        );
    }
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
    for name in ["play", "load"] {
        method(i, st, &element, name, |i, _st, _id, _a| Ok(i.promise_resolve(Value::Undefined)));
    }
    method(i, st, &element, "pause", |_i, _st, _id, _a| Ok(Value::Undefined));

    // ---------------------------------------------------------- Document
    getter(i, st, &document, "documentElement", |i, st, id| {
        let found = document_element(&st.borrow().dom, id);
        Ok(wrap_opt(i, st, found))
    });
    getter(i, st, &document, "head", |i, st, id| {
        let found = html_child(&st.borrow().dom, id, &["head"]);
        Ok(wrap_opt(i, st, found))
    });
    getter(i, st, &document, "body", |i, st, id| {
        let found = html_child(&st.borrow().dom, id, &["body", "frameset"]);
        Ok(wrap_opt(i, st, found))
    });
    getter(i, st, &document, "scrollingElement", |i, st, id| {
        let found = document_element(&st.borrow().dom, id);
        Ok(wrap_opt(i, st, found))
    });
    accessor(
        i,
        st,
        &document,
        "title",
        |_i, st, id| {
            let s = st.borrow();
            let title = descendants(&s.dom, id).into_iter().find(|&n| s.dom.tag(n) == Some("title"));
            Ok(str_val(&title.map(|t| s.dom.text(t).split_ascii_whitespace().collect::<Vec<_>>().join(" ")).unwrap_or_default()))
        },
        |i, st, id, v| {
            let t = i.to_str(&v)?;
            let mut s = st.borrow_mut();
            let found = descendants(&s.dom, id).into_iter().find(|&n| s.dom.tag(n) == Some("title"));
            let title = match found {
                Some(t) => t,
                None => {
                    let Some(head) = html_child(&s.dom, id, &["head"]) else { return Ok(()) };
                    let t = new_node(&mut s.dom, Data::Element { name: "title".into(), attrs: Vec::new() });
                    let _ = insert(&mut s.dom, head, t, None);
                    t
                }
            };
            set_text(&mut s.dom, title, &t);
            s.generation += 1;
            Ok(())
        },
    );
    getter(i, st, &document, "URL", |_i, st, id| Ok(str_val(&doc_url(&st.borrow(), id))));
    getter(i, st, &document, "documentURI", |_i, st, id| Ok(str_val(&doc_url(&st.borrow(), id))));
    getter(i, st, &document, "baseURI", |_i, st, id| {
        let s = st.borrow();
        Ok(str_val(&if id == ROOT { s.base() } else { base_url_in(&s.dom, id, &doc_url(&s, id)) }))
    });
    getter(i, st, &document, "referrer", |_i, _st, _id| Ok(str_val("")));
    for name in ["characterSet", "charset", "inputEncoding"] {
        getter(i, st, &document, name, |_i, _st, _id| Ok(str_val("UTF-8")));
    }
    getter(i, st, &document, "compatMode", |_i, _st, _id| Ok(str_val("CSS1Compat")));
    getter(i, st, &document, "contentType", |_i, st, id| Ok(str_val(st.borrow().documents.get(&id).map(String::as_str).unwrap_or("text/html"))));
    {
        // __newDocument(contentType): an empty document; __parseDocument(html): one parsed from markup.
        let st2 = st.clone();
        let f = i.native(move |i, _t, a| {
            let ty = i.to_str(&arg(&a, 0))?.to_string();
            let d = new_node(&mut st2.borrow_mut().dom, Data::Document);
            st2.borrow_mut().documents.insert(d, ty);
            Ok(wrap(i, &st2, d))
        });
        i.global_obj.borrow_mut().set_own("__newDocument", f);
        let st3 = st.clone();
        let f = i.native(move |i, _t, a| {
            let markup = i.to_str(&arg(&a, 0))?;
            let ty = match arg(&a, 1) {
                Value::Undefined => "text/html".to_string(),
                v => i.to_str(&v)?.to_string(),
            };
            let url = match arg(&a, 2) {
                Value::Undefined => None,
                v => Some(i.to_str(&v)?.to_string()),
            };
            let xml = ty != "text/html" && (ty.ends_with("/xml") || ty.ends_with("+xml"));
            let mut parsed = if xml {
                volant_core::xml::parse(&markup).unwrap_or_else(|e| {
                    // Not well-formed: the document says so, as browsers do.
                    let msg = format!("<parsererror xmlns=\"http://www.mozilla.org/newlayout/xml/parsererror.xml\">XML Parsing Error: {}</parsererror>", e.replace('<', "&lt;"));
                    volant_core::xml::parse(&msg).expect("parsererror")
                })
            } else {
                html::parse_with_scripts(&markup)
            };
            if ty == "text/html" {
                ensure_structure(&mut parsed);
            }
            let mut s = st3.borrow_mut();
            let d = new_node(&mut s.dom, Data::Document);
            s.documents.insert(d, ty);
            if let Some(u) = url {
                s.doc_urls.insert(d, u);
            }
            let frag = import(&mut s.dom, &parsed);
            for c in s.dom.children(frag).to_vec() {
                let _ = insert(&mut s.dom, d, c, None);
            }
            drop(s);
            Ok(wrap(i, &st3, d))
        });
        i.global_obj.borrow_mut().set_own("__parseDocument", f);
    }
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
        |_i, st, _id| {
            let (host, url) = {
                let s = st.borrow();
                (s.host.clone(), s.url.clone())
            };
            let c = match host.cookie(&url) {
                Some(c) => c,
                None => match volant_core::http::Url::parse(&url) {
                    Some(u) => st.borrow_mut().cookies.header(&u, volant_core::cookies::now(), true),
                    None => String::new(),
                },
            };
            Ok(str_val(&c))
        },
        |i, st, _id, v| {
            let c = i.to_str(&v)?.to_string();
            let (host, url) = {
                let s = st.borrow();
                (s.host.clone(), s.url.clone())
            };
            if !host.set_cookie(&url, &c) {
                if let Some(u) = volant_core::http::Url::parse(&url) {
                    st.borrow_mut().cookies.set(&u, &c, volant_core::cookies::now(), true);
                }
            }
            Ok(())
        },
    );
    for (name, tag) in [("forms", "form"), ("images", "img"), ("scripts", "script"), ("links", "a"), ("embeds", "embed")] {
        getter(i, st, &document, name, move |i, st, id| {
            let found: Vec<NodeId> = {
                let s = st.borrow();
                descendants(&s.dom, id).into_iter().filter(|&n| s.dom.tag(n) == Some(tag)).collect()
            };
            Ok(wrap_list(i, st, found))
        });
    }
    method(i, st, &document, "createElement", |i, st, doc, a| {
        let t = i.to_str(&arg(&a, 0))?;
        if !is_element_local_name(&t) {
            return i.throw("InvalidCharacterError", "not a valid element name");
        }
        // HTML documents lower-case the name; XML ones keep it.
        let ty = st.borrow().documents.get(&owner_doc(&st.borrow(), doc)).cloned();
        let html = ty.as_deref().is_none_or(|t| t == "text/html");
        let t = if html { t.to_ascii_lowercase() } else { t.to_string() };
        let n = new_node(&mut st.borrow_mut().dom, Data::Element { name: t.into(), attrs: Vec::new() });
        // Elements of XML documents (other than XHTML) are in no namespace.
        if !html && ty.as_deref() != Some("application/xhtml+xml") {
            st.borrow_mut().dom.ns.insert(n, (None, None));
        }
        made_by(st, doc, n);
        Ok(wrap(i, st, n))
    });
    method(i, st, &document, "createElementNS", |i, st, doc, a| {
        let namespace = match arg(&a, 0) {
            Value::Null | Value::Undefined => None,
            v => Some(i.to_str(&v)?.to_string()).filter(|n| !n.is_empty()),
        };
        let qname = i.to_str(&arg(&a, 1))?.to_string();
        let (prefix, _local) = match validate_and_extract(namespace.as_deref(), &qname) {
            Ok(x) => x,
            Err((name, msg)) => return i.throw(name, msg),
        };
        let n = new_node(&mut st.borrow_mut().dom, Data::Element { name: qname.into(), attrs: Vec::new() });
        st.borrow_mut().dom.ns.insert(n, (namespace.map(Into::into), prefix.map(Into::into)));
        made_by(st, doc, n);
        Ok(wrap(i, st, n))
    });
    method(i, st, &document, "createTextNode", |i, st, doc, a| {
        let t = i.to_str(&arg(&a, 0))?;
        let n = new_node(&mut st.borrow_mut().dom, Data::Text(t.to_string()));
        made_by(st, doc, n);
        Ok(wrap(i, st, n))
    });
    method(i, st, &document, "createComment", |i, st, doc, a| {
        let t = i.to_str(&arg(&a, 0))?;
        let n = new_node(&mut st.borrow_mut().dom, Data::Comment(t.to_string()));
        made_by(st, doc, n);
        Ok(wrap(i, st, n))
    });
    method(i, st, &document, "createProcessingInstruction", |i, st, doc, a| {
        let target = i.to_str(&arg(&a, 0))?;
        let data = i.to_str(&arg(&a, 1))?;
        if !is_xml_name(&target) || data.contains("?>") {
            return i.throw("InvalidCharacterError", "not a valid processing instruction");
        }
        let n = new_node(&mut st.borrow_mut().dom, Data::Pi { target: target.to_string().into(), data: data.to_string() });
        made_by(st, doc, n);
        Ok(wrap(i, st, n))
    });
    getter(i, st, &document, "doctype", |i, st, id| {
        let found = st.borrow().dom.children(id).iter().copied().find(|&c| matches!(st.borrow().dom.nodes[c as usize].data, Data::Doctype { .. }));
        Ok(wrap_opt(i, st, found))
    });
    {
        let st2 = st.clone();
        let f = i.native(move |i, _t, a| {
            let name = i.to_str(&arg(&a, 0))?;
            if !is_doctype_name(&name) {
                return i.throw("InvalidCharacterError", "not a valid doctype name");
            }
            let public_id = i.to_str(&arg(&a, 1))?.to_string();
            let system_id = i.to_str(&arg(&a, 2))?.to_string();
            let n = new_node(&mut st2.borrow_mut().dom, Data::Doctype { name: name.to_string().into(), public_id, system_id });
            // Made by the document whose DOMImplementation was asked.
            if let Some(doc) = node_of(&arg(&a, 3)) {
                made_by(&st2, doc, n);
            }
            Ok(wrap(i, &st2, n))
        });
        i.global_obj.borrow_mut().set_own("__createDocumentType", f);
    }
    method(i, st, &document, "createDocumentFragment", |i, st, doc, _a| {
        let n = new_node(&mut st.borrow_mut().dom, Data::Element { name: FRAGMENT.into(), attrs: Vec::new() });
        made_by(st, doc, n);
        Ok(wrap(i, st, n))
    });
    method(i, st, &document, "createEvent", |i, st, _id, _a| Ok(make_event(i, st, "", false, false)));
    method(i, st, &document, "importNode", |i, st, doc, a| {
        let Some(n) = node_of(&arg(&a, 0)) else { return i.throw("TypeError", "the argument is not a Node") };
        if matches!(st.borrow().dom.nodes[n as usize].data, Data::Document) {
            return i.throw("NotSupportedError", "a document can't be imported");
        }
        let c = clone_node(&mut st.borrow_mut().dom, n, Interp::truthy(&arg(&a, 1)));
        adopt(st, doc, c);
        Ok(wrap(i, st, c))
    });
    method(i, st, &document, "adoptNode", |i, st, doc, a| {
        let Some(n) = node_of(&arg(&a, 0)) else { return i.throw("TypeError", "the argument is not a Node") };
        if matches!(st.borrow().dom.nodes[n as usize].data, Data::Document) {
            return i.throw("NotSupportedError", "a document can't be adopted");
        }
        detach(&mut st.borrow_mut().dom, n);
        adopt(st, doc, n);
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
        // __fetch(method, url, body, [[name, value]…], credentials, mode)
        //   → [status, contentType, text, finalUrl, headers], or null if it failed or wasn't allowed
        let method = i.to_str(&arg(&a, 0))?.to_ascii_uppercase();
        let credentials = match arg(&a, 4) {
            Value::Undefined => "same-origin".to_string(),
            v => i.to_str(&v)?.to_string(),
        };
        let mode = match arg(&a, 5) {
            Value::Undefined => "cors".to_string(),
            v => i.to_str(&v)?.to_string(),
        };
        let url = i.to_str(&arg(&a, 1))?;
        let body = match arg(&a, 2) {
            Value::Undefined | Value::Null => None,
            b => Some(i.to_str(&b)?.to_string()),
        };
        let mut headers = Vec::new();
        if let Value::Obj(list) = arg(&a, 3) {
            let len = i.get(&Value::Obj(list.clone()), "length")?;
            let n = i.to_number(&len).unwrap_or(0.0) as usize;
            for k in 0..n.min(100) {
                let pair = i.get(&Value::Obj(list.clone()), &k.to_string())?;
                let name = i.get(&pair, "0")?;
                let value = i.get(&pair, "1")?;
                headers.push((i.to_str(&name)?.to_string(), i.to_str(&value)?.to_string()));
            }
        }
        let (host, abs, page) = {
            let s = st2.borrow();
            (s.host.clone(), resolve_url(&s.url, &url), s.url.clone())
        };
        let r = match cors_fetch(&*host, &page, &method, &abs, headers, body.as_deref().map(str::as_bytes), &credentials, &mode) {
            Ok(r) => r,
            Err(why) => {
                i.output.push(format!("{why}"));
                return Ok(Value::Null);
            }
        };
        let text = String::from_utf8_lossy(&r.body).into_owned();
        let head: String = r.headers.iter().filter(|(k, _)| k != "set-cookie" && k != "set-cookie2").map(|(k, v)| format!("{k}: {v}\r\n")).collect();
        let final_url = if r.url.is_empty() { abs } else { r.url };
        Ok(i.array(vec![Value::Num(r.status as f64), str_val(&r.content_type), str_val(&text), str_val(&final_url), str_val(&head)]))
    });
    hook(i, "__fetch", f);
    let st2 = st.clone();
    let f = i.native(move |i, _t, a| {
        let q = i.to_str(&arg(&a, 0))?;
        let env = st2.borrow().env;
        Ok(Value::Bool(css::media_applies(&q, &env)))
    });
    hook(i, "__media", f);
    let f = i.native(move |i, _t, a| {
        // __measure(text, font): a canvas's measureText width
        let text = i.to_str(&arg(&a, 0))?.to_string();
        let spec = i.to_str(&arg(&a, 1))?.to_string();
        let (size, bold, serif, mono) = crate::canvas::font(&spec);
        let family = if mono { style::Family::Mono } else if serif { style::Family::Serif } else { style::Family::Sans };
        let font = crate::Font { family, size, weight: if bold { 700 } else { 400 }, italic: false };
        Ok(Value::Num(crate::Fonts::width(&crate::ApproxFonts, &text, &font) as f64))
    });
    hook(i, "__measure", f);
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
    let f = i.native(move |i, _t, a| {
        let u = i.to_str(&arg(&a, 0))?;
        Ok(str_val(&origin_of(&u)))
    });
    hook(i, "__origin", f);
    let st2 = st.clone();
    let f = i.native(move |i, _t, a| {
        let u = i.to_str(&arg(&a, 0))?;
        let mut s = st2.borrow_mut();
        let abs = resolve_url(&s.url, &u);
        let same_doc = abs.split('#').next() == s.url.split('#').next() && abs.contains('#');
        if same_doc {
            s.url = abs;
            s.generation += 1;
            s.scroll_to_anchor = true;
        } else {
            s.navigate = Some(Navigation::get(abs));
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

    // Members are defined once, on Element's and Node's prototypes; each
    // interface's prototype also holds its own (the same functions), so code that
    // reads or wraps `HTMLScriptElement.prototype.src` and the like finds them.
    let own = |from: &[&Obj], to: &Obj, names: &[&str]| {
        for n in names {
            let slot = from.iter().find_map(|o| o.borrow().get_own(n).cloned());
            if let Some(slot) = slot {
                to.borrow_mut().set_slot(n, slot);
            }
        }
    };
    own(&[&element], &html_element, HTML_ELEMENT_MEMBERS);
    for (name, members) in INTERFACE_MEMBERS {
        let proto = ifaces.iter().find(|(n, _)| n == name).map(|(_, p)| p.clone());
        if let Some(p) = proto {
            own(&[&element, &html_element], &p, members);
        }
    }
    own(&[&node, &element], &document, DOCUMENT_MEMBERS);
    own(&[&node], &element, ELEMENT_MIXIN_MEMBERS);
}

/// What Element's prototype holds itself (the ParentNode and ChildNode members) rather than inheriting from Node.
const ELEMENT_MIXIN_MEMBERS: &[&str] = &[
    "after", "before", "remove", "replaceWith", "append", "prepend", "replaceChildren", "querySelector", "querySelectorAll", "children", "firstElementChild",
    "lastElementChild", "childElementCount", "nextElementSibling", "previousElementSibling", "getElementsByTagName", "getElementsByClassName", "closest", "matches",
];

/// Element interfaces: (name, tags, what it extends: "html", "media", "svg", "svg-graphics").
const ELEMENT_INTERFACES: &[(&str, &[&str], &str)] = &[
    ("HTMLDivElement", &["div"], "html"), ("HTMLSpanElement", &["span"], "html"), ("HTMLAnchorElement", &["a"], "html"), ("HTMLImageElement", &["img"], "html"),
    ("HTMLInputElement", &["input"], "html"), ("HTMLButtonElement", &["button"], "html"), ("HTMLFormElement", &["form"], "html"), ("HTMLSelectElement", &["select"], "html"),
    ("HTMLOptionElement", &["option"], "html"), ("HTMLOptGroupElement", &["optgroup"], "html"), ("HTMLTextAreaElement", &["textarea"], "html"), ("HTMLCanvasElement", &["canvas"], "html"),
    ("HTMLIFrameElement", &["iframe"], "html"), ("HTMLScriptElement", &["script"], "html"), ("HTMLStyleElement", &["style"], "html"), ("HTMLLinkElement", &["link"], "html"),
    ("HTMLMetaElement", &["meta"], "html"), ("HTMLBodyElement", &["body"], "html"), ("HTMLHeadElement", &["head"], "html"), ("HTMLHtmlElement", &["html"], "html"),
    ("HTMLParagraphElement", &["p"], "html"), ("HTMLHeadingElement", &["h1", "h2", "h3", "h4", "h5", "h6"], "html"), ("HTMLUListElement", &["ul"], "html"),
    ("HTMLOListElement", &["ol"], "html"), ("HTMLLIElement", &["li"], "html"), ("HTMLDListElement", &["dl"], "html"), ("HTMLTableElement", &["table"], "html"),
    ("HTMLTableSectionElement", &["thead", "tbody", "tfoot"], "html"), ("HTMLTableRowElement", &["tr"], "html"), ("HTMLTableCellElement", &["td", "th"], "html"),
    ("HTMLTableCaptionElement", &["caption"], "html"), ("HTMLTableColElement", &["col", "colgroup"], "html"), ("HTMLVideoElement", &["video"], "media"),
    ("HTMLAudioElement", &["audio"], "media"), ("HTMLTemplateElement", &["template"], "html"), ("HTMLLabelElement", &["label"], "html"),
    ("HTMLDetailsElement", &["details"], "html"), ("HTMLDialogElement", &["dialog"], "html"), ("HTMLPictureElement", &["picture"], "html"),
    ("HTMLSourceElement", &["source"], "html"), ("HTMLTrackElement", &["track"], "html"), ("HTMLBRElement", &["br"], "html"), ("HTMLHRElement", &["hr"], "html"),
    ("HTMLPreElement", &["pre", "listing", "xmp"], "html"), ("HTMLSlotElement", &["slot"], "html"), ("HTMLObjectElement", &["object"], "html"),
    ("HTMLEmbedElement", &["embed"], "html"), ("HTMLFieldSetElement", &["fieldset"], "html"), ("HTMLLegendElement", &["legend"], "html"),
    ("HTMLProgressElement", &["progress"], "html"), ("HTMLMeterElement", &["meter"], "html"), ("HTMLOutputElement", &["output"], "html"),
    ("HTMLTimeElement", &["time"], "html"), ("HTMLDataElement", &["data"], "html"), ("HTMLDataListElement", &["datalist"], "html"), ("HTMLMapElement", &["map"], "html"),
    ("HTMLAreaElement", &["area"], "html"), ("HTMLBaseElement", &["base"], "html"), ("HTMLTitleElement", &["title"], "html"),
    ("HTMLQuoteElement", &["blockquote", "q"], "html"), ("HTMLModElement", &["ins", "del"], "html"), ("HTMLMenuElement", &["menu"], "html"),
    ("HTMLFontElement", &["font"], "html"), ("HTMLParamElement", &["param"], "html"), ("HTMLDirectoryElement", &["dir"], "html"),
    ("HTMLFrameElement", &["frame"], "html"), ("HTMLFrameSetElement", &["frameset"], "html"), ("HTMLMarqueeElement", &["marquee"], "html"),
    ("HTMLFormControlsCollection-unused", &[], "html"), ("HTMLNoScriptElement-unused", &[], "html"),
    ("SVGSVGElement", &["svg"], "svg-graphics"), ("SVGPathElement", &["path"], "svg-graphics"), ("SVGGElement", &["g"], "svg-graphics"),
    ("SVGCircleElement", &["circle"], "svg-graphics"), ("SVGRectElement", &["rect"], "svg-graphics"), ("SVGUseElement", &["use"], "svg-graphics"),
    ("SVGLineElement", &["line"], "svg-graphics"), ("SVGPolygonElement", &["polygon"], "svg-graphics"), ("SVGPolylineElement", &["polyline"], "svg-graphics"),
    ("SVGEllipseElement", &["ellipse"], "svg-graphics"), ("SVGTextElement", &["text"], "svg-graphics"), ("SVGImageElement", &["image"], "svg-graphics"),
    ("SVGSymbolElement", &["symbol"], "svg"), ("SVGDefsElement", &["defs"], "svg-graphics"), ("SVGStopElement", &["stop"], "svg"),
    ("SVGLinearGradientElement", &["lineargradient", "linearGradient"], "svg"), ("SVGRadialGradientElement", &["radialgradient", "radialGradient"], "svg"),
    ("SVGClipPathElement", &["clippath", "clipPath"], "svg"), ("SVGMaskElement", &["mask"], "svg"), ("SVGTitleElement", &["title-svg-unused"], "svg"),
];

/// HTML elements whose interface is HTMLElement itself.
const HTML_PLAIN: &[&str] = &[
    "abbr", "acronym", "address", "article", "aside", "b", "basefont", "bdi", "bdo", "big", "center", "cite", "code", "dd", "dfn", "dt", "em", "figcaption",
    "figure", "footer", "header", "hgroup", "i", "kbd", "main", "mark", "nav", "nobr", "noembed", "noframes", "noscript", "plaintext", "rb", "rp", "rt", "rtc",
    "ruby", "s", "samp", "search", "section", "small", "strike", "sub", "summary", "sup", "tt", "u", "var", "wbr",
];

/// Events with an `on…` property on HTMLElement's prototype.
const EVENT_HANDLERS: &[&str] = &[
    "abort", "blur", "cancel", "change", "click", "close", "contextmenu", "dblclick", "drag", "dragend", "dragenter", "dragleave", "dragover", "dragstart", "drop",
    "error", "focus", "focusin", "focusout", "input", "invalid", "keydown", "keypress", "keyup", "load", "mousedown", "mouseenter", "mouseleave", "mousemove",
    "mouseout", "mouseover", "mouseup", "pointerdown", "pointermove", "pointerup", "pointercancel", "pointerover", "pointerout", "pointerenter", "pointerleave",
    "reset", "resize", "scroll", "select", "submit", "toggle", "wheel", "animationend", "transitionend",
    "loadstart", "loadeddata", "loadedmetadata", "canplay", "play", "pause", "ended", "timeupdate", "volumechange", "beforeinput", "copy", "cut", "paste",
];

/// What HTMLElement's prototype holds itself.
const HTML_ELEMENT_MEMBERS: &[&str] = &[
    "title", "lang", "dir", "hidden", "inert", "accessKey", "draggable", "spellcheck", "autocapitalize", "innerText", "outerText", "translate", "click", "focus",
    "blur", "style", "dataset", "tabIndex", "offsetParent", "offsetTop", "offsetLeft", "offsetWidth", "offsetHeight", "contentEditable", "isContentEditable",
    "enterKeyHint", "inputMode", "nonce", "popover", "showPopover", "hidePopover", "togglePopover", "attachInternals",
];

/// What each interface's prototype holds itself (those Volant has).
const INTERFACE_MEMBERS: &[(&str, &[&str])] = &[
    ("HTMLScriptElement", &["src", "type", "noModule", "async", "defer", "crossOrigin", "text", "integrity", "charset", "referrerPolicy"]),
    ("HTMLImageElement", &["alt", "src", "srcset", "sizes", "crossOrigin", "useMap", "isMap", "width", "height", "naturalWidth", "naturalHeight", "complete", "currentSrc", "referrerPolicy", "decoding", "loading", "decode"]),
    ("HTMLIFrameElement", &["src", "srcdoc", "name", "sandbox", "allow", "allowFullscreen", "width", "height", "referrerPolicy", "loading", "contentDocument", "contentWindow"]),
    ("HTMLAnchorElement", &["target", "download", "ping", "rel", "relList", "hreflang", "type", "text", "referrerPolicy", "href", "origin", "protocol", "username", "password", "host", "hostname", "port", "pathname", "search", "hash"]),
    ("HTMLAreaElement", &["alt", "coords", "shape", "target", "download", "rel", "href"]),
    ("HTMLLinkElement", &["href", "crossOrigin", "rel", "as", "relList", "media", "integrity", "hreflang", "type", "sizes", "referrerPolicy", "disabled", "sheet"]),
    ("HTMLInputElement", &["accept", "alt", "autocomplete", "defaultChecked", "checked", "dirName", "disabled", "form", "files", "formAction", "height", "indeterminate", "list", "max", "maxLength", "min", "minLength", "multiple", "name", "pattern", "placeholder", "readOnly", "required", "size", "src", "step", "type", "defaultValue", "value", "valueAsDate", "valueAsNumber", "width", "select", "setSelectionRange", "selectionStart", "selectionEnd", "setRangeText", "checkValidity", "reportValidity", "setCustomValidity", "validity", "validationMessage", "willValidate", "labels", "stepUp", "stepDown", "showPicker"]),
    ("HTMLTextAreaElement", &["autocomplete", "cols", "rows", "defaultValue", "value", "name", "placeholder", "readOnly", "required", "disabled", "maxLength", "minLength", "wrap", "form", "select", "selectionStart", "selectionEnd", "setSelectionRange", "setRangeText", "textLength", "checkValidity", "reportValidity", "setCustomValidity", "validity", "labels"]),
    ("HTMLSelectElement", &["autocomplete", "disabled", "form", "multiple", "name", "required", "size", "type", "options", "length", "item", "namedItem", "add", "remove", "selectedOptions", "selectedIndex", "value", "checkValidity", "reportValidity", "setCustomValidity", "validity", "labels"]),
    ("HTMLOptionElement", &["disabled", "form", "label", "defaultSelected", "selected", "value", "text", "index"]),
    ("HTMLButtonElement", &["disabled", "form", "formAction", "name", "type", "value", "checkValidity", "reportValidity", "setCustomValidity", "validity", "labels"]),
    ("HTMLFormElement", &["acceptCharset", "action", "autocomplete", "enctype", "encoding", "method", "name", "noValidate", "target", "rel", "elements", "length", "submit", "requestSubmit", "reset", "checkValidity", "reportValidity"]),
    ("HTMLCanvasElement", &["width", "height", "getContext", "toDataURL", "toBlob"]),
    ("HTMLMediaElement", &["src", "currentSrc", "crossOrigin", "preload", "autoplay", "loop", "controls", "muted", "defaultMuted", "volume", "paused", "play", "pause", "load", "currentTime", "duration", "ended", "readyState", "networkState", "canPlayType"]),
    ("HTMLVideoElement", &["width", "height", "poster", "videoWidth", "videoHeight", "playsInline"]),
    ("HTMLStyleElement", &["media", "type", "disabled", "sheet"]),
    ("HTMLMetaElement", &["name", "content", "httpEquiv", "media"]),
    ("HTMLTemplateElement", &["content"]),
    ("HTMLDetailsElement", &["open", "name"]),
    ("HTMLDialogElement", &["open", "returnValue", "show", "showModal", "close"]),
    ("HTMLLabelElement", &["htmlFor", "control", "form"]),
    ("HTMLBaseElement", &["href", "target"]),
    ("HTMLSourceElement", &["src", "srcset", "sizes", "media", "type"]),
    ("HTMLTableCellElement", &["colSpan", "rowSpan", "headers", "cellIndex"]),
    ("HTMLTableRowElement", &["cells", "rowIndex", "insertCell", "deleteCell"]),
    ("HTMLTableElement", &["caption", "tHead", "tFoot", "tBodies", "rows", "insertRow", "deleteRow", "createTHead"]),
    ("HTMLOListElement", &["start", "reversed", "type"]),
    ("HTMLLIElement", &["value"]),
    ("HTMLProgressElement", &["value", "max", "position", "labels"]),
    ("HTMLOutputElement", &["value", "name", "form"]),
    ("HTMLTimeElement", &["dateTime"]),
    ("HTMLSlotElement", &["name", "assignedNodes", "assignedElements"]),
    ("HTMLObjectElement", &["data", "type", "name", "width", "height"]),
    ("HTMLEmbedElement", &["src", "type", "width", "height"]),
    ("HTMLTitleElement", &["text"]),
    ("HTMLQuoteElement", &["cite"]),
];

/// What Document's prototype holds itself rather than inheriting from Node.
const DOCUMENT_MEMBERS: &[&str] = &[
    "append", "prepend", "replaceChildren", "querySelector", "querySelectorAll", "getElementsByTagName", "getElementsByTagNameNS", "getElementsByClassName",
    "getElementById", "children", "firstElementChild", "lastElementChild", "childElementCount", "createElement", "createElementNS", "createTextNode",
    "createComment", "createDocumentFragment", "createEvent", "importNode", "adoptNode",
];

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
    if (id as usize) >= s.dom.nodes.len() {
        return String::new();
    }
    if s.computed.as_ref().map(|c| c.generation) != Some(generation) {
        use std::hash::{Hash, Hasher};
        let mut all = crate::style_sheets(&s.dom);
        all.extend(s.sheets.iter().cloned());
        let mut h = std::collections::hash_map::DefaultHasher::new();
        all.hash(&mut h);
        s.env.width.to_bits().hash(&mut h);
        s.env.dark.hash(&mut h);
        let key = h.finish();
        let cascade = match &s.computed {
            Some(c) if c.sheets == key => c.cascade.clone(),
            _ => Rc::new(style::Cascade::new(&all, &s.env)),
        };
        s.computed = Some(Computed { generation, sheets: key, cascade, memo: HashMap::new() });
    }
    let state = &mut *s;
    let Some(c) = state.computed.as_mut() else { return String::new() };
    let st = &c.cascade.clone().style_of(&state.dom, id, &mut c.memo);
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
    /// What's drawn on each `<canvas>`, by the `src` layout gives its box.
    pub canvases: HashMap<String, std::sync::Arc<crate::canvas::Canvas>>,
    /// For each node of `dom`, the runtime's node (to send clicks back).
    pub map: Vec<NodeId>,
    pub title: String,
    pub url: String,
    pub generation: u64,
}

/// Does this page look like it came out right in Simple Page? `None` if so,
/// otherwise why not: the reasons a browser should hand the page to WebKit.
/// `errors` is how many uncaught script errors the page had.
/// The User-Agent pages see, in requests and as `navigator.userAgent` (prelude.js says the same).
pub const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Safari/605.1.15 Volant/1.0";

pub fn assess(snap: &Snapshot, errors: usize) -> Option<&'static str> {
    fn walk(dom: &Dom, id: NodeId, text: &mut usize, scripts: &mut usize, media: &mut usize, frameset: &mut bool) {
        match dom.tag(id) {
            Some("script") => {
                *scripts += 1;
                return;
            }
            Some("style" | "noscript" | "template" | "head" | "title") => return,
            Some("frameset") => *frameset = true,
            Some("video" | "audio" | "embed" | "object") => *media += 1,
            _ => {}
        }
        if let Data::Text(t) = &dom.nodes[id as usize].data {
            *text += t.chars().filter(|c| !c.is_whitespace()).count();
        }
        for &c in dom.children(id) {
            walk(dom, c, text, scripts, media, frameset);
        }
    }
    let (mut text, mut scripts, mut media, mut frameset) = (0, 0, 0, false);
    walk(&snap.dom, ROOT, &mut text, &mut scripts, &mut media, &mut frameset);
    if frameset {
        Some("the page is made of frames")
    } else if text < 80 && scripts > 0 {
        Some("the page is built by scripts and came out empty")
    } else if text < 80 && media > 0 {
        Some("the page is a video or plug-in")
    } else if errors >= 3 && text < 600 {
        Some("the page's scripts failed")
    } else {
        None
    }
}

pub struct Runtime {
    pub interp: Interp,
    pub state: S,
    started: std::time::Instant,
    preload: std::sync::Arc<Preload>,
    /// Steps each script, timer or event may take before it's stopped.
    pub budget: u64,
}

const PRELUDE: &str = include_str!("prelude.js");

impl Runtime {
    /// Parses `html` (keeping scripts) and sets up the page's JavaScript world; runs nothing yet.
    pub fn new(html: &str, url: &str, env: Env, host: Rc<dyn Host>) -> Runtime {
        let mut dom = html::parse_with_scripts(html);
        ensure_structure(&mut dom);
        // Everything the page will need first starts on its way now, all at once.
        let preload = std::sync::Arc::new(Preload::default());
        if let Some(fetch) = host.background() {
            preload.start(subresources(&dom, &base_url(&dom, url)), url, fetch);
        }
        let host: Rc<dyn Host> = Rc::new(Preloading { inner: host, preload: preload.clone() });
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
            cookies: volant_core::cookies::Jar::new(),
            caret: 0,
            hover: None,
            hover_rules: None,
            menu: None,
            canvases: HashMap::new(),
            scroll_to_anchor: false,
            current_script: None,
            ran: std::collections::HashSet::new(),
            loader: Rc::new(PageLoader { host: host.clone(), imports: RefCell::new(Vec::new()) }),
            host,
            protos: None,
            generation: 1,
            computed: None,
            base_cache: RefCell::new(None),
            owners: HashMap::new(),
            doc_urls: HashMap::new(),
            documents: HashMap::new(),
        }));
        let mut interp = Interp::new();
        // Real pages nest calls deeply (module loaders, jQuery); run on a big stack (see STACK).
        interp.max_depth = 3000;
        interp.base_url = url.into();
        interp.loader = Some(state.borrow().loader.clone());
        install(&mut interp, &state);
        interp.reset_steps(BUDGET);
        if let Err(e) = interp.run(PRELUDE) {
            interp.errors.push(format!("prelude: {e}"));
        }
        Runtime { interp, state, started: std::time::Instant::now(), preload, budget: BUDGET }
    }

    /// The page's network (preloaded files come from the preload), for its style sheets and the like.
    pub fn host(&self) -> Rc<dyn Host> {
        self.state.borrow().host.clone()
    }

    /// Milliseconds the page has spent waiting for preloaded files.
    pub fn preload_waited(&self) -> f64 {
        *self.preload.waited.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The document's scripts, in the order they run (deferred ones last).
    fn scripts(&self) -> Vec<NodeId> {
        let s = self.state.borrow();
        let (mut normal, mut deferred) = (Vec::new(), Vec::new());
        for id in descendants(&s.dom, ROOT) {
            if s.dom.tag(id) != Some("script") || !is_js(&s.dom, id) {
                continue;
            }
            // Modules and `defer`red scripts wait for the whole document.
            let module = script_kind(&s.dom, id) == Some(ScriptKind::Module);
            if module || (s.dom.attr(id, "defer").is_some() && s.dom.attr(id, "src").is_some()) {
                deferred.push(id);
            } else {
                normal.push(id);
            }
        }
        normal.extend(deferred);
        normal
    }

    /// Runs the page's scripts, then DOMContentLoaded and load, then what they scheduled (about a second's worth).
    pub fn run_scripts(&mut self) {
        for id in self.scripts() {
            if !self.state.borrow_mut().ran.insert(id) {
                continue;
            }
            self.interp.reset_steps(self.budget);
            execute(&mut self.interp, &self.state, id);
        }
        self.state.borrow_mut().current_script = None;
        self.interp.reset_steps(self.budget);
        let i = &mut self.interp;
        fire(i, &self.state, ROOT, "readystatechange", false);
        fire(i, &self.state, ROOT, "DOMContentLoaded", true);
        // The page's frames load first (same-origin ones are fetched now).
        let start = i.global_obj.borrow().get_own("__startFrames").cloned();
        if let Some(Slot::Data(f)) = start {
            let _ = i.call(&f, Value::Undefined, Vec::new());
            i.run_microtasks();
        }
        fire(i, &self.state, WINDOW, "load", false);
        fire(i, &self.state, WINDOW, "pageshow", false);
        self.interp.run_microtasks();
        self.settle(1000.0);
    }

    /// Runs timers due within the next `ms` of virtual time.
    fn settle(&mut self, ms: f64) {
        self.interp.reset_steps(self.budget);
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
        self.interp.reset_steps(self.budget);
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
        self.interp.reset_steps(self.budget);
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
                let mut v: Vec<char> = field_value(&s.dom, f).chars().collect();
                let at = s.caret.min(v.len());
                v.insert(at, c);
                set_field_value(&mut s.dom, f, &v.into_iter().collect::<String>());
                s.caret = at + 1;
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

    /// A special key: "Backspace", "Delete", "Enter", "Tab", "Escape", the arrows, "Home" or "End".
    pub fn key(&mut self, key: &str) {
        self.interp.reset_steps(self.budget);
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
            "ArrowLeft" => 37.0,
            "ArrowUp" => 38.0,
            "ArrowRight" => 39.0,
            "ArrowDown" => 40.0,
            "Delete" => 46.0,
            "Home" => 36.0,
            "End" => 35.0,
            _ => 0.0,
        }));
        if dispatch(i, &self.state, target, &ev) {
            let field = self.state.borrow().focus.filter(|&f| is_text_field(&self.state.borrow().dom, f));
            match (key, field) {
                ("Backspace" | "Delete", Some(f)) => {
                    let changed = {
                        let mut s = self.state.borrow_mut();
                        let mut v: Vec<char> = field_value(&s.dom, f).chars().collect();
                        let at = s.caret.min(v.len());
                        let gone = if key == "Backspace" { at.checked_sub(1) } else { (at < v.len()).then_some(at) };
                        if let Some(k) = gone {
                            v.remove(k);
                            set_field_value(&mut s.dom, f, &v.into_iter().collect::<String>());
                            s.caret = k;
                            s.generation += 1;
                        }
                        gone.is_some()
                    };
                    if changed {
                        let ev = make_event(i, &self.state, "input", true, false);
                        let _ = i.set(&ev, "inputType", str_val(if key == "Backspace" { "deleteContentBackward" } else { "deleteContentForward" }));
                        dispatch(i, &self.state, f, &ev);
                    }
                }
                ("ArrowLeft" | "ArrowRight" | "Home" | "End" | "ArrowUp" | "ArrowDown", Some(f)) => {
                    let mut s = self.state.borrow_mut();
                    let v: Vec<char> = field_value(&s.dom, f).chars().collect();
                    let at = s.caret.min(v.len());
                    // Up and Down move a line in a textarea (to its start or end in a single-line field).
                    let line_start = v[..at].iter().rposition(|&c| c == '\n').map(|p| p + 1).unwrap_or(0);
                    let line_end = v[at..].iter().position(|&c| c == '\n').map(|p| at + p).unwrap_or(v.len());
                    s.caret = match key {
                        "ArrowLeft" => at.saturating_sub(1),
                        "ArrowRight" => (at + 1).min(v.len()),
                        "Home" => line_start,
                        "End" => line_end,
                        "ArrowUp" => match line_start.checked_sub(1) {
                            Some(prev_end) => {
                                let prev_start = v[..prev_end].iter().rposition(|&c| c == '\n').map(|p| p + 1).unwrap_or(0);
                                (prev_start + (at - line_start)).min(prev_end)
                            }
                            None => 0,
                        },
                        _ => match v[line_end..].first() {
                            Some('\n') => {
                                let next_start = line_end + 1;
                                let next_end = v[next_start..].iter().position(|&c| c == '\n').map(|p| next_start + p).unwrap_or(v.len());
                                (next_start + (at - line_start)).min(next_end)
                            }
                            _ => v.len(),
                        },
                    };
                    s.generation += 1;
                }
                ("Enter", Some(f)) => {
                    let (is_area, form) = {
                        let s = self.state.borrow();
                        (s.dom.tag(f) == Some("textarea"), closest(&s.dom, f, |d, n| d.tag(n) == Some("form")))
                    };
                    if is_area {
                        {
                            let mut s = self.state.borrow_mut();
                            let mut v: Vec<char> = field_value(&s.dom, f).chars().collect();
                            let at = s.caret.min(v.len());
                            v.insert(at, '\n');
                            set_field_value(&mut s.dom, f, &v.into_iter().collect::<String>());
                            s.caret = at + 1;
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

    /// A `<select>` the person clicked, once, with its options for a menu.
    pub fn take_menu(&mut self) -> Option<(NodeId, Vec<MenuItem>)> {
        let n = self.state.borrow_mut().menu.take()?;
        let s = self.state.borrow();
        let chosen = crate::forms::selected_option(&s.dom, n);
        let mut items = Vec::new();
        fn walk(dom: &Dom, id: NodeId, chosen: Option<NodeId>, group_off: bool, out: &mut Vec<MenuItem>) {
            for &c in dom.children(id) {
                match dom.tag(c) {
                    Some("option") => out.push(MenuItem {
                        label: dom.attr(c, "label").map(str::to_string).unwrap_or_else(|| dom.text(c).split_whitespace().collect::<Vec<_>>().join(" ")),
                        disabled: group_off || dom.attr(c, "disabled").is_some(),
                        selected: Some(c) == chosen,
                        header: false,
                    }),
                    Some("optgroup") => {
                        out.push(MenuItem { label: dom.attr(c, "label").unwrap_or("").to_string(), disabled: true, selected: false, header: true });
                        walk(dom, c, chosen, group_off || dom.attr(c, "disabled").is_some(), out);
                    }
                    _ => {}
                }
            }
        }
        walk(&s.dom, n, chosen, false, &mut items);
        Some((n, items))
    }

    /// The person picked the `index`th option (counting options only, not group headers).
    pub fn choose(&mut self, select: NodeId, index: usize) {
        self.interp.reset_steps(self.budget);
        let changed = {
            let mut s = self.state.borrow_mut();
            if s.dom.tag(select) != Some("select") {
                return;
            }
            let options: Vec<NodeId> = descendants(&s.dom, select).into_iter().filter(|&o| s.dom.tag(o) == Some("option")).collect();
            let Some(&pick) = options.get(index) else { return };
            if s.dom.attr(pick, "disabled").is_some() {
                return;
            }
            let before = crate::forms::selected_option(&s.dom, select);
            for &o in &options {
                remove_attr(&mut s.dom, o, "selected");
            }
            set_attr(&mut s.dom, pick, "selected", "");
            s.generation += 1;
            before != Some(pick)
        };
        if changed {
            fire(&mut self.interp, &self.state, select, "input", true);
            fire(&mut self.interp, &self.state, select, "change", true);
        }
        self.settle(50.0);
    }

    /// The pointer is over `node` (or has left the page): fires mouseover/out
    /// and enter/leave, and tells whether a `:hover` style changed (so the page
    /// needs laying out again).
    pub fn hover(&mut self, node: Option<NodeId>) -> bool {
        let node = {
            let s = self.state.borrow();
            node.filter(|&n| (n as usize) < s.dom.nodes.len()).map(|n| if matches!(s.dom.nodes[n as usize].data, Data::Text(_)) { s.dom.parent(n) } else { n })
        };
        let old = self.state.borrow().hover;
        if old == node {
            return false;
        }
        self.interp.reset_steps(self.budget);
        let (old_chain, new_chain) = {
            let s = self.state.borrow();
            (old.map(|n| element_chain(&s.dom, n)).unwrap_or_default(), node.map(|n| element_chain(&s.dom, n)).unwrap_or_default())
        };
        let i = &mut self.interp;
        if let Some(o) = old.filter(|&o| connected(&self.state.borrow().dom, o)) {
            for ty in ["pointerout", "mouseout"] {
                fire(i, &self.state, o, ty, true);
            }
            for &n in old_chain.iter().filter(|n| !new_chain.contains(n)) {
                fire(i, &self.state, n, "mouseleave", false);
            }
        }
        self.state.borrow_mut().hover = node;
        if let Some(n) = node {
            for ty in ["pointerover", "mouseover"] {
                fire(i, &self.state, n, ty, true);
            }
            for &x in new_chain.iter().rev().filter(|x| !old_chain.contains(x)) {
                fire(i, &self.state, x, "mouseenter", false);
            }
        }
        let matters = self.hover_matters(&old_chain, &new_chain);
        if matters {
            self.state.borrow_mut().generation += 1;
        }
        self.settle(20.0);
        matters
    }

    /// Would moving the hover from `old` to `new` change what any `:hover` rule matches?
    fn hover_matters(&mut self, old: &[NodeId], new: &[NodeId]) -> bool {
        let rules = {
            let mut s = self.state.borrow_mut();
            let mut all = crate::style_sheets(&s.dom);
            all.extend(s.sheets.iter().cloned());
            let key = all.iter().map(|t| t.len()).sum::<usize>() ^ (all.len() << 48);
            match &s.hover_rules {
                Some((k, r)) if *k == key => r.clone(),
                _ => {
                    let r = Rc::new(style::hover_selectors(&all, &s.env));
                    s.hover_rules = Some((key, r.clone()));
                    r
                }
            }
        };
        if rules.is_empty() {
            return false;
        }
        let changed: Vec<NodeId> = old.iter().filter(|n| !new.contains(n)).chain(new.iter().filter(|n| !old.contains(n))).copied().collect();
        let mut s = self.state.borrow_mut();
        let dom = &mut s.dom;
        // A :hover rule can only change for the elements that changed state and what's inside them.
        let mut affected: Vec<NodeId> = Vec::new();
        for &c in &changed {
            affected.push(c);
            affected.extend(descendants(dom, c).into_iter().filter(|&n| dom.tag(n).is_some()));
            if affected.len() > 20_000 {
                return true;
            }
        }
        let mut seen = std::collections::HashSet::new();
        affected.retain(|n| seen.insert(*n));
        let matched = |dom: &Dom| -> Vec<bool> { affected.iter().flat_map(|&n| rules.iter().map(move |r| (n, r))).map(|(n, r)| style::matches(dom, n, r)).collect() };
        let mark = |dom: &mut Dom, chain: &[NodeId], on: bool| {
            for &n in chain {
                if on {
                    set_attr(dom, n, css::STATE_HOVER, "");
                } else {
                    remove_attr(dom, n, css::STATE_HOVER);
                }
            }
        };
        mark(dom, old, true);
        let before = matched(dom);
        mark(dom, old, false);
        mark(dom, new, true);
        let after = matched(dom);
        mark(dom, new, false);
        before != after
    }

    /// What a `<canvas>` has drawn (None if it has no 2D context yet).
    fn canvas(&mut self, node: NodeId) -> Option<std::sync::Arc<crate::canvas::Canvas>> {
        let wrapper = self.state.borrow().wrappers.get(&node).cloned()?;
        let i = &mut self.interp;
        let ctx = i.get(&wrapper, "__ctx").ok().filter(|v| matches!(v, Value::Obj(_)))?;
        let Ok(Value::Obj(list)) = i.get(&ctx, "_ops") else { return None };
        let (w, h) = {
            let s = self.state.borrow();
            let px = |k: &str, d: f32| s.dom.attr(node, k).and_then(|v| v.trim().parse::<f32>().ok()).filter(|v| *v > 0.0).unwrap_or(d);
            (px("width", 300.0), px("height", 150.0))
        };
        let ops: Vec<Value> = match &list.borrow().kind {
            Kind::Array(a) => a.clone(),
            _ => return None,
        };
        let key = (Rc::as_ptr(&list) as usize, ops.len());
        if let Some((p, n, c)) = self.state.borrow().canvases.get(&node) {
            if (*p, *n) == key && c.width == w && c.height == h {
                return Some(c.clone());
            }
        }
        let c = std::sync::Arc::new(crate::canvas::Canvas { width: w, height: h, ops: ops.iter().filter_map(decode_canvas_op).collect() });
        self.state.borrow_mut().canvases.insert(node, (key.0, key.1, c.clone()));
        Some(c)
    }

    /// Whether the page moved to a `#fragment` since the last call (so the app scrolls to it).
    pub fn take_anchor_scroll(&mut self) -> bool {
        std::mem::take(&mut self.state.borrow_mut().scroll_to_anchor)
    }

    /// Where the page asked to go (a link, a form, `location.href = …`), once.
    pub fn take_navigation(&mut self) -> Option<Navigation> {
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
        let mut dom = Dom { nodes: vec![Node { parent: ROOT, children: Vec::new(), data: Data::Document }], ns: Default::default(), attr_ns: Default::default() };
        let mut map = vec![ROOT];
        // Interaction states, marked on the copy for `:hover`, `:focus`, `:focus-within` and `:target`.
        let mut marks: HashMap<NodeId, Vec<&'static str>> = HashMap::new();
        if let Some(h) = s.hover.filter(|&h| connected(&s.dom, h)) {
            for n in element_chain(&s.dom, h) {
                marks.entry(n).or_default().push(css::STATE_HOVER);
            }
        }
        if let Some(f) = s.focus.filter(|&f| connected(&s.dom, f)) {
            marks.entry(f).or_default().push(css::STATE_FOCUS);
            for n in element_chain(&s.dom, f) {
                marks.entry(n).or_default().push(css::STATE_FOCUS_WITHIN);
            }
        }
        if let Some(frag) = s.url.split_once('#').map(|(_, f)| percent_decode(f)).filter(|f| !f.is_empty()) {
            if let Some(t) = anchor_target(&s.dom, &frag) {
                marks.entry(t).or_default().push(css::STATE_TARGET);
            }
        }
        struct Copy<'a> {
            src: &'a Dom,
            marks: &'a HashMap<NodeId, Vec<&'static str>>,
            focus: Option<NodeId>,
            focus_copy: Option<NodeId>,
            /// (copy, original) for each canvas.
            canvases: Vec<(NodeId, NodeId)>,
        }
        fn copy(c: &mut Copy, sid: NodeId, dst: &mut Dom, parent: NodeId, map: &mut Vec<NodeId>) {
            let data = match &c.src.nodes[sid as usize].data {
                Data::Element { name, .. } if name.starts_with('#') => return,
                Data::Element { name, attrs } => {
                    let mut attrs = attrs.clone();
                    if let Some(m) = c.marks.get(&sid) {
                        attrs.extend(m.iter().map(|k| (k.to_string().into(), String::new().into())));
                    }
                    Data::Element { name: name.clone(), attrs }
                }
                Data::Text(t) => Data::Text(t.clone()),
                Data::Document | Data::Comment(_) | Data::Doctype { .. } | Data::Pi { .. } => return,
            };
            let id = dst.nodes.len() as NodeId;
            dst.nodes.push(Node { parent, children: Vec::new(), data });
            dst.nodes[parent as usize].children.push(id);
            map.push(sid);
            if c.focus == Some(sid) {
                c.focus_copy = Some(id);
            }
            if c.src.tag(sid) == Some("canvas") {
                c.canvases.push((id, sid));
            }
            for &k in c.src.children(sid) {
                copy(c, k, dst, id, map);
            }
        }
        let mut cp = Copy { src: &s.dom, marks: &marks, focus: s.focus, focus_copy: None, canvases: Vec::new() };
        for &c in s.dom.children(ROOT) {
            copy(&mut cp, c, &mut dom, ROOT, &mut map);
        }
        let caret = match (cp.focus_copy, s.focus) {
            (Some(copy), Some(f)) if is_text_field(&s.dom, f) => Some((copy, s.caret.min(field_value(&s.dom, f).chars().count()))),
            _ => None,
        };
        let before = dom.nodes.len();
        crate::forms::prepare_with(&mut dom, caret);
        // What form controls added maps to the control.
        for k in before..dom.nodes.len() {
            let mut p = dom.nodes[k].parent as usize;
            while p >= before {
                p = dom.nodes[p].parent as usize;
            }
            map.push(map[p]);
        }
        let title = dom.find("title").map(|t| dom.text(t).split_whitespace().collect::<Vec<_>>().join(" ")).unwrap_or_default();
        let (url, generation, canvas_nodes) = (s.url.clone(), s.generation, cp.canvases);
        drop(s);
        let mut canvases = HashMap::new();
        for (copy, orig) in canvas_nodes {
            if let Some(c) = self.canvas(orig) {
                canvases.insert(crate::canvas::src_for(copy), c);
            }
        }
        Snapshot { dom, canvases, map, title, url, generation }
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
