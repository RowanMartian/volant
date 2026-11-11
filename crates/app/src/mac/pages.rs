//! Volant's own pages (volant://welcome, newtab, settings, history) and the
//! messages they send back.

use super::{actions, blocking, ns, theme, ui, Browser};
use block2::RcBlock;
use objc2_app_kit::NSWorkspace;
use objc2_foundation::{NSBundle, NSDate, NSError, NSURL};
use objc2_web_kit::WKWebsiteDataStore;
use volant_core::json::{self, Obj};
use volant_core::search::{self, ENGINES};
use volant_core::tabs::TabId;

const STYLE: &str = include_str!("../../../../assets/pages/style.css");
const SCRIPT: &str = include_str!("../../../../assets/pages/volant.js");
const WELCOME: &str = include_str!("../../../../assets/pages/welcome.html");
const NEWTAB: &str = include_str!("../../../../assets/pages/newtab.html");
const SETTINGS: &str = include_str!("../../../../assets/pages/settings.html");
const HISTORY: &str = include_str!("../../../../assets/pages/history.html");
const ERROR: &str = include_str!("../../../../assets/pages/error.html");
const READER: &str = include_str!("../../../../assets/pages/reader.html");
const ICON: &str = include_str!("../../../../assets/icon.svg");

fn engines_json() -> String {
    json::array(ENGINES.iter().map(|e| json::array([json::string(e.id), json::string(e.name)])))
}

fn settings_json(b: &Browser) -> String {
    let s = &b.settings;
    Obj::new()
        .str("engine", &s.engine)
        .str("blocking", s.blocking.as_str())
        .str("appearance", s.appearance.as_str())
        .bool("restore_session", s.restore_session)
        .num("suspend_minutes", s.suspend_minutes)
        .done()
}

/// Is Volant the app that opens web links?
pub fn is_default() -> bool {
    let ws = NSWorkspace::sharedWorkspace();
    let Some(probe) = NSURL::URLWithString(&ns("https://example.com")) else { return false };
    let Some(app) = ws.URLForApplicationToOpenURL(&probe) else { return false };
    let me = NSBundle::mainBundle().bundleURL();
    app.path().map(|p| p.to_string()) == me.path().map(|p| p.to_string())
}

fn binary_mb() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    let len = std::fs::metadata(exe).ok()?.len() as f64 / 1_048_576.0;
    Some(if len < 10.0 { format!("{len:.1} MB") } else { format!("{len:.0} MB") })
}

fn page_data(b: &Browser, page: &str) -> String {
    let now = super::now();
    match page {
        "welcome" => {
            let mut o = Obj::new().raw("engines", &engines_json()).raw("settings", &settings_json(b)).bool("isDefault", is_default()).str("version", super::VERSION);
            if let Some(mb) = binary_mb() {
                o = o.str("binaryMB", &mb);
            }
            o.done()
        }
        "newtab" => {
            let top = b
                .history
                .top_sites(16, now)
                .into_iter()
                .filter(|(h, _, _)| !b.settings.hidden.iter().any(|x| x == h))
                .take(8)
                .map(|(h, u, t)| json::array([json::string(&h), json::string(&u), json::string(&t)]));
            Obj::new().raw("top", &json::array(top)).str("engineName", search::engine(&b.settings.engine).name).done()
        }
        "settings" => Obj::new()
            .raw("engines", &engines_json())
            .raw("settings", &settings_json(b))
            .num("rules", b.blocking.rules)
            .str("blockError", &b.blocking.error)
            .num("listsUpdated", b.settings.lists_updated)
            .bool("isDefault", is_default())
            .str("version", super::VERSION)
            .str("commit", super::COMMIT.map(|c| &c[..c.len().min(7)]).unwrap_or(""))
            .num("historyCount", b.history.len())
            .done(),
        "history" => {
            let items = b.history.recent(3000).into_iter().map(|p| json::array([json::string(&p.url), json::string(&p.title), p.last.to_string()]));
            Obj::new().raw("history", &json::array(items)).done()
        }
        _ => "{}".into(),
    }
}

fn fill(template: &str, data: &str) -> Vec<u8> {
    template.replacen("__VOLANT_DATA__", data, 1).into_bytes()
}

