//! A from-scratch HTML parser: tokenizer and tree builder in one pass.
//!
//! It is built for reading pages, not running them: it produces a document
//! tree from real-world HTML (unclosed `<p>` and `<li>`, entities, void
//! elements, raw text in `<script>`), throws away what reading never needs
//! (comments, script and style bodies), and does it in a single linear scan
//! with one allocation per node. It is not a full HTML5 tree builder (no
//! adoption agency, no foreign content), which reading doesn't need either.

pub type NodeId = u32;

#[derive(Debug)]
pub enum Data {
    Document,
    Element { name: Box<str>, attrs: Vec<(Box<str>, String)> },
    Text(String),
}

#[derive(Debug)]
pub struct Node {
    pub parent: NodeId,
    pub children: Vec<NodeId>,
    pub data: Data,
}

/// A parsed document. Nodes are stored in document order, so every node's
/// parent comes before it, which makes bottom-up passes a reverse loop.
pub struct Dom {
    pub nodes: Vec<Node>,
}

pub const ROOT: NodeId = 0;

const VOID: &[&str] = &["area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "source", "track", "wbr", "param", "keygen"];
/// Elements whose content is raw text up to the matching end tag.
const RAW: &[&str] = &["script", "style", "textarea", "title", "xmp", "iframe", "noembed", "noframes", "noscript", "template", "svg", "math"];
/// Raw elements whose text is worth keeping.
const KEEP_RAW: &[&str] = &["title", "textarea"];
/// Starting one of these closes an open `<p>`.
const CLOSES_P: &[&str] = &[
    "address", "article", "aside", "blockquote", "details", "div", "dl", "fieldset", "figcaption", "figure", "footer", "form", "h1", "h2", "h3", "h4",
    "h5", "h6", "header", "hgroup", "hr", "main", "menu", "nav", "ol", "p", "pre", "section", "table", "ul", "li", "dd", "dt",
];

impl Dom {
    pub fn tag(&self, id: NodeId) -> Option<&str> {
        match &self.nodes[id as usize].data {
            Data::Element { name, .. } => Some(name),
            _ => None,
        }
    }

    pub fn attr(&self, id: NodeId, key: &str) -> Option<&str> {
        match &self.nodes[id as usize].data {
            Data::Element { attrs, .. } => attrs.iter().find(|(k, _)| &**k == key).map(|(_, v)| v.as_str()),
            _ => None,
        }
    }

    pub fn children(&self, id: NodeId) -> &[NodeId] {
        &self.nodes[id as usize].children
    }

    pub fn parent(&self, id: NodeId) -> NodeId {
        self.nodes[id as usize].parent
    }

    /// The first element with this tag name, in document order.
    pub fn find(&self, tag: &str) -> Option<NodeId> {
        (0..self.nodes.len() as NodeId).find(|&i| self.tag(i) == Some(tag))
    }

    /// All text inside a node, concatenated.
    pub fn text(&self, id: NodeId) -> String {
        let mut out = String::new();
        self.text_into(id, &mut out);
        out
    }

    fn text_into(&self, id: NodeId, out: &mut String) {
        match &self.nodes[id as usize].data {
            Data::Text(t) => out.push_str(t),
            _ => {
                for &c in &self.nodes[id as usize].children {
                    self.text_into(c, out);
                }
            }
        }
    }
}

struct Builder {
    nodes: Vec<Node>,
    stack: Vec<NodeId>,
}

impl Builder {
    fn current(&self) -> NodeId {
        *self.stack.last().unwrap_or(&ROOT)
    }

    fn add(&mut self, data: Data) -> NodeId {
        let parent = self.current();
        let id = self.nodes.len() as NodeId;
        self.nodes.push(Node { parent, children: Vec::new(), data });
        self.nodes[parent as usize].children.push(id);
        id
    }

    fn text(&mut self, s: &str) {
        if s.is_empty() {
            return;
        }
        let parent = self.current();
        if let Some(&last) = self.nodes[parent as usize].children.last() {
            if let Data::Text(t) = &mut self.nodes[last as usize].data {
                decode_into(s, t);
                return;
            }
        }
        let mut t = String::with_capacity(s.len());
        decode_into(s, &mut t);
        self.add(Data::Text(t));
    }

    fn open_name(&self, i: usize) -> &str {
        match &self.nodes[self.stack[i] as usize].data {
            Data::Element { name, .. } => name,
            _ => "",
        }
    }

    /// Pops up to and including the nearest open `name`, if it's open
    /// within the given boundary elements.
    fn close(&mut self, name: &str, boundary: &[&str]) -> bool {
        for i in (0..self.stack.len()).rev() {
            let n = self.open_name(i);
            if n == name {
                self.stack.truncate(i);
                return true;
            }
            if boundary.contains(&n) {
                return false;
            }
        }
        false
    }

    /// The implied end tags HTML relies on (`<p>a<p>b`, `<li>a<li>b`, ...).
    fn implied_ends(&mut self, name: &str) {
        const SCOPE: &[&str] = &["table", "td", "th", "button", "html", "body", "object", "marquee", "template"];
        if CLOSES_P.contains(&name) {
            self.close("p", SCOPE);
        }
        match name {
            "li" => {
                self.close("li", &["ul", "ol", "menu", "table", "body"]);
            }
            "dt" | "dd" => {
                if !self.close("dd", &["dl", "table", "body"]) {
                    self.close("dt", &["dl", "table", "body"]);
                }
            }
            "tr" => {
                self.close("tr", &["table", "tbody", "thead", "tfoot"]);
            }
            "td" | "th" => {
                if !self.close("td", &["tr", "table"]) {
                    self.close("th", &["tr", "table"]);
                }
            }
            "tbody" | "thead" | "tfoot" => {
                for t in ["tbody", "thead", "tfoot"] {
                    self.close(t, &["table"]);
                }
            }
            "option" => {
                self.close("option", &["select", "datalist", "optgroup"]);
            }
            "a" => {
                // Links don't nest.
                self.close("a", &["p", "div", "li", "td", "body"]);
            }
            _ => {}
        }
    }
}

fn find_ci(hay: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    let first = needle[0].to_ascii_lowercase();
    let mut i = from;
    while i + needle.len() <= hay.len() {
        let j = hay[i..hay.len() - needle.len() + 1].iter().position(|b| b.to_ascii_lowercase() == first)? + i;
        if hay[j..j + needle.len()].eq_ignore_ascii_case(needle) {
            return Some(j);
        }
        i = j + 1;
    }
    None
}

/// Parses a whole document.
pub fn parse(html: &str) -> Dom {
    let bytes = html.as_bytes();
    let mut b = Builder { nodes: Vec::with_capacity(html.len() / 40 + 16), stack: Vec::with_capacity(64) };
    b.nodes.push(Node { parent: ROOT, children: Vec::new(), data: Data::Document });
    let mut i = 0usize;
    let n = bytes.len();
    while i < n {
        // Text up to the next tag.
        let lt = memchr(b'<', &bytes[i..]).map(|p| p + i).unwrap_or(n);
        if lt > i {
            b.text(&html[i..lt]);
        }
        i = lt;
        if i >= n {
            break;
        }
        let rest = &bytes[i..];
        if rest.starts_with(b"<!--") {
            i = find(bytes, i + 4, b"-->").map(|p| p + 3).unwrap_or(n);
            continue;
        }
        if rest.len() > 1 && (rest[1] == b'!' || rest[1] == b'?') {
            i = memchr(b'>', &bytes[i..]).map(|p| p + i + 1).unwrap_or(n);
            continue;
        }
        if rest.len() > 2 && rest[1] == b'/' && rest[2].is_ascii_alphabetic() {
            let (name, end) = read_name(html, i + 2);
            i = memchr(b'>', &bytes[end..]).map(|p| p + end + 1).unwrap_or(n);
            match name.as_str() {
                "p" if !b.stack.iter().any(|&s| matches!(&b.nodes[s as usize].data, Data::Element { name, .. } if &**name == "p")) => {
                    // `</p>` with no open <p> means an empty paragraph.
                    b.add(Data::Element { name: "p".into(), attrs: Vec::new() });
                }
                "br" => {
                    b.add(Data::Element { name: "br".into(), attrs: Vec::new() });
                }
                _ => {
                    b.close(&name, &[]);
                }
            }
            continue;
        }
        if rest.len() > 1 && rest[1].is_ascii_alphabetic() {
            let (name, mut j) = read_name(html, i + 1);
            let mut attrs: Vec<(Box<str>, String)> = Vec::new();
            let mut self_closing = false;
            // Attributes.
            loop {
                while j < n && bytes[j].is_ascii_whitespace() {
                    j += 1;
                }
                if j >= n {
                    break;
                }
                match bytes[j] {
                    b'>' => {
                        j += 1;
                        break;
                    }
                    b'/' => {
                        self_closing = true;
                        j += 1;
                        continue;
                    }
                    _ => {}
                }
                self_closing = false;
                let ks = j;
                while j < n && !bytes[j].is_ascii_whitespace() && !matches!(bytes[j], b'=' | b'>' | b'/') {
                    j += 1;
                }
                if j == ks {
                    j += 1; // stray character such as a lone quote
                    continue;
                }
                let key = html[ks..j].to_ascii_lowercase();
                while j < n && bytes[j].is_ascii_whitespace() {
                    j += 1;
                }
                let mut value = String::new();
                if j < n && bytes[j] == b'=' {
                    j += 1;
                    while j < n && bytes[j].is_ascii_whitespace() {
                        j += 1;
                    }
                    if j < n && (bytes[j] == b'"' || bytes[j] == b'\'') {
                        let q = bytes[j];
                        let vs = j + 1;
                        let ve = memchr(q, &bytes[vs..]).map(|p| p + vs).unwrap_or(n);
                        decode_into(&html[vs..ve], &mut value);
                        j = (ve + 1).min(n);
                    } else {
                        let vs = j;
                        while j < n && !bytes[j].is_ascii_whitespace() && bytes[j] != b'>' {
                            j += 1;
                        }
                        decode_into(&html[vs..j], &mut value);
                    }
                }
                if !attrs.iter().any(|(k, _)| **k == *key) {
                    attrs.push((key.into_boxed_str(), value));
                }
            }
            i = j;
            b.implied_ends(&name);
            if RAW.contains(&name.as_str()) {
                let close = format!("</{name}");
                let end = find_ci(bytes, i, close.as_bytes()).unwrap_or(n);
                let keep = KEEP_RAW.contains(&name.as_str());
                let id = b.add(Data::Element { name: name.clone().into_boxed_str(), attrs });
                if keep && end > i {
                    let mut t = String::new();
                    decode_into(&html[i..end], &mut t);
                    let tid = b.nodes.len() as NodeId;
                    b.nodes.push(Node { parent: id, children: Vec::new(), data: Data::Text(t) });
                    b.nodes[id as usize].children.push(tid);
                }
                i = memchr(b'>', &bytes[end.min(n)..]).map(|p| p + end + 1).unwrap_or(n);
                continue;
            }
            let void = VOID.contains(&name.as_str());
            let id = b.add(Data::Element { name: name.into_boxed_str(), attrs });
            if !void && !self_closing {
                b.stack.push(id);
            }
            continue;
        }
        // A lone `<` is text.
        b.text("<");
        i += 1;
    }
    Dom { nodes: b.nodes }
}

fn read_name(html: &str, start: usize) -> (String, usize) {
    let bytes = html.as_bytes();
    let mut j = start;
    while j < bytes.len() && !bytes[j].is_ascii_whitespace() && !matches!(bytes[j], b'>' | b'/') {
        j += 1;
    }
    (html[start..j].to_ascii_lowercase(), j)
}

fn memchr(needle: u8, hay: &[u8]) -> Option<usize> {
    hay.iter().position(|&b| b == needle)
}

fn find(hay: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    if from >= hay.len() {
        return None;
    }
    hay[from..].windows(needle.len()).position(|w| w == needle).map(|p| p + from)
}

/// The named character references that actually show up in articles.
const ENTITIES: &[(&str, &str)] = &[
    ("amp", "&"), ("lt", "<"), ("gt", ">"), ("quot", "\""), ("apos", "'"), ("nbsp", "\u{a0}"), ("mdash", "—"), ("ndash", "–"),
    ("hellip", "…"), ("rsquo", "’"), ("lsquo", "‘"), ("rdquo", "”"), ("ldquo", "“"), ("laquo", "«"), ("raquo", "»"), ("copy", "©"),
    ("reg", "®"), ("trade", "™"), ("middot", "·"), ("bull", "•"), ("times", "×"), ("divide", "÷"), ("euro", "€"), ("pound", "£"),
    ("yen", "¥"), ("cent", "¢"), ("deg", "°"), ("plusmn", "±"), ("frac12", "½"), ("frac14", "¼"), ("frac34", "¾"), ("sect", "§"),
    ("para", "¶"), ("shy", "\u{ad}"), ("zwj", "\u{200d}"), ("zwnj", "\u{200c}"), ("thinsp", "\u{2009}"), ("ensp", "\u{2002}"),
    ("emsp", "\u{2003}"), ("prime", "′"), ("Prime", "″"), ("larr", "←"), ("rarr", "→"), ("uarr", "↑"), ("darr", "↓"), ("hearts", "♥"),
    ("eacute", "é"), ("egrave", "è"), ("aacute", "á"), ("agrave", "à"), ("ouml", "ö"), ("uuml", "ü"), ("auml", "ä"), ("szlig", "ß"),
    ("ccedil", "ç"), ("ntilde", "ñ"), ("iacute", "í"), ("oacute", "ó"), ("uacute", "ú"), ("Eacute", "É"), ("minus", "−"), ("le", "≤"),
    ("ge", "≥"), ("ne", "≠"), ("asymp", "≈"), ("infin", "∞"), ("alpha", "α"), ("beta", "β"), ("pi", "π"), ("mu", "μ"),
];

/// Appends `s` to `out`, decoding character references.
pub fn decode_into(s: &str, out: &mut String) {
    let bytes = s.as_bytes();
    let mut last = 0;
    let mut i = 0;
    while let Some(p) = memchr(b'&', &bytes[i..]) {
        let amp = i + p;
        out.push_str(&s[last..amp]);
        let semi = bytes[amp + 1..bytes.len().min(amp + 34)].iter().position(|&b| b == b';').map(|q| q + amp + 1);
        let mut decoded = false;
        if let Some(semi) = semi {
            let name = &s[amp + 1..semi];
            if let Some(num) = name.strip_prefix('#') {
                let v = if let Some(h) = num.strip_prefix('x').or_else(|| num.strip_prefix('X')) { u32::from_str_radix(h, 16).ok() } else { num.parse().ok() };
                if let Some(c) = v.and_then(|v| char::from_u32(if v == 0 { 0xfffd } else { v })) {
                    out.push(c);
                    decoded = true;
                }
            } else if let Some((_, v)) = ENTITIES.iter().find(|(k, _)| *k == name) {
                out.push_str(v);
                decoded = true;
            }
            if decoded {
                last = semi + 1;
                i = last;
                continue;
            }
        }
        out.push('&');
        last = amp + 1;
        i = last;
    }
    out.push_str(&s[last..]);
}

/// Escapes text for HTML output.
pub fn escape_into(s: &str, out: &mut String, attr: bool) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' if attr => out.push_str("&quot;"),
            '\u{a0}' => out.push_str("&nbsp;"),
            c => out.push(c),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outline(d: &Dom, id: NodeId, out: &mut String) {
        match &d.nodes[id as usize].data {
            Data::Text(t) => out.push_str(t),
            Data::Element { name, .. } => {
                out.push('(');
                out.push_str(name);
                for &c in d.children(id) {
                    out.push(' ');
                    outline(d, c, out);
                }
                out.push(')');
            }
            Data::Document => {
                for &c in d.children(id) {
                    outline(d, c, out);
                }
            }
        }
    }

    fn shape(html: &str) -> String {
        let d = parse(html);
        let mut s = String::new();
        outline(&d, ROOT, &mut s);
        s
    }

    #[test]
    fn implied_end_tags() {
        assert_eq!(shape("<p>a<p>b"), "(p a)(p b)");
        assert_eq!(shape("<ul><li>a<li>b</ul>"), "(ul (li a) (li b))");
        assert_eq!(shape("<p>a<div>b</div>"), "(p a)(div b)");
        assert_eq!(shape("<table><tr><td>1<td>2<tr><td>3</table>"), "(table (tr (td 1) (td 2)) (tr (td 3)))");
        assert_eq!(shape("<b>x</i>y</b>"), "(b xy)");
    }

    #[test]
    fn raw_text_entities_and_attrs() {
        let d = parse(r#"<title>A &amp; B</title><script>if (a<b) {}</script><img src=x.png alt='hi "there"'><a href="/p?a=1&amp;b=2">l</a><!-- <p>no</p> -->&#x1F600;&copy;&bogus;"#);
        assert_eq!(d.text(d.find("title").unwrap()), "A & B");
        assert_eq!(d.attr(d.find("img").unwrap(), "alt"), Some("hi \"there\""));
        assert_eq!(d.attr(d.find("a").unwrap(), "href"), Some("/p?a=1&b=2"));
        assert!(d.find("p").is_none());
        assert_eq!(d.text(ROOT), "A & Bl😀©&bogus;");
    }

    #[test]
    fn garbage_does_not_panic() {
        for s in ["<", "<<>>", "</", "<a", "<a href=", "<a href=\"", "&", "&#", "&#xZZ;", "<p></p></p></div>", "<!doctype", "<!--", "<script>", "é<é>"] {
            let _ = parse(s);
        }
    }
}
