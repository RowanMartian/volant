//! Ad and tracker blocking, compiled to run inside WebKit itself.
//!
//! JavaScript blockers inspect every request from inside each page. Volant
//! instead translates Adblock Plus / uBlock Origin filter lists into WebKit's
//! content-rule format once. WebKit then compiles those rules into a bytecode
//! state machine that runs in its networking and style code. Pages get no
//! extra scripts, no extra processes, and nothing to slow them down.
//!
//! Supported: network filters (`||host^`, `|https://`, wildcards, separators),
//! the common options (`third-party`, resource types, `domain=`, `match-case`,
//! `document` exceptions), `@@` exceptions, element hiding (`##`, `domain##`,
//! `#@#` exceptions), and hosts-file lines. Anything WebKit can't express
//! (regex filters, redirects, scriptlets, procedural cosmetics) is skipped.

use crate::json::{push_str, string};
use std::collections::{HashMap, HashSet};

/// The list Volant ships with: the big ad networks and trackers.
pub const BUILTIN: &str = include_str!("../lists/builtin.txt");

/// Downloaded and compiled on your Mac in Strict mode.
pub const STRICT_LISTS: &[(&str, &str)] = &[
    ("EasyList", "https://easylist.to/easylist/easylist.txt"),
    ("EasyPrivacy", "https://easylist.to/easylist/easyprivacy.txt"),
];

/// WebKit refuses lists above this many rules.
pub const MAX_RULES: usize = 150_000;

#[derive(Debug, Default)]
pub struct Compiled {
    /// The JSON to hand to `WKContentRuleListStore`.
    pub json: String,
    pub rules: usize,
    pub skipped: usize,
}

#[derive(Default)]
struct Trigger {
    url_filter: String,
    case_sensitive: bool,
    resource_types: Vec<&'static str>,
    load_type: Option<&'static str>,
    if_domain: Vec<String>,
    unless_domain: Vec<String>,
}

impl Trigger {
    fn write(&self, out: &mut String) {
        out.push_str("{\"url-filter\":");
        push_str(out, &self.url_filter);
        if self.case_sensitive {
            out.push_str(",\"url-filter-is-case-sensitive\":true");
        }
        if !self.resource_types.is_empty() {
            out.push_str(",\"resource-type\":");
            list(out, self.resource_types.iter().copied());
        }
        if let Some(lt) = self.load_type {
            out.push_str(",\"load-type\":[\"");
            out.push_str(lt);
            out.push_str("\"]");
        }
        if !self.if_domain.is_empty() {
            out.push_str(",\"if-domain\":");
            list(out, self.if_domain.iter().map(String::as_str));
        } else if !self.unless_domain.is_empty() {
            out.push_str(",\"unless-domain\":");
            list(out, self.unless_domain.iter().map(String::as_str));
        }
        out.push('}');
    }
}

fn list<'a>(out: &mut String, items: impl Iterator<Item = &'a str>) {
    out.push('[');
    for (i, s) in items.enumerate() {
        if i > 0 {
            out.push(',');
        }
        push_str(out, s);
    }
    out.push(']');
}

const ALL_TYPES: &[&str] = &["document", "image", "style-sheet", "script", "font", "raw", "svg-document", "media", "popup", "ping"];

fn resource_type(opt: &str) -> Option<&'static [&'static str]> {
    Some(match opt {
        "script" => &["script"],
        "image" | "img" => &["image"],
        "stylesheet" | "css" => &["style-sheet"],
        "font" => &["font"],
        "media" | "object" => &["media"],
        "xmlhttprequest" | "xhr" | "websocket" | "other" => &["raw"],
        "subdocument" | "frame" => &["document"],
        "ping" | "beacon" => &["ping"],
        "popup" => &["popup"],
        "document" | "doc" => &["document"],
        _ => return None,
    })
}

