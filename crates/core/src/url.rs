//! The WHATWG URL Standard: the basic URL parser (with the state overrides
//! the `URL` setters use), hosts (domains, IPv4, IPv6, opaque), serializing,
//! origins and `application/x-www-form-urlencoded`.
//!
//! Domains go through a light form of UTS #46: case folding, the full-width
//! forms and Punycode. It doesn't carry Unicode's full mapping table.

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Url {
    pub scheme: String,
    pub username: String,
    pub password: String,
    /// The serialized host; `Some("")` is the empty host.
    pub host: Option<String>,
    pub port: Option<u16>,
    pub path: Vec<String>,
    /// Set instead of `path` for URLs like `mailto:x` and `data:,x`.
    pub opaque: Option<String>,
    pub query: Option<String>,
    pub fragment: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum State {
    SchemeStart,
    Scheme,
    NoScheme,
    SpecialRelativeOrAuthority,
    PathOrAuthority,
    Relative,
    RelativeSlash,
    SpecialAuthoritySlashes,
    SpecialAuthorityIgnoreSlashes,
    Authority,
    Host,
    Hostname,
    Port,
    File,
    FileSlash,
    FileHost,
    PathStart,
    Path,
    OpaquePath,
    Query,
    Fragment,
}

pub fn default_port(scheme: &str) -> Option<u16> {
    match scheme {
        "ftp" => Some(21),
        "http" | "ws" => Some(80),
        "https" | "wss" => Some(443),
        _ => None,
    }
}

pub fn is_special(scheme: &str) -> bool {
    matches!(scheme, "ftp" | "file" | "http" | "https" | "ws" | "wss")
}

/// Parses `input`, relative to `base` when given. `None` means failure.
pub fn parse(input: &str, base: Option<&Url>) -> Option<Url> {
    let mut url = Url::default();
    let trimmed = input.trim_matches(|c: char| c <= ' ');
    run(trimmed, base, &mut url, None).ok()?;
    Some(url)
}

/// Resolves `href` against the URL string `base`, as `<a href>` does.
pub fn resolve(base: &str, href: &str) -> Option<String> {
    let b = parse(base, None);
    parse(href, b.as_ref()).map(|u| u.href())
}

impl Url {
    pub fn href(&self) -> String {
        let mut o = String::with_capacity(self.scheme.len() + 32);
        o.push_str(&self.scheme);
        o.push(':');
        if let Some(h) = &self.host {
            o.push_str("//");
            if self.has_credentials() {
                o.push_str(&self.username);
                if !self.password.is_empty() {
                    o.push(':');
                    o.push_str(&self.password);
                }
                o.push('@');
            }
            o.push_str(h);
            if let Some(p) = self.port {
                o.push(':');
                o.push_str(&p.to_string());
            }
        }
        if self.host.is_none() && self.opaque.is_none() && self.path.len() > 1 && self.path[0].is_empty() {
            o.push_str("/.");
        }
        o.push_str(&self.pathname());
        if let Some(q) = &self.query {
            o.push('?');
            o.push_str(q);
        }
        if let Some(f) = &self.fragment {
            o.push('#');
            o.push_str(f);
        }
        o
    }

    pub fn pathname(&self) -> String {
        match &self.opaque {
            Some(s) => s.clone(),
            None => self.path.iter().fold(String::new(), |mut o, s| {
                o.push('/');
                o.push_str(s);
                o
            }),
        }
    }

    /// The ASCII serialization of the origin ("null" when opaque).
    pub fn origin(&self) -> String {
        match self.scheme.as_str() {
            "blob" => match parse(&self.pathname(), None) {
                Some(u) if u.scheme == "http" || u.scheme == "https" => u.origin(),
                _ => "null".into(),
            },
            "ftp" | "http" | "https" | "ws" | "wss" => {
                let port = self.port.map(|p| format!(":{p}")).unwrap_or_default();
                format!("{}://{}{port}", self.scheme, self.host.as_deref().unwrap_or(""))
            }
            _ => "null".into(),
        }
    }

    pub fn host_port(&self) -> String {
        match (&self.host, self.port) {
            (None, _) => String::new(),
            (Some(h), None) => h.clone(),
            (Some(h), Some(p)) => format!("{h}:{p}"),
        }
    }

    fn has_credentials(&self) -> bool {
        !self.username.is_empty() || !self.password.is_empty()
    }

    fn cannot_have_credentials(&self) -> bool {
        matches!(self.host.as_deref(), None | Some("")) || self.scheme == "file"
    }

    /// One of the `URL` setters other than `href`: protocol, username,
    /// password, host, hostname, port, pathname, search or hash. Values
    /// the standard rejects leave the URL as it was. `query` sets the query
    /// to an already-encoded string ("" clears it), for `URLSearchParams`.
    pub fn set(&mut self, which: &str, value: &str) {
        match which {
            "protocol" => {
                let _ = run(&format!("{value}:"), None, self, Some(State::SchemeStart));
            }
            "username" | "password" => {
                if self.cannot_have_credentials() {
                    return;
                }
                let v = encode(value, userinfo_set);
                if which == "username" {
                    self.username = v
                } else {
                    self.password = v
                }
            }
            "host" | "hostname" => {
                if self.opaque.is_none() {
                    let _ = run(value, None, self, Some(if which == "host" { State::Host } else { State::Hostname }));
                }
            }
            "port" => {
                if self.cannot_have_credentials() {
                    return;
                }
                if value.is_empty() {
                    self.port = None;
                } else {
                    let _ = run(value, None, self, Some(State::Port));
                }
            }
            "pathname" => {
                if self.opaque.is_none() {
                    self.path.clear();
                    let _ = run(value, None, self, Some(State::PathStart));
                }
            }
            "search" => {
                if value.is_empty() {
                    self.query = None;
                    return;
                }
                self.query = Some(String::new());
                let _ = run(value.strip_prefix('?').unwrap_or(value), None, self, Some(State::Query));
            }
            "hash" => {
                if value.is_empty() {
                    self.fragment = None;
                    return;
                }
                self.fragment = Some(String::new());
                let _ = run(value.strip_prefix('#').unwrap_or(value), None, self, Some(State::Fragment));
            }
            "query" => self.query = if value.is_empty() { None } else { Some(value.to_string()) },
            _ => {}
        }
    }

    fn shorten(&mut self) {
        if self.scheme == "file" && self.path.len() == 1 && is_drive(&self.path[0], true) {
            return;
        }
        self.path.pop();
    }
}

// ------------------------------------------------------------- percent-encoding

fn c0_set(b: u8) -> bool {
    !(0x20..=0x7E).contains(&b)
}
fn fragment_set(b: u8) -> bool {
    c0_set(b) || matches!(b, b' ' | b'"' | b'<' | b'>' | b'`')
}
fn query_set(b: u8) -> bool {
    c0_set(b) || matches!(b, b' ' | b'"' | b'#' | b'<' | b'>')
}
fn special_query_set(b: u8) -> bool {
    query_set(b) || b == b'\''
}
fn path_set(b: u8) -> bool {
    query_set(b) || matches!(b, b'?' | b'^' | b'`' | b'{' | b'}')
}
fn userinfo_set(b: u8) -> bool {
    path_set(b) || matches!(b, b'/' | b':' | b';' | b'=' | b'@' | b'['..=b'^' | b'|')
}
fn component_set(b: u8) -> bool {
    userinfo_set(b) || matches!(b, b'$'..=b'&' | b'+' | b',')
}
fn form_set(b: u8) -> bool {
    component_set(b) || matches!(b, b'!' | b'\''..=b')' | b'~')
}

const HEX: &[u8; 16] = b"0123456789ABCDEF";

fn push_byte(out: &mut String, b: u8) {
    out.push('%');
    out.push(HEX[(b >> 4) as usize] as char);
    out.push(HEX[(b & 15) as usize] as char);
}

fn encode_char(out: &mut String, c: char, set: fn(u8) -> bool) {
    let mut buf = [0u8; 4];
    for &b in c.encode_utf8(&mut buf).as_bytes() {
        if set(b) {
            push_byte(out, b)
        } else {
            out.push(b as char)
        }
    }
}

fn encode(s: &str, set: fn(u8) -> bool) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        encode_char(&mut o, c, set);
    }
    o
}

