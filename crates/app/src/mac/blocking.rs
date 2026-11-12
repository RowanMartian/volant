//! Block ads and trackers, powered by uBlock.
//!
//! uBlock Origin's filter lists are downloaded and compiled (volant-core's
//! `ublock`) into WebKit content-rule lists plus a site index of scriptlets
//! and cosmetic filters. WebKit keeps compiled rule lists on disk, so at
//! launch Volant only looks them up by name; nothing is parsed.
//!
//! Every tab has its own content controller. Just before a page loads,
//! [`prepare_navigation`] gives that tab the rule lists (or none, on a trusted
//! site) and the scriptlets and cosmetic filters for that one site.

use super::{ns, on_main, pages, with, Browser};
use block2::RcBlock;
use objc2::rc::Retained;
use objc2::MainThreadOnly;
use objc2_foundation::{NSArray, NSData, NSError, NSString, NSURL};
use objc2_web_kit::{WKContentRuleList, WKContentRuleListStore, WKUserContentController, WKUserScript, WKUserScriptInjectionTime};
use std::path::PathBuf;
use volant_core::json::{self, Obj};
use volant_core::ublock::{self, Options, SiteIndex};

const SCRIPTLETS_JS: &str = include_str!("../../../../assets/ublock/scriptlets.js");
const COSMETIC_JS: &str = include_str!("../../../../assets/ublock/cosmetic.js");
const PICKER_JS: &str = include_str!("../../../../assets/ublock/picker.js");

/// Bump when the compiler changes what it emits, so compiled lists are rebuilt.
const COMPILER_VERSION: u32 = 3;
/// uBlock Origin refreshes lists every few days; so does Volant.
const UPDATE_EVERY: u64 = 4 * 86_400;

#[derive(Default)]
pub struct State {
    /// The compiled lists every (untrusted) tab gets.
    pub lists: Vec<Retained<WKContentRuleList>>,
    pub sites: SiteIndex,
    pub trusted: Vec<String>,
    pub my_filters: String,
    pub counts: Counts,
    /// What the blocker is doing right now, for the Settings pages ("" when idle).
    pub status: String,
    pub error: String,
    pub picker_nonce: String,
    /// How long the last download / compile took, for the self-test report.
    pub timings: String,
    generation: u64,
    busy: bool,
}

#[derive(Default, Clone, Copy)]
pub struct Counts {
    pub network: usize,
    pub cosmetic: usize,
    pub scriptlets: usize,
    pub procedural: usize,
}

fn dir(b: &Browser) -> PathBuf {
    let d = b.paths.data.join("ublock");
    let _ = std::fs::create_dir_all(d.join("lists"));
    d
}

fn list_path(b: &Browser, id: &str) -> PathBuf {
    dir(b).join("lists").join(format!("{id}.txt"))
}

fn options(b: &Browser) -> Options {
    Options { cosmetic: b.settings.ublock_cosmetic, block_fonts: b.settings.ublock_fonts, block_pings: b.settings.ublock_pings }
}

fn fnv(s: &str) -> u64 {
    s.bytes().fold(0xcbf29ce484222325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3))
}

/// Names this exact build of the rules: lists, options, My filters and when the lists were fetched.
fn build_key(b: &Browser) -> String {
    let o = options(b);
    let what = format!(
        "{COMPILER_VERSION}|{}|{}|{}|{}|{}|{}",
        b.settings.ublock_lists.join(","),
        o.cosmetic,
        o.block_fonts,
        o.block_pings,
        b.settings.lists_updated,
        fnv(&b.blocking.my_filters)
    );
    format!("{:016x}", fnv(&what))
}

pub fn init(b: &mut Browser) {
    let d = dir(b);
    b.blocking.trusted = std::fs::read_to_string(d.join("trusted-sites.txt"))
        .map(|t| t.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).map(String::from).collect())
        .unwrap_or_default();
    b.blocking.my_filters = std::fs::read_to_string(d.join("my-filters.txt")).unwrap_or_default();
    b.blocking.picker_nonce = format!("{:016x}", fnv(&format!("{:?}{}", std::time::SystemTime::now(), std::process::id())));
    if !b.settings.ublock {
        return;
    }
    // Compiled already? Look the lists up by name: no parsing, no compiling.
    let manifest = std::fs::read_to_string(d.join("compiled.txt")).unwrap_or_default();
    let mut lines = manifest.lines();
    let key = build_key(b);
    if lines.next() == Some(key.as_str()) {
        let ids: Vec<String> = lines.next().unwrap_or("").split(',').filter(|s| !s.is_empty()).map(String::from).collect();
        let counts: Vec<usize> = lines.next().unwrap_or("").split(',').filter_map(|s| s.parse().ok()).collect();
        if let [n, c, s, p] = counts[..] {
            b.blocking.counts = Counts { network: n, cosmetic: c, scriptlets: s, procedural: p };
        }
        b.blocking.sites = SiteIndex::parse(&std::fs::read_to_string(d.join("sites.txt")).unwrap_or_default());
        if !ids.is_empty() {
            look_up(b, ids);
            maybe_update(b);
            return;
        }
    }
    rebuild(b);
}

