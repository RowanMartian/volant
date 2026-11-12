//! Volant's content blocker, powered by uBlock Origin.
//!
//! uBlock Origin is a browser extension; its engine runs in JavaScript inside
//! the extension. Volant uses uBlock Origin's filter lists, filter syntax and
//! default list selection, and maps each kind of filter onto the fastest thing
//! WebKit can do with it:
//!
//! * Network filters become WebKit content rules. WebKit compiles them into
//!   bytecode that runs inside its loader: no script runs on any page to block
//!   a request. WebKit caps a rule list at 150,000 rules, so the network rules
//!   are split across several lists, each carrying every exception so `@@`
//!   filters keep working across the split.
//! * Plain element-hiding filters become WebKit `css-display-none` rules
//!   (generic ones batched, site-specific ones grouped by site).
//! * Scriptlets (`##+js(...)`), procedural cosmetic filters (`:has-text`,
//!   `:upward`, `:remove`, `:style`, ...) and selectors WebKit's rule compiler
//!   can't take are indexed by site, and Volant injects just the ones for the
//!   site you're on, before the page's own scripts run.
//!
//! Also handled: `!#if` preprocessing, `$important`, `$badfilter`,
//! `$redirect` (as a block), `$to=`, `$document`/`$generichide`/`$elemhide`/
//! `$specifichide` exceptions, entity domains (`google.*`) and regex filters
//! that fit WebKit's regex subset. Options WebKit can't express (`$removeparam`,
//! `$csp`, `$replace`, `$header`, ...) are skipped, as uBO Lite does on Safari.

use crate::json::push_str;
use std::collections::{BTreeMap, HashMap, HashSet};

/// One of uBlock Origin's filter lists.
pub struct FilterList {
    pub id: &'static str,
    pub title: &'static str,
    pub group: &'static str,
    pub url: &'static str,
    /// On in a fresh uBlock Origin install.
    pub default_on: bool,
}

const fn l(id: &'static str, title: &'static str, group: &'static str, url: &'static str, default_on: bool) -> FilterList {
    FilterList { id, title, group, url, default_on }
}

/// uBlock Origin's own catalog (assets/assets.json), minus regional lists.
pub const CATALOG: &[FilterList] = &[
    l("ublock-filters", "uBlock filters – Ads", "Built-in", "https://ublockorigin.github.io/uAssetsCDN/filters/filters.min.txt", true),
    l("ublock-badware", "uBlock filters – Badware risks", "Built-in", "https://ublockorigin.github.io/uAssetsCDN/filters/badware.min.txt", true),
    l("ublock-privacy", "uBlock filters – Privacy", "Built-in", "https://ublockorigin.github.io/uAssetsCDN/filters/privacy.min.txt", true),
    l("ublock-unbreak", "uBlock filters – Unbreak", "Built-in", "https://ublockorigin.github.io/uAssetsCDN/filters/unbreak.min.txt", true),
    l("ublock-quick-fixes", "uBlock filters – Quick fixes", "Built-in", "https://ublockorigin.github.io/uAssetsCDN/filters/quick-fixes.min.txt", true),
    l("ublock-experimental", "uBlock filters – Experimental", "Built-in", "https://ublockorigin.github.io/uAssetsCDN/filters/experimental.min.txt", false),
    l("easylist", "EasyList", "Ads", "https://cdn.jsdelivr.net/gh/uBlockOrigin/uAssetsCDN@main/thirdparties/easylist.txt", true),
    l("adguard-generic", "AdGuard – Ads", "Ads", "https://filters.adtidy.org/extension/ublock/filters/2_without_easylist.txt", false),
    l("adguard-mobile", "AdGuard – Mobile Ads", "Ads", "https://filters.adtidy.org/extension/ublock/filters/11.txt", false),
    l("easyprivacy", "EasyPrivacy", "Privacy", "https://cdn.jsdelivr.net/gh/uBlockOrigin/uAssetsCDN@main/thirdparties/easyprivacy.txt", true),
    l("adguard-spyware-url", "AdGuard/uBO – URL Tracking Protection", "Privacy", "https://ublockorigin.github.io/uAssetsCDN/filters/privacy-removeparam.txt", false),
    l("block-lan", "Block Outsider Intrusion into LAN", "Privacy", "https://ublockorigin.github.io/uAssetsCDN/filters/lan-block.txt", false),
    l("urlhaus-1", "Online Malicious URL Blocklist", "Malware protection, security", "https://curbengh.github.io/malware-filter/urlhaus-filter-vivaldi-online.txt", true),
    l("curben-phishing", "Phishing URL Blocklist", "Malware protection, security", "https://curbengh.github.io/phishing-filter/phishing-filter.txt", false),
    l("plowe-0", "Peter Lowe’s Ad and tracking server list", "Multipurpose", "https://pgl.yoyo.org/adservers/serverlist.php?hostformat=hosts&showintro=1&mimetype=plaintext", true),
    l("dpollock-0", "Dan Pollock’s hosts file", "Multipurpose", "https://someonewhocares.org/hosts/hosts", false),
    l("ublock-cookies-easylist", "uBlock filters – Cookie Notices", "Cookie notices", "https://ublockorigin.github.io/uAssetsCDN/filters/annoyances-cookies.txt", false),
    l("fanboy-cookiemonster", "EasyList – Cookie Notices", "Cookie notices", "https://ublockorigin.github.io/uAssetsCDN/thirdparties/easylist-cookies.txt", false),
    l("adguard-cookies", "AdGuard – Cookie Notices", "Cookie notices", "https://filters.adtidy.org/extension/ublock/filters/18.txt", false),
    l("fanboy-social", "EasyList – Social Widgets", "Social widgets", "https://ublockorigin.github.io/uAssetsCDN/thirdparties/easylist-social.txt", false),
    l("adguard-social", "AdGuard – Social Widgets", "Social widgets", "https://filters.adtidy.org/extension/ublock/filters/4.txt", false),
    l("fanboy-thirdparty_social", "Fanboy – Anti-Facebook", "Social widgets", "https://secure.fanboy.co.nz/fanboy-antifacebook.txt", false),
    l("ublock-annoyances", "uBlock filters – Annoyances", "Annoyances", "https://ublockorigin.github.io/uAssetsCDN/filters/annoyances.min.txt", false),
    l("easylist-annoyances", "EasyList – Other Annoyances", "Annoyances", "https://ublockorigin.github.io/uAssetsCDN/thirdparties/easylist-annoyances.txt", false),
    l("easylist-chat", "EasyList – Chat Widgets", "Annoyances", "https://ublockorigin.github.io/uAssetsCDN/thirdparties/easylist-chat.txt", false),
    l("fanboy-ai-suggestions", "EasyList – AI Widgets", "Annoyances", "https://ublockorigin.github.io/uAssetsCDN/thirdparties/easylist-ai.txt", false),
    l("easylist-newsletters", "EasyList – Newsletter Notices", "Annoyances", "https://ublockorigin.github.io/uAssetsCDN/thirdparties/easylist-newsletters.txt", false),
    l("easylist-notifications", "EasyList – Notifications", "Annoyances", "https://ublockorigin.github.io/uAssetsCDN/thirdparties/easylist-notifications.txt", false),
    l("adguard-popup-overlays", "AdGuard – Popup Overlays", "Annoyances", "https://filters.adtidy.org/extension/ublock/filters/19.txt", false),
    l("adguard-mobile-app-banners", "AdGuard – Mobile App Banners", "Annoyances", "https://filters.adtidy.org/extension/ublock/filters/20.txt", false),
    l("adguard-other-annoyances", "AdGuard – Other Annoyances", "Annoyances", "https://filters.adtidy.org/extension/ublock/filters/21.txt", false),
    l("adguard-widgets", "AdGuard – Widgets", "Annoyances", "https://filters.adtidy.org/extension/ublock/filters/22.txt", false),
];

