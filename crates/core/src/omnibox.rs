//! Turning what you type into somewhere to go.

use crate::search;

const SCHEMES: &[&str] = &["http://", "https://", "file://", "about:", "volant:", "data:", "mailto:", "blob:"];

/// Returns a URL if `input` already looks like one (possibly missing its scheme).
pub fn as_url(input: &str) -> Option<String> {
    let s = input.trim();
    if s.is_empty() || s.starts_with('?') {
        return None;
    }
    let lower = s.to_ascii_lowercase();
    if SCHEMES.iter().any(|p| lower.starts_with(p)) {
        return Some(s.to_string());
    }
    if s.chars().any(char::is_whitespace) {
        return None;
    }
    let end = s.find(['/', '?', '#']).unwrap_or(s.len());
    let authority = &s[..end];
    let authority = authority.rsplit_once('@').map(|(_, h)| h).unwrap_or(authority);
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) if !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) => (h, Some(p)),
        Some((_, p)) if p.is_empty() => return None,
        Some(_) => return None, // "foo:bar" is a search, not a URL
        None => (authority, None),
    };
    let host_l = host.to_ascii_lowercase();
    if host_l == "localhost" || host_l.ends_with(".localhost") || is_ipv4(host) {
        return Some(format!("http://{s}"));
    }
    if host.starts_with('[') && host.ends_with(']') {
        return Some(format!("http://{s}"));
    }
    let labels: Vec<&str> = host_l.split('.').collect();
    if labels.len() < 2 || labels.iter().any(|l| l.is_empty() || l.starts_with('-') || l.ends_with('-')) {
        return None;
    }
    if !labels.iter().all(|l| l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b >= 0x80)) {
        return None;
    }
    let tld = labels[labels.len() - 1];
    let tld_ok = tld.len() >= 2 && (tld.bytes().all(|b| b.is_ascii_alphabetic()) || tld.starts_with("xn--") || tld.bytes().any(|b| b >= 0x80));
    if !tld_ok {
        return None;
    }
    let _ = port;
    // Local development names usually run without TLS.
    let scheme = if tld == "local" || tld == "test" || tld == "internal" { "http" } else { "https" };
    Some(format!("{scheme}://{s}"))
}

fn is_ipv4(h: &str) -> bool {
    let parts: Vec<&str> = h.split('.').collect();
    parts.len() == 4 && parts.iter().all(|p| !p.is_empty() && p.len() <= 3 && p.parse::<u16>().map(|n| n <= 255).unwrap_or(false))
}

/// Where Enter takes you: the URL itself, or a search.
pub fn resolve(input: &str, engine: &str) -> String {
    let s = input.trim();
    match as_url(s) {
        Some(u) => u,
        None => search::search_url(engine, s.strip_prefix('?').unwrap_or(s)),
    }
}

/// The host of a URL, lowercased, without `www.`.
pub fn host(url: &str) -> &str {
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let auth = &rest[..end];
    let auth = auth.rsplit_once('@').map(|(_, h)| h).unwrap_or(auth);
    let h = if auth.starts_with('[') {
        auth.find(']').map(|i| &auth[..=i]).unwrap_or(auth)
    } else {
        auth.split(':').next().unwrap_or(auth)
    };
    h.strip_prefix("www.").unwrap_or(h)
}

/// A short, friendly form of a URL for the sidebar: `example.com/path`.
pub fn display(url: &str) -> String {
    if let Some(q) = search::query_of(url) {
        return q;
    }
    if url.starts_with("volant:") {
        return url.to_string();
    }
    let rest = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://")).unwrap_or(url);
    let rest = rest.strip_prefix("www.").unwrap_or(rest);
    rest.strip_suffix('/').unwrap_or(rest).to_string()
}

/// True for pages Volant ships itself.
pub fn is_internal(url: &str) -> bool {
    url.starts_with("volant://")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn urls() {
        assert_eq!(as_url("example.com").unwrap(), "https://example.com");
        assert_eq!(as_url("github.com/rust-lang/rust").unwrap(), "https://github.com/rust-lang/rust");
        assert_eq!(as_url("localhost:3000").unwrap(), "http://localhost:3000");
        assert_eq!(as_url("192.168.1.1/admin").unwrap(), "http://192.168.1.1/admin");
        assert_eq!(as_url("https://a.b/c d").unwrap(), "https://a.b/c d");
        assert_eq!(as_url("volant://settings").unwrap(), "volant://settings");
        assert_eq!(as_url("nas.local:8096").unwrap(), "http://nas.local:8096");
        assert_eq!(as_url("münchen.de").unwrap(), "https://münchen.de");
    }
    #[test]
    fn searches() {
        for s in ["rust lang", "hello", "c++", "1.5", "what is 2.5", "foo:bar", "?example.com", "a..b", "-a.com"] {
            assert!(as_url(s).is_none(), "{s} should be a search");
        }
        assert_eq!(resolve("?example.com", "duckduckgo"), "https://duckduckgo.com/?q=example.com");
    }
    #[test]
    fn hosts() {
        assert_eq!(host("https://www.example.com/a?b"), "example.com");
        assert_eq!(host("http://user@localhost:8080/"), "localhost");
        assert_eq!(host("https://[::1]:80/"), "[::1]");
        assert_eq!(display("https://www.example.com/"), "example.com");
        assert_eq!(display("https://duckduckgo.com/?q=hi+there"), "hi there");
    }
}
