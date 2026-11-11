//! Search engines Volant can send queries to.

pub struct Engine {
    pub id: &'static str,
    pub name: &'static str,
    /// `%s` is replaced with the percent-encoded query.
    pub template: &'static str,
}

pub const ENGINES: &[Engine] = &[
    Engine { id: "duckduckgo", name: "DuckDuckGo", template: "https://duckduckgo.com/?q=%s" },
    Engine { id: "google", name: "Google", template: "https://www.google.com/search?q=%s" },
    Engine { id: "brave", name: "Brave Search", template: "https://search.brave.com/search?q=%s" },
    Engine { id: "kagi", name: "Kagi", template: "https://kagi.com/search?q=%s" },
    Engine { id: "bing", name: "Bing", template: "https://www.bing.com/search?q=%s" },
    Engine { id: "ecosia", name: "Ecosia", template: "https://www.ecosia.org/search?q=%s" },
];

pub const DEFAULT_ENGINE: &str = "duckduckgo";

pub fn engine(id: &str) -> &'static Engine {
    ENGINES.iter().find(|e| e.id == id).unwrap_or(&ENGINES[0])
}

/// Percent-encodes a query for use in a URL query string (spaces become `+`).
pub fn encode_query(q: &str) -> String {
    let mut out = String::with_capacity(q.len() * 3 / 2);
    for b in q.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            b' ' => out.push('+'),
            _ => {
                out.push('%');
                out.push(char::from(b"0123456789ABCDEF"[(b >> 4) as usize]));
                out.push(char::from(b"0123456789ABCDEF"[(b & 15) as usize]));
            }
        }
    }
    out
}

pub fn search_url(engine_id: &str, query: &str) -> String {
    engine(engine_id).template.replace("%s", &encode_query(query.trim()))
}

/// If `url` is a results page of one of our engines, returns the query that produced it.
pub fn query_of(url: &str) -> Option<String> {
    for e in ENGINES {
        let prefix = &e.template[..e.template.find("%s")?];
        if let Some(rest) = url.strip_prefix(prefix) {
            let raw = rest.split('&').next().unwrap_or("");
            return Some(decode_query(raw));
        }
    }
    None
}

pub fn decode_query(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => {
                let h = |c: u8| (c as char).to_digit(16);
                match (h(b[i + 1]), h(b[i + 2])) {
                    (Some(x), Some(y)) => {
                        out.push((x * 16 + y) as u8);
                        i += 2;
                    }
                    _ => out.push(b'%'),
                }
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn round_trip() {
        let u = search_url("duckduckgo", "rust & café");
        assert_eq!(u, "https://duckduckgo.com/?q=rust+%26+caf%C3%A9");
        assert_eq!(query_of(&u).unwrap(), "rust & café");
        assert_eq!(query_of("https://www.google.com/search?q=a+b&hl=en").unwrap(), "a b");
        assert_eq!(decode_query("100%"), "100%");
        assert_eq!(decode_query("%zz"), "%zz");
    }
}