pub fn list(id: &str) -> Option<&'static FilterList> {
    CATALOG.iter().find(|l| l.id == id)
}

pub fn default_lists() -> Vec<String> {
    CATALOG.iter().filter(|l| l.default_on).map(|l| l.id.to_string()).collect()
}

/// Used only until the real lists have been downloaded once (e.g. the very first launch offline).
pub const FALLBACK: &str = include_str!("../lists/builtin.txt");

/// uBlock Origin's own default trusted sites.
pub const DEFAULT_TRUSTED: &[&str] = &[];

/// WebKit refuses a rule list above 150,000 rules; stay well clear so
/// compiling stays quick.
pub const SHARD_RULES: usize = 60_000;

/// Settings that change what gets compiled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Options {
    /// uBO's "Disable cosmetic filtering" switched off (i.e. cosmetic filtering on).
    pub cosmetic: bool,
    /// uBO's "Block remote fonts".
    pub block_fonts: bool,
    /// uBO's "Disable hyperlink auditing" (block `<a ping>` beacons).
    pub block_pings: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self { cosmetic: true, block_fonts: false, block_pings: true }
    }
}

// ---------------------------------------------------------------- preprocessor

/// The environment uBlock Origin's `!#if` directives are evaluated against:
/// Volant behaves like uBO on Safari, without HTML filtering.
fn env_token(t: &str) -> bool {
    matches!(t, "env_safari" | "ext_ublock" | "cap_user_stylesheet")
}

fn eval_condition(expr: &str) -> bool {
    // Tokens, `!`, `&&`, `||` and parentheses, evaluated with a tiny recursive descent.
    let toks: Vec<String> = {
        let mut v = Vec::new();
        let mut cur = String::new();
        for c in expr.chars() {
            match c {
                '(' | ')' | '!' => {
                    if !cur.is_empty() {
                        v.push(std::mem::take(&mut cur));
                    }
                    v.push(c.to_string());
                }
                '&' | '|' => {
                    if !cur.is_empty() && !cur.starts_with(c) {
                        v.push(std::mem::take(&mut cur));
                    }
                    cur.push(c);
                    if cur.len() == 2 {
                        v.push(std::mem::take(&mut cur));
                    }
                }
                c if c.is_whitespace() => {
                    if !cur.is_empty() {
                        v.push(std::mem::take(&mut cur));
                    }
                }
                c => cur.push(c),
            }
        }
        if !cur.is_empty() {
            v.push(cur);
        }
        v
    };
    fn or(t: &[String], i: &mut usize) -> bool {
        let mut v = and(t, i);
        while *i < t.len() && t[*i] == "||" {
            *i += 1;
            let r = and(t, i);
            v = v || r;
        }
        v
    }
    fn and(t: &[String], i: &mut usize) -> bool {
        let mut v = unary(t, i);
        while *i < t.len() && t[*i] == "&&" {
            *i += 1;
            let r = unary(t, i);
            v = v && r;
        }
        v
    }
    fn unary(t: &[String], i: &mut usize) -> bool {
        if *i >= t.len() {
            return false;
        }
        match t[*i].as_str() {
            "!" => {
                *i += 1;
                !unary(t, i)
            }
            "(" => {
                *i += 1;
                let v = or(t, i);
                if *i < t.len() && t[*i] == ")" {
                    *i += 1;
                }
                v
            }
            tok => {
                *i += 1;
                env_token(tok)
            }
        }
    }
    let mut i = 0;
    or(&toks, &mut i)
}

/// Lines that survive uBO's `!#if` / `!#else` / `!#endif` directives.
pub fn preprocess(text: &str) -> impl Iterator<Item = &str> {
    let mut stack: Vec<(bool, bool)> = Vec::new(); // (this branch active, parent active)
    text.lines().filter(move |raw| {
        let line = raw.trim();
        if let Some(cond) = line.strip_prefix("!#if ") {
            let parent = stack.last().map(|s| s.0).unwrap_or(true);
            stack.push((parent && eval_condition(cond), parent));
            return false;
        }
        if line.starts_with("!#else") {
            if let Some(top) = stack.last_mut() {
                top.0 = top.1 && !top.0;
            }
            return false;
        }
        if line.starts_with("!#endif") {
            stack.pop();
            return false;
        }
        stack.last().map(|s| s.0).unwrap_or(true)
    })
}

// ---------------------------------------------------------------- WebKit rules

#[derive(Default, Clone)]
struct Trigger {
    url_filter: String,
    case_sensitive: bool,
    resource_types: Vec<&'static str>,
    load_type: Option<&'static str>,
    if_domain: Vec<String>,
    unless_domain: Vec<String>,
}

fn json_list<'a>(out: &mut String, items: impl Iterator<Item = &'a str>) {
    out.push('[');
    for (i, s) in items.enumerate() {
        if i > 0 {
            out.push(',');
        }
        push_str(out, s);
    }
    out.push(']');
}

impl Trigger {
    fn everywhere() -> Self {
        Self { url_filter: ".*".into(), ..Default::default() }
    }

    fn write(&self, out: &mut String) {
        out.push_str("{\"url-filter\":");
        push_str(out, &self.url_filter);
        if self.case_sensitive {
            out.push_str(",\"url-filter-is-case-sensitive\":true");
        }
        if !self.resource_types.is_empty() {
            out.push_str(",\"resource-type\":");
            json_list(out, self.resource_types.iter().copied());
        }
        if let Some(lt) = self.load_type {
            out.push_str(",\"load-type\":[\"");
            out.push_str(lt);
            out.push_str("\"]");
        }
        if !self.if_domain.is_empty() {
            out.push_str(",\"if-domain\":");
            json_list(out, self.if_domain.iter().map(String::as_str));
        } else if !self.unless_domain.is_empty() {
            out.push_str(",\"unless-domain\":");
            json_list(out, self.unless_domain.iter().map(String::as_str));
        }
        out.push('}');
    }

    fn key(&self) -> String {
        let mut k = String::new();
        self.write(&mut k);
        k
    }
}

/// Builds one WebKit rule list.
struct RuleList {
    json: String,
    rules: usize,
}

impl RuleList {
    fn new() -> Self {
        Self { json: String::from("["), rules: 0 }
    }
    fn push(&mut self, t: &Trigger, action: &str, selector: Option<&str>) {
        if self.rules > 0 {
            self.json.push(',');
        }
        self.json.push_str("{\"trigger\":");
        t.write(&mut self.json);
        self.json.push_str(",\"action\":{\"type\":\"");
        self.json.push_str(action);
        self.json.push('"');
        if let Some(s) = selector {
            self.json.push_str(",\"selector\":");
            push_str(&mut self.json, s);
        }
        self.json.push_str("}}");
        self.rules += 1;
    }
    fn finish(mut self) -> String {
        self.json.push(']');
        self.json
    }
}