/// A domain as WebKit wants it: lowercase ASCII, `*` to include subdomains.
fn webkit_domain(d: &str) -> Option<String> {
    let d = d.trim().trim_start_matches("*.").to_ascii_lowercase();
    if d.is_empty() || !d.is_ascii() || d.contains(['/', '*', ' ']) {
        return None;
    }
    // Entity filters like `google.*` can't be expressed.
    if d.ends_with('.') {
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

/// Translates the address part of a network filter to WebKit's regex subset.
fn pattern_to_regex(p: &str) -> Option<String> {
    if !p.is_ascii() {
        return None;
    }
    let mut out = String::with_capacity(p.len() + 32);
    let mut rest = p;
    let mut in_host = false;
    if let Some(r) = rest.strip_prefix("||") {
        out.push_str("^[^:]+:(//)?([^/]+\\.)?");
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
                    // WebKit always sees a `/` or `:port` right after the host.
                    out.push_str("[/:]");
                    in_host = false;
                } else if i + 1 == chars.len() {
                    // A trailing separator also matches the end of the address; matching
                    // a little more is the safe direction here.
                } else {
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

/// The domain a `||domain^` filter targets, for `$document` exceptions.
fn anchored_host(p: &str) -> Option<&str> {
    let r = p.strip_prefix("||")?;
    let end = r.find(['^', '/', '*', '|']).unwrap_or(r.len());
    let h = &r[..end];
    (!h.is_empty()).then_some(h)
}

const PROCEDURAL: &[&str] = &[
    ":has-text(", ":-abp-", ":xpath(", ":upward(", ":remove(", ":style(", ":matches-css", ":min-text-length(", ":watch-attr(", ":others(",
    ":matches-path(", ":matches-attr(", ":matches-prop(", ":remove-attr(", ":remove-class(", ":contains(", ":nth-ancestor(", ":if(", ":if-not(",
];

fn selector_ok(s: &str) -> bool {
    !s.is_empty() && s.is_ascii() && !PROCEDURAL.iter().any(|p| s.contains(p)) && !s.starts_with('+') && !s.contains("{") && !s.contains('}')
}

enum Parsed {
    Network { exception: bool, trigger: Trigger },
    HideGeneric(String),
    HideScoped { selector: String, include: Vec<String>, exclude: Vec<String> },
    HideException { selector: String, domains: Vec<String> },
    Skip,
    Ignore,
}

fn parse_line(line: &str) -> Parsed {
    let line = line.trim();
    if line.is_empty() || line.starts_with('!') || line.starts_with('[') || line.starts_with('#') && !line.starts_with("##") {
        return Parsed::Ignore;
    }
    // Hosts-file lines: `0.0.0.0 tracker.example`.
    if let Some(rest) = line.strip_prefix("0.0.0.0 ").or_else(|| line.strip_prefix("127.0.0.1 ")) {
        let host = rest.split_whitespace().next().unwrap_or("");
        if host.is_empty() || host == "localhost" || host == "0.0.0.0" {
            return Parsed::Ignore;
        }
        return parse_line(&format!("||{host}^"));
    }
    // Cosmetic filters.
    for sep in ["#@#", "##", "#?#", "#$#", "#%#", "#@$#", "#@?#", "$$", "$@$"] {
        if let Some((domains, selector)) = line.split_once(sep) {
            if sep.contains('?') || sep.contains('$') || sep.contains('%') || selector.starts_with('^') || selector.starts_with("+js") {
                return Parsed::Skip;
            }
            if domains.contains('/') || domains.contains('|') {
                // Not a cosmetic filter after all (e.g. a URL with a `##` fragment).
                break;
            }
            let selector = selector.trim().to_string();
            if !selector_ok(&selector) {
                return Parsed::Skip;
            }
            let mut include = Vec::new();
            let mut exclude = Vec::new();
            for d in domains.split(',').filter(|d| !d.is_empty()) {
                match d.strip_prefix('~') {
                    Some(x) => exclude.extend(webkit_domain(x)),
                    None => match webkit_domain(d) {
                        Some(x) => include.push(x),
                        None => return Parsed::Skip,
                    },
                }
            }
            return if sep == "#@#" {
                if include.is_empty() { Parsed::Skip } else { Parsed::HideException { selector, domains: include } }
            } else if include.is_empty() && exclude.is_empty() {
                Parsed::HideGeneric(selector)
            } else {
                Parsed::HideScoped { selector, include, exclude }
            };
        }
    }
    // Network filters.
    let (exception, body) = match line.strip_prefix("@@") {
        Some(b) => (true, b),
        None => (false, line),
    };
    if body.starts_with('/') && body.len() > 1 {
        let pat_end = body.rfind('$').filter(|&i| body[..i].ends_with('/') && i > 1).unwrap_or(body.len());
        if body[..pat_end].ends_with('/') {
            return Parsed::Skip; // regex filter
        }
    }
    let (pattern, opts) = match body.rfind('$') {
        Some(i) if !body[i + 1..].contains('/') || body[i + 1..].starts_with("domain=") => (&body[..i], Some(&body[i + 1..])),
        _ => (body, None),
    };
    let mut t = Trigger::default();
    let mut types: Vec<&'static str> = Vec::new();
    let mut not_types: Vec<&'static str> = Vec::new();
    let mut document_exception = false;
    if let Some(opts) = opts {
        for o in opts.split(',') {
            let o = o.trim();
            let (neg, name) = match o.strip_prefix('~') {
                Some(n) => (true, n),
                None => (false, o),
            };
            match name {
                "third-party" | "3p" => t.load_type = Some(if neg { "first-party" } else { "third-party" }),
                "first-party" | "1p" => t.load_type = Some(if neg { "third-party" } else { "first-party" }),
                "match-case" => t.case_sensitive = !neg,
                "important" | "all" | "" => {}
                n if n.starts_with("domain=") || n.starts_with("from=") => {
                    let list = n.split_once('=').map(|x| x.1).unwrap_or("");
                    for d in list.split('|') {
                        match d.strip_prefix('~') {
                            Some(x) => t.unless_domain.extend(webkit_domain(x)),
                            None => match webkit_domain(d) {
                                Some(x) => t.if_domain.push(x),
                                None => return Parsed::Skip,
                            },
                        }
                    }
                }
                "document" | "doc" if exception => document_exception = true,
                n => match resource_type(n) {
                    Some(rt) if neg => not_types.extend_from_slice(rt),
                    Some(rt) => types.extend_from_slice(rt),
                    // redirect, csp, removeparam, generichide, … can't be expressed.
                    None => return Parsed::Skip,
                },
            }
        }
    }
    if !not_types.is_empty() {
        let base: Vec<&str> = if types.is_empty() { ALL_TYPES.to_vec() } else { types.clone() };
        types = base.into_iter().filter(|x| !not_types.contains(x)).collect();
        if types.is_empty() {
            return Parsed::Skip;
        }
    }
    types.sort_unstable();
    types.dedup();
    if document_exception {
        // `@@||site.com^$document`: turn blocking off on that site entirely.
        let Some(h) = anchored_host(pattern).and_then(webkit_domain) else { return Parsed::Skip };
        t.url_filter = ".*".into();
        t.if_domain = vec![h];
        t.unless_domain.clear();
        t.resource_types.clear();
        return Parsed::Network { exception: true, trigger: t };
    }
    if pattern.is_empty() && t.if_domain.is_empty() && types.is_empty() {
        return Parsed::Skip; // would match everything
    }
    // Very short bare patterns ("ad", "-ads-") cause far more harm than good.
    if !pattern.starts_with('|') && pattern.trim_matches('*').len() < 4 && !pattern.is_empty() {
        return Parsed::Skip;
    }
    match pattern_to_regex(pattern) {
        Some(r) => t.url_filter = r,
        None => return Parsed::Skip,
    }
    t.resource_types = types;
    Parsed::Network { exception, trigger: t }
}

fn rule(out: &mut String, n: &mut usize, trigger: &Trigger, action: &str, selector: Option<&str>) {
    if *n > 0 {
        out.push(',');
    }
    out.push_str("{\"trigger\":");
    trigger.write(out);
    out.push_str(",\"action\":{\"type\":\"");
    out.push_str(action);
    out.push('"');
    if let Some(s) = selector {
        out.push_str(",\"selector\":");
        push_str(out, s);
    }
    out.push_str("}}");
    *n += 1;
}

/// Compiles one or more filter lists into a single WebKit content-rule list.
pub fn compile(lists: &[&str]) -> Compiled {
    let mut blocks: Vec<Trigger> = Vec::new();
    let mut allows: Vec<Trigger> = Vec::new();
    let mut generic: Vec<String> = Vec::new();
    let mut seen_generic: HashSet<String> = HashSet::new();
    let mut scoped: Vec<(String, Vec<String>, Vec<String>)> = Vec::new();
    let mut hide_exceptions: HashMap<String, Vec<String>> = HashMap::new();
    let mut seen_filters: HashSet<String> = HashSet::new();
    let mut skipped = 0;

    for text in lists {
        for line in text.lines() {
            match parse_line(line) {
                Parsed::Network { exception, trigger } => {
                    let mut key = String::new();
                    trigger.write(&mut key);
                    key.push(if exception { 'a' } else { 'b' });
                    if seen_filters.insert(key) {
                        if exception { allows.push(trigger) } else { blocks.push(trigger) }
                    }
                }
                Parsed::HideGeneric(s) => {
                    if seen_generic.insert(s.clone()) {
                        generic.push(s);
                    }
                }
                Parsed::HideScoped { selector, include, exclude } => scoped.push((selector, include, exclude)),
                Parsed::HideException { selector, domains } => hide_exceptions.entry(selector).or_default().extend(domains),
                Parsed::Skip => skipped += 1,
                Parsed::Ignore => {}
            }
        }
    }

    let mut out = String::with_capacity(blocks.len() * 120 + generic.len() * 40);
    out.push('[');
    let mut n = 0usize;
    let room = |n: usize| n < MAX_RULES - allows.len().min(MAX_RULES / 4);
    for t in &blocks {
        if !room(n) {
            skipped += 1;
            continue;
        }
        rule(&mut out, &mut n, t, "block", None);
    }
    // Generic hiding: batch selectors into a few big rules (WebKit handles one
    // large stylesheet far better than thousands of tiny ones). Selectors with
    // `#@#` exceptions get their own rule that skips those sites.
    let mut batch = String::new();
    let mut in_batch = 0;
    let everywhere = Trigger { url_filter: ".*".into(), ..Default::default() };
    for s in &generic {
        if let Some(domains) = hide_exceptions.get(s) {
            if room(n) {
                let t = Trigger { url_filter: ".*".into(), unless_domain: domains.clone(), ..Default::default() };
                rule(&mut out, &mut n, &t, "css-display-none", Some(s));
            }
            continue;
        }
        if in_batch > 0 {
            batch.push_str(", ");
        }
        batch.push_str(s);
        in_batch += 1;
        if in_batch == 500 {
            if room(n) {
                rule(&mut out, &mut n, &everywhere, "css-display-none", Some(&batch));
            }
            batch.clear();
            in_batch = 0;
        }
    }
    if in_batch > 0 && room(n) {
        rule(&mut out, &mut n, &everywhere, "css-display-none", Some(&batch));
    }
    for (selector, include, exclude) in &scoped {
        if !room(n) {
            skipped += 1;
            continue;
        }
        let t = Trigger { url_filter: ".*".into(), if_domain: include.clone(), unless_domain: exclude.clone(), ..Default::default() };
        rule(&mut out, &mut n, &t, "css-display-none", Some(selector));
    }
    // Exceptions last: `ignore-previous-rules` only cancels rules listed before it.
    for t in &allows {
        if n >= MAX_RULES {
            skipped += 1;
            continue;
        }
        rule(&mut out, &mut n, t, "ignore-previous-rules", None);
    }
    out.push(']');
    Compiled { json: out, rules: n, skipped }
}

/// A rule list that turns blocking off on the sites you've allowed.
pub fn allowlist(domains: &[String]) -> String {
    let doms: Vec<String> = domains.iter().filter_map(|d| webkit_domain(d)).collect();
    if doms.is_empty() {
        return "[]".into();
    }
    let mut o = String::from("[{\"trigger\":{\"url-filter\":\".*\",\"if-domain\":");
    list(&mut o, doms.iter().map(String::as_str));
    o.push_str("},\"action\":{\"type\":\"ignore-previous-rules\"}}]");
    let _ = string;
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(line: &str) -> String {
        let c = compile(&[line]);
        assert_eq!(c.rules, 1, "{line} → {}", c.json);
        c.json
    }

    #[test]
    fn network_filters() {
        assert_eq!(
            one("||doubleclick.net^"),
            r#"[{"trigger":{"url-filter":"^[^:]+:(//)?([^/]+\\.)?doubleclick\\.net[/:]"},"action":{"type":"block"}}]"#
        );
        let j = one("||ads.example.com/banner/*.gif$image,third-party");
        assert!(j.contains(r#""url-filter":"^[^:]+:(//)?([^/]+\\.)?ads\\.example\\.com/banner/.*\\.gif""#), "{j}");
        assert!(j.contains(r#""resource-type":["image"]"#) && j.contains(r#""load-type":["third-party"]"#));
        let j = one("/adserver/*$script,domain=a.com|b.org");
        assert!(j.contains(r#""if-domain":["*a.com","*b.org"]"#), "{j}");
        let j = one("@@||example.com^$document");
        assert!(j.contains(r#""if-domain":["*example.com"]"#) && j.contains("ignore-previous-rules"), "{j}");
        let j = one("|https://track.io/p?x=1|");
        assert!(j.contains(r#""url-filter":"^https://track\\.io/p\\?x=1$""#), "{j}");
        let j = one("0.0.0.0 tracker.example");
        assert!(j.contains("tracker\\\\.example"), "{j}");
    }

    #[test]
    fn skips_what_webkit_cannot_do() {
        for l in [
            "/ads[0-9]+\\.js/$script",
            "||x.com^$redirect=noop.js",
            "example.com##+js(abort-on-property-read, x)",
            "example.com#?#div:has-text(Ad)",
            "||x.com^$csp=script-src 'none'",
            "ad",
        ] {
            let c = compile(&[l]);
            assert_eq!(c.rules, 0, "{l} should be skipped: {}", c.json);
        }
        assert_eq!(compile(&["! comment", "[Adblock Plus 2.0]", ""]).rules, 0);
    }

    #[test]
    fn cosmetic_filters() {
        let c = compile(&["##.ad-banner", "##.sponsored", "example.com##.promo", "~news.com##.x", "##.keep", "site.org#@#.keep"]);
        assert!(c.json.contains(r#""selector":".ad-banner, .sponsored""#), "{}", c.json);
        assert!(c.json.contains(r#""if-domain":["*example.com"]"#));
        assert!(c.json.contains(r#""unless-domain":["*news.com"]"#));
        assert!(c.json.contains(r#""unless-domain":["*site.org"]},"action":{"type":"css-display-none","selector":".keep"}"#), "{}", c.json);
    }

    #[test]
    fn exceptions_come_last_and_dupes_collapse() {
        let c = compile(&["@@||good.com^", "||bad.com^", "||bad.com^"]);
        assert_eq!(c.rules, 2);
        assert!(c.json.find("block").unwrap() < c.json.find("ignore-previous-rules").unwrap());
    }

    #[test]
    fn builtin_list_compiles() {
        let c = compile(&[BUILTIN]);
        assert!(c.rules > 100, "{} rules", c.rules);
        assert!(c.json.starts_with('[') && c.json.ends_with(']'));
    }
}
