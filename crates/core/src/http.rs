//! An HTTP/1.1 client, written from scratch: URLs, requests, response framing
//! (Content-Length, chunked, read-until-close), gzip, redirects, size limits
//! and an overall deadline. The bytes travel over a [`Transport`]: the app's
//! is TLS from macOS's Network framework (encryption stays the system's); the
//! tests here use plain TCP.

use crate::deflate;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq)]
pub struct Url {
    pub https: bool,
    /// As it goes in the Host header (`[::1]` keeps its brackets).
    pub host: String,
    pub port: u16,
    /// Path and query, always starting with `/`.
    pub target: String,
}

impl Url {
    pub fn parse(s: &str) -> Option<Url> {
        let s = s.trim();
        let (scheme, rest) = s.split_once("://")?;
        let https = match scheme.to_ascii_lowercase().as_str() {
            "https" => true,
            "http" => false,
            _ => return None,
        };
        let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        let authority = &rest[..end];
        let authority = authority.rsplit_once('@').map(|(_, h)| h).unwrap_or(authority);
        let (host, port) = if let Some(v6) = authority.strip_prefix('[') {
            let close = v6.find(']')?;
            let port = v6[close + 1..].strip_prefix(':');
            (format!("[{}]", &v6[..close]), port)
        } else {
            match authority.rsplit_once(':') {
                Some((h, p)) => (h.to_string(), Some(p)),
                None => (authority.to_string(), None),
            }
        };
        let host = host.to_ascii_lowercase();
        if host.is_empty() || !host.bytes().all(|c| c.is_ascii_alphanumeric() || b".-_[]:".contains(&c)) {
            return None; // includes non-ASCII names, which would need punycode
        }
        let port = match port {
            Some(p) if !p.is_empty() => p.parse().ok()?,
            _ if https => 443,
            _ => 80,
        };
        let mut target = rest[end..].split('#').next().unwrap_or("").to_string();
        if !target.starts_with('/') {
            target.insert(0, '/');
        }
        if target.bytes().any(|c| c <= b' ' || c == 0x7f) {
            target = target.replace(' ', "%20");
            if target.bytes().any(|c| c < b' ' || c == 0x7f) {
                return None;
            }
        }
        Some(Url { https, host, port, target })
    }

    /// The host without IPv6 brackets, for connecting.
    pub fn connect_host(&self) -> &str {
        self.host.trim_start_matches('[').trim_end_matches(']')
    }

    fn default_port(&self) -> bool {
        self.port == if self.https { 443 } else { 80 }
    }

    pub fn origin(&self) -> String {
        let scheme = if self.https { "https" } else { "http" };
        if self.default_port() {
            format!("{scheme}://{}", self.host)
        } else {
            format!("{scheme}://{}:{}", self.host, self.port)
        }
    }

    pub fn as_string(&self) -> String {
        format!("{}{}", self.origin(), self.target)
    }

    /// Resolves a `Location` (absolute, scheme-relative, absolute-path or relative).
    pub fn join(&self, loc: &str) -> Option<Url> {
        let loc = loc.trim();
        if loc.contains("://") {
            return Url::parse(loc);
        }
        if let Some(rest) = loc.strip_prefix("//") {
            return Url::parse(&format!("{}://{rest}", if self.https { "https" } else { "http" }));
        }
        if loc.starts_with('/') {
            return Url::parse(&format!("{}{loc}", self.origin()));
        }
        let path = self.target.split('?').next().unwrap_or("/");
        let dir = &path[..=path.rfind('/').unwrap_or(0)];
        Url::parse(&format!("{}{dir}{loc}", self.origin()))
    }
}

/// Opens connections. The app's opens TLS through the Network framework.
pub trait Transport {
    type Conn: Connection;
    fn connect(&self, host: &str, port: u16, tls: bool, timeout: Duration) -> Result<Self::Conn, String>;
}

pub trait Connection {
    fn send(&mut self, data: &[u8]) -> Result<(), String>;
    /// The next bytes, or `None` once the server has closed the connection.
    fn recv(&mut self, timeout: Duration) -> Result<Option<Vec<u8>>, String>;
}

#[derive(Debug, Clone)]
pub struct Options {
    /// Extra request headers (`If-None-Match`, `Accept`, ...).
    pub headers: Vec<(String, String)>,
    /// The most body bytes to accept, after decompression.
    pub limit: usize,
    /// For the whole exchange, redirects included.
    pub deadline: Duration,
    pub max_redirects: u8,
    pub user_agent: String,
}

impl Default for Options {
    fn default() -> Self {
        Self { headers: Vec::new(), limit: 32 << 20, deadline: Duration::from_secs(60), max_redirects: 5, user_agent: "Volant".into() }
    }
}