pub fn percent_decode(input: &[u8]) -> Vec<u8> {
    let hex = |b: u8| (b as char).to_digit(16).map(|d| d as u8);
    let mut out = Vec::with_capacity(input.len());
    let mut i = 0;
    while i < input.len() {
        if input[i] == b'%' && i + 2 < input.len() {
            if let (Some(h), Some(l)) = (hex(input[i + 1]), hex(input[i + 2])) {
                out.push(h << 4 | l);
                i += 3;
                continue;
            }
        }
        out.push(input[i]);
        i += 1;
    }
    out
}

// ---------------------------------------------------------------- form data

/// Parses `application/x-www-form-urlencoded` into name/value pairs.
pub fn form_parse(input: &str) -> Vec<(String, String)> {
    let dec = |s: &str| {
        let b: Vec<u8> = s.bytes().map(|b| if b == b'+' { b' ' } else { b }).collect();
        String::from_utf8_lossy(&percent_decode(&b)).into_owned()
    };
    input
        .split('&')
        .filter(|s| !s.is_empty())
        .map(|seq| {
            let (n, v) = seq.split_once('=').unwrap_or((seq, ""));
            (dec(n), dec(v))
        })
        .collect()
}

/// Encodes one name or value for `application/x-www-form-urlencoded`.
pub fn form_encode(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        if b == b' ' {
            o.push('+')
        } else if form_set(b) {
            push_byte(&mut o, b)
        } else {
            o.push(b as char)
        }
    }
    o
}

