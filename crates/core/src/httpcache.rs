//! An HTTP cache in memory (RFC 9111, as a private browser cache): fresh
//! answers come back without asking the server, stale ones are revalidated
//! with `If-None-Match`/`If-Modified-Since` and a `304` refreshes them.
//!
//! Only GETs are cached, and only answers that are safe to replay: no
//! `no-store`, no `Set-Cookie`, no `Vary: Cookie` (cookies aren't part of the
//! key), nothing for a request that carried `Authorization`. The least
//! recently used answers go first when it's full.

use crate::http::Response;
use std::collections::HashMap;

pub struct Cache {
    entries: HashMap<String, Entry>,
    bytes: usize,
    max_bytes: usize,
    clock: u64,
}

struct Entry {
    response: Response,
    /// When it stops being fresh (Unix seconds).
    fresh_until: u64,
    etag: Option<String>,
    last_modified: Option<String>,
    /// The request headers the answer varies on, and their values then.
    vary: Vec<(String, String)>,
    used: u64,
}

/// What the cache has for a request.
pub enum Lookup {
    /// Use it as it is.
    Fresh(Response),
    /// Ask the server with these conditional headers; a `304` means the kept copy is good.
    Revalidate(Vec<(String, String)>),
    Miss,
}

/// The biggest answer worth keeping.
const MAX_ENTRY: usize = 8 << 20;

fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
}

/// `Cache-Control` directives, lower-cased: (name, value).
fn directives(headers: &[(String, String)]) -> Vec<(String, String)> {
    headers
        .iter()
        .filter(|(k, _)| k.eq_ignore_ascii_case("cache-control"))
        .flat_map(|(_, v)| v.split(','))
        .map(|d| {
            let d = d.trim().to_ascii_lowercase();
            match d.split_once('=') {
                Some((k, v)) => (k.trim().to_string(), v.trim().trim_matches('"').to_string()),
                None => (d, String::new()),
            }
        })
        .collect()
}

impl Cache {
    pub fn new(max_bytes: usize) -> Cache {
        Cache { entries: HashMap::new(), bytes: 0, max_bytes, clock: 0 }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }

    fn key(url: &str) -> String {
        url.split('#').next().unwrap_or(url).to_string()
    }

    /// What to do for a GET of `url` with `headers` at `now`.
    pub fn lookup(&mut self, url: &str, headers: &[(String, String)], now: u64) -> Lookup {
        // The page asked for its own conditions, or for a fresh copy: go to the server.
        let asks = directives(headers);
        if header(headers, "authorization").is_some()
            || header(headers, "if-none-match").is_some()
            || header(headers, "if-modified-since").is_some()
            || header(headers, "range").is_some()
        {
            return Lookup::Miss;
        }
        self.clock += 1;
        let clock = self.clock;
        let Some(e) = self.entries.get_mut(&Self::key(url)) else { return Lookup::Miss };
        if e.vary.iter().any(|(k, v)| header(headers, k).unwrap_or("") != v) {
            return Lookup::Miss;
        }
        e.used = clock;
        let no_cache = asks.iter().any(|(k, _)| k == "no-cache" || k == "no-store") || asks.iter().any(|(k, v)| k == "max-age" && v == "0") || header(headers, "pragma").map(|p| p.contains("no-cache")).unwrap_or(false);
        if !no_cache && now < e.fresh_until {
            return Lookup::Fresh(e.response.clone());
        }
        let mut cond = Vec::new();
        if let Some(t) = &e.etag {
            cond.push(("If-None-Match".to_string(), t.clone()));
        }
        if let Some(m) = &e.last_modified {
            cond.push(("If-Modified-Since".to_string(), m.clone()));
        }
        if cond.is_empty() {
            Lookup::Miss
        } else {
            Lookup::Revalidate(cond)
        }
    }