const ALL_TYPES: &[&str] = &["document", "image", "style-sheet", "script", "font", "raw", "svg-document", "media", "popup", "ping"];

fn resource_type(opt: &str) -> Option<&'static [&'static str]> {
    Some(match opt {
        "script" => &["script"],
        "image" | "img" => &["image"],
        "stylesheet" | "css" => &["style-sheet"],
        "font" => &["font"],
        "media" | "object" | "object-subrequest" => &["media"],
        "xmlhttprequest" | "xhr" | "websocket" | "other" | "fetch" | "webrtc" => &["raw"],
        "subdocument" | "frame" | "document" | "doc" => &["document"],
        "ping" | "beacon" => &["ping"],
        "popup" => &["popup"],
        _ => return None,
    })
}

/// A domain as WebKit's `if-domain` wants it: lowercase ASCII, `*` to include subdomains.
fn webkit_domain(d: &str) -> Option<String> {
    let d = d.trim().trim_start_matches("*.").to_ascii_lowercase();
    if d.is_empty() || !d.is_ascii() || d.contains(['/', '*', ' ', '|']) || d.ends_with('.') || d.ends_with(".*") {
        return None;
    }
    Some(format!("*{d}"))
}

fn escape_regex(c: char, out: &mut String) {
    if matches!(c, '.' | '?' | '+' | '(' | ')' | '[' | ']' | '{' | '}' | '\\' | '$' | '|' | '^') {
        out.push('\\');
    }
    out.push(c);
}

const HOST_PREFIX: &str = "^[^:]+:(//)?([^/]+\\.)?";

/// Translates the address part of a network filter to WebKit's regex subset.
fn pattern_to_regex(p: &str) -> Option<String> {
    if !p.is_ascii() {
        return None;
    }
    let mut out = String::with_capacity(p.len() + 32);
    let mut rest = p;
    let mut in_host = false;
    if let Some(r) = rest.strip_prefix("||") {
        out.push_str(HOST_PREFIX);
        rest = r;
        in_host = true;
    } else if let Some(r) = rest.strip_prefix('|') {
        out.push('^');
        rest = r;
    }
    let mut end_anchor = false;
    if let Some(r) = rest.strip_suffix('|') {
        rest = r;
        end_anchor = true;
    }
    let rest = rest.trim_start_matches('*');
    let rest = if end_anchor { rest } else { rest.trim_end_matches('*') };
    let chars: Vec<char> = rest.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        match c {
            '*' => {
                out.push_str(".*");
                in_host = false;
            }
            '^' => {
                if in_host {
                    out.push_str("[/:]");
                    in_host = false;
                } else if i + 1 != chars.len() {
                    out.push_str("[^a-zA-Z0-9_.%-]");
                }
            }
            '/' | '?' | ':' | '=' | '&' => {
                in_host = false;
                escape_regex(c, &mut out);
            }
            c => escape_regex(c, &mut out),
        }
    }
    if end_anchor {
        out.push('$');
    }
    if out.is_empty() || out == "^" {
        out = ".*".into();
    }
    Some(out)
}

/// A regex filter (`/.../`) in WebKit's subset, if it fits.
fn regex_to_webkit(re: &str) -> Option<String> {
    if !re.is_ascii() || re.is_empty() {
        return None;
    }
    let b = re.as_bytes();
    let mut out = String::with_capacity(re.len() + 16);
    let mut i = 0;
    let mut in_class = false;
    while i < b.len() {
        let c = b[i] as char;
        match c {
            '\\' => {
                let n = *b.get(i + 1)? as char;
                match n {
                    'd' if !in_class => out.push_str("[0-9]"),
                    'w' if !in_class => out.push_str("[A-Za-z0-9_]"),
                    'd' | 'w' | 'b' | 'B' | 's' | 'S' | 'W' | 'D' | '1'..='9' | 'u' | 'x' | 'p' => return None,
                    n => {
                        out.push('\\');
                        out.push(n);
                    }
                }
                i += 2;
                continue;
            }
            '[' if !in_class => in_class = true,
            ']' if in_class => in_class = false,
            '|' | '{' | '}' if !in_class => return None,
            '(' if !in_class && b.get(i + 1) == Some(&b'?') => return None,
            _ => {}
        }
        out.push(c);
        i += 1;
    }
    if in_class {
        return None;
    }
    Some(out)
}

fn host_regex(domain: &str) -> String {
    let mut r = String::from(HOST_PREFIX);
    for c in domain.chars() {
        escape_regex(c, &mut r);
    }
    r.push_str("[/:]");
    r
}

fn anchored_host(p: &str) -> Option<&str> {
    let r = p.strip_prefix("||")?;
    let end = r.find(['^', '/', '*', '|', '$']).unwrap_or(r.len());
    let h = &r[..end];
    (!h.is_empty()).then_some(h)
}

// ---------------------------------------------------------------- cosmetics

const PROCEDURAL: &[&str] = &[
    ":has-text(", ":-abp-", ":xpath(", ":upward(", ":remove(", ":style(", ":matches-css", ":min-text-length(", ":watch-attr(", ":others(",
    ":matches-path(", ":matches-attr(", ":matches-prop(", ":remove-attr(", ":remove-class(", ":contains(", ":nth-ancestor(", ":if(", ":if-not(",
    ":matches-media(", ":spath(",
];

/// Pseudo-classes WebKit's content-rule compiler takes for sure. Anything else
/// goes to the page script instead: one bad selector would sink a whole list.
const SAFE_PSEUDO: &[&str] = &[
    ":not(", ":nth-child(", ":nth-of-type(", ":nth-last-child(", ":nth-last-of-type(", ":first-child", ":last-child", ":first-of-type",
    ":last-of-type", ":only-child", ":only-of-type", ":empty", ":root",
];

fn is_procedural(s: &str) -> bool {
    PROCEDURAL.iter().any(|p| s.contains(p))
}

/// Can WebKit's content-rule compiler take this selector?
fn webkit_safe_selector(s: &str) -> bool {
    if !s.is_ascii() || s.contains(['{', '}']) || s.contains("::") || s.contains(":has(") {
        return false;
    }
    // Check each `:` outside attribute selectors and quoted strings.
    let b = s.as_bytes();
    let (mut in_attr, mut quote) = (false, 0u8);
    for (i, &c) in b.iter().enumerate() {
        match c {
            b'"' | b'\'' if in_attr => {
                if quote == 0 {
                    quote = c;
                } else if quote == c {
                    quote = 0;
                }
            }
            b'[' if quote == 0 => in_attr = true,
            b']' if quote == 0 => in_attr = false,
            b':' if !in_attr && quote == 0 => {
                if !SAFE_PSEUDO.iter().any(|ok| s[i..].starts_with(ok)) {
                    return false;
                }
            }
            _ => {}
        }
    }
    !in_attr && quote == 0
}

fn selector_ok(s: &str) -> bool {
    !s.is_empty() && !s.contains('{') && !s.contains('}') && !s.starts_with('+') && !s.starts_with('^')
}

