//! Just enough JSON for Volant's needs: writing content rules and the data
//! handed to its own pages, and reading API answers (GitHub's release, uBO's
//! version) with a small parser.

/// Appends `s` to `out` as a quoted JSON string.
pub fn push_str(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            // Keep the output safe to drop into a <script> or evaluateJavaScript call.
            '<' => out.push_str("\\u003c"),
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

pub fn string(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    push_str(&mut o, s);
    o
}

/// A small builder for one JSON object.
pub struct Obj {
    buf: String,
    first: bool,
}

impl Default for Obj {
    fn default() -> Self {
        Self::new()
    }
}

impl Obj {
    pub fn new() -> Self {
        Self { buf: String::from("{"), first: true }
    }
    fn key(&mut self, k: &str) {
        if !self.first {
            self.buf.push(',');
        }
        self.first = false;
        push_str(&mut self.buf, k);
        self.buf.push(':');
    }
    pub fn str(mut self, k: &str, v: &str) -> Self {
        self.key(k);
        push_str(&mut self.buf, v);
        self
    }
    pub fn num(mut self, k: &str, v: impl std::fmt::Display) -> Self {
        self.key(k);
        self.buf.push_str(&v.to_string());
        self
    }
    pub fn bool(mut self, k: &str, v: bool) -> Self {
        self.key(k);
        self.buf.push_str(if v { "true" } else { "false" });
        self
    }
    /// Inserts already-encoded JSON.
    pub fn raw(mut self, k: &str, json: &str) -> Self {
        self.key(k);
        self.buf.push_str(json);
        self
    }
    pub fn done(mut self) -> String {
        self.buf.push('}');
        self.buf
    }
}

/// Joins already-encoded JSON values into an array.
pub fn array<I: IntoIterator<Item = String>>(items: I) -> String {
    let mut o = String::from("[");
    for (i, it) in items.into_iter().enumerate() {
        if i > 0 {
            o.push(',');
        }
        o.push_str(&it);
    }
    o.push(']');
    o
}

/// Every string value stored under `key` anywhere in `json`, in order. Enough
/// for picking a few fields out of an API response without a full parser.
pub fn strings_for(json: &str, key: &str) -> Vec<String> {
    let needle = format!("\"{key}\"");
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(p) = json[from..].find(&needle).map(|p| p + from) {
        from = p + needle.len();
        let rest = json[from..].trim_start();
        let Some(rest) = rest.strip_prefix(':') else { continue };
        let rest = rest.trim_start();
        let Some(body) = rest.strip_prefix('"') else { continue };
        let mut v = String::new();
        let mut chars = body.chars();
        while let Some(c) = chars.next() {
            match c {
                '"' => break,
                '\\' => match chars.next() {
                    Some('n') => v.push('\n'),
                    Some('t') => v.push('\t'),
                    Some('u') => {
                        let hex: String = chars.by_ref().take(4).collect();
                        if let Some(c) = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                            v.push(c);
                        }
                    }
                    Some(c) => v.push(c),
                    None => break,
                },
                c => v.push(c),
            }
        }
        out.push(v);
    }
    out
}

/// A parsed JSON value.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Value>),
    /// Keys in document order.
    Obj(Vec<(String, Value)>),
}

impl Value {
    /// The value under `key`, if this is an object that has it.
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Obj(kv) => kv.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Num(n) => Some(*n),
            _ => None,
        }
    }
    pub fn as_array(&self) -> &[Value] {
        match self {
            Value::Arr(a) => a,
            _ => &[],
        }
    }
    /// `get(key)` as a string.
    pub fn str_at(&self, key: &str) -> Option<&str> {
        self.get(key).and_then(Value::as_str)
    }
}

/// Parses a JSON document. `None` if it isn't valid JSON (or nests absurdly deep).
pub fn parse(text: &str) -> Option<Value> {
    let mut p = Parser { s: text.as_bytes(), i: 0, depth: 0 };
    let v = p.value()?;
    p.ws();
    (p.i == p.s.len()).then_some(v)
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
    depth: u32,
}