// ------------------------------------------------------------------- parser

/// A Windows drive letter ("c:" or "c|"; with `normalized`, only "c:").
fn is_drive(s: &str, normalized: bool) -> bool {
    let b = s.as_bytes();
    b.len() == 2 && b[0].is_ascii_alphabetic() && (b[1] == b':' || (!normalized && b[1] == b'|'))
}

fn starts_with_drive(c: &[char]) -> bool {
    c.len() >= 2 && c[0].is_ascii_alphabetic() && matches!(c[1], ':' | '|') && (c.len() == 2 || matches!(c[2], '/' | '\\' | '?' | '#'))
}

fn is_single_dot(s: &str) -> bool {
    s == "." || s.eq_ignore_ascii_case("%2e")
}

fn is_double_dot(s: &str) -> bool {
    matches!(s.to_ascii_lowercase().as_str(), ".." | ".%2e" | "%2e." | "%2e%2e")
}

/// The basic URL parser. `Err` is failure; with a state override some
/// changes to `url` can stay even then, as the standard has it.
fn run(input: &str, base: Option<&Url>, url: &mut Url, over: Option<State>) -> Result<(), ()> {
    let chars: Vec<char> = input.chars().filter(|c| !matches!(c, '\t' | '\n' | '\r')).collect();
    let n = chars.len() as isize;
    let at = |i: isize| if i >= 0 && i < n { Some(chars[i as usize]) } else { None };
    let mut state = over.unwrap_or(State::SchemeStart);
    let mut buf = String::new();
    let (mut at_seen, mut in_brackets, mut password_seen) = (false, false, false);
    let mut p: isize = 0;
    loop {
        let c = at(p);
        let special = is_special(&url.scheme);
        match state {
            State::SchemeStart => match c {
                Some(ch) if ch.is_ascii_alphabetic() => {
                    buf.push(ch.to_ascii_lowercase());
                    state = State::Scheme;
                }
                _ if over.is_none() => {
                    state = State::NoScheme;
                    p -= 1;
                }
                _ => return Err(()),
            },
            State::Scheme => match c {
                Some(ch) if ch.is_ascii_alphanumeric() || matches!(ch, '+' | '-' | '.') => buf.push(ch.to_ascii_lowercase()),
                Some(':') => {
                    if over.is_some() {
                        if special != is_special(&buf) {
                            return Ok(());
                        }
                        if (url.has_credentials() || url.port.is_some()) && buf == "file" {
                            return Ok(());
                        }
                        if url.scheme == "file" && url.host.as_deref() == Some("") {
                            return Ok(());
                        }
                    }
                    url.scheme = std::mem::take(&mut buf);
                    if over.is_some() {
                        if url.port.is_some() && url.port == default_port(&url.scheme) {
                            url.port = None;
                        }
                        return Ok(());
                    }
                    let special = is_special(&url.scheme);
                    if url.scheme == "file" {
                        state = State::File;
                    } else if special && base.is_some_and(|b| b.scheme == url.scheme) {
                        state = State::SpecialRelativeOrAuthority;
                    } else if special {
                        state = State::SpecialAuthoritySlashes;
                    } else if at(p + 1) == Some('/') {
                        state = State::PathOrAuthority;
                        p += 1;
                    } else {
                        url.opaque = Some(String::new());
                        state = State::OpaquePath;
                    }
                }
                _ if over.is_none() => {
                    buf.clear();
                    state = State::NoScheme;
                    p = -1;
                }
                _ => return Err(()),
            },
            State::NoScheme => {
                let Some(b) = base else { return Err(()) };
                if b.opaque.is_some() {
                    if c != Some('#') {
                        return Err(());
                    }
                    url.scheme = b.scheme.clone();
                    url.opaque = b.opaque.clone();
                    url.query = b.query.clone();
                    url.fragment = Some(String::new());
                    state = State::Fragment;
                } else {
                    state = if b.scheme != "file" { State::Relative } else { State::File };
                    p -= 1;
                }
            }
            State::SpecialRelativeOrAuthority => {
                if c == Some('/') && at(p + 1) == Some('/') {
                    state = State::SpecialAuthorityIgnoreSlashes;
                    p += 1;
                } else {
                    state = State::Relative;
                    p -= 1;
                }
            }
            State::PathOrAuthority => {
                if c == Some('/') {
                    state = State::Authority;
                } else {
                    state = State::Path;
                    p -= 1;
                }
            }
            State::Relative => {
                let Some(b) = base else { return Err(()) };
                url.scheme = b.scheme.clone();
                if c == Some('/') || (is_special(&url.scheme) && c == Some('\\')) {
                    state = State::RelativeSlash;
                } else {
                    url.username = b.username.clone();
                    url.password = b.password.clone();
                    url.host = b.host.clone();
                    url.port = b.port;
                    url.path = b.path.clone();
                    url.query = b.query.clone();
                    match c {
                        Some('?') => {
                            url.query = Some(String::new());
                            state = State::Query;
                        }
                        Some('#') => {
                            url.fragment = Some(String::new());
                            state = State::Fragment;
                        }
                        Some(_) => {
                            url.query = None;
                            url.shorten();
                            state = State::Path;
                            p -= 1;
                        }
                        None => {}
                    }
                }
            }
            State::RelativeSlash => {
                if special && matches!(c, Some('/' | '\\')) {
                    state = State::SpecialAuthorityIgnoreSlashes;
                } else if c == Some('/') {
                    state = State::Authority;
                } else {
                    let Some(b) = base else { return Err(()) };
                    url.username = b.username.clone();
                    url.password = b.password.clone();
                    url.host = b.host.clone();
                    url.port = b.port;
                    state = State::Path;
                    p -= 1;
                }
            }
            State::SpecialAuthoritySlashes => {
                state = State::SpecialAuthorityIgnoreSlashes;
                if c == Some('/') && at(p + 1) == Some('/') {
                    p += 1;
                } else {
                    p -= 1;
                }
            }
            State::SpecialAuthorityIgnoreSlashes => {
                if !matches!(c, Some('/' | '\\')) {
                    state = State::Authority;
                    p -= 1;
                }
            }
            State::Authority => {
                if c == Some('@') {
                    if at_seen {
                        buf.insert_str(0, "%40");
                    }
                    at_seen = true;
                    for cp in buf.chars() {
                        if cp == ':' && !password_seen {
                            password_seen = true;
                            continue;
                        }
                        encode_char(if password_seen { &mut url.password } else { &mut url.username }, cp, userinfo_set);
                    }
                    buf.clear();
                } else if matches!(c, None | Some('/' | '?' | '#')) || (special && c == Some('\\')) {
                    if at_seen && buf.is_empty() {
                        return Err(());
                    }
                    p -= buf.chars().count() as isize + 1;
                    buf.clear();
                    state = State::Host;
                } else if let Some(ch) = c {
                    buf.push(ch);
                }
            }
            State::Host | State::Hostname => {
                if over.is_some() && url.scheme == "file" {
                    p -= 1;
                    state = State::FileHost;
                } else if c == Some(':') && !in_brackets {
                    if buf.is_empty() || over == Some(State::Hostname) {
                        return Err(());
                    }
                    url.host = Some(host_parse(&buf, !special)?);
                    buf.clear();
                    state = State::Port;
                } else if matches!(c, None | Some('/' | '?' | '#')) || (special && c == Some('\\')) {
                    p -= 1;
                    if special && buf.is_empty() {
                        return Err(());
                    }
                    if over.is_some() && buf.is_empty() && (url.has_credentials() || url.port.is_some()) {
                        return Err(());
                    }
                    url.host = Some(host_parse(&buf, !special)?);
                    buf.clear();
                    state = State::PathStart;
                    if over.is_some() {
                        return Ok(());
                    }
                } else if let Some(ch) = c {
                    if ch == '[' {
                        in_brackets = true;
                    }
                    if ch == ']' {
                        in_brackets = false;
                    }
                    buf.push(ch);
                }
            }
            State::Port => {
                if let Some(d) = c.filter(|c| c.is_ascii_digit()) {
                    buf.push(d);
                } else if matches!(c, None | Some('/' | '?' | '#')) || (special && c == Some('\\')) || over.is_some() {
                    if !buf.is_empty() {
                        let port = buf.bytes().fold(0u32, |a, d| (a * 10 + (d - b'0') as u32).min(70_000));
                        if port > 65535 {
                            return Err(());
                        }
                        let port = port as u16;
                        url.port = if default_port(&url.scheme) == Some(port) { None } else { Some(port) };
                        buf.clear();
                        if over.is_some() {
                            return Ok(());
                        }
                    }
                    if over.is_some() {
                        return Err(());
                    }
                    state = State::PathStart;
                    p -= 1;
                } else {
                    return Err(());
                }
            }
            State::File => {
                url.scheme = "file".into();
                url.host = Some(String::new());
                if matches!(c, Some('/' | '\\')) {
                    state = State::FileSlash;
                } else if let Some(b) = base.filter(|b| b.scheme == "file") {
                    url.host = b.host.clone();
                    url.path = b.path.clone();
                    url.query = b.query.clone();
                    match c {
                        Some('?') => {
                            url.query = Some(String::new());
                            state = State::Query;
                        }
                        Some('#') => {
                            url.fragment = Some(String::new());
                            state = State::Fragment;
                        }
                        Some(_) => {
                            url.query = None;
                            if !starts_with_drive(&chars[p as usize..]) {
                                url.shorten();
                            } else {
                                url.path.clear();
                            }
                            state = State::Path;
                            p -= 1;
                        }
                        None => {}
                    }
                } else {
                    state = State::Path;
                    p -= 1;
                }
            }
            State::FileSlash => {
                if matches!(c, Some('/' | '\\')) {
                    state = State::FileHost;
                } else {
                    if let Some(b) = base.filter(|b| b.scheme == "file") {
                        url.host = b.host.clone();
                        if !starts_with_drive(&chars[p.clamp(0, n) as usize..]) && b.path.first().is_some_and(|s| is_drive(s, true)) {
                            url.path.push(b.path[0].clone());
                        }
                    }
                    state = State::Path;
                    p -= 1;
                }
            }
            State::FileHost => {
                if matches!(c, None | Some('/' | '\\' | '?' | '#')) {
                    p -= 1;
                    if over.is_none() && is_drive(&buf, false) {
                        state = State::Path;
                    } else if buf.is_empty() {
                        url.host = Some(String::new());
                        if over.is_some() {
                            return Ok(());
                        }
                        state = State::PathStart;
                    } else {
                        let mut h = host_parse(&buf, !special)?;
                        if h == "localhost" {
                            h.clear();
                        }
                        url.host = Some(h);
                        if over.is_some() {
                            return Ok(());
                        }
                        buf.clear();
                        state = State::PathStart;
                    }
                } else if let Some(ch) = c {
                    buf.push(ch);
                }
            }
            State::PathStart => {
                if special {
                    state = State::Path;
                    if !matches!(c, Some('/' | '\\')) {
                        p -= 1;
                    }
                } else if over.is_none() && c == Some('?') {
                    url.query = Some(String::new());
                    state = State::Query;
                } else if over.is_none() && c == Some('#') {
                    url.fragment = Some(String::new());
                    state = State::Fragment;
                } else if c.is_some() {
                    state = State::Path;
                    if c != Some('/') {
                        p -= 1;
                    }
                } else if over.is_some() && url.host.is_none() {
                    url.path.push(String::new());
                }
            }
            State::Path => {
                let slash = c == Some('/') || (special && c == Some('\\'));
                if c.is_none() || slash || (over.is_none() && matches!(c, Some('?' | '#'))) {
                    if is_double_dot(&buf) {
                        url.shorten();
                        if !slash {
                            url.path.push(String::new());
                        }
                    } else if is_single_dot(&buf) {
                        if !slash {
                            url.path.push(String::new());
                        }
                    } else {
                        if url.scheme == "file" && url.path.is_empty() && is_drive(&buf, false) {
                            buf.replace_range(1..2, ":");
                        }
                        url.path.push(std::mem::take(&mut buf));
                    }
                    buf.clear();
                    if c == Some('?') {
                        url.query = Some(String::new());
                        state = State::Query;
                    } else if c == Some('#') {
                        url.fragment = Some(String::new());
                        state = State::Fragment;
                    }
                } else if let Some(ch) = c {
                    encode_char(&mut buf, ch, path_set);
                }
            }
            State::OpaquePath => match c {
                Some('?') => {
                    url.query = Some(String::new());
                    state = State::Query;
                }
                Some('#') => {
                    url.fragment = Some(String::new());
                    state = State::Fragment;
                }
                Some(' ') => {
                    let o = url.opaque.get_or_insert_with(String::new);
                    if matches!(at(p + 1), Some('?' | '#')) {
                        o.push_str("%20");
                    } else {
                        o.push(' ');
                    }
                }
                Some(ch) => encode_char(url.opaque.get_or_insert_with(String::new), ch, c0_set),
                None => {}
            },
            State::Query => {
                if (over.is_none() && c == Some('#')) || c.is_none() {
                    let set = if special { special_query_set } else { query_set };
                    let q = url.query.get_or_insert_with(String::new);
                    for ch in buf.chars() {
                        encode_char(q, ch, set);
                    }
                    buf.clear();
                    if c == Some('#') {
                        url.fragment = Some(String::new());
                        state = State::Fragment;
                    }
                } else if let Some(ch) = c {
                    buf.push(ch);
                }
            }
            State::Fragment => {
                if let Some(ch) = c {
                    encode_char(url.fragment.get_or_insert_with(String::new), ch, fragment_set);
                }
            }
        }
        if p >= n {
            break;
        }
        p += 1;
    }
    Ok(())
}

