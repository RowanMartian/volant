//! A small, non-validating XML parser with namespaces, for XML documents
//! (XHTML, SVG, feeds) and `DOMParser`. It builds the same [`Dom`] the HTML
//! parser does, with every element's namespace in [`Dom::ns`]. Malformed
//! input is an error, as XML wants; callers show a `<parsererror>`.

use crate::html::{Data, Dom, Node, NodeId, ROOT};

const XML_NS: &str = "http://www.w3.org/XML/1998/namespace";
const XMLNS_NS: &str = "http://www.w3.org/2000/xmlns/";

pub fn parse(text: &str) -> Result<Dom, String> {
    let mut p = Parser { s: text, i: 0, dom: Dom { nodes: vec![Node { parent: ROOT, children: Vec::new(), data: Data::Document }], ns: Default::default() } };
    p.run()?;
    Ok(p.dom)
}

struct Parser<'a> {
    s: &'a str,
    i: usize,
    dom: Dom,
}

fn is_name_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | ':' | '-' | '.' | '\u{B7}') || c as u32 >= 0x80
}

impl<'a> Parser<'a> {
    fn rest(&self) -> &'a str {
        &self.s[self.i..]
    }

    fn err<T>(&self, what: &str) -> Result<T, String> {
        let line = self.s[..self.i].matches('\n').count() + 1;
        Err(format!("{what} at line {line}"))
    }

    fn add(&mut self, parent: NodeId, data: Data) -> NodeId {
        let id = self.dom.nodes.len() as NodeId;
        self.dom.nodes.push(Node { parent, children: Vec::new(), data });
        self.dom.nodes[parent as usize].children.push(id);
        id
    }

    fn skip_ws(&mut self) {
        let n = self.rest().len() - self.rest().trim_start_matches([' ', '\t', '\n', '\r']).len();
        self.i += n;
    }

    fn name(&mut self) -> Result<String, String> {
        let r = self.rest();
        let end = r.find(|c: char| !is_name_char(c)).unwrap_or(r.len());
        if end == 0 || r.starts_with(|c: char| c.is_ascii_digit() || c == '-' || c == '.') {
            return self.err("expected a name");
        }
        self.i += end;
        Ok(r[..end].to_string())
    }

    /// Up to (and past) `end`; what came before it.
    fn until(&mut self, end: &str, what: &str) -> Result<String, String> {
        match self.rest().find(end) {
            Some(k) => {
                let v = self.rest()[..k].to_string();
                self.i += k + end.len();
                Ok(v)
            }
            None => self.err(what),
        }
    }

    fn run(&mut self) -> Result<(), String> {
        if self.rest().starts_with('\u{FEFF}') {
            self.i += 3;
        }
        if self.rest().starts_with("<?xml") && self.rest()[5..].starts_with([' ', '\t', '\n', '\r', '?']) {
            self.until("?>", "an unclosed XML declaration")?;
        }
        // (open element, the namespace bindings it made)
        let mut stack: Vec<(NodeId, String)> = Vec::new();
        let mut scopes: Vec<Vec<(String, String)>> = vec![vec![("xml".into(), XML_NS.into()), ("xmlns".into(), XMLNS_NS.into())]];
        let mut root_seen = false;
        while self.i < self.s.len() {
            let parent = stack.last().map_or(ROOT, |e| e.0);
            let r = self.rest();
            if r.starts_with("<!--") {
                self.i += 4;
                let c = self.until("-->", "an unclosed comment")?;
                self.add(parent, Data::Comment(c));
            } else if r.starts_with("<![CDATA[") {
                if stack.is_empty() {
                    return self.err("CDATA outside the root element");
                }
                self.i += 9;
                let c = self.until("]]>", "an unclosed CDATA section")?;
                self.text(parent, c);
            } else if r.starts_with("<!DOCTYPE") {
                if root_seen || !stack.is_empty() {
                    return self.err("a misplaced doctype");
                }
                self.i += 9;
                self.skip_ws();
                let name = self.name()?;
                self.skip_ws();
                let (mut public_id, mut system_id) = (String::new(), String::new());
                if self.rest().starts_with("PUBLIC") {
                    self.i += 6;
                    self.skip_ws();
                    public_id = self.quoted()?;
                    self.skip_ws();
                    if !self.rest().starts_with(['>', '[']) {
                        system_id = self.quoted()?;
                    }
                } else if self.rest().starts_with("SYSTEM") {
                    self.i += 6;
                    self.skip_ws();
                    system_id = self.quoted()?;
                }
                self.skip_ws();
                if self.rest().starts_with('[') {
                    self.until("]", "an unclosed internal subset")?;
                    self.skip_ws();
                }
                if !self.rest().starts_with('>') {
                    return self.err("an unclosed doctype");
                }
                self.i += 1;
                self.add(parent, Data::Doctype { name: name.into(), public_id, system_id });
            } else if r.starts_with("<?") {
                self.i += 2;
                let target = self.name()?;
                if target.eq_ignore_ascii_case("xml") {
                    return self.err("a misplaced XML declaration");
                }
                let data = self.until("?>", "an unclosed processing instruction")?;
                self.add(parent, Data::Pi { target: target.into(), data: data.trim_start().to_string() });
            } else if r.starts_with("</") {
                self.i += 2;
                let name = self.name()?;
                self.skip_ws();
                if !self.rest().starts_with('>') {
                    return self.err("a malformed end tag");
                }
                self.i += 1;
                match stack.pop() {
                    Some((_, open)) if open == name => {
                        scopes.pop();
                    }
                    _ => return self.err(&format!("an unexpected </{name}>")),
                }
            } else if r.starts_with('<') {
                if stack.is_empty() && root_seen {
                    return self.err("content after the root element");
                }
                self.i += 1;
                let name = self.name()?;
                let mut attrs: Vec<(Box<str>, String)> = Vec::new();
                let mut bindings: Vec<(String, String)> = Vec::new();
                loop {
                    let before = self.i;
                    self.skip_ws();
                    if self.rest().starts_with("/>") || self.rest().starts_with('>') {
                        break;
                    }
                    if self.i == before {
                        return self.err("expected whitespace between attributes");
                    }
                    let k = self.name()?;
                    self.skip_ws();
                    if !self.rest().starts_with('=') {
                        return self.err("an attribute without a value");
                    }
                    self.i += 1;
                    self.skip_ws();
                    let v = self.quoted()?;
                    if v.contains('<') {
                        return self.err("'<' in an attribute value");
                    }
                    let v = unescape(&v).replace(['\t', '\n', '\r'], " ");
                    if attrs.iter().any(|(a, _)| **a == *k) {
                        return self.err("a repeated attribute");
                    }
                    if k == "xmlns" {
                        bindings.push((String::new(), v.clone()));
                    } else if let Some(p) = k.strip_prefix("xmlns:") {
                        bindings.push((p.to_string(), v.clone()));
                    }
                    attrs.push((k.into(), v));
                }
                let empty = self.rest().starts_with("/>");
                self.i += if empty { 2 } else { 1 };
                let mut scope = scopes.last().cloned().unwrap_or_default();
                scope.extend(bindings);
                let (prefix, _) = match name.split_once(':') {
                    Some((p, l)) => (Some(p.to_string()), l),
                    None => (None, name.as_str()),
                };
                let key = prefix.clone().unwrap_or_default();
                let ns = scope.iter().rev().find(|(p, _)| *p == key).map(|(_, u)| u.clone()).filter(|u| !u.is_empty());
                if prefix.is_some() && ns.is_none() {
                    return self.err(&format!("an unbound prefix in <{name}>"));
                }
                let id = self.add(parent, Data::Element { name: name.clone().into(), attrs });
                self.dom.ns.insert(id, (ns.map(Into::into), prefix.map(Into::into)));
                if stack.is_empty() {
                    root_seen = true;
                }
                if !empty {
                    stack.push((id, name));
                    scopes.push(scope);
                }
            } else {
                let end = r.find('<').unwrap_or(r.len());
                let t = &r[..end];
                self.i += end;
                if stack.is_empty() {
                    if !t.trim().is_empty() {
                        return self.err("text outside the root element");
                    }
                } else {
                    let t = unescape(t);
                    self.text(parent, t);
                }
            }
        }
        if let Some((_, open)) = stack.last() {
            return self.err(&format!("an unclosed <{open}>"));
        }
        if !root_seen {
            return self.err("no root element");
        }
        Ok(())
    }

    fn quoted(&mut self) -> Result<String, String> {
        let q = match self.rest().chars().next() {
            Some(c @ ('"' | '\'')) => c,
            _ => return self.err("expected a quoted value"),
        };
        self.i += 1;
        let mut end = String::new();
        end.push(q);
        self.until(&end, "an unclosed quoted value")
    }

    /// Text joins a text node just before it, as the DOM keeps them.
    fn text(&mut self, parent: NodeId, t: String) {
        if t.is_empty() {
            return;
        }
        if let Some(&last) = self.dom.nodes[parent as usize].children.last() {
            if let Data::Text(prev) = &mut self.dom.nodes[last as usize].data {
                prev.push_str(&t);
                return;
            }
        }
        self.add(parent, Data::Text(t));
    }
}

