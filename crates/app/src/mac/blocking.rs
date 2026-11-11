//! Turns filter lists into compiled WebKit content rules and installs them.
//!
//! WebKit keeps compiled rule lists on disk, so after the first run Volant
//! just looks the list up by name: no parsing, no compiling, at launch.

use super::{ns, on_main, pages, with, Browser};
use block2::RcBlock;
use objc2::rc::Retained;
use objc2_foundation::{NSArray, NSData, NSError, NSString, NSURL};
use objc2_web_kit::{WKContentRuleList, WKContentRuleListStore};
use volant_core::blocker::{self, BUILTIN, STRICT_LISTS};
use volant_core::store::Blocking;

#[derive(Default)]
pub struct State {
    pub rules: usize,
    pub error: String,
    generation: u64,
    downloading: bool,
}

/// Bump when the compiler changes what it emits, so cached lists are rebuilt.
const COMPILER_VERSION: u32 = 1;
const WEEK: u64 = 7 * 86_400;

fn fnv(s: &str) -> u64 {
    s.bytes().fold(0xcbf29ce484222325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3))
}

pub fn init(b: &mut Browser) {
    apply(b);
    if b.settings.blocking == Blocking::Strict && super::now().saturating_sub(b.settings.lists_updated) > WEEK {
        download_lists(b, false);
    }
}

fn list_path(b: &Browser, i: usize) -> std::path::PathBuf {
    b.paths.lists.join(format!("list{i}.txt"))
}

fn have_lists(b: &Browser) -> bool {
    (0..STRICT_LISTS.len()).all(|i| list_path(b, i).exists())
}

/// Installs the rules for the current blocking level (compiling them if needed).
pub fn apply(b: &mut Browser) {
    b.blocking.generation += 1;
    b.blocking.rules = 0;
    b.blocking.error.clear();
    let generation = b.blocking.generation;
    unsafe { b.content.removeAllContentRuleLists() };
    let level = b.settings.blocking;
    if level == Blocking::Off {
        status(b);
        return;
    }
    let strict = level == Blocking::Strict && have_lists(b);
    if level == Blocking::Strict && !have_lists(b) {
        download_lists(b, false);
    }
    let id = if strict {
        format!("volant-strict-{COMPILER_VERSION}-{:x}-{}", fnv(BUILTIN), b.settings.lists_updated)
    } else {
        format!("volant-standard-{COMPILER_VERSION}-{:x}", fnv(BUILTIN))
    };
    let paths: Vec<std::path::PathBuf> = if strict { (0..STRICT_LISTS.len()).map(|i| list_path(b, i)).collect() } else { vec![] };
    let count_file = b.paths.lists.join(format!("{id}.count"));
    let Some(store) = (unsafe { WKContentRuleListStore::defaultStore(b.mtm) }) else { return };

    let id2 = id.clone();
    let store2 = store.clone();
    let looked_up = RcBlock::new(move |list: *mut WKContentRuleList, _err: *mut NSError| {
        if let Some(list) = unsafe { Retained::retain(list) } {
            let count = std::fs::read_to_string(&count_file).ok().and_then(|s| s.trim().parse().ok()).unwrap_or(0);
            with(move |b| install(b, generation, &list, count));
            return;
        }
        // Not compiled yet: translate the lists on a background thread, then let WebKit compile them.
        let (id, store, paths, count_file) = (id2.clone(), store2.clone(), paths.clone(), count_file.clone());
        let store_ptr = MainBox(store);
        std::thread::spawn(move || {
            let texts: Vec<String> = paths.iter().filter_map(|p| std::fs::read_to_string(p).ok()).collect();
            let mut all: Vec<&str> = vec![BUILTIN];
            all.extend(texts.iter().map(String::as_str));
            let compiled = blocker::compile(&all);
            let _ = std::fs::write(&count_file, compiled.rules.to_string());
            let rules = compiled.rules;
            let json = compiled.json;
            on_main(move || {
                let store = store_ptr;
                let done = RcBlock::new(move |list: *mut WKContentRuleList, err: *mut NSError| match unsafe { Retained::retain(list) } {
                    Some(list) => with(move |b| install(b, generation, &list, rules)),
                    None => {
                        let msg = unsafe { err.as_ref() }.map(|e| e.localizedDescription().to_string()).unwrap_or_default();
                        with(move |b| {
                            if b.blocking.generation == generation {
                                b.blocking.error = format!("Couldn't compile the rules: {msg}");
                                status(b);
                            }
                        });
                    }
                });
                unsafe { store.0.compileContentRuleListForIdentifier_encodedContentRuleList_completionHandler(Some(&ns(&id)), Some(&ns(&json)), Some(&done)) };
            });
        });
    });
    unsafe { store.lookUpContentRuleListForIdentifier_completionHandler(Some(&ns(&id)), Some(&looked_up)) };
}