// -------------------------------------------------------------------- hosts

fn forbidden_host(c: char) -> bool {
    matches!(c, '\0' | '\t' | '\n' | '\r' | ' ' | '#' | '/' | ':' | '<' | '>' | '?' | '@' | '[' | '\\' | ']' | '^' | '|')
}

fn forbidden_domain(c: char) -> bool {
    forbidden_host(c) || c <= '\u{1F}' || c == '%' || c == '\u{7F}'
}

/// The host parser: the serialized host, or `Err` on failure.
pub fn host_parse(input: &str, opaque: bool) -> Result<String, ()> {
    if let Some(rest) = input.strip_prefix('[') {
        let inner = rest.strip_suffix(']').ok_or(())?;
        return ipv6_parse(inner).map(|a| format!("[{}]", ipv6_serialize(&a)));
    }
    if opaque {
        if input.chars().any(forbidden_host) {
            return Err(());
        }
        return Ok(encode(input, c0_set));
    }
    let bytes = percent_decode(input.as_bytes());
    let domain = String::from_utf8_lossy(&bytes);
    let ascii = domain_to_ascii(&domain)?;
    if ascii.chars().any(forbidden_domain) {
        return Err(());
    }
    if ends_in_number(&ascii) {
        let v = ipv4_parse(&ascii)?;
        return Ok(format!("{}.{}.{}.{}", v >> 24, (v >> 16) & 255, (v >> 8) & 255, v & 255));
    }
    Ok(ascii)
}