/// Refreshes the lists if they're older than a few days.
pub fn maybe_update(b: &mut Browser) {
    if b.settings.ublock && super::now().saturating_sub(b.settings.lists_updated) > UPDATE_EVERY {
        download(b, true);
    }
}

fn look_up(b: &mut Browser, ids: Vec<String>) {
    let Some(store) = (unsafe { WKContentRuleListStore::defaultStore(b.mtm) }) else {
        b.blocking.busy = false;
        return;
    };
    let generation = b.blocking.generation;
    let total = ids.len();
    // (answers so far, the lists found in order)
    type Found = (usize, Vec<Option<Retained<WKContentRuleList>>>);
    let found: std::rc::Rc<std::cell::RefCell<Found>> = std::rc::Rc::new(std::cell::RefCell::new((0, vec![None; total])));
    for (i, id) in ids.into_iter().enumerate() {
        let found = found.clone();
        let done = RcBlock::new(move |list: *mut WKContentRuleList, _e: *mut NSError| {
            let mut f = found.borrow_mut();
            f.1[i] = unsafe { Retained::retain(list) };
            f.0 += 1;
            if f.0 == total {
                let all = std::mem::take(&mut f.1);
                with(move |b| {
                    if b.blocking.generation != generation {
                        return;
                    }
                    // Any list missing (WebKit cleared its store): rebuild everything.
                    if all.iter().all(Option::is_some) {
                        install(b, all.into_iter().flatten().collect());
                    } else {
                        rebuild(b);
                    }
                });
            }
        });
        unsafe { store.lookUpContentRuleListForIdentifier_completionHandler(Some(&ns(&id)), Some(&done)) };
    }
}

fn status(b: &mut Browser, text: &str) {
    b.blocking.status = text.to_string();
    push_status(b);
}

/// Tells Volant's own pages how the blocker is doing.
pub fn push_status(b: &Browser) {
    pages::notify(b, &format!("window.volantBlocker && volantBlocker({})", status_json(b)));
}

pub fn status_json(b: &Browser) -> String {
    let c = b.blocking.counts;
    Obj::new()
        .bool("on", b.settings.ublock)
        .num("network", c.network)
        .num("cosmetic", c.cosmetic)
        .num("scriptlets", c.scriptlets)
        .num("procedural", c.procedural)
        .str("status", &b.blocking.status)
        .str("error", &b.blocking.error)
        .num("updated", b.settings.lists_updated)
        .done()
}

/// Recompiles after a change (lists, options, My filters), downloading lists it doesn't have yet.
pub fn rebuild(b: &mut Browser) {
    b.blocking.generation += 1;
    if !b.settings.ublock {
        b.blocking.lists.clear();
        apply_to_all_tabs(b);
        status(b, "");
        return;
    }
    let missing: Vec<String> = b.settings.ublock_lists.iter().filter(|id| !list_path(b, id).exists()).cloned().collect();
    if !missing.is_empty() {
        download(b, false);
        return;
    }
    compile(b);
}

/// Downloads the selected lists (all of them, or just the missing ones), then recompiles.
pub fn download(b: &mut Browser, all: bool) {
    if b.blocking.busy {
        return;
    }
    let want: Vec<(String, String, PathBuf)> = b
        .settings
        .ublock_lists
        .iter()
        .filter_map(|id| ublock::list(id))
        .map(|l| (l.id.to_string(), l.url.to_string(), list_path(b, l.id)))
        .filter(|(_, _, p)| all || !p.exists())
        .collect();
    if want.is_empty() {
        compile(b);
        return;
    }
    b.blocking.busy = true;
    status(b, "Downloading filter lists…");
    let started = std::time::Instant::now();
    std::thread::spawn(move || {
        let mut failed = Vec::new();
        for (id, url, path) in &want {
            let data = NSURL::URLWithString(&NSString::from_str(url)).and_then(|u| NSData::dataWithContentsOfURL(&u));
            match data {
                // A real list is more than a few hundred bytes; anything smaller is an error page.
                Some(d) if d.length() > 200 => {
                    let tmp = path.with_extension("tmp");
                    if std::fs::write(&tmp, d.to_vec()).and_then(|_| std::fs::rename(&tmp, path)).is_err() {
                        failed.push(id.clone());
                    }
                }
                _ => failed.push(id.clone()),
            }
        }
        let took = started.elapsed().as_millis();
        on_main(move || {
            with(move |b| {
                b.blocking.busy = false;
                b.blocking.timings.push_str(&format!("download ({} lists): {took} ms; ", want.len()));
                b.blocking.error = if failed.is_empty() { String::new() } else { format!("Couldn't download {} of the lists. Using what's here.", failed.len()) };
                if failed.len() < want.len() {
                    b.settings.lists_updated = super::now();
                    b.settings.save(&b.paths.settings);
                }
                compile(b);
            })
        });
    });
}