/// What Volant injects into a page for one site.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum Kind {
    /// A selector to hide, added as a style sheet (for selectors WebKit's rule compiler won't take).
    Hide = b'h',
    /// A procedural cosmetic filter, run by the page script.
    Procedural = b'p',
    /// A scriptlet call: `name, arg1, arg2...`.
    Scriptlet = b's',
    /// Turns off a matching cosmetic filter on this site.
    HideException = b'H',
    /// Turns off a scriptlet on this site (empty: all scriptlets).
    ScriptletException = b'S',
}

impl Kind {
    fn from(b: u8) -> Option<Self> {
        Some(match b {
            b'h' => Kind::Hide,
            b'p' => Kind::Procedural,
            b's' => Kind::Scriptlet,
            b'H' => Kind::HideException,
            b'S' => Kind::ScriptletException,
            _ => return None,
        })
    }
}

/// Site-specific scriptlets and cosmetic filters, looked up when a page loads.
#[derive(Default, Debug)]
pub struct SiteIndex {
    map: HashMap<String, Vec<u32>>,
    entries: Vec<(Kind, String)>,
    dedupe: HashMap<(Kind, String), u32>,
    /// Sites where filtering is off entirely (`@@||site^$document`).
    page_allow: HashSet<String>,
    /// Sites where generic / all / specific cosmetic filtering is off.
    ghide: HashSet<String>,
    ehide: HashSet<String>,
    shide: HashSet<String>,
}

/// What one page gets.
#[derive(Default, Debug, PartialEq)]
pub struct Bundle {
    pub hide: Vec<String>,
    pub procedural: Vec<String>,
    pub scriptlets: Vec<String>,
}

impl Bundle {
    pub fn is_empty(&self) -> bool {
        self.hide.is_empty() && self.procedural.is_empty() && self.scriptlets.is_empty()
    }
}

impl SiteIndex {
    fn add(&mut self, host: &str, kind: Kind, text: &str) {
        let key = (kind, text.to_string());
        let id = match self.dedupe.get(&key) {
            Some(&id) => id,
            None => {
                self.entries.push(key.clone());
                let id = (self.entries.len() - 1) as u32;
                self.dedupe.insert(key, id);
                id
            }
        };
        let v = self.map.entry(host.to_ascii_lowercase()).or_default();
        if !v.contains(&id) {
            v.push(id);
        }
    }

    pub fn sites(&self) -> usize {
        self.map.len()
    }

    /// The host and its parent domains, then entity forms (`google.*`).
    fn keys(host: &str) -> Vec<String> {
        let host = host.to_ascii_lowercase();
        let host = host.trim_end_matches('.');
        let mut keys = Vec::new();
        let mut h = host;
        loop {
            keys.push(h.to_string());
            // Entity: drop one or two trailing labels (example.com → example.*, example.co.uk → example.*).
            let labels: Vec<&str> = h.split('.').collect();
            if labels.len() >= 2 {
                keys.push(format!("{}.*", labels[..labels.len() - 1].join(".")));
            }
            if labels.len() >= 3 {
                keys.push(format!("{}.*", labels[..labels.len() - 2].join(".")));
            }
            match h.find('.') {
                Some(i) => h = &h[i + 1..],
                None => break,
            }
        }
        keys
    }

    fn any_key_in(set: &HashSet<String>, keys: &[String]) -> bool {
        keys.iter().any(|k| set.contains(k))
    }

    /// Everything to inject on a page at `host`.
    pub fn lookup(&self, host: &str) -> Bundle {
        let keys = Self::keys(host);
        let mut b = Bundle::default();
        if keys.is_empty() || Self::any_key_in(&self.page_allow, &keys) {
            return b;
        }
        let no_cosmetic = Self::any_key_in(&self.ehide, &keys);
        let no_generic = no_cosmetic || Self::any_key_in(&self.ghide, &keys);
        let no_specific = no_cosmetic || Self::any_key_in(&self.shide, &keys);
        let mut ids: Vec<u32> = Vec::new();
        for k in &keys {
            if let Some(v) = self.map.get(k) {
                ids.extend(v);
            }
        }
        let generic: &[u32] = self.map.get("*").map(Vec::as_slice).unwrap_or(&[]);
        let mut hide_ex: HashSet<&str> = HashSet::new();
        let mut script_ex: HashSet<&str> = HashSet::new();
        for &id in &ids {
            let (k, t) = &self.entries[id as usize];
            match k {
                Kind::HideException => {
                    hide_ex.insert(t);
                }
                Kind::ScriptletException => {
                    script_ex.insert(t);
                }
                _ => {}
            }
        }
        let all_scripts_off = script_ex.contains("");
        let mut seen: HashSet<u32> = HashSet::new();
        let specific = ids.iter().map(|&i| (i, false));
        let generic = generic.iter().map(|&i| (i, true));
        for (id, is_generic) in specific.chain(generic) {
            if !seen.insert(id) {
                continue;
            }
            let (k, t) = &self.entries[id as usize];
            match k {
                Kind::Hide | Kind::Procedural => {
                    if (is_generic && no_generic) || (!is_generic && no_specific) || hide_ex.contains(t.as_str()) {
                        continue;
                    }
                    if *k == Kind::Hide { b.hide.push(t.clone()) } else { b.procedural.push(t.clone()) }
                }
                Kind::Scriptlet => {
                    if !all_scripts_off && !script_ex.contains(t.as_str()) {
                        b.scriptlets.push(t.clone());
                    }
                }
                _ => {}
            }
        }
        b
    }

    /// A compact text form, so Volant can load the index at launch without re-reading the lists.
    pub fn serialize(&self) -> String {
        let mut out = String::with_capacity(self.entries.len() * 48);
        for (i, (k, t)) in self.entries.iter().enumerate() {
            out.push_str(&format!("e\t{}\t{}\t{}\n", i, *k as u8 as char, t.replace(['\n', '\t'], " ")));
        }
        let mut hosts: Vec<(&String, &Vec<u32>)> = self.map.iter().collect();
        hosts.sort();
        for (h, ids) in hosts {
            let ids: Vec<String> = ids.iter().map(|i| i.to_string()).collect();
            out.push_str(&format!("m\t{h}\t{}\n", ids.join(",")));
        }
        for (tag, set) in [("a", &self.page_allow), ("g", &self.ghide), ("x", &self.ehide), ("s", &self.shide)] {
            for d in set {
                out.push_str(&format!("{tag}\t{d}\n"));
            }
        }
        out
    }

    pub fn parse(text: &str) -> Self {
        let mut s = Self::default();
        for line in text.lines() {
            let mut f = line.splitn(4, '\t');
            match f.next() {
                Some("e") => {
                    let (Some(_), Some(k), Some(t)) = (f.next(), f.next(), f.next()) else { continue };
                    let Some(k) = k.bytes().next().and_then(Kind::from) else { continue };
                    s.entries.push((k, t.to_string()));
                }
                Some("m") => {
                    let (Some(h), Some(ids)) = (f.next(), f.next()) else { continue };
                    let ids: Vec<u32> = ids.split(',').filter_map(|x| x.parse().ok()).filter(|&i: &u32| (i as usize) < s.entries.len()).collect();
                    s.map.insert(h.to_string(), ids);
                }
                Some(tag @ ("a" | "g" | "x" | "s")) => {
                    let Some(d) = f.next() else { continue };
                    let set = match tag {
                        "a" => &mut s.page_allow,
                        "g" => &mut s.ghide,
                        "x" => &mut s.ehide,
                        _ => &mut s.shide,
                    };
                    set.insert(d.to_string());
                }
                _ => {}
            }
        }
        s
    }
}