#[derive(Debug, Clone)]
pub struct Response {
    pub status: u16,
    /// Lower-case names, in order.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// Where the answer finally came from, after redirects.
    pub url: String,
}

impl Response {
    pub fn header(&self, name: &str) -> Option<&str> {
        let name = name.to_ascii_lowercase();
        self.headers.iter().find(|(k, _)| *k == name).map(|(_, v)| v.as_str())
    }
    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

/// The request bytes for `url`.
pub fn request(url: &Url, opts: &Options) -> Result<Vec<u8>, String> {
    let host = if url.default_port() { url.host.clone() } else { format!("{}:{}", url.host, url.port) };
    let mut r = format!(
        "GET {} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: {}\r\nAccept: */*\r\nAccept-Encoding: gzip\r\nConnection: close\r\n",
        url.target, opts.user_agent
    );
    for (k, v) in &opts.headers {
        if k.is_empty() || k.contains([':', '\r', '\n']) || v.contains(['\r', '\n']) {
            return Err(format!("bad request header {k:?}"));
        }
        r.push_str(&format!("{k}: {v}\r\n"));
    }
    r.push_str("\r\n");
    Ok(r.into_bytes())
}

enum Framing {
    Length(usize),
    Chunked(Chunks),
    UntilClose,
    Empty,
}

/// An incremental chunked-transfer decoder.
#[derive(Default)]
struct Chunks {
    out: Vec<u8>,
    /// Bytes still to copy from the current chunk (plus its CRLF).
    left: usize,
    in_data: bool,
    done: bool,
    line: Vec<u8>,
    trailers: bool,
}

impl Chunks {
    fn feed(&mut self, mut data: &[u8], limit: usize) -> Result<(), String> {
        while !data.is_empty() && !self.done {
            if self.in_data {
                let take = self.left.min(data.len());
                // The chunk's data, then the CRLF after it (not copied).
                let payload = take.min(self.left.saturating_sub(2));
                self.out.extend_from_slice(&data[..payload]);
                if self.out.len() > limit {
                    return Err("the response is bigger than allowed".into());
                }
                self.left -= take;
                data = &data[take..];
                if self.left == 0 {
                    self.in_data = false;
                }
                continue;
            }
            let nl = data.iter().position(|&c| c == b'\n');
            let (part, rest) = match nl {
                Some(i) => (&data[..=i], &data[i + 1..]),
                None => (data, &data[data.len()..]),
            };
            self.line.extend_from_slice(part);
            data = rest;
            if self.line.len() > 8192 {
                return Err("a chunk header is too long".into());
            }
            if nl.is_none() {
                break;
            }
            let line = String::from_utf8_lossy(&self.line).trim().to_string();
            self.line.clear();
            if self.trailers {
                if line.is_empty() {
                    self.done = true;
                }
                continue;
            }
            let hex = line.split(';').next().unwrap_or("").trim();
            let size = usize::from_str_radix(hex, 16).map_err(|_| format!("bad chunk size {hex:?}"))?;
            if size == 0 {
                self.trailers = true;
            } else {
                self.left = size + 2;
                self.in_data = true;
            }
        }
        Ok(())
    }
}

/// Splits a response head: status and lower-cased headers.
fn parse_head(head: &str) -> Result<(u16, Vec<(String, String)>), String> {
    let mut lines = head.split("\r\n");
    let status_line = lines.next().unwrap_or("");
    let mut parts = status_line.splitn(3, ' ');
    let version = parts.next().unwrap_or("");
    if !version.starts_with("HTTP/1.") {
        return Err(format!("not an HTTP/1 response: {:?}", &status_line[..status_line.len().min(40)]));
    }
    let status: u16 = parts.next().and_then(|s| s.parse().ok()).ok_or("bad status line")?;
    let headers = lines
        .filter(|l| !l.is_empty())
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    Ok((status, headers))
}

/// One request/response exchange on a fresh connection.
fn exchange<T: Transport>(t: &T, url: &Url, opts: &Options, until: Instant) -> Result<Response, String> {
    let left = |until: Instant| until.checked_duration_since(Instant::now()).filter(|d| !d.is_zero()).ok_or("timed out".to_string());
    let mut conn = t.connect(url.connect_host(), url.port, url.https, left(until)?.min(Duration::from_secs(20)))?;
    conn.send(&request(url, opts)?)?;
    let mut buf: Vec<u8> = Vec::new();
    // Read the head (skipping any 1xx interim answers).
    let (status, headers, body_start) = loop {
        let end = buf.windows(4).position(|w| w == b"\r\n\r\n");
        if let Some(end) = end {
            let head = String::from_utf8_lossy(&buf[..end]).into_owned();
            let (status, headers) = parse_head(&head)?;
            if (100..200).contains(&status) {
                buf.drain(..end + 4);
                continue;
            }
            break (status, headers, end + 4);
        }
        if buf.len() > 64 * 1024 {
            return Err("the response headers are too long".into());
        }
        match conn.recv(left(until)?)? {
            Some(more) => buf.extend_from_slice(&more),
            None => return Err("the server closed the connection without answering".into()),
        }
    };
    let get = |name: &str| headers.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str());
    let mut framing = if status == 204 || status == 304 {
        Framing::Empty
    } else if get("transfer-encoding").map(|v| v.to_ascii_lowercase().contains("chunked")).unwrap_or(false) {
        Framing::Chunked(Chunks::default())
    } else if let Some(n) = get("content-length") {
        let n: usize = n.parse().map_err(|_| "bad Content-Length")?;
        if n > opts.limit {
            return Err("the response is bigger than allowed".into());
        }
        Framing::Length(n)
    } else {
        Framing::UntilClose
    };
    // A compressed body may legitimately be smaller than `limit` and still unpack past it.
    let raw_limit = opts.limit;
    let mut body: Vec<u8> = Vec::new();
    let mut pending: Vec<u8> = buf.split_off(body_start);
    loop {
        match &mut framing {
            Framing::Empty => break,
            Framing::Length(n) => {
                body.extend_from_slice(&pending);
                if body.len() >= *n {
                    body.truncate(*n);
                    break;
                }
            }
            Framing::Chunked(c) => {
                c.feed(&pending, raw_limit)?;
                if c.done {
                    break;
                }
            }
            Framing::UntilClose => {
                body.extend_from_slice(&pending);
                if body.len() > raw_limit {
                    return Err("the response is bigger than allowed".into());
                }
            }
        }
        pending.clear();
        match conn.recv(left(until)?)? {
            Some(more) => pending = more,
            None => match framing {
                Framing::UntilClose => break,
                _ => return Err("the connection closed before the response was complete".into()),
            },
        }
    }
    if let Framing::Chunked(c) = framing {
        body = c.out;
    }
    match get("content-encoding").map(|v| v.trim().to_ascii_lowercase()) {
        None => {}
        Some(e) if e.is_empty() || e == "identity" => {}
        Some(e) if e == "gzip" || e == "x-gzip" => body = deflate::gunzip(&body, opts.limit).map_err(|e| format!("gzip: {e}"))?,
        Some(e) => return Err(format!("the server used an encoding Volant didn't ask for ({e})")),
    }
    Ok(Response { status, headers, body, url: url.as_string() })
}