/// Compiles the lists on a background thread, then has WebKit compile the rule lists.
fn compile(b: &mut Browser) {
    if b.blocking.busy {
        return;
    }
    b.blocking.busy = true;
    b.blocking.generation += 1;
    let generation = b.blocking.generation;
    status(b, "Compiling filters…");
    let sources: Vec<(String, PathBuf)> = b.settings.ublock_lists.iter().map(|id| (id.clone(), list_path(b, id))).collect();
    let my = b.blocking.my_filters.clone();
    let opts = options(b);
    let key = build_key(b);
    let d = dir(b);
    std::thread::spawn(move || {
        let started = std::time::Instant::now();
        let mut texts: Vec<(String, String)> = sources.iter().filter_map(|(id, p)| std::fs::read_to_string(p).ok().map(|t| (id.clone(), t))).collect();
        if texts.is_empty() {
            // Nothing downloaded yet (offline first launch): Volant's small built-in list.
            texts.push(("fallback".into(), ublock::FALLBACK.to_string()));
        }
        texts.push(("my-filters".into(), my));
        let refs: Vec<(&str, &str)> = texts.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
        let c = ublock::compile(&refs, opts);
        let _ = std::fs::write(d.join("sites.txt"), c.sites.serialize());
        let mut lists: Vec<String> = c.network;
        if !c.cosmetic.is_empty() {
            lists.push(c.cosmetic);
        }
        lists.push(c.extras);
        let counts = Counts { network: c.stats.network_rules, cosmetic: c.stats.cosmetic_rules, scriptlets: c.stats.scriptlets, procedural: c.stats.procedural };
        let sites_text = c.sites.serialize();
        let took = started.elapsed();
        eprintln!("ublock: compiled {:?} in {took:?}", c.stats);
        let rust_ms = took.as_millis();
        on_main(move || {
            with(move |b| {
                if b.blocking.generation != generation {
                    // Something changed while we compiled: start again with the new settings.
                    b.blocking.busy = false;
                    rebuild(b);
                    return;
                }
                b.blocking.counts = counts;
                b.blocking.sites = SiteIndex::parse(&sites_text);
                b.blocking.timings.push_str(&format!("compile (Rust): {rust_ms} ms; "));
                webkit_compile(b, generation, key, lists, counts);
            })
        });
    });
}

fn webkit_compile(b: &mut Browser, generation: u64, key: String, lists: Vec<String>, counts: Counts) {
    let Some(store) = (unsafe { WKContentRuleListStore::defaultStore(b.mtm) }) else { return };
    status(b, "Compiling filters…");
    let ids: Vec<String> = (0..lists.len()).map(|i| format!("volant-ub-{key}-{i}")).collect();
    let started = std::time::Instant::now();
    let results: std::rc::Rc<std::cell::RefCell<Vec<Option<Result<Retained<WKContentRuleList>, String>>>>> =
        std::rc::Rc::new(std::cell::RefCell::new(vec![None; lists.len()]));
    for (i, json) in lists.into_iter().enumerate() {
        let results = results.clone();
        let ids2 = ids.clone();
        let key = key.clone();
        let done = RcBlock::new(move |list: *mut WKContentRuleList, err: *mut NSError| {
            let r = match unsafe { Retained::retain(list) } {
                Some(l) => Ok(l),
                None => Err(unsafe { err.as_ref() }.map(|e| e.localizedDescription().to_string()).unwrap_or_else(|| "unknown error".into())),
            };
            let mut all = results.borrow_mut();
            all[i] = Some(r);
            if all.iter().all(Option::is_some) {
                let outcome: Vec<Result<Retained<WKContentRuleList>, String>> = all.drain(..).flatten().collect();
                let ids = ids2.clone();
                let key = key.clone();
                let took = started.elapsed();
                with(move |b| {
                    b.blocking.busy = false;
                    if b.blocking.generation != generation {
                        rebuild(b);
                        return;
                    }
                    let errors: Vec<String> = outcome.iter().filter_map(|r| r.as_ref().err().cloned()).collect();
                    let lists: Vec<Retained<WKContentRuleList>> = outcome.into_iter().filter_map(Result::ok).collect();
                    eprintln!("ublock: WebKit compiled {} lists in {took:?} ({} failed)", lists.len(), errors.len());
                    b.blocking.timings.push_str(&format!("compile (WebKit, {} lists): {} ms; ", lists.len() + errors.len(), took.as_millis()));
                    if !errors.is_empty() {
                        b.blocking.error = format!("WebKit couldn't compile part of the filters: {}", errors[0]);
                    } else {
                        let manifest = format!("{key}\n{}\n{},{},{},{}\n", ids.join(","), counts.network, counts.cosmetic, counts.scriptlets, counts.procedural);
                        let _ = volant_core::store::write_atomic(&dir(b).join("compiled.txt"), &manifest);
                    }
                    install(b, lists);
                    status(b, "");
                    cleanup(b, ids);
                });
            }
        });
        unsafe { store.compileContentRuleListForIdentifier_encodedContentRuleList_completionHandler(Some(&ns(&ids[i])), Some(&ns(&json)), Some(&done)) };
    }
}

