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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn writes() {
        let j = Obj::new().str("a", "x\"<y\n").num("n", 3).bool("b", true).raw("l", &array(vec!["1".into(), "2".into()])).done();
        assert_eq!(j, r#"{"a":"x\"\u003cy\n","n":3,"b":true,"l":[1,2]}"#);
    }
}