fn ends_in_number(s: &str) -> bool {
    let mut parts: Vec<&str> = s.split('.').collect();
    if parts.last() == Some(&"") {
        if parts.len() == 1 {
            return false;
        }
        parts.pop();
    }
    let last = parts.last().copied().unwrap_or("");
    (!last.is_empty() && last.bytes().all(|b| b.is_ascii_digit())) || ipv4_number(last).is_ok()
}

fn ipv4_number(s: &str) -> Result<u64, ()> {
    if s.is_empty() {
        return Err(());
    }
    let (radix, digits) = if s.starts_with("0x") || s.starts_with("0X") {
        (16, &s[2..])
    } else if s.len() > 1 && s.starts_with('0') {
        (8, &s[1..])
    } else {
        (10, s)
    };
    let mut v: u64 = 0;
    for ch in digits.chars() {
        let d = ch.to_digit(radix).ok_or(())?;
        v = v.saturating_mul(radix as u64).saturating_add(d as u64);
    }
    Ok(v)
}

fn ipv4_parse(s: &str) -> Result<u32, ()> {
    let mut parts: Vec<&str> = s.split('.').collect();
    if parts.last() == Some(&"") && parts.len() > 1 {
        parts.pop();
    }
    if parts.len() > 4 {
        return Err(());
    }
    let nums = parts.iter().map(|p| ipv4_number(p)).collect::<Result<Vec<u64>, ()>>()?;
    let (last, init) = nums.split_last().ok_or(())?;
    if init.iter().any(|&n| n > 255) || *last >= 256u64.pow(5 - nums.len() as u32) {
        return Err(());
    }
    let v = init.iter().enumerate().fold(*last, |v, (i, &n)| v + n * 256u64.pow(3 - i as u32));
    Ok(v as u32)
}