/// Answers a request for a volant:// URL.
pub fn serve(b: &Browser, url: &str) -> (&'static str, Vec<u8>) {
    let rest = url.strip_prefix("volant://").unwrap_or("");
    let path = rest.split(['?', '#']).next().unwrap_or("").trim_end_matches('/');
    match path {
        "app/style.css" => ("text/css", STYLE.as_bytes().to_vec()),
        "app/volant.js" => ("text/javascript", SCRIPT.as_bytes().to_vec()),
        "app/icon.svg" => ("image/svg+xml", ICON.as_bytes().to_vec()),
        "welcome" => ("text/html", fill(WELCOME, &page_data(b, "welcome"))),
        "newtab" | "" => ("text/html", fill(NEWTAB, &page_data(b, "newtab"))),
        "settings" => ("text/html", fill(SETTINGS, &page_data(b, "settings"))),
        "history" => ("text/html", fill(HISTORY, &page_data(b, "history"))),
        _ => ("text/html", fill(ERROR, &Obj::new().str("message", "Volant has no page here.").num("code", 404).done())),
    }
}

/// Reader mode's page for an extracted article.
pub fn reader_page(b: &Browser, a: &volant_core::reader::Article, url: &str) -> String {
    let data = Obj::new()
        .str("title", &a.title)
        .str("byline", &a.byline)
        .str("site", &a.site)
        .str("host", volant_core::omnibox::host(url))
        .str("url", url)
        .num("minutes", a.minutes())
        .num("size", b.settings.reader_size)
        .str("font", &b.settings.reader_font)
        .str("html", &a.html)
        .done();
    READER.replacen("__VOLANT_DATA__", &data, 1)
}

pub fn error_page(url: &str, message: &str, code: isize) -> String {
    let data = Obj::new().str("url", url).str("host", volant_core::omnibox::host(url)).str("message", message).num("code", code).done();
    ERROR.replacen("__VOLANT_DATA__", &data, 1)
}

/// Runs a script in every open Volant page (to tell Settings about changes).
pub fn notify(b: &Browser, js: &str) {
    for (id, w) in &b.webs {
        if b.tabs.get(*id).map(|t| t.url.starts_with("volant://")).unwrap_or(false) {
            unsafe { w.web.evaluateJavaScript_completionHandler(&ns(js), None) };
        }
    }
}

/// A message from one of Volant's pages.
pub fn handle(b: &mut Browser, tab: Option<TabId>, cmd: &str, value: &str) {
    match cmd {
        "set" => {
            let Some((k, v)) = value.split_once('=') else { return };
            if !b.settings.set(k, v) {
                return;
            }
            b.settings.save(&b.paths.settings);
            match k {
                "appearance" => {
                    theme::apply_appearance(b.settings.appearance, b.mtm);
                    theme::refresh(b);
                }
                "blocking" => blocking::apply(b),
                "sidebar" => ui::layout(b),
                _ => {}
            }
        }
        "onboarded" => {
            b.settings.onboarded = true;
            b.settings.save(&b.paths.settings);
            if let Some(id) = tab {
                actions::select(b, id);
            }
            actions::navigate(b, actions::NEWTAB);
        }
        "command" => super::command::open(b, super::command::Mode::Current, ""),
        "open" => {
            if !value.is_empty() {
                actions::navigate(b, value);
            }
        }
        "makeDefault" => make_default(),
        "hideSite" => {
            if !b.settings.hidden.iter().any(|h| h == value) {
                b.settings.hidden.push(value.to_string());
                b.settings.save(&b.paths.settings);
            }
        }
        "removeHistory" => b.history.remove(value),
        "clearHistory" => b.history.clear(),
        "clearData" => clear_website_data(b),
        "updateLists" => blocking::download_lists(b, true),
        "exitReader" => actions::toggle_reader(b),
        _ => {}
    }
}

fn make_default() {
    let ws = NSWorkspace::sharedWorkspace();
    let me = NSBundle::mainBundle().bundleURL();
    for scheme in ["http", "https"] {
        let done = RcBlock::new(move |_err: *mut NSError| {
            super::on_main(|| {
                super::with(|b| {
                    let yes = is_default();
                    notify(b, &format!("window.volantDefaultChanged && volantDefaultChanged({yes})"));
                })
            });
        });
        ws.setDefaultApplicationAtURL_toOpenURLsWithScheme_completionHandler(&me, &ns(scheme), Some(&done));
    }
}

fn clear_website_data(b: &Browser) {
    unsafe {
        let store = WKWebsiteDataStore::defaultDataStore(b.mtm);
        let types = WKWebsiteDataStore::allWebsiteDataTypes(b.mtm);
        let done = RcBlock::new(|| {});
        store.removeDataOfTypes_modifiedSince_completionHandler(&types, &NSDate::distantPast(), &done);
    }
}