/// GETs `url`, following redirects.
pub fn get<T: Transport>(t: &T, url: &str, opts: &Options) -> Result<Response, String> {
    let until = Instant::now() + opts.deadline;
    let mut url = Url::parse(url).ok_or_else(|| format!("not a web address Volant can fetch: {url}"))?;
    for _ in 0..=opts.max_redirects {
        let r = exchange(t, &url, opts, until)?;
        if matches!(r.status, 301 | 302 | 303 | 307 | 308) {
            if let Some(next) = r.header("location").and_then(|l| url.join(l)) {
                // Never step down from HTTPS to plain HTTP.
                if url.https && !next.https {
                    return Err("the server redirected to an insecure address".into());
                }
                url = next;
                continue;
            }
        }
        return Ok(r);
    }
    Err("too many redirects".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};

    struct Tcp;
    struct TcpConn(TcpStream);

    impl Transport for Tcp {
        type Conn = TcpConn;
        fn connect(&self, host: &str, port: u16, tls: bool, timeout: Duration) -> Result<TcpConn, String> {
            assert!(!tls, "tests use plain TCP");
            let addr = format!("{host}:{port}").parse().map_err(|e| format!("{e}"))?;
            TcpStream::connect_timeout(&addr, timeout).map(TcpConn).map_err(|e| e.to_string())
        }
    }

    impl Connection for TcpConn {
        fn send(&mut self, d: &[u8]) -> Result<(), String> {
            self.0.write_all(d).map_err(|e| e.to_string())
        }
        fn recv(&mut self, timeout: Duration) -> Result<Option<Vec<u8>>, String> {
            self.0.set_read_timeout(Some(timeout)).map_err(|e| e.to_string())?;
            let mut b = vec![0u8; 7]; // tiny reads exercise every boundary
            match self.0.read(&mut b) {
                Ok(0) => Ok(None),
                Ok(n) => Ok(Some(b[..n].to_vec())),
                Err(e) => Err(e.to_string()),
            }
        }
    }

    /// A server that answers each connection with the next canned response,
    /// and reports the requests it saw.
    fn serve(responses: Vec<Vec<u8>>) -> (u16, std::sync::mpsc::Receiver<String>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for r in responses {
                let (mut s, _) = l.accept().unwrap();
                let mut req = Vec::new();
                let mut b = [0u8; 1024];
                while !req.windows(4).any(|w| w == b"\r\n\r\n") {
                    let n = s.read(&mut b).unwrap();
                    req.extend_from_slice(&b[..n]);
                }
                tx.send(String::from_utf8_lossy(&req).into_owned()).unwrap();
                s.write_all(&r).unwrap();
            }
        });
        (port, rx)
    }

    fn gz(data: &[u8]) -> Vec<u8> {
        // A stored-block gzip: header, raw deflate, CRC and size.
        let mut out = vec![0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 3];
        out.extend(deflate::deflate(data));
        out.extend(crate::hash::crc32(data).to_le_bytes());
        out.extend((data.len() as u32).to_le_bytes());
        out
    }

    #[test]
    fn urls() {
        let u = Url::parse("HTTPS://Example.COM/a/b?q=1#frag").unwrap();
        assert_eq!((u.https, u.host.as_str(), u.port, u.target.as_str()), (true, "example.com", 443, "/a/b?q=1"));
        assert_eq!(Url::parse("http://h:8080").unwrap().target, "/");
        assert_eq!(Url::parse("http://[::1]:9/x").unwrap().connect_host(), "::1");
        assert!(Url::parse("ftp://x/").is_none() && Url::parse("https://bücher.de/").is_none());
        assert_eq!(u.join("/root").unwrap().as_string(), "https://example.com/root");
        assert_eq!(u.join("c").unwrap().as_string(), "https://example.com/a/c");
        assert_eq!(u.join("//cdn.example/x").unwrap().as_string(), "https://cdn.example/x");
        assert_eq!(u.join("https://other/").unwrap().host, "other");
    }

    #[test]
    fn chunked_gzip_redirect_and_limits() {
        let text = b"! Title: EasyList\n||ads.example^\n".repeat(200);
        let z = gz(&text);
        let mut chunked = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nContent-Encoding: gzip\r\nETag: \"v1\"\r\n\r\n".to_vec();
        for part in z.chunks(100) {
            chunked.extend(format!("{:x};ext=1\r\n", part.len()).as_bytes());
            chunked.extend(part);
            chunked.extend(b"\r\n");
        }
        chunked.extend(b"0\r\nX-Trailer: t\r\n\r\n");
        let (port, seen) = serve(vec![
            b"HTTP/1.1 302 Found\r\nLocation: /lists/easylist.txt\r\nContent-Length: 0\r\n\r\n".to_vec(),
            b"HTTP/1.1 100 Continue\r\n\r\n".iter().chain(chunked.iter()).copied().collect(),
            b"HTTP/1.1 304 Not Modified\r\nETag: \"v1\"\r\n\r\n".to_vec(),
            b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello and more".to_vec(),
            b"HTTP/1.0 200 OK\r\n\r\nuntil close".to_vec(),
            b"HTTP/1.1 200 OK\r\nContent-Length: 999999\r\n\r\n".to_vec(),
        ]);
        let base = format!("http://127.0.0.1:{port}");
        let opts = Options { user_agent: "Volant/test".into(), ..Default::default() };
        let r = get(&Tcp, &format!("{base}/start"), &opts).unwrap();
        assert_eq!(r.status, 200);
        assert_eq!(r.body, text);
        assert_eq!(r.header("ETag"), Some("\"v1\""));
        assert!(r.url.ends_with("/lists/easylist.txt"));
        let first = seen.recv().unwrap();
        assert!(first.starts_with("GET /start HTTP/1.1\r\n") && first.contains("User-Agent: Volant/test") && first.contains("Accept-Encoding: gzip"));
        assert!(seen.recv().unwrap().starts_with("GET /lists/easylist.txt "));
        let cond = Options { headers: vec![("If-None-Match".into(), "\"v1\"".into())], ..opts.clone() };
        let r = get(&Tcp, &format!("{base}/again"), &cond).unwrap();
        assert_eq!((r.status, r.body.len()), (304, 0));
        assert!(seen.recv().unwrap().contains("If-None-Match: \"v1\""));
        assert_eq!(get(&Tcp, &format!("{base}/len"), &opts).unwrap().body, b"hello");
        assert_eq!(get(&Tcp, &format!("{base}/close"), &opts).unwrap().body, b"until close");
        let small = Options { limit: 1000, ..opts };
        assert!(get(&Tcp, &format!("{base}/huge"), &small).unwrap_err().contains("bigger"));
    }

    #[test]
    fn refuses_bad_headers() {
        let u = Url::parse("https://example.com/").unwrap();
        let o = Options { headers: vec![("X".into(), "a\r\nEvil: 1".into())], ..Default::default() };
        assert!(request(&u, &o).is_err());
        assert!(parse_head("SSH-2.0-OpenSSH\r\n").is_err());
    }
}