// ---------------------------------------------------------------- the compiler

/// Everything a compile produces.
#[derive(Default)]
pub struct Compiled {
    /// Network rule lists for WebKit, each under its rule limit.
    pub network: Vec<String>,
    /// Element hiding for WebKit (empty when cosmetic filtering is off).
    pub cosmetic: String,
    /// Remote fonts / pings, from the switches in Settings.
    pub extras: String,
    pub sites: SiteIndex,
    pub stats: Stats,
}

#[derive(Default, Debug)]
pub struct Stats {
    pub network_rules: usize,
    pub cosmetic_rules: usize,
    pub scriptlets: usize,
    pub procedural: usize,
    pub skipped: usize,
    /// Filters taken from each list, by list id.
    pub per_list: Vec<(String, usize)>,
}

impl Stats {
    pub fn total(&self) -> usize {
        self.network_rules + self.cosmetic_rules + self.scriptlets + self.procedural
    }
}

struct Net {
    trigger: Trigger,
    important: bool,
}

#[derive(Default)]
struct Builder {
    blocks: Vec<Net>,
    allows: Vec<Trigger>,
    seen: HashSet<String>,
    badfilters: HashSet<String>,
    canon: Vec<(String, usize, bool)>, // (canonical filter, index, is_allow)
    generic_hide: Vec<String>,
    seen_generic: HashSet<String>,
    generic_exceptions: HashMap<String, Vec<String>>,
    /// selector → (include domains, exclude domains) for WebKit-safe site-specific hiding
    specific: BTreeMap<(Vec<String>, Vec<String>), Vec<String>>,
    specific_exceptions: HashSet<(String, String)>,
    sites: SiteIndex,
    stats: Stats,
}

/// `||example.com^$script,third-party` → `||example.com^$script,third-party`
/// with options sorted, so `$badfilter` can find its target.
fn canonical(pattern: &str, opts: &[&str]) -> String {
    let mut o: Vec<&str> = opts.iter().copied().filter(|o| *o != "badfilter" && !o.is_empty()).collect();
    o.sort_unstable();
    if o.is_empty() { pattern.to_string() } else { format!("{pattern}${}", o.join(",")) }
}

impl Builder {
    fn line(&mut self, raw: &str) -> bool {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('!') || (line.starts_with('[') && line.ends_with(']')) {
            return false;
        }
        // Hosts-file lines.
        if let Some(rest) = line.strip_prefix("0.0.0.0 ").or_else(|| line.strip_prefix("127.0.0.1 ")) {
            let host = rest.split_whitespace().next().unwrap_or("");
            if host.is_empty() || host == "localhost" || host == "0.0.0.0" || host.contains('#') {
                return false;
            }
            return self.network(&format!("||{host}^"));
        }
        if let Some(i) = cosmetic_split(line) {
            return self.cosmetic(line, i);
        }
        self.network(line)
    }

    fn cosmetic(&mut self, line: &str, (at, sep): (usize, &str)) -> bool {
        let domains = &line[..at];
        let body = line[at + sep.len()..].trim();
        let exception = sep.contains('@');
        // AdGuard-only syntaxes, HTML filtering and response-body filters: not supported.
        if sep.contains('$') || sep.contains('%') || sep.contains('?') || body.starts_with('^') {
            self.stats.skipped += 1;
            return false;
        }
        let mut include = Vec::new();
        let mut exclude = Vec::new();
        for d in domains.split(',').map(str::trim).filter(|d| !d.is_empty()) {
            if d.starts_with('/') {
                self.stats.skipped += 1;
                return false; // regex domains
            }
            match d.strip_prefix('~') {
                Some(x) => exclude.push(x.to_ascii_lowercase()),
                None => include.push(d.to_ascii_lowercase()),
            }
        }
        // Scriptlets.
        if let Some(call) = body.strip_prefix("+js(").and_then(|b| b.strip_suffix(')')) {
            let call = call.trim();
            if exception {
                for d in if include.is_empty() { vec!["*".to_string()] } else { include } {
                    self.sites.add(&d, Kind::ScriptletException, call);
                }
                return true;
            }
            if include.is_empty() || call.is_empty() {
                self.stats.skipped += 1;
                return false; // uBO doesn't run generic scriptlets
            }
            for d in &include {
                self.sites.add(d, Kind::Scriptlet, call);
            }
            for d in &exclude {
                self.sites.add(d, Kind::ScriptletException, call);
            }
            self.stats.scriptlets += 1;
            return true;
        }
        if !selector_ok(body) {
            self.stats.skipped += 1;
            return false;
        }
        let procedural = is_procedural(body);
        let entity = include.iter().chain(&exclude).any(|d| d.ends_with(".*"));
        if exception {
            if include.is_empty() {
                self.stats.skipped += 1;
                return false;
            }
            for d in &include {
                self.sites.add(d, Kind::HideException, body);
                self.specific_exceptions.insert((d.clone(), body.to_string()));
                if let Some(w) = webkit_domain(d) {
                    self.generic_exceptions.entry(body.to_string()).or_default().push(w);
                }
            }
            return true;
        }
        if procedural {
            let hosts = if include.is_empty() { vec!["*".to_string()] } else { include };
            for d in &hosts {
                self.sites.add(d, Kind::Procedural, body);
            }
            for d in &exclude {
                self.sites.add(d, Kind::HideException, body);
            }
            self.stats.procedural += 1;
            return true;
        }
        let safe = webkit_safe_selector(body);
        if include.is_empty() && exclude.is_empty() {
            if safe {
                if self.seen_generic.insert(body.to_string()) {
                    self.generic_hide.push(body.to_string());
                }
            } else {
                self.sites.add("*", Kind::Hide, body);
            }
            self.stats.cosmetic_rules += 1;
            return true;
        }
        if include.is_empty() {
            // `~site##.ad`: everywhere except those sites.
            if safe {
                let ex: Vec<String> = exclude.iter().filter_map(|d| webkit_domain(d)).collect();
                self.specific.entry((Vec::new(), ex)).or_default().push(body.to_string());
            } else {
                self.sites.add("*", Kind::Hide, body);
                for d in &exclude {
                    self.sites.add(d, Kind::HideException, body);
                }
            }
            self.stats.cosmetic_rules += 1;
            return true;
        }
        if safe && !entity && exclude.is_empty() {
            let inc: Vec<String> = include.iter().filter_map(|d| webkit_domain(d)).collect();
            if inc.len() == include.len() {
                let mut inc = inc;
                inc.sort();
                self.specific.entry((inc, Vec::new())).or_default().push(body.to_string());
                self.stats.cosmetic_rules += 1;
                return true;
            }
        }
        for d in &include {
            self.sites.add(d, Kind::Hide, body);
        }
        for d in &exclude {
            self.sites.add(d, Kind::HideException, body);
        }
        self.stats.cosmetic_rules += 1;
        true
    }

