//! A cookie jar (RFC 6265), written from scratch: `Set-Cookie` parsing,
//! domain and path matching, expiry, `Secure` and `HttpOnly`, and a plain
//! line format to keep cookies between launches.
//!
//! Times are Unix seconds, passed in, so the jar is easy to test.

use crate::http::Url;

#[derive(Debug, Clone, PartialEq)]
pub struct Cookie {
    pub name: String,
    pub value: String,
    /// Lower case, without a leading dot.
    pub domain: String,
    /// Set by a `Domain` attribute: subdomains get it too.
    pub host_only: bool,
    pub path: String,
    /// Unix seconds; `None` for a session cookie.
    pub expires: Option<u64>,
    pub secure: bool,
    pub http_only: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Jar {
    cookies: Vec<Cookie>,
    /// Changed since the last save.
    pub dirty: bool,
}

/// The most cookies kept for one domain, and in all.
const PER_DOMAIN: usize = 60;
const TOTAL: usize = 4000;

impl Jar {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.cookies.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cookies.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Cookie> {
        self.cookies.iter()
    }

    /// Stores one `Set-Cookie` header from a response for `url`. From a
    /// script (`document.cookie = …`), `HttpOnly` cookies can't be set or replaced.
    pub fn set(&mut self, url: &Url, header: &str, now: u64, from_script: bool) {
        let mut parts = header.split(';');
        let first = parts.next().unwrap_or("");
        let (name, value) = match first.split_once('=') {
            Some((n, v)) => (n.trim(), v.trim()),
            None => ("", first.trim()),
        };
        if name.is_empty() && value.is_empty() || name.len() + value.len() > 4096 {
            return;
        }
        let host = url.connect_host().to_ascii_lowercase();
        let mut c = Cookie {
            name: name.to_string(),
            value: value.trim_matches('"').to_string(),
            domain: host.clone(),
            host_only: true,
            path: default_path(&url.target),
            expires: None,
            secure: false,
            http_only: false,
        };
        let mut max_age: Option<i64> = None;
        for attr in parts {
            let (k, v) = match attr.split_once('=') {
                Some((k, v)) => (k.trim().to_ascii_lowercase(), v.trim()),
                None => (attr.trim().to_ascii_lowercase(), ""),
            };
            match k.as_str() {
                "domain" => {
                    let d = v.trim_start_matches('.').to_ascii_lowercase();
                    if d.is_empty() {
                        continue;
                    }
                    // The page's own domain or a parent of it, never a bare top-level one.
                    if !domain_match(&host, &d) || !d.contains('.') {
                        return;
                    }
                    c.domain = d;
                    c.host_only = false;
                }
                "path" if v.starts_with('/') => c.path = v.to_string(),
                "max-age" => max_age = v.parse().ok(),
                "expires" if max_age.is_none() => c.expires = parse_date(v).or(Some(0)),
                "secure" => c.secure = true,
                "httponly" => c.http_only = true,
                _ => {}
            }
        }
        if let Some(a) = max_age {
            c.expires = Some(if a <= 0 { 0 } else { now.saturating_add(a as u64) });
        }
        if from_script && c.http_only {
            return;
        }
        // Only a secure page may set a Secure cookie.
        if c.secure && !url.https {
            return;
        }
        let same = |x: &Cookie| x.name == c.name && x.domain == c.domain && x.path == c.path;
        if let Some(at) = self.cookies.iter().position(same) {
            if from_script && self.cookies[at].http_only {
                return;
            }
            self.cookies.remove(at);
        }
        self.dirty = true;
        if c.expires.map(|e| e <= now).unwrap_or(false) {
            return; // an expiry in the past deletes it
        }
        let on_domain = self.cookies.iter().filter(|x| x.domain == c.domain).count();
        if on_domain >= PER_DOMAIN {
            if let Some(at) = self.cookies.iter().position(|x| x.domain == c.domain) {
                self.cookies.remove(at);
            }
        }
        if self.cookies.len() >= TOTAL {
            self.cookies.remove(0);
        }
        self.cookies.push(c);
    }