    /// The server answered a revalidation with `304`: the kept copy, refreshed by its headers.
    pub fn revalidated(&mut self, url: &str, not_modified: &Response, now: u64) -> Option<Response> {
        let e = self.entries.get_mut(&Self::key(url))?;
        // The 304's caching headers replace the stored ones.
        for name in ["cache-control", "expires", "etag", "last-modified", "date", "age"] {
            if let Some(v) = header(&not_modified.headers, name) {
                e.response.headers.retain(|(k, _)| !k.eq_ignore_ascii_case(name));
                e.response.headers.push((name.to_string(), v.to_string()));
            }
        }
        e.fresh_until = now + freshness(&e.response.headers, now);
        e.etag = header(&e.response.headers, "etag").map(str::to_string);
        e.last_modified = header(&e.response.headers, "last-modified").map(str::to_string);
        Some(e.response.clone())
    }

    /// Keeps the answer to a GET of `url` (made with `headers`), if it may be kept.
    pub fn store(&mut self, url: &str, headers: &[(String, String)], r: &Response, now: u64) {
        let key = Self::key(url);
        // Only what came from this very address (not after a redirect), and whole.
        if r.url.split('#').next() != Some(key.as_str()) || !matches!(r.status, 200 | 203 | 204 | 300 | 301 | 308 | 404 | 410) {
            return;
        }
        let d = directives(&r.headers);
        let vary_on: Vec<String> = r.headers.iter().filter(|(k, _)| k.eq_ignore_ascii_case("vary")).flat_map(|(_, v)| v.split(',')).map(|v| v.trim().to_ascii_lowercase()).filter(|v| !v.is_empty()).collect();
        let forbidden = d.iter().any(|(k, _)| k == "no-store")
            || header(headers, "authorization").is_some()
            || header(&r.headers, "set-cookie").is_some()
            || vary_on.iter().any(|v| v == "*" || v == "cookie")
            || r.body.len() > MAX_ENTRY
            || r.body.len() * 4 > self.max_bytes;
        if forbidden {
            self.remove(&key);
            return;
        }
        let fresh = if d.iter().any(|(k, _)| k == "no-cache") { 0 } else { freshness(&r.headers, now) };
        let etag = header(&r.headers, "etag").map(str::to_string);
        let last_modified = header(&r.headers, "last-modified").map(str::to_string);
        if fresh == 0 && etag.is_none() && last_modified.is_none() {
            self.remove(&key);
            return; // nothing to reuse it by
        }
        let vary = vary_on.iter().map(|k| (k.clone(), header(headers, k).unwrap_or("").to_string())).collect();
        self.remove(&key);
        self.clock += 1;
        let size = r.body.len() + r.headers.iter().map(|(k, v)| k.len() + v.len()).sum::<usize>() + 256;
        self.bytes += size;
        self.entries.insert(key, Entry { response: r.clone(), fresh_until: now + fresh, etag, last_modified, vary, used: self.clock });
        while self.bytes > self.max_bytes {
            let Some(oldest) = self.entries.iter().min_by_key(|(_, e)| e.used).map(|(k, _)| k.clone()) else { break };
            self.remove(&oldest);
        }
    }

    pub fn remove(&mut self, url: &str) {
        if let Some(e) = self.entries.remove(&Self::key(url)) {
            self.bytes -= e.response.body.len() + e.response.headers.iter().map(|(k, v)| k.len() + v.len()).sum::<usize>() + 256;
        }
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.bytes = 0;
    }
}