    fn network(&mut self, line: &str) -> bool {
        let (exception, body) = match line.strip_prefix("@@") {
            Some(b) => (true, b),
            None => (false, line),
        };
        // Split off options at the last `$` that isn't part of a regex.
        let (pattern, opts) = split_options(body);
        let opts: Vec<&str> = opts.map(|o| o.split(',').map(str::trim).collect()).unwrap_or_default();
        let canon = canonical(pattern, &opts);
        if opts.contains(&"badfilter") {
            self.badfilters.insert(format!("{}{canon}", if exception { "@@" } else { "" }));
            return true;
        }
        let mut t = Trigger::default();
        let mut types: Vec<&'static str> = Vec::new();
        let mut not_types: Vec<&'static str> = Vec::new();
        let mut important = false;
        let mut page_allow = false;
        let mut cosmetic_off: Option<&str> = None;
        let mut to_domains: Vec<String> = Vec::new();
        let mut entity_domain = false;
        for o in &opts {
            let (neg, name) = match o.strip_prefix('~') {
                Some(n) => (true, n),
                None => (false, *o),
            };
            let (key, value) = name.split_once('=').unwrap_or((name, ""));
            match key {
                "third-party" | "3p" | "strict3p" => t.load_type = Some(if neg { "first-party" } else { "third-party" }),
                "first-party" | "1p" | "strict1p" => t.load_type = Some(if neg { "third-party" } else { "first-party" }),
                "match-case" => t.case_sensitive = !neg,
                "important" => important = true,
                "all" | "" | "reason" => {}
                "redirect" | "rewrite" | "empty" | "mp4" => {} // block; WebKit can't serve a stand-in resource
                "domain" | "from" => {
                    for d in value.split('|') {
                        if d.ends_with(".*") {
                            entity_domain = true;
                            continue;
                        }
                        match d.strip_prefix('~') {
                            Some(x) => t.unless_domain.extend(webkit_domain(x)),
                            None => match webkit_domain(d) {
                                Some(x) => t.if_domain.push(x),
                                None => {
                                    self.stats.skipped += 1;
                                    return false;
                                }
                            },
                        }
                    }
                }
                "to" => {
                    for d in value.split('|').filter(|d| !d.starts_with('~') && !d.ends_with(".*")) {
                        to_domains.push(d.to_ascii_lowercase());
                    }
                }
                "document" | "doc" if exception => page_allow = true,
                "generichide" | "ghide" | "elemhide" | "ehide" | "specifichide" | "shide" if exception => cosmetic_off = Some(key),
                k => match resource_type(k) {
                    Some(rt) if neg => not_types.extend_from_slice(rt),
                    Some(rt) => types.extend_from_slice(rt),
                    None => {
                        // removeparam, csp, redirect-rule, denyallow, header, replace, urlskip, method, ...
                        self.stats.skipped += 1;
                        return false;
                    }
                },
            }
        }
        if entity_domain && t.if_domain.is_empty() {
            self.stats.skipped += 1;
            return false;
        }
        // Page-level exceptions: by the anchored host, or the domain= list.
        if page_allow || cosmetic_off.is_some() {
            let mut doms: Vec<String> = t.if_domain.iter().map(|d| d.trim_start_matches('*').to_string()).collect();
            if let Some(h) = anchored_host(pattern) {
                doms.push(h.to_ascii_lowercase());
            }
            if doms.is_empty() {
                self.stats.skipped += 1;
                return false;
            }
            for d in doms {
                let set = match cosmetic_off {
                    None => &mut self.sites.page_allow,
                    Some("generichide" | "ghide") => &mut self.sites.ghide,
                    Some("specifichide" | "shide") => &mut self.sites.shide,
                    _ => &mut self.sites.ehide,
                };
                set.insert(d);
            }
            return true;
        }
        if !not_types.is_empty() {
            let base: Vec<&str> = if types.is_empty() { ALL_TYPES.to_vec() } else { types.clone() };
            types = base.into_iter().filter(|x| !not_types.contains(x)).collect();
            if types.is_empty() {
                self.stats.skipped += 1;
                return false;
            }
        }
        types.sort_unstable();
        types.dedup();
        t.resource_types = types;
        let mut triggers = Vec::new();
        if !to_domains.is_empty() {
            if !(pattern.is_empty() || pattern == "*") {
                self.stats.skipped += 1;
                return false;
            }
            for d in &to_domains {
                let mut tt = t.clone();
                tt.url_filter = host_regex(d);
                triggers.push(tt);
            }
        } else {
            let regex = pattern.len() > 2 && pattern.starts_with('/') && pattern.ends_with('/');
            let filter = if regex { regex_to_webkit(&pattern[1..pattern.len() - 1]) } else { pattern_to_regex(pattern) };
            let Some(f) = filter else {
                self.stats.skipped += 1;
                return false;
            };
            if f == ".*" && t.if_domain.is_empty() && t.resource_types.is_empty() && !exception {
                self.stats.skipped += 1;
                return false; // would block everything
            }
            if !regex && !pattern.starts_with('|') && pattern.trim_matches('*').len() < 4 && !pattern.is_empty() {
                self.stats.skipped += 1;
                return false; // tiny bare patterns ("ad") do more harm than good
            }
            t.url_filter = f;
            triggers.push(t);
        }
        for tt in triggers {
            if !self.seen.insert(format!("{}{}", if exception { "a" } else { "b" }, tt.key())) {
                continue;
            }
            let idx = if exception { self.allows.len() } else { self.blocks.len() };
            self.canon.push((format!("{}{canon}", if exception { "@@" } else { "" }), idx, exception));
            if exception {
                self.allows.push(tt);
            } else {
                self.blocks.push(Net { trigger: tt, important });
            }
        }
        true
    }