impl Parser<'_> {
    fn ws(&mut self) {
        while self.i < self.s.len() && matches!(self.s[self.i], b' ' | b'\t' | b'\n' | b'\r') {
            self.i += 1;
        }
    }
    fn eat(&mut self, lit: &[u8]) -> bool {
        if self.s[self.i..].starts_with(lit) {
            self.i += lit.len();
            true
        } else {
            false
        }
    }
    fn value(&mut self) -> Option<Value> {
        self.ws();
        match *self.s.get(self.i)? {
            b'{' => {
                self.nest()?;
                self.i += 1;
                let mut kv = Vec::new();
                self.ws();
                if !self.eat(b"}") {
                    loop {
                        self.ws();
                        let k = self.string()?;
                        self.ws();
                        if !self.eat(b":") {
                            return None;
                        }
                        kv.push((k, self.value()?));
                        self.ws();
                        if self.eat(b"}") {
                            break;
                        }
                        if !self.eat(b",") {
                            return None;
                        }
                    }
                }
                self.depth -= 1;
                Some(Value::Obj(kv))
            }
            b'[' => {
                self.nest()?;
                self.i += 1;
                let mut a = Vec::new();
                self.ws();
                if !self.eat(b"]") {
                    loop {
                        a.push(self.value()?);
                        self.ws();
                        if self.eat(b"]") {
                            break;
                        }
                        if !self.eat(b",") {
                            return None;
                        }
                    }
                }
                self.depth -= 1;
                Some(Value::Arr(a))
            }
            b'"' => self.string().map(Value::Str),
            b't' if self.eat(b"true") => Some(Value::Bool(true)),
            b'f' if self.eat(b"false") => Some(Value::Bool(false)),
            b'n' if self.eat(b"null") => Some(Value::Null),
            b'-' | b'0'..=b'9' => {
                let start = self.i;
                while self.i < self.s.len() && matches!(self.s[self.i], b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9') {
                    self.i += 1;
                }
                std::str::from_utf8(&self.s[start..self.i]).ok()?.parse().ok().map(Value::Num)
            }
            _ => None,
        }
    }
    fn nest(&mut self) -> Option<()> {
        self.depth += 1;
        (self.depth <= 256).then_some(())
    }
    fn hex4(&mut self) -> Option<u32> {
        let h = std::str::from_utf8(self.s.get(self.i..self.i + 4)?).ok()?;
        self.i += 4;
        u32::from_str_radix(h, 16).ok()
    }
    fn string(&mut self) -> Option<String> {
        if !self.eat(b"\"") {
            return None;
        }
        let mut out = Vec::new();
        loop {
            let c = *self.s.get(self.i)?;
            self.i += 1;
            match c {
                b'"' => return String::from_utf8(out).ok(),
                b'\\' => {
                    let e = *self.s.get(self.i)?;
                    self.i += 1;
                    let ch = match e {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'u' => {
                            let mut u = self.hex4()?;
                            if (0xd800..0xdc00).contains(&u) && self.eat(b"\\u") {
                                let lo = self.hex4()?;
                                u = 0x10000 + ((u - 0xd800) << 10) + (lo.wrapping_sub(0xdc00) & 0x3ff);
                            }
                            char::from_u32(u).unwrap_or('\u{fffd}')
                        }
                        _ => return None,
                    };
                    let mut buf = [0u8; 4];
                    out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                }
                c if c < 0x20 => return None,
                c => out.push(c),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses() {
        let j = r#" {"a": [1, -2.5e3, true, false, null], "s": "x\"y\u00e9\ud83d\ude00\n", "o": {"k": "v"}, "e": {}, "z": []} "#;
        let v = parse(j).unwrap();
        assert_eq!(v.get("a").unwrap().as_array().len(), 5);
        assert_eq!(v.get("a").unwrap().as_array()[1].as_f64(), Some(-2500.0));
        assert_eq!(v.str_at("s"), Some("x\"yé😀\n"));
        assert_eq!(v.get("o").unwrap().str_at("k"), Some("v"));
        assert_eq!(v.get("e"), Some(&Value::Obj(vec![])));
        for bad in ["", "{", "[1,]", "{\"a\" 1}", r#""\u12""#, "tru", "[1] x", "\"a\nb\""] {
            assert!(parse(bad).is_none(), "{bad:?}");
        }
        assert!(parse(&"[".repeat(300)).is_none(), "depth limit");
        // A string written by this module reads back the same.
        let s = "q\"<\u{2028}\t\\é";
        assert_eq!(parse(&string(s)).unwrap(), Value::Str(s.into()));
    }

    #[test]
    fn reads_fields() {
        let j = r#"{"target_commitish": "abc123", "assets": [{"name":"Volant.dmg","browser_download_url":"https://x/Volant.dmg"},{"browser_download_url" : "https://x/Volant-mac.zip"}], "body":"a \"q\" \u00e9"}"#;
        assert_eq!(strings_for(j, "target_commitish"), ["abc123"]);
        assert_eq!(strings_for(j, "browser_download_url"), ["https://x/Volant.dmg", "https://x/Volant-mac.zip"]);
        assert_eq!(strings_for(j, "body"), ["a \"q\" é"]);
        assert!(strings_for(j, "missing").is_empty());
    }

    #[test]
    fn writes() {
        let j = Obj::new().str("a", "x\"<y\n").num("n", 3).bool("b", true).raw("l", &array(vec!["1".into(), "2".into()])).done();
        assert_eq!(j, r#"{"a":"x\"\u003cy\n","n":3,"b":true,"l":[1,2]}"#);
    }
}
