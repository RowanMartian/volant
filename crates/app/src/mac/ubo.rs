//! Keeps Volant's blocker matched to uBlock Origin's latest release.
//!
//! Each time the filter lists are refreshed, Volant asks jsDelivr which
//! release of uBlock Origin is the latest. When that changes, a hidden web view
//! (its own process, no cookies, no storage) loads uBO's scriptlet modules and
//! `assets/assets.json` straight from that release and hands back:
//!
//! * every scriptlet with its aliases, helpers, world and trust requirement,
//!   saved to `ublock/scriptlets.txt` and run on pages from then on, and
//! * uBO's filter-list catalogue (titles, groups, defaults and download
//!   mirrors), saved to `ublock/catalog.txt` for the settings page and the
//!   list downloads.
//!
//! Then the lists themselves are downloaded. If any step fails, Volant keeps
//! what it had and the lists still update.

use super::{blocking, ns, on_main, with, Browser};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::MainThreadOnly;
use objc2_foundation::NSURL;
use objc2_web_kit::{WKUserContentController, WKWebView, WKWebViewConfiguration, WKWebsiteDataStore};
use volant_core::scriptlets::Resources;
use volant_core::ublock;

/// The newest uBlock Origin release (jsDelivr skips betas and release candidates).
const LATEST: &str = "https://data.jsdelivr.com/v1/packages/gh/gorhill/uBlock/resolved?specifier=latest";
/// How long the hidden page may take before Volant gives up for this round.
const TIMEOUT_SECS: f64 = 90.0;
pub const HANDLER: &str = "volantUbo";

#[derive(Default)]
pub struct State {
    web: Option<Retained<WKWebView>>,
    version: String,
    round: u64,
}

fn scriptlets_path(b: &Browser) -> std::path::PathBuf {
    b.paths.data.join("ublock").join("scriptlets.txt")
}

fn catalog_path(b: &Browser) -> std::path::PathBuf {
    b.paths.data.join("ublock").join("catalog.txt")
}

/// Loads what an earlier sync saved.
pub fn load(b: &mut Browser) {
    b.blocking.resources = std::fs::read_to_string(scriptlets_path(b)).ok().and_then(|t| Resources::parse(&t));
    let catalog = std::fs::read_to_string(catalog_path(b)).map(|t| ublock::parse_catalog(&t)).unwrap_or_default();
    b.blocking.catalog = if catalog.is_empty() { ublock::builtin_catalog() } else { catalog };
}

/// The uBlock Origin release Volant's scriptlets come from ("" before the first sync).
pub fn version(b: &Browser) -> &str {
    b.blocking.resources.as_ref().map(|r| r.version.as_str()).unwrap_or("")
}

/// Checks for a new uBlock Origin release, takes its scriptlets and catalogue
/// if there is one, then downloads every selected list.
pub fn sync(b: &mut Browser) {
    b.ubo.round += 1;
    let round = b.ubo.round;
    blocking::set_status(b, "Checking for a new uBlock Origin…");
    std::thread::spawn(move || {
        let text = super::net::fetch_text(LATEST, 1 << 20).unwrap_or_default();
        let latest = volant_core::json::parse(&text)
            .and_then(|v| v.str_at("version").map(String::from))
            .filter(|v| !v.is_empty() && v.len() < 24 && v.bytes().all(|c| c.is_ascii_digit() || c == b'.'));
        on_main(move || {
            with(move |b| {
                if b.ubo.round != round {
                    return;
                }
                match latest {
                    Some(v) if v != version(b) || !catalog_path(b).exists() => fetch(b, v),
                    _ => finish(b),
                }
            })
        });
    });
}

