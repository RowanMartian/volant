//! volant-core: the parts of Volant that don't depend on macOS.
//!
//! Everything here is written from scratch with zero dependencies, so it compiles
//! in a blink and stays tiny in the final binary:
//!
//! * [`omnibox`]: turns whatever you type into a URL or a search.
//! * [`history`]: an append-only visit log with frecency ranking (no SQLite).
//! * [`favicon`]: finds a sharp icon for a site (apple-touch-icon, sized icons, SVG).
//! * [`fuzzy`]: the matcher that ranks suggestions as you type.
//! * [`ublock`]: the content blocker, powered by uBlock Origin: its filter
//!   lists and syntax, compiled to WebKit content rules plus per-site
//!   scriptlets and cosmetic filters.
//! * [`scriptlets`]: uBlock Origin's own scriptlets, from its latest release, assembled per page.
//! * [`html`]: an HTML tokenizer and tree builder, written from scratch.
//! * [`reader`]: Reader mode, which finds the article in a page and rewrites it as clean HTML.
//! * [`store`]: settings and session files in a plain line format.
//! * [`tabs`]: tab order, pinning, closed-tab memory and sleeping tabs.
//! * [`json`]: the JSON writer and parser the app needs.
//! * [`http`]: an HTTP/1.1 client (GET and POST, redirects, chunked, gzip, limits) over any transport.
//! * [`sites`]: which engine draws each site (Simple Page by default, WebKit for applications and fallbacks).
//! * [`httpcache`]: an in-memory HTTP cache (freshness, revalidation, Vary, LRU).
//! * [`woff`]: WOFF 1 web fonts unpacked to TrueType/OpenType.
//! * [`cookies`]: the cookie jar (domains, paths, expiry, Secure, HttpOnly), saved as plain lines.
//! * [`deflate`], [`hash`], [`zip`]: DEFLATE both ways, gzip, SHA-256, CRC-32 and
//!   zip archives, for updates, downloads and packaging.
//! * [`macho`]: universal (fat) Mach-O binaries, as `lipo -create` makes them.


pub mod cookies;
pub mod deflate;
pub mod favicon;
pub mod hash;
pub mod fuzzy;
pub mod history;
pub mod json;
pub mod macho;
pub mod html;
pub mod http;
pub mod httpcache;
pub mod omnibox;
pub mod reader;
pub mod scriptlets;
pub mod search;
pub mod sites;
pub mod store;
pub mod tabs;
pub mod woff;
pub mod zip;
pub mod ublock;

/// Seconds since the Unix epoch.
pub fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}