/// How many seconds an answer stays fresh: `max-age`, else `Expires` − `Date`,
/// else a tenth of its age since `Last-Modified` (at most a day).
fn freshness(headers: &[(String, String)], now: u64) -> u64 {
    let d = directives(headers);
    let age = header(headers, "age").and_then(|a| a.trim().parse::<u64>().ok()).unwrap_or(0);
    if let Some(max) = d.iter().find(|(k, _)| k == "max-age").and_then(|(_, v)| v.parse::<u64>().ok()) {
        return max.saturating_sub(age);
    }
    let date = header(headers, "date").and_then(crate::cookies::parse_date).unwrap_or(now);
    if let Some(exp) = header(headers, "expires") {
        return crate::cookies::parse_date(exp).map(|e| e.saturating_sub(date)).unwrap_or(0).saturating_sub(age);
    }
    if let Some(lm) = header(headers, "last-modified").and_then(crate::cookies::parse_date) {
        return (date.saturating_sub(lm) / 10).min(86_400).saturating_sub(age);
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resp(url: &str, headers: &[(&str, &str)], body: &str) -> Response {
        Response { status: 200, headers: headers.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(), body: body.as_bytes().to_vec(), url: url.to_string() }
    }

    #[test]
    fn fresh_stale_and_revalidate() {
        let mut c = Cache::new(1 << 20);
        let now = 1_700_000_000;
        let u = "https://cdn.example/app.js";
        assert!(matches!(c.lookup(u, &[], now), Lookup::Miss));
        c.store(u, &[], &resp(u, &[("cache-control", "public, max-age=60"), ("etag", "\"v1\"")], "js"), now);
        assert!(matches!(c.lookup(u, &[], now + 59), Lookup::Fresh(r) if r.body == b"js"));
        // Stale: ask with the validator; a 304 refreshes it.
        let Lookup::Revalidate(h) = c.lookup(u, &[], now + 61) else { panic!("stale") };
        assert_eq!(h, vec![("If-None-Match".to_string(), "\"v1\"".to_string())]);
        let back = c.revalidated(u, &Response { status: 304, headers: vec![("cache-control".into(), "max-age=120".into())], body: vec![], url: u.into() }, now + 61).unwrap();
        assert_eq!(back.body, b"js");
        assert!(matches!(c.lookup(u, &[], now + 150), Lookup::Fresh(_)));
        // A reload (no-cache) revalidates even a fresh copy.
        assert!(matches!(c.lookup(u, &[("Cache-Control".into(), "no-cache".into())], now + 62), Lookup::Revalidate(_)));
        // Expires and Last-Modified heuristics.
        let e = "https://e.example/x";
        c.store(e, &[], &resp(e, &[("date", "Wed, 21 Oct 2015 07:28:00 GMT"), ("expires", "Wed, 21 Oct 2015 07:38:00 GMT")], "x"), now);
        assert!(matches!(c.lookup(e, &[], now + 599), Lookup::Fresh(_)));
        assert!(matches!(c.lookup(e, &[], now + 601), Lookup::Miss), "stale with nothing to revalidate by");
    }

    #[test]
    fn what_is_not_kept() {
        let mut c = Cache::new(1 << 20);
        let now = 1_700_000_000;
        let u = "https://a.example/";
        for h in [&[("cache-control", "no-store, max-age=60")][..], &[("cache-control", "max-age=60"), ("set-cookie", "a=1")], &[("cache-control", "max-age=60"), ("vary", "Cookie")], &[("content-type", "text/html")]] {
            c.store(u, &[], &resp(u, h, "page"), now);
            assert!(c.is_empty(), "{h:?}");
        }
        // Not after a redirect, and not for a request with credentials of its own.
        c.store(u, &[], &resp("https://a.example/elsewhere", &[("cache-control", "max-age=60")], "x"), now);
        c.store(u, &[("Authorization".into(), "Bearer t".into())], &resp(u, &[("cache-control", "max-age=60")], "x"), now);
        assert!(c.is_empty());
        // Vary: a different Accept-Language is a different answer.
        c.store(u, &[("Accept-Language".into(), "en".into())], &resp(u, &[("cache-control", "max-age=60"), ("vary", "Accept-Language")], "hello"), now);
        assert!(matches!(c.lookup(u, &[("accept-language".into(), "en".into())], now), Lookup::Fresh(_)));
        assert!(matches!(c.lookup(u, &[("accept-language".into(), "fr".into())], now), Lookup::Miss));
    }

    #[test]
    fn least_recently_used_goes_first() {
        let mut c = Cache::new(4000);
        let now = 1_700_000_000;
        let body = "x".repeat(600);
        for n in 0..3 {
            let u = format!("https://a.example/{n}");
            c.store(&u, &[], &resp(&u, &[("cache-control", "max-age=60")], &body), now);
        }
        let _ = c.lookup("https://a.example/0", &[], now); // used again
        for n in 3..5 {
            let u = format!("https://a.example/{n}");
            c.store(&u, &[], &resp(&u, &[("cache-control", "max-age=60")], &body), now);
        }
        assert!(c.bytes() <= 4000);
        assert!(matches!(c.lookup("https://a.example/0", &[], now), Lookup::Fresh(_)));
        assert!(matches!(c.lookup("https://a.example/1", &[], now), Lookup::Miss));
    }
}
