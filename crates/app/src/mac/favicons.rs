//! Sharp icons for the home page's favourites. For each favourite site Volant
//! reads the site's home page once, picks its biggest icon (volant-core's
//! `favicon`), and keeps it in `icons/` so the page draws it instantly and
//! offline. Volant's pages ask for `volant://<page>/site-icon/<host>`.

use super::{on_main, pages, with, Browser};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use volant_core::favicon;

/// Sites already looked up (or being looked up) since launch.
static ASKED: Mutex<Option<HashSet<String>>> = Mutex::new(None);
/// Look a site up again after this long, in case its icon changed.
const REFRESH_SECS: u64 = 30 * 86_400;

/// `example.com` from any URL, lower-case and without `www.`; `None` for anything odd.
fn key(url: &str) -> Option<String> {
    let h = volant_core::omnibox::host(url).to_ascii_lowercase();
    let ok = !h.is_empty() && h.len() < 200 && h.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'.' || c == b'-') && !h.starts_with('.');
    ok.then_some(h)
}

fn dir(data: &Path) -> PathBuf {
    data.join("icons")
}

fn path(data: &Path, key: &str) -> PathBuf {
    dir(data).join(key)
}

/// The saved icon for `key`, as (MIME type, bytes).
pub fn serve(b: &Browser, key: &str) -> Option<(&'static str, Vec<u8>)> {
    if key.is_empty() || !key.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'.' || c == b'-') || key.starts_with('.') {
        return None;
    }
    let bytes = std::fs::read(path(&b.paths.data, key)).ok()?;
    Some((favicon::mime(&bytes)?, bytes))
}

fn get(url: &str, limit: usize) -> Option<Vec<u8>> {
    super::net::fetch(url, limit).ok().filter(|b| !b.is_empty())
}

/// Finds and saves a sharp icon for every favourite that doesn't have one yet.
pub fn ensure(b: &Browser) {
    let data = b.paths.data.clone();
    let mut todo = Vec::new();
    {
        let mut asked = ASKED.lock().unwrap_or_else(|e| e.into_inner());
        let asked = asked.get_or_insert_with(HashSet::new);
        for f in &b.favorites.list {
            let Some(k) = key(&f.url) else { continue };
            let fresh = std::fs::metadata(path(&data, &k))
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .map(|age| age.as_secs() < REFRESH_SECS)
                .unwrap_or(false);
            if !fresh && asked.insert(k.clone()) {
                todo.push((k, f.url.clone()));
            }
        }
    }
    if todo.is_empty() {
        return;
    }
    let _ = std::fs::create_dir_all(dir(&data));
    std::thread::spawn(move || {
        for (k, url) in todo {
            if let Some(bytes) = find(&url) {
                let p = path(&data, &k);
                let tmp = p.with_extension("tmp");
                if std::fs::write(&tmp, &bytes).and_then(|_| std::fs::rename(&tmp, &p)).is_ok() {
                    on_main(move || {
                        with(move |b| pages::notify(b, &format!("window.volantIconReady && volantIconReady({})", volant_core::json::string(&k))))
                    });
                }
            }
        }
    });
}

/// The best icon the site offers: the first one big enough, else the biggest seen.
fn find(url: &str) -> Option<Vec<u8>> {
    let origin = favicon::resolve(url, "/")?;
    let page = get(&origin, 3 << 20).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
    let mut best: Option<(u32, Vec<u8>)> = None;
    for candidate in favicon::candidates(&page, &origin).into_iter().take(6) {
        let Some(bytes) = get(&candidate, 1 << 20) else { continue };
        if favicon::mime(&bytes).is_none() {
            continue; // an error page
        }
        let px = favicon::pixels(&bytes).unwrap_or(64);
        if px >= favicon::SHARP {
            return Some(bytes);
        }
        if best.as_ref().map(|(p, _)| px > *p).unwrap_or(true) {
            best = Some((px, bytes));
        }
    }
    best.map(|(_, b)| b)
}