    /// The `Cookie` header value for a request to `url` (empty if none). For a
    /// script (`document.cookie`), `HttpOnly` cookies are left out.
    pub fn header(&mut self, url: &Url, now: u64, for_script: bool) -> String {
        self.expire(now);
        let host = url.connect_host().to_ascii_lowercase();
        let path = url.target.split('?').next().unwrap_or("/");
        let mut found: Vec<&Cookie> = self
            .cookies
            .iter()
            .filter(|c| if c.host_only { c.domain == host } else { domain_match(&host, &c.domain) })
            .filter(|c| path_match(path, &c.path))
            .filter(|c| !c.secure || url.https || is_local(&host))
            .filter(|c| !(for_script && c.http_only))
            .collect();
        // Longer paths first, as browsers send them.
        found.sort_by(|a, b| b.path.len().cmp(&a.path.len()));
        let mut out = String::new();
        for c in found {
            if !out.is_empty() {
                out.push_str("; ");
            }
            if !c.name.is_empty() {
                out.push_str(&c.name);
                out.push('=');
            }
            out.push_str(&c.value);
        }
        out
    }

    /// Drops cookies whose time has come.
    pub fn expire(&mut self, now: u64) {
        let before = self.cookies.len();
        self.cookies.retain(|c| c.expires.map(|e| e > now).unwrap_or(true));
        if self.cookies.len() != before {
            self.dirty = true;
        }
    }

    /// Forgets every cookie for a domain and its subdomains (or all of them).
    pub fn clear(&mut self, domain: Option<&str>) {
        match domain {
            Some(d) => self.cookies.retain(|c| !domain_match(&c.domain, d)),
            None => self.cookies.clear(),
        }
        self.dirty = true;
    }

    /// Persistent cookies, one per line (session cookies aren't kept).
    pub fn to_text(&self) -> String {
        let mut out = String::from("# Volant cookies\n");
        for c in self.cookies.iter().filter(|c| c.expires.is_some()) {
            let flags = format!("{}{}{}", if c.host_only { "h" } else { "" }, if c.secure { "s" } else { "" }, if c.http_only { "o" } else { "" });
            out.push_str(&format!("{}\t{}\t{}\t{}\t{}\t{}\n", c.domain, c.path, c.expires.unwrap_or(0), flags, c.name, c.value));
        }
        out
    }