fn ipv6_parse(input: &str) -> Result<[u16; 8], ()> {
    let c: Vec<char> = input.chars().collect();
    let at = |i: usize| c.get(i).copied();
    let mut addr = [0u16; 8];
    let (mut piece, mut p) = (0usize, 0usize);
    let mut compress: Option<usize> = None;
    if at(0) == Some(':') {
        if at(1) != Some(':') {
            return Err(());
        }
        p = 2;
        piece = 1;
        compress = Some(1);
    }
    while at(p).is_some() {
        if piece == 8 {
            return Err(());
        }
        if at(p) == Some(':') {
            if compress.is_some() {
                return Err(());
            }
            p += 1;
            piece += 1;
            compress = Some(piece);
            continue;
        }
        let (mut value, mut length) = (0u32, 0usize);
        while length < 4 {
            let Some(d) = at(p).and_then(|ch| ch.to_digit(16)) else { break };
            value = value * 16 + d;
            p += 1;
            length += 1;
        }
        if at(p) == Some('.') {
            if length == 0 {
                return Err(());
            }
            p -= length;
            if piece > 6 {
                return Err(());
            }
            let mut seen = 0;
            while at(p).is_some() {
                if seen > 0 {
                    if at(p) == Some('.') && seen < 4 {
                        p += 1;
                    } else {
                        return Err(());
                    }
                }
                if !at(p).is_some_and(|ch| ch.is_ascii_digit()) {
                    return Err(());
                }
                let mut part: Option<u32> = None;
                while let Some(d) = at(p).and_then(|ch| ch.to_digit(10)) {
                    part = match part {
                        None => Some(d),
                        Some(0) => return Err(()),
                        Some(v) => Some(v * 10 + d),
                    };
                    if part > Some(255) {
                        return Err(());
                    }
                    p += 1;
                }
                addr[piece] = addr[piece].wrapping_mul(0x100) + part.unwrap_or(0) as u16;
                seen += 1;
                if seen == 2 || seen == 4 {
                    piece += 1;
                }
            }
            if seen != 4 {
                return Err(());
            }
            break;
        } else if at(p) == Some(':') {
            p += 1;
            if at(p).is_none() {
                return Err(());
            }
        } else if at(p).is_some() {
            return Err(());
        }
        addr[piece] = value as u16;
        piece += 1;
    }
    if let Some(cp) = compress {
        let mut swaps = piece - cp;
        piece = 7;
        while piece != 0 && swaps > 0 {
            addr.swap(piece, cp + swaps - 1);
            piece -= 1;
            swaps -= 1;
        }
    } else if piece != 8 {
        return Err(());
    }
    Ok(addr)
}