    fn finish(mut self, opts: Options) -> Compiled {
        // $badfilter: drop the filters it names.
        if !self.badfilters.is_empty() {
            let mut kill_block = HashSet::new();
            let mut kill_allow = HashSet::new();
            for (c, idx, allow) in &self.canon {
                if self.badfilters.contains(c) {
                    if *allow { kill_allow.insert(*idx) } else { kill_block.insert(*idx) };
                }
            }
            self.blocks = self.blocks.into_iter().enumerate().filter(|(i, _)| !kill_block.contains(i)).map(|(_, n)| n).collect();
            self.allows = self.allows.into_iter().enumerate().filter(|(i, _)| !kill_allow.contains(i)).map(|(_, t)| t).collect();
        }
        let page_allow: Vec<String> = self.sites.page_allow.iter().filter_map(|d| webkit_domain(d)).collect();
        let page_allow_trigger = |doms: &[String]| Trigger { url_filter: ".*".into(), if_domain: doms.to_vec(), ..Default::default() };

        // Network: shards of ordinary blocks, each followed by every exception
        // (and page-level exceptions), then `$important` blocks, which beat exceptions.
        let (important, ordinary): (Vec<Net>, Vec<Net>) = self.blocks.into_iter().partition(|n| n.important);
        let room = SHARD_RULES.saturating_sub(self.allows.len() + 1).max(10_000);
        let mut network = Vec::new();
        let mut net_rules = 0;
        let chunks: Vec<&[Net]> = if ordinary.is_empty() { vec![&[]] } else { ordinary.chunks(room).collect() };
        for (i, chunk) in chunks.iter().enumerate() {
            let mut list = RuleList::new();
            for n in *chunk {
                list.push(&n.trigger, "block", None);
            }
            for a in &self.allows {
                list.push(a, "ignore-previous-rules", None);
            }
            for chunk in page_allow.chunks(500) {
                list.push(&page_allow_trigger(chunk), "ignore-previous-rules", None);
            }
            if i == 0 {
                for n in &important {
                    list.push(&n.trigger, "block", None);
                }
            }
            net_rules += list.rules;
            network.push(list.finish());
        }

        // Cosmetic: generic hiding batched (separately for selectors with `#@#`
        // exceptions), site hiding grouped by site; then generichide/elemhide.
        let mut cos = RuleList::new();
        if opts.cosmetic {
            let mut batch = String::new();
            let mut n = 0;
            for s in &self.generic_hide {
                if let Some(doms) = self.generic_exceptions.get(s) {
                    let t = Trigger { url_filter: ".*".into(), unless_domain: doms.clone(), ..Default::default() };
                    cos.push(&t, "css-display-none", Some(s));
                    continue;
                }
                if n > 0 {
                    batch.push_str(", ");
                }
                batch.push_str(s);
                n += 1;
                if n == 400 {
                    cos.push(&Trigger::everywhere(), "css-display-none", Some(&batch));
                    batch.clear();
                    n = 0;
                }
            }
            if n > 0 {
                cos.push(&Trigger::everywhere(), "css-display-none", Some(&batch));
            }
            // generichide sites: cancel the generic rules above, before the site-specific ones.
            let ghide: Vec<String> = self.sites.ghide.iter().chain(&self.sites.ehide).filter_map(|d| webkit_domain(d)).collect();
            for chunk in ghide.chunks(500) {
                cos.push(&page_allow_trigger(chunk), "ignore-previous-rules", None);
            }
            for ((inc, exc), sels) in &self.specific {
                let sels: Vec<&String> = sels
                    .iter()
                    .filter(|s| !inc.iter().any(|d| self.specific_exceptions.contains(&(d.trim_start_matches('*').to_string(), (*s).clone()))))
                    .collect();
                for group in sels.chunks(200) {
                    let joined = group.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ");
                    let t = Trigger { url_filter: ".*".into(), if_domain: inc.clone(), unless_domain: exc.clone(), ..Default::default() };
                    cos.push(&t, "css-display-none", Some(&joined));
                }
            }
            let off: Vec<String> = self.sites.ehide.iter().chain(&self.sites.shide).chain(&self.sites.page_allow).filter_map(|d| webkit_domain(d)).collect();
            for chunk in off.chunks(500) {
                cos.push(&page_allow_trigger(chunk), "ignore-previous-rules", None);
            }
        }
        let cosmetic_rules = cos.rules;

        let mut extras = RuleList::new();
        if opts.block_fonts {
            extras.push(&Trigger { url_filter: ".*".into(), resource_types: vec!["font"], ..Default::default() }, "block", None);
        }
        if opts.block_pings {
            extras.push(&Trigger { url_filter: ".*".into(), resource_types: vec!["ping"], ..Default::default() }, "block", None);
        }
        if extras.rules == 0 {
            // WebKit won't compile an empty list; a rule that can never match keeps the shape simple.
            extras.push(&Trigger { url_filter: "^volant-never-matches:".into(), ..Default::default() }, "block", None);
        }
        if !opts.cosmetic {
            self.sites.entries.retain(|_| true);
            // Cosmetic filtering off: keep only scriptlets in the site index.
            let keep: Vec<bool> = self.sites.entries.iter().map(|(k, _)| matches!(k, Kind::Scriptlet | Kind::ScriptletException)).collect();
            for ids in self.sites.map.values_mut() {
                ids.retain(|&i| keep[i as usize]);
            }
        }
        self.stats.network_rules = net_rules;
        Compiled { network, cosmetic: if cosmetic_rules > 0 { cos.finish() } else { String::new() }, extras: extras.finish(), sites: self.sites, stats: self.stats }
    }
}

/// Where a cosmetic filter's separator is, if this is one.
fn cosmetic_split(line: &str) -> Option<(usize, &'static str)> {
    for sep in ["#@#", "#?#", "#$#", "#%#", "#@$#", "#@?#", "#@%#", "##"] {
        if let Some(i) = line.find(sep) {
            // `##` inside a URL is part of a network filter.
            if line[..i].contains('/') || line[..i].contains('|') || line[..i].contains('$') {
                return None;
            }
            return Some((i, sep));
        }
    }
    if let Some(i) = line.find("$$").or_else(|| line.find("$@$")) {
        if !line[..i].contains('/') {
            return Some((i, if line[i..].starts_with("$@$") { "$@$" } else { "$$" }));
        }
    }
    None
}

fn split_options(body: &str) -> (&str, Option<&str>) {
    if body.starts_with('/') {
        // A regex filter: options follow the closing slash.
        if let Some(end) = body.rfind("/$") {
            if end > 0 {
                return (&body[..end + 1], Some(&body[end + 2..]));
            }
        }
        return (body, None);
    }
    match body.rfind('$') {
        Some(i) if !body[i + 1..].contains('/') || body[i + 1..].starts_with("domain=") || body[i + 1..].contains("domain=") => (&body[..i], Some(&body[i + 1..])),
        _ => (body, None),
    }
}

/// Compiles filter lists (`(list id, text)`) into WebKit rule lists and the site index.
pub fn compile(lists: &[(&str, &str)], opts: Options) -> Compiled {
    let mut b = Builder::default();
    for (id, text) in lists {
        let before = b.stats.total() + b.blocks.len() + b.allows.len();
        for line in preprocess(text) {
            b.line(line);
        }
        let after = b.stats.total() + b.blocks.len() + b.allows.len();
        b.stats.per_list.push((id.to_string(), after.saturating_sub(before)));
    }
    b.finish(opts)
}

/// Splits a scriptlet call (`name, arg one, 'arg, two'`) into its parts the
/// way uBlock Origin does: commas separate, `\,` is a literal comma, and an
/// argument may be quoted.
pub fn scriptlet_args(call: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = call.chars().peekable();
    let mut quote: Option<char> = None;
    let mut started = false;
    while let Some(c) = chars.next() {
        match (c, quote) {
            ('\\', _) if chars.peek() == Some(&',') => {
                cur.push(',');
                chars.next();
            }
            ('\\', Some(q)) if chars.peek() == Some(&q) => {
                cur.push(q);
                chars.next();
            }
            (q, None) if !started && (q == '\'' || q == '"' || q == '`') => {
                quote = Some(q);
                started = true;
            }
            (c, Some(q)) if c == q => quote = None,
            (',', None) => {
                out.push(cur.trim().to_string());
                cur.clear();
                started = false;
            }
            (c, _) => {
                if !c.is_whitespace() {
                    started = true;
                }
                cur.push(c);
            }
        }
    }
    out.push(cur.trim().to_string());
    if out.len() == 1 && out[0].is_empty() {
        out.clear();
    }
    out
}