/// XML's five entities and character references (other `&` stays as written).
fn unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.replace("\r\n", "\n");
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(k) = rest.find('&') {
        out.push_str(&rest[..k]);
        rest = &rest[k..];
        let Some(semi) = rest.find(';').filter(|&e| e < 12) else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let ent = &rest[1..semi];
        let c = match ent {
            "lt" => Some('<'),
            "gt" => Some('>'),
            "amp" => Some('&'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => ent
                .strip_prefix("#x")
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| ent.strip_prefix('#').and_then(|d| d.parse().ok()))
                .and_then(char::from_u32),
        };
        match c {
            Some(c) => {
                out.push(c);
                rest = &rest[semi + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out.replace("\r\n", "\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_namespaces_and_rejects_malformed() {
        let d = parse("<?xml version='1.0'?><!DOCTYPE svg><!--c--><svg xmlns='http://www.w3.org/2000/svg' xmlns:x='urn:x'><x:a b=\"1 &amp; 2\"/>t&lt;<![CDATA[<raw>]]></svg>").unwrap();
        let svg = d.children(ROOT)[2];
        assert_eq!(d.tag(svg), Some("svg"));
        assert_eq!(d.ns[&svg].0.as_deref(), Some("http://www.w3.org/2000/svg"));
        let a = d.children(svg)[0];
        assert_eq!(d.ns[&a], (Some("urn:x".into()), Some("x".into())));
        assert_eq!(d.attr(a, "b"), Some("1 & 2"));
        assert_eq!(d.text(svg), "t<<raw>");
        assert!(parse("<a><b></a>").is_err());
        assert!(parse("<a/><b/>").is_err());
        assert!(parse("<p:a/>").is_err());
        assert!(parse("").is_err());
    }
}