fn ipv6_serialize(a: &[u16; 8]) -> String {
    // The first longest run of two or more zero pieces becomes "::".
    let (mut best, mut best_len, mut i) = (None, 1, 0);
    while i < 8 {
        if a[i] == 0 {
            let s = i;
            while i < 8 && a[i] == 0 {
                i += 1;
            }
            if i - s > best_len {
                best = Some(s);
                best_len = i - s;
            }
        } else {
            i += 1;
        }
    }
    let mut out = String::new();
    let mut i = 0;
    while i < 8 {
        if best == Some(i) {
            out.push_str(if i == 0 { "::" } else { ":" });
            i += best_len;
            continue;
        }
        out.push_str(&format!("{:x}", a[i]));
        if i != 7 {
            out.push(':');
        }
        i += 1;
    }
    out
}

/// UTS #46 ToASCII as the URL Standard calls it (not strict).
fn domain_to_ascii(domain: &str) -> Result<String, ()> {
    let xn = |l: &str| l.len() >= 4 && l.as_bytes()[..4].eq_ignore_ascii_case(b"xn--");
    if domain.is_ascii() && !domain.split('.').any(xn) {
        let r = domain.to_ascii_lowercase();
        return if r.is_empty() { Err(()) } else { Ok(r) };
    }
    let mut mapped = String::with_capacity(domain.len());
    for ch in domain.chars() {
        match ch {
            '\u{3002}' | '\u{FF0E}' | '\u{FF61}' => mapped.push('.'),
            '\u{FF01}'..='\u{FF5E}' => mapped.extend(char::from_u32(ch as u32 - 0xFEE0).unwrap_or(ch).to_lowercase()),
            '\u{AD}' | '\u{200B}' | '\u{2060}' | '\u{FEFF}' | '\u{34F}' | '\u{180B}'..='\u{180D}' | '\u{FE00}'..='\u{FE0F}' => {}
            '\u{200C}' | '\u{200D}' | '\u{FFFD}' | '\u{FDD0}'..='\u{FDEF}' => return Err(()),
            _ if (ch as u32) & 0xFFFE == 0xFFFE => return Err(()),
            // Spaces fold to U+0020, which a host can't hold.
            '\u{A0}' | '\u{2000}'..='\u{200A}' | '\u{202F}' | '\u{205F}' | '\u{3000}' => mapped.push(' '),
            // Mathematical alphanumerics fold to plain letters and digits.
            '\u{1D400}'..='\u{1D6A3}' => {
                let n = (ch as u32 - 0x1D400) % 52;
                mapped.push((if n < 26 { b'a' + n as u8 } else { b'a' + (n - 26) as u8 }) as char);
            }
            '\u{1D7CE}'..='\u{1D7FF}' => mapped.push((b'0' + ((ch as u32 - 0x1D7CE) % 10) as u8) as char),
            _ => mapped.extend(ch.to_lowercase()),
        }
    }
    let mut out = Vec::new();
    for label in mapped.split('.') {
        if let Some(rest) = label.strip_prefix("xn--") {
            if !label.is_ascii() {
                return Err(());
            }
            let dec = punycode_decode(rest).ok_or(())?;
            if !dec.is_empty() && dec.is_ascii() || dec.chars().any(forbidden_domain) || dec.chars().any(|c| c.is_uppercase()) {
                return Err(());
            }
            out.push(label.to_string());
        } else if label.is_ascii() {
            out.push(label.to_string());
        } else {
            if label.chars().any(|c| forbidden_domain(c) || c.is_control()) {
                return Err(());
            }
            out.push(format!("xn--{}", punycode_encode(label).ok_or(())?));
        }
    }
    let r = out.join(".");
    if r.is_empty() {
        Err(())
    } else {
        Ok(r)
    }
}

const BASE: u32 = 36;

fn adapt(mut delta: u32, points: u32, first: bool) -> u32 {
    delta = if first { delta / 700 } else { delta / 2 };
    delta += delta / points;
    let mut k = 0;
    while delta > ((BASE - 1) * 26) / 2 {
        delta /= BASE - 1;
        k += BASE;
    }
    k + (BASE * delta) / (delta + 38)
}

fn threshold(k: u32, bias: u32) -> u32 {
    if k <= bias {
        1
    } else if k >= bias + 26 {
        26
    } else {
        k - bias
    }
}

fn digit(d: u32) -> char {
    (if d < 26 { b'a' + d as u8 } else { b'0' + (d - 26) as u8 }) as char
}