/// What gets injected into a page, as a JavaScript object literal.
pub fn bundle_json(b: &Bundle) -> String {
    let strs = |v: &[String]| crate::json::array(v.iter().map(|s| crate::json::string(s)));
    let calls = crate::json::array(b.scriptlets.iter().map(|c| strs(&scriptlet_args(c))));
    format!("{{\"hide\":{},\"procedural\":{},\"scriptlets\":{calls}}}", strs(&b.hide), strs(&b.procedural))
}

/// A list for the sites you've trusted: blocking off there.
pub fn trusted_list(domains: &[String]) -> Option<String> {
    let doms: Vec<String> = domains.iter().filter_map(|d| webkit_domain(d)).collect();
    if doms.is_empty() {
        return None;
    }
    let mut l = RuleList::new();
    l.push(&Trigger { url_filter: ".*".into(), if_domain: doms, ..Default::default() }, "ignore-previous-rules", None);
    Some(l.finish())
}

/// Whether `host` is one of the trusted sites (or under one).
pub fn is_trusted(host: &str, trusted: &[String]) -> bool {
    let host = host.to_ascii_lowercase();
    let host = host.strip_prefix("www.").unwrap_or(&host);
    trusted.iter().any(|t| {
        let t = t.trim().trim_start_matches("*.").to_ascii_lowercase();
        let t = t.strip_prefix("www.").unwrap_or(&t);
        !t.is_empty() && (host == t || host.ends_with(&format!(".{t}")))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(text: &str) -> Compiled {
        compile(&[("test", text)], Options::default())
    }

    #[test]
    fn preprocessor() {
        let t = "a\n!#if env_safari\nb\n!#else\nc\n!#endif\n!#if !ext_ublock || env_chromium\nd\n!#endif\n!#if (env_safari && !env_mobile)\ne\n!#endif\nf";
        assert_eq!(preprocess(t).collect::<Vec<_>>(), ["a", "b", "e", "f"]);
    }

    #[test]
    fn network_and_exceptions_in_every_shard() {
        let mut text = String::new();
        for i in 0..(SHARD_RULES + 10) {
            text.push_str(&format!("||ads{i}.example^\n"));
        }
        text.push_str("@@||ads1.example^\n||imp.example^$important\n");
        let c = one(&text);
        assert!(c.network.len() >= 2, "split into shards");
        for shard in &c.network {
            assert!(shard.contains("ignore-previous-rules"), "every shard carries the exceptions");
        }
        let first = &c.network[0];
        assert!(first.rfind("imp\\\\.example").unwrap() > first.rfind("ignore-previous-rules").unwrap(), "$important comes after exceptions");
    }

    #[test]
    fn badfilter_and_options() {
        let c = one("||bad.example^$script,third-party\n||bad.example^$third-party,script,badfilter\n||good.example^$redirect=noopjs,script\n||x.example^$removeparam=utm\n*$script,to=tracker.a|tracker.b\n/ad[0-9]+\\.js/$script\n/a|b/");
        let all = c.network.join("");
        assert!(!all.contains("bad\\\\.example"), "badfilter removed it: {all}");
        assert!(all.contains("good\\\\.example"), "redirect= still blocks");
        assert!(!all.contains("x\\\\.example"), "removeparam skipped");
        assert!(all.contains("tracker\\\\.a") && all.contains("tracker\\\\.b"), "to= expands");
        assert!(all.contains("ad[0-9]+\\\\.js"), "regex kept: {all}");
        assert_eq!(c.stats.skipped, 2, "removeparam and the alternation regex");
    }

    #[test]
    fn cosmetics_and_sites() {
        let c = one(concat!(
            "##.ad\n##div:has(> .sponsored)\nexample.com##.promo\nexample.com,~sub.example.com##.banner\n",
            "example.com##h1:has-text(Ad)\nexample.com##+js(set, foo, true)\nexample.com#@#+js(set, foo, true)\n",
            "news.example##+js(aopr, adBlock)\ngoogle.*##.g-ad\n@@||quiet.example^$document\n@@||nogeneric.example^$ghide\n",
        ));
        assert!(c.cosmetic.contains("\".ad\"") || c.cosmetic.contains(".ad"), "{}", c.cosmetic);
        assert!(c.cosmetic.contains("*example.com") && c.cosmetic.contains(".promo"));
        let b = c.sites.lookup("www.example.com");
        assert!(b.procedural.contains(&"h1:has-text(Ad)".to_string()));
        assert!(b.scriptlets.is_empty(), "exception cancels the scriptlet");
        assert!(b.hide.contains(&"div:has(> .sponsored)".to_string()), "generic :has goes to the page");
        assert!(b.hide.contains(&".banner".to_string()));
        assert!(!c.sites.lookup("sub.example.com").hide.contains(&".banner".to_string()), "~sub excluded");
        assert_eq!(c.sites.lookup("news.example").scriptlets, vec!["aopr, adBlock".to_string()]);
        assert!(c.sites.lookup("www.google.co.uk").hide.contains(&".g-ad".to_string()), "entity domain");
        assert!(c.sites.lookup("quiet.example").is_empty(), "$document turns everything off");
        assert!(!c.sites.lookup("nogeneric.example").hide.contains(&"div:has(> .sponsored)".to_string()), "ghide");
        let again = SiteIndex::parse(&c.sites.serialize());
        assert_eq!(again.lookup("www.example.com"), b, "index round-trips");
    }

    #[test]
    fn scriptlet_arguments() {
        assert_eq!(scriptlet_args("set, foo.bar, true"), ["set", "foo.bar", "true"]);
        assert_eq!(scriptlet_args(r"rmnt, script, a\, b"), ["rmnt", "script", "a, b"]);
        assert_eq!(scriptlet_args("aeld, 'click, touch', /x/"), ["aeld", "click, touch", "/x/"]);
        assert_eq!(scriptlet_args("nowebrtc"), ["nowebrtc"]);
        let j = bundle_json(&Bundle { hide: vec![".a".into()], procedural: vec![], scriptlets: vec!["set, x, 1".into()] });
        assert_eq!(j, r#"{"hide":[".a"],"procedural":[],"scriptlets":[["set","x","1"]]}"#);
    }

    #[test]
    fn safe_selectors() {
        assert!(webkit_safe_selector(r#"a[href^="https://ads.example/"]"#));
        assert!(webkit_safe_selector("div.ad:not(.keep) > span:nth-child(2)"));
        assert!(!webkit_safe_selector("div:has(> .ad)"));
        assert!(!webkit_safe_selector("div::before"));
        assert!(!webkit_safe_selector("a:hover"));
        assert!(!webkit_safe_selector("div[unterminated"));
    }

    #[test]
    fn trusted() {
        let t = vec!["example.com".to_string()];
        assert!(is_trusted("www.example.com", &t) && is_trusted("a.example.com", &t));
        assert!(!is_trusted("notexample.com", &t));
        assert!(trusted_list(&t).unwrap().contains("*example.com"));
    }

    #[test]
    fn fallback_compiles() {
        let c = compile(&[("fallback", FALLBACK)], Options::default());
        assert!(c.stats.network_rules > 100);
    }
}