/// Removes compiled lists from earlier builds.
fn cleanup(b: &Browser, keep: Vec<String>) {
    let Some(store) = (unsafe { WKContentRuleListStore::defaultStore(b.mtm) }) else { return };
    let store2 = store.clone();
    let done = RcBlock::new(move |ids: *mut NSArray<NSString>| {
        let Some(ids) = (unsafe { ids.as_ref() }) else { return };
        for id in ids.iter() {
            let s = id.to_string();
            if s.starts_with("volant-") && !keep.contains(&s) {
                unsafe { store2.removeContentRuleListForIdentifier_completionHandler(Some(&id), None) };
            }
        }
    });
    unsafe { store.getAvailableContentRuleListIdentifiers(Some(&done)) };
}

fn install(b: &mut Browser, lists: Vec<Retained<WKContentRuleList>>) {
    b.blocking.lists = lists;
    apply_to_all_tabs(b);
    push_status(b);
}

/// Gives every open tab the current lists (or none, if its site is trusted).
fn apply_to_all_tabs(b: &Browser) {
    for w in b.webs.values() {
        let host = unsafe { w.web.URL() }.and_then(|u| u.host()).map(|h| h.to_string()).unwrap_or_default();
        set_lists(b, &w.ucc, &host);
    }
}

fn set_lists(b: &Browser, ucc: &WKUserContentController, host: &str) {
    unsafe { ucc.removeAllContentRuleLists() };
    if !b.settings.ublock || ublock::is_trusted(host, &b.blocking.trusted) {
        return;
    }
    for l in &b.blocking.lists {
        unsafe { ucc.addContentRuleList(l) };
    }
}

pub fn is_busy(b: &Browser) -> bool {
    b.blocking.busy
}

pub fn site_on(b: &Browser, host: &str) -> bool {
    b.settings.ublock && !host.is_empty() && !ublock::is_trusted(host, &b.blocking.trusted)
}

/// The script injected into a page at `host`: its scriptlets and cosmetic filters.
pub fn page_script(b: &Browser, host: &str) -> Option<String> {
    if !site_on(b, host) {
        return None;
    }
    let bundle = b.blocking.sites.lookup(host);
    let mut calls: Vec<Vec<String>> = bundle.scriptlets.iter().map(|c| ublock::scriptlet_args(c)).collect();
    if b.settings.ublock_webrtc {
        calls.push(vec!["nowebrtc".into()]);
    }
    let mut js = String::new();
    if !calls.is_empty() {
        let calls_json = json::array(calls.iter().map(|c| json::array(c.iter().map(|a| json::string(a)))));
        js.push_str(SCRIPTLETS_JS.trim_end());
        js.push_str(&format!("({calls_json});\n"));
    }
    if b.settings.ublock_cosmetic && (!bundle.hide.is_empty() || !bundle.procedural.is_empty()) {
        let strs = |v: &[String]| json::array(v.iter().map(|s| json::string(s)));
        js.push_str(COSMETIC_JS.trim_end());
        js.push_str(&format!("({}, {});\n", strs(&bundle.hide), strs(&bundle.procedural)));
    }
    (!js.is_empty()).then_some(js)
}

