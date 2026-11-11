//! volant-core: the parts of Volant that don't depend on macOS.
//!
//! Everything here is written from scratch with zero dependencies, so it compiles
//! in a blink and stays tiny in the final binary:
//!
//! * [`omnibox`]: turns whatever you type into a URL or a search.
//! * [`history`]: an append-only visit log with frecency ranking (no SQLite).
//! * [`fuzzy`]: the matcher that ranks suggestions as you type.
//! * [`blocker`]: compiles Adblock Plus / uBlock filter lists into WebKit
//!   content-rule JSON, so blocking runs inside the engine as compiled bytecode
//!   with zero JavaScript.
//! * [`html`]: an HTML tokenizer and tree builder, written from scratch.
//! * [`reader`]: Reader mode, which finds the article in a page and rewrites it as clean HTML.
//! * [`store`]: settings and session files in a plain line format.
//! * [`tabs`]: tab order, pinning, closed-tab memory and sleeping tabs.
//! * [`json`]: the minimal JSON writer/reader the app needs.

pub mod blocker;
pub mod fuzzy;
pub mod history;
pub mod json;
pub mod html;
pub mod omnibox;
pub mod reader;
pub mod search;
pub mod store;
pub mod tabs;

/// Seconds since the Unix epoch.
pub fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}
