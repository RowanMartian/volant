//! Just enough JSON writing for Volant's needs (content rules and data handed
//! to its own pages). No reflection, no allocations beyond the output string.

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

#[cfg(test)]
mod tests {
    use super::*;
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