/// Called just before a page loads in a tab: the right lists and scripts for its site.
pub fn prepare_navigation(b: &Browser, ucc: &WKUserContentController, host: &str) {
    set_lists(b, ucc, host);
    unsafe { ucc.removeAllUserScripts() };
    if let Some(js) = page_script(b, host) {
        let script = unsafe {
            WKUserScript::initWithSource_injectionTime_forMainFrameOnly(WKUserScript::alloc(b.mtm), &ns(&js), WKUserScriptInjectionTime::AtDocumentStart, true)
        };
        unsafe { ucc.addUserScript(&script) };
    }
}

// ---------------------------------------------------------------- what the pages change

fn save_trusted(b: &Browser) {
    let _ = volant_core::store::write_atomic(&dir(b).join("trusted-sites.txt"), &(b.blocking.trusted.join("\n") + "\n"));
}

/// The shield button: turn blocking off (or back on) for the site in front, and reload it.
pub fn toggle_site(b: &mut Browser) {
    let Some(w) = b.tabs.active.and_then(|id| b.webs.get(&id)) else { return };
    let Some(host) = unsafe { w.web.URL() }.and_then(|u| u.host()).map(|h| h.to_string()) else { return };
    let bare = host.strip_prefix("www.").unwrap_or(&host).to_ascii_lowercase();
    if ublock::is_trusted(&host, &b.blocking.trusted) {
        b.blocking.trusted.retain(|t| !ublock::is_trusted(&host, std::slice::from_ref(t)));
    } else {
        b.blocking.trusted.push(bare);
    }
    save_trusted(b);
    let web = w.web.clone();
    let ucc = w.ucc.clone();
    prepare_navigation(b, &ucc, &host);
    unsafe { web.reload() };
    super::ui::update_chrome(b);
    push_status(b);
}

pub fn set_trusted(b: &mut Browser, text: &str) {
    b.blocking.trusted = text.lines().map(|l| l.trim().trim_start_matches("https://").trim_start_matches("http://").trim_end_matches('/').to_ascii_lowercase()).filter(|l| !l.is_empty()).collect();
    save_trusted(b);
    apply_to_all_tabs(b);
}

pub fn set_my_filters(b: &mut Browser, text: &str) {
    b.blocking.my_filters = text.to_string();
    let _ = volant_core::store::write_atomic(&dir(b).join("my-filters.txt"), text);
    rebuild(b);
}

pub fn set_lists_selected(b: &mut Browser, ids: &str) {
    b.settings.set("ublock_lists", ids);
    b.settings.save(&b.paths.settings);
    rebuild(b);
}

/// Starts the element picker in the tab in front.
pub fn pick_element(b: &Browser) {
    let Some(w) = b.tabs.active.and_then(|id| b.webs.get(&id)) else { return };
    let url = unsafe { w.web.URL() }.and_then(|u| u.absoluteString()).map(|s| s.to_string()).unwrap_or_default();
    if !url.starts_with("http") {
        return;
    }
    let js = format!("{}({});", PICKER_JS.trim_end(), json::string(&b.blocking.picker_nonce));
    unsafe { w.web.evaluateJavaScript_completionHandler(&ns(&js), None) };
}

/// The picker chose something: save `host##selector` to My filters.
pub fn picked(b: &mut Browser, message: &str) {
    let field = |k: &str| json::strings_for(message, k).into_iter().next().unwrap_or_default();
    if field("nonce") != b.blocking.picker_nonce {
        return; // not from Volant's picker
    }
    let (host, selector) = (field("host"), field("selector"));
    if host.is_empty() || selector.is_empty() || selector.contains(['\n', '{', '}']) {
        return;
    }
    let host = host.strip_prefix("www.").unwrap_or(&host).to_string();
    let mut text = b.blocking.my_filters.clone();
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(&format!("{host}##{selector}\n"));
    set_my_filters(b, &text);
}

/// The data the blocker settings page shows.
pub fn page_data(b: &Browser) -> String {
    let lists = json::array(ublock::CATALOG.iter().map(|l| {
        let on = b.settings.ublock_lists.iter().any(|x| x == l.id);
        let have = list_path(b, l.id).exists();
        Obj::new().str("id", l.id).str("title", l.title).str("group", l.group).bool("on", on).bool("downloaded", have).bool("default", l.default_on).done()
    }));
    let s = &b.settings;
    Obj::new()
        .raw("status", &status_json(b))
        .raw("lists", &lists)
        .bool("cosmetic", s.ublock_cosmetic)
        .bool("fonts", s.ublock_fonts)
        .bool("noscript", s.ublock_noscript)
        .bool("pings", s.ublock_pings)
        .bool("webrtc", s.ublock_webrtc)
        .str("myFilters", &b.blocking.my_filters)
        .str("trusted", &b.blocking.trusted.join("\n"))
        .done()
}