    pub fn from_text(text: &str, now: u64) -> Jar {
        let mut jar = Jar::new();
        for line in text.lines().filter(|l| !l.starts_with('#')) {
            let f: Vec<&str> = line.splitn(6, '\t').collect();
            let [domain, path, expires, flags, name, value] = f[..] else { continue };
            let Ok(expires) = expires.parse::<u64>() else { continue };
            if expires <= now {
                continue;
            }
            jar.cookies.push(Cookie {
                name: name.into(),
                value: value.into(),
                domain: domain.into(),
                host_only: flags.contains('h'),
                path: path.into(),
                expires: Some(expires),
                secure: flags.contains('s'),
                http_only: flags.contains('o'),
            });
        }
        jar
    }
}

/// Is `host` the domain `d` or one of its subdomains?
pub fn domain_match(host: &str, d: &str) -> bool {
    host == d || (host.len() > d.len() && host.ends_with(d) && host.as_bytes()[host.len() - d.len() - 1] == b'.' && !host.bytes().all(|c| c.is_ascii_digit() || c == b'.'))
}

fn path_match(path: &str, cookie_path: &str) -> bool {
    path == cookie_path || (path.starts_with(cookie_path) && (cookie_path.ends_with('/') || path.as_bytes().get(cookie_path.len()) == Some(&b'/')))
}

fn is_local(host: &str) -> bool {
    host == "localhost" || host == "127.0.0.1" || host == "::1"
}

/// The directory of a request path (`/a/b/c` → `/a/b`).
fn default_path(target: &str) -> String {
    let path = target.split('?').next().unwrap_or("/");
    match path.rfind('/') {
        Some(0) | None => "/".into(),
        Some(i) => path[..i].into(),
    }
}

/// An HTTP date (`Wed, 21 Oct 2015 07:28:00 GMT`, and the older dashed and
/// asctime forms) as Unix seconds.
pub fn parse_date(s: &str) -> Option<u64> {
    let (mut day, mut month, mut year, mut hms) = (None, None, None, None);
    for tok in s.split(|c: char| c == ' ' || c == ',' || c == '-').filter(|t| !t.is_empty()) {
        if hms.is_none() && tok.contains(':') {
            let p: Vec<u64> = tok.split(':').filter_map(|x| x.parse().ok()).collect();
            if p.len() == 3 {
                hms = Some((p[0], p[1], p[2]));
                continue;
            }
        }
        if month.is_none() && tok.len() >= 3 {
            let m = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"].iter().position(|m| tok[..3].eq_ignore_ascii_case(m));
            if let Some(m) = m {
                month = Some(m as u64 + 1);
                continue;
            }
        }
        if let Ok(n) = tok.parse::<u64>() {
            if day.is_none() && (1..=31).contains(&n) && tok.len() <= 2 {
                day = Some(n);
            } else if year.is_none() {
                year = Some(match n {
                    0..=69 => n + 2000,
                    70..=99 => n + 1900,
                    _ => n,
                });
            }
        }
    }
    let (d, m, y, (hh, mm, ss)) = (day?, month?, year?, hms?);
    if y < 1970 || hh > 23 || mm > 59 || ss > 60 {
        return if y < 1970 { Some(0) } else { None };
    }
    // Days since 1970-01-01 (civil-from-days, inverted).
    let (y2, m2) = if m <= 2 { (y - 1, m + 9) } else { (y, m - 3) };
    let era = y2 / 400;
    let yoe = y2 - era * 400;
    let doy = (153 * m2 + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some(days * 86400 + hh * 3600 + mm * 60 + ss)
}

/// Unix seconds now.
pub fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn dates() {
        assert_eq!(parse_date("Wed, 21 Oct 2015 07:28:00 GMT"), Some(1445412480));
        assert_eq!(parse_date("Wednesday, 21-Oct-15 07:28:00 GMT"), Some(1445412480));
        assert_eq!(parse_date("Wed Oct 21 07:28:00 2015"), Some(1445412480));
        assert_eq!(parse_date("Thu, 01 Jan 1970 00:00:00 GMT"), Some(0));
        assert_eq!(parse_date("nonsense"), None);
    }

    #[test]
    fn matching_and_scope() {
        let mut j = Jar::new();
        let now = 1_700_000_000;
        j.set(&u("https://www.example.com/account/login"), "sid=abc; Path=/; Secure; HttpOnly", now, false);
        j.set(&u("https://www.example.com/account/login"), "theme=dark", now, false);
        j.set(&u("https://www.example.com/"), "wide=1; Domain=.example.com; Max-Age=60", now, false);
        j.set(&u("https://www.example.com/"), "bad=1; Domain=com", now, false);
        j.set(&u("https://www.example.com/"), "evil=1; Domain=other.com", now, false);
        assert_eq!(j.header(&u("https://www.example.com/account/x"), now, false), "theme=dark; sid=abc; wide=1");
        assert_eq!(j.header(&u("https://www.example.com/"), now, false), "sid=abc; wide=1");
        assert_eq!(j.header(&u("https://www.example.com/"), now, true), "wide=1");
        assert_eq!(j.header(&u("https://shop.example.com/"), now, false), "wide=1");
        assert_eq!(j.header(&u("http://www.example.com/"), now, false), "wide=1");
        assert_eq!(j.header(&u("https://example.org/"), now, false), "");
        // Max-Age runs out; a script can't touch HttpOnly cookies.
        assert_eq!(j.header(&u("https://www.example.com/"), now + 61, false), "sid=abc");
        j.set(&u("https://www.example.com/"), "sid=stolen; Path=/", now, true);
        assert_eq!(j.header(&u("https://www.example.com/"), now, false), "sid=abc");
        // Deleting by expiring.
        j.set(&u("https://www.example.com/account/"), "theme=; Expires=Thu, 01 Jan 1970 00:00:00 GMT", now, false);
        assert_eq!(j.header(&u("https://www.example.com/account/x"), now, false), "sid=abc");
    }

    #[test]
    fn saves_persistent_cookies() {
        let mut j = Jar::new();
        let now = 1_700_000_000;
        j.set(&u("https://a.example/"), "keep=1; Max-Age=3600; Secure", now, false);
        j.set(&u("https://a.example/"), "session=1", now, false);
        let back = Jar::from_text(&j.to_text(), now);
        assert_eq!(back.len(), 1);
        let mut back = back;
        assert_eq!(back.header(&u("https://a.example/"), now, false), "keep=1");
        assert!(Jar::from_text(&j.to_text(), now + 4000).is_empty());
    }
}