/// WebKit objects are fine to hand back to the main thread untouched.
struct MainBox<T>(T);
// SAFETY: the value is only used again on the main thread.
unsafe impl<T> Send for MainBox<T> {}

fn install(b: &mut Browser, generation: u64, list: &WKContentRuleList, rules: usize) {
    if b.blocking.generation != generation {
        return; // The level changed while this was compiling.
    }
    unsafe {
        b.content.removeAllContentRuleLists();
        b.content.addContentRuleList(list);
    }
    b.blocking.rules = rules;
    status(b);
    cleanup(b, unsafe { list.identifier() }.to_string());
}

/// Removes compiled lists we no longer use.
fn cleanup(b: &Browser, keep: String) {
    let Some(store) = (unsafe { WKContentRuleListStore::defaultStore(b.mtm) }) else { return };
    let store2 = store.clone();
    let done = RcBlock::new(move |ids: *mut NSArray<NSString>| {
        let Some(ids) = (unsafe { ids.as_ref() }) else { return };
        for id in ids.iter() {
            let s = id.to_string();
            if s.starts_with("volant-") && s != keep {
                unsafe { store2.removeContentRuleListForIdentifier_completionHandler(Some(&id), None) };
            }
        }
    });
    unsafe { store.getAvailableContentRuleListIdentifiers(Some(&done)) };
}

fn status(b: &Browser) {
    let err = volant_core::json::string(&b.blocking.error);
    pages::notify(b, &format!("window.volantRules && volantRules({}, {err})", b.blocking.rules));
}

/// Downloads EasyList and EasyPrivacy (Strict mode), then recompiles.
pub fn download_lists(b: &mut Browser, force: bool) {
    if b.blocking.downloading || (!force && have_lists(b) && super::now().saturating_sub(b.settings.lists_updated) < WEEK) {
        return;
    }
    b.blocking.downloading = true;
    let targets: Vec<(String, std::path::PathBuf)> = STRICT_LISTS.iter().enumerate().map(|(i, (_, url))| (url.to_string(), list_path(b, i))).collect();
    std::thread::spawn(move || {
        let mut ok = true;
        for (url, path) in &targets {
            let data = NSURL::URLWithString(&NSString::from_str(url)).and_then(|u| NSData::dataWithContentsOfURL(&u));
            match data {
                // A real list is hundreds of KB; anything tiny is an error page.
                Some(d) if d.length() > 10_000 => {
                    let tmp = path.with_extension("tmp");
                    ok &= std::fs::write(&tmp, d.to_vec()).and_then(|_| std::fs::rename(&tmp, path)).is_ok();
                }
                _ => ok = false,
            }
        }
        on_main(move || {
            with(move |b| {
                b.blocking.downloading = false;
                if ok {
                    b.settings.lists_updated = super::now();
                    b.settings.save(&b.paths.settings);
                    apply(b);
                } else {
                    b.blocking.error = "Couldn't download the filter lists. Using the built-in list.".into();
                    status(b);
                }
            })
        });
    });
}