/// RFC 3492 Punycode, the part after "xn--".
pub fn punycode_encode(input: &str) -> Option<String> {
    let cps: Vec<u32> = input.chars().map(|c| c as u32).collect();
    let mut out: String = input.chars().filter(|c| c.is_ascii()).collect();
    let b = out.len() as u32;
    let mut h = b;
    if b > 0 {
        out.push('-');
    }
    let (mut n, mut delta, mut bias) = (128u32, 0u32, 72u32);
    while (h as usize) < cps.len() {
        let m = *cps.iter().filter(|&&c| c >= n).min()?;
        delta = delta.checked_add((m - n).checked_mul(h + 1)?)?;
        n = m;
        for &c in &cps {
            if c < n {
                delta = delta.checked_add(1)?;
            }
            if c == n {
                let (mut q, mut k) = (delta, BASE);
                loop {
                    let t = threshold(k, bias);
                    if q < t {
                        break;
                    }
                    out.push(digit(t + (q - t) % (BASE - t)));
                    q = (q - t) / (BASE - t);
                    k += BASE;
                }
                out.push(digit(q));
                bias = adapt(delta, h + 1, h == b);
                delta = 0;
                h += 1;
            }
        }
        delta += 1;
        n += 1;
    }
    Some(out)
}

pub fn punycode_decode(input: &str) -> Option<String> {
    let (basic, rest) = match input.rfind('-') {
        Some(i) => (&input[..i], &input[i + 1..]),
        None => ("", input),
    };
    if !basic.is_ascii() {
        return None;
    }
    let mut out: Vec<char> = basic.chars().collect();
    let (mut n, mut i, mut bias) = (128u32, 0u32, 72u32);
    let mut bytes = rest.bytes().peekable();
    while bytes.peek().is_some() {
        let (old, mut w, mut k) = (i, 1u32, BASE);
        loop {
            let d = match bytes.next()? {
                b @ b'a'..=b'z' => b - b'a',
                b @ b'A'..=b'Z' => b - b'A',
                b @ b'0'..=b'9' => b - b'0' + 26,
                _ => return None,
            } as u32;
            i = i.checked_add(d.checked_mul(w)?)?;
            let t = threshold(k, bias);
            if d < t {
                break;
            }
            w = w.checked_mul(BASE - t)?;
            k += BASE;
        }
        let len = out.len() as u32 + 1;
        bias = adapt(i - old, len, old == 0);
        n = n.checked_add(i / len)?;
        i %= len;
        if n < 0x80 {
            return None;
        }
        out.insert(i as usize, char::from_u32(n)?);
        i += 1;
    }
    Some(out.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn href(input: &str, base: &str) -> Option<String> {
        let b = parse(base, None);
        parse(input, b.as_ref()).map(|u| u.href())
    }

    #[test]
    fn parses_like_the_standard() {
        assert_eq!(href("  HTTP://EXAMPLE.com:80/a/./b/../c?x y#z ", "about:blank").as_deref(), Some("http://example.com/a/c?x%20y#z"));
        assert_eq!(href("../d", "http://h/a/b/c").as_deref(), Some("http://h/a/d"));
        assert_eq!(href("//x/y", "https://h/").as_deref(), Some("https://x/y"));
        assert_eq!(href("http://0x7f.1/", "about:blank").as_deref(), Some("http://127.0.0.1/"));
        assert_eq!(href("http://[1:0::0:2]/", "about:blank").as_deref(), Some("http://[1::2]/"));
        assert_eq!(href("http://[::127.0.0.1]", "about:blank").as_deref(), Some("http://[::7f00:1]/"));
        assert_eq!(href("http://bücher.de/", "about:blank").as_deref(), Some("http://xn--bcher-kva.de/"));
        assert_eq!(href("file:///C|/x", "about:blank").as_deref(), Some("file:///C:/x"));
        assert_eq!(href("mailto:a b ?c", "about:blank").as_deref(), Some("mailto:a b%20?c"));
        assert_eq!(href("http://a b/", "about:blank"), None);
        assert_eq!(href("http://1.2.3.256/", "about:blank"), None);
        assert_eq!(href("x", "mailto:y"), None);
        assert_eq!(href("web+demo:/.//not-a-host/", "about:blank").as_deref(), Some("web+demo:/.//not-a-host/"));
        let mut u = parse("http://u:p@h:8080/p?q#f", None).unwrap();
        assert_eq!(u.origin(), "http://h:8080");
        u.set("port", "80");
        u.set("protocol", "https");
        u.set("hostname", "Ex.com");
        u.set("search", "a=1");
        assert_eq!(u.href(), "https://u:p@ex.com/p?a=1#f");
        assert_eq!(form_parse("a=1&b+c=%E2%82%AC&&d"), vec![("a".into(), "1".into()), ("b c".into(), "€".into()), ("d".into(), "".into())]);
        assert_eq!(form_encode("a b&€*"), "a+b%26%E2%82%AC*");
        assert_eq!(punycode_decode("bcher-kva").as_deref(), Some("bücher"));
    }
}