/// The page the hidden web view runs: it imports uBO's scriptlets as uBO does
/// and flattens them, with the catalogue, into plain text.
fn extractor(version: &str) -> String {
    format!(
        r#"<!doctype html><meta charset="utf-8"><script type="module">
const RS = '\u001e', FS = '\u001f', GS = '\u001d', VERSION = {version:?};
const post = (s) => window.webkit.messageHandlers.{HANDLER}.postMessage(s);
(async () => {{
  try {{
    const [m, assets] = await Promise.all([
      import('./src/js/resources/scriptlets.js'),
      fetch('./assets/assets.json').then((r) => r.json()),
    ]);
    const res = [['{magic}', VERSION].join(FS)];
    for (const s of m.builtinScriptlets) {{
      if (!s || typeof s.fn !== 'function' || !s.name || !s.fn.name) continue;
      const deps = (s.dependencies || []).map((d) => typeof d === 'function' ? d.details && d.details.name : d).filter((d) => typeof d === 'string');
      res.push([s.name, s.fn.name, s.world || 'MAIN', s.requiresTrust ? '1' : '0', (s.aliases || []).join(' '), deps.join(' '), s.fn.toString()].join(FS));
    }}
    const clean = (s) => String(s || '').replace(/[\t\n\r]/g, ' ');
    const cat = [];
    for (const [id, a] of Object.entries(assets)) {{
      if (!a || a.content !== 'filters') continue;
      const urls = [].concat(a.cdnURLs || [], a.contentURL || []).filter((u) => typeof u === 'string' && u.startsWith('https://'));
      cat.push([id, a.group, a.group2, a.off ? '1' : '0', a.ua, a.title, urls.join(' ')].map(clean).join('\t'));
    }}
    post(['ok', VERSION, cat.join('\n'), res.join(RS)].join(GS));
  }} catch (e) {{
    post(['error', String(e && e.message || e)].join(GS));
  }}
}})();
</script>"#,
        magic = volant_core::scriptlets::MAGIC,
    )
}

fn fetch(b: &mut Browser, version: String) {
    let base = format!("https://cdn.jsdelivr.net/gh/gorhill/uBlock@{version}/");
    let Some(base_url) = NSURL::URLWithString(&ns(&base)) else { return finish(b) };
    blocking::set_status(b, &format!("Updating to uBlock Origin {version}…"));
    let web = unsafe {
        let config = WKWebViewConfiguration::new(b.mtm);
        config.setWebsiteDataStore(&WKWebsiteDataStore::nonPersistentDataStore(b.mtm));
        let ucc = WKUserContentController::new(b.mtm);
        ucc.addScriptMessageHandler_name(ProtocolObject::from_ref(&*b.web_delegate), &ns(HANDLER));
        config.setUserContentController(&ucc);
        let web = WKWebView::initWithFrame_configuration(WKWebView::alloc(b.mtm), super::rect(0.0, 0.0, 16.0, 16.0), &config);
        web.loadHTMLString_baseURL(&ns(&extractor(&version)), Some(&base_url));
        web
    };
    b.ubo.web = Some(web);
    b.ubo.version = version;
    let round = b.ubo.round;
    super::after(TIMEOUT_SECS, move || {
        with(move |b| {
            if b.ubo.round == round && b.ubo.web.is_some() {
                eprintln!("ubo: timed out");
                b.blocking.timings.push_str("uBO sync: timed out; ");
                drop_view(b);
                finish(b);
            }
        })
    });
}

fn drop_view(b: &mut Browser) {
    if let Some(web) = b.ubo.web.take() {
        unsafe {
            web.configuration().userContentController().removeScriptMessageHandlerForName(&ns(HANDLER));
            web.stopLoading();
        }
    }
}

/// The hidden page's answer. `web` is the view it came from.
pub fn received(b: &mut Browser, web: usize, text: &str) {
    let ours = b.ubo.web.as_ref().map(|w| Retained::as_ptr(w) as usize) == Some(web);
    if !ours {
        return;
    }
    drop_view(b);
    let parts: Vec<&str> = text.split('\u{1d}').collect();
    match parts[..] {
        ["ok", version, catalog, scriptlets] if version == b.ubo.version => {
            let res = Resources::parse(scriptlets);
            let cat = ublock::parse_catalog(catalog);
            match res {
                Some(res) if !cat.is_empty() => {
                    let d = b.paths.data.join("ublock");
                    let _ = volant_core::store::write_atomic(&d.join("scriptlets.txt"), scriptlets);
                    let _ = volant_core::store::write_atomic(&d.join("catalog.txt"), catalog);
                    eprintln!("ubo: {} scriptlets, {} lists from uBlock Origin {version}", res.len(), cat.len());
                    b.blocking.timings.push_str(&format!("uBO {version}: {} scriptlets, {} lists; ", res.len(), cat.len()));
                    b.blocking.resources = Some(res);
                    b.blocking.catalog = cat;
                }
                _ => b.blocking.timings.push_str("uBO sync: unreadable; "),
            }
        }
        ["error", why] => {
            eprintln!("ubo: {why}");
            b.blocking.timings.push_str(&format!("uBO sync failed: {why}; "));
        }
        _ => {}
    }
    finish(b);
}

fn finish(b: &mut Browser) {
    b.ubo.round += 1;
    blocking::download_lists(b, true);
}
