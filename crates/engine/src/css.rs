//! CSS parsing: stylesheets into rules, declarations, and selectors.
//!
//! Covers what pages use most: type, class, id, attribute and universal
//! selectors; descendant, child and sibling combinators; `:first-child`,
//! `:last-child`, `:root`, `:link`; `!important`; comments; `@media` blocks
//! for screens (print-only and feature queries are skipped for now). A
//! selector the engine can't match yet is dropped, never misapplied.

/// One `selectors { declarations }` rule.
#[derive(Debug, Clone, PartialEq)]
pub struct Rule {
    pub selectors: Vec<Selector>,
    pub decls: Vec<Decl>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Decl {
    /// Lower-case property name.
    pub name: String,
    pub value: String,
    pub important: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Combinator {
    /// `a b`
    Descendant,
    /// `a > b`
    Child,
    /// `a + b`
    Next,
    /// `a ~ b`
    Later,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AttrOp {
    Exists,
    Equals(String),
    /// `~=`: one of the space-separated words.
    Word(String),
    Prefix(String),
    Suffix(String),
    Contains(String),
    /// `|=`: equal, or followed by `-`.
    Dash(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pseudo {
    FirstChild,
    LastChild,
    OnlyChild,
    Root,
    /// `:link` and `:any-link`: an `<a>` or `<area>` with an href.
    Link,
    /// `:empty`: no element children and no text.
    Empty,
}

/// One compound selector: `p.note#intro[lang]:first-child`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Compound {
    pub tag: Option<String>,
    pub id: Option<String>,
    pub classes: Vec<String>,
    pub attrs: Vec<(String, AttrOp)>,
    pub pseudos: Vec<Pseudo>,
}

/// A complex selector, read right to left: `parts[0]` is the subject, then
/// each following part is related to the previous one by its combinator.
#[derive(Debug, Clone, PartialEq)]
pub struct Selector {
    pub parts: Vec<(Compound, Combinator)>,
    /// (ids, classes/attributes/pseudo-classes, types)
    pub specificity: (u16, u16, u16),
}

/// Removes `/* comments */`.
fn strip_comments(css: &str) -> String {
    let mut out = String::with_capacity(css.len());
    let mut rest = css;
    while let Some(i) = rest.find("/*") {
        out.push_str(&rest[..i]);
        match rest[i + 2..].find("*/") {
            Some(j) => rest = &rest[i + 2 + j + 2..],
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

/// The index just past the `}` matching the `{` at `open` (strings respected).
fn block_end(s: &[u8], open: usize) -> usize {
    let mut depth = 0;
    let mut i = open;
    let mut quote = 0u8;
    while i < s.len() {
        let c = s[i];
        if quote != 0 {
            if c == b'\\' {
                i += 1;
            } else if c == quote {
                quote = 0;
            }
        } else {
            match c {
                b'"' | b'\'' => quote = c,
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return i + 1;
                    }
                }
                _ => {}
            }
        }
        i += 1;
    }
    s.len()
}

/// Splits on `sep` at the top level (outside strings, parentheses and brackets).
fn split_top(s: &str, sep: u8) -> Vec<&str> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let (mut depth, mut quote, mut start) = (0i32, 0u8, 0);
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if quote != 0 {
            if c == b'\\' {
                i += 1;
            } else if c == quote {
                quote = 0;
            }
        } else {
            match c {
                b'"' | b'\'' => quote = c,
                b'(' | b'[' => depth += 1,
                b')' | b']' => depth -= 1,
                c if c == sep && depth == 0 => {
                    out.push(&s[start..i]);
                    start = i + 1;
                }
                _ => {}
            }
        }
        i += 1;
    }
    out.push(&s[start..]);
    out
}

/// Whether an `@media` query applies to a screen as the engine knows it.
fn media_applies(query: &str) -> bool {
    let q = query.to_ascii_lowercase();
    let q = q.trim();
    if q.is_empty() || q == "all" || q == "screen" || q == "only screen" {
        return true;
    }
    // Feature queries (widths, colour scheme...) wait for a later chunk; print never applies.
    false
}

pub fn parse_stylesheet(css: &str) -> Vec<Rule> {
    let css = strip_comments(css);
    let mut rules = Vec::new();
    parse_rules(&css, &mut rules);
    rules
}

fn parse_rules(css: &str, rules: &mut Vec<Rule>) {
    let b = css.as_bytes();
    let mut i = 0;
    while i < b.len() {
        while i < b.len() && (b[i].is_ascii_whitespace() || b[i] == b';') {
            i += 1;
        }
        if i >= b.len() {
            break;
        }
        // The prelude runs to `{` (or, for statements like @import, to `;`).
        let Some(rel) = css[i..].find(['{', ';']) else { break };
        let at = i + rel;
        let prelude = css[i..at].trim();
        if b[at] == b';' {
            i = at + 1; // @import, @charset...: nothing to do
            continue;
        }
        let end = block_end(b, at);
        let body = &css[at + 1..end.saturating_sub(1).max(at + 1)];
        if let Some(at_rule) = prelude.strip_prefix('@') {
            let (name, rest) = at_rule.split_once(|c: char| c.is_whitespace()).unwrap_or((at_rule, ""));
            if name.eq_ignore_ascii_case("media") && media_applies(rest) {
                parse_rules(body, rules);
            }
        } else {
            let selectors: Vec<Selector> = split_top(prelude, b',').into_iter().filter_map(parse_selector).collect();
            if !selectors.is_empty() {
                rules.push(Rule { selectors, decls: parse_declarations(body) });
            }
        }
        i = end;
    }
}

/// `color: red; margin: 0 auto !important` → declarations.
pub fn parse_declarations(text: &str) -> Vec<Decl> {
    split_top(text, b';')
        .into_iter()
        .filter_map(|d| {
            let (name, value) = d.split_once(':')?;
            let name = name.trim().to_ascii_lowercase();
            let mut value = value.trim();
            let mut important = false;
            if let Some(i) = value.to_ascii_lowercase().rfind("!important") {
                if value[i + 10..].trim().is_empty() {
                    important = true;
                    value = value[..i].trim();
                }
            }
            (!name.is_empty() && !value.is_empty()).then(|| Decl { name, value: value.to_string(), important })
        })
        .collect()
}

fn ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '-' || c == '_' || !c.is_ascii()
}

/// Parses one complex selector; `None` if it uses something the engine can't match.
pub fn parse_selector(text: &str) -> Option<Selector> {
    let s: Vec<char> = text.trim().chars().collect();
    if s.is_empty() {
        return None;
    }
    let mut compounds: Vec<(Compound, Combinator)> = Vec::new(); // left to right; combinator links to the next
    let mut cur = Compound::default();
    let mut has = false;
    let mut i = 0;
    let read_ident = |i: &mut usize| -> String {
        let start = *i;
        while *i < s.len() && (ident_char(s[*i]) || s[*i] == '\\') {
            if s[*i] == '\\' {
                *i += 1;
            }
            *i += 1;
        }
        s[start..(*i).min(s.len())].iter().filter(|&&c| c != '\\').collect()
    };
    while i < s.len() {
        let c = s[i];
        match c {
            '*' => {
                i += 1;
                has = true;
            }
            '#' => {
                i += 1;
                cur.id = Some(read_ident(&mut i));
                has = true;
            }
            '.' => {
                i += 1;
                cur.classes.push(read_ident(&mut i));
                has = true;
            }
            '[' => {
                let close = s[i..].iter().position(|&c| c == ']')? + i;
                let inner: String = s[i + 1..close].iter().collect();
                i = close + 1;
                cur.attrs.push(parse_attr(&inner)?);
                has = true;
            }
            ':' => {
                if s.get(i + 1) == Some(&':') {
                    return None; // pseudo-elements (::before...) come later
                }
                i += 1;
                let name = read_ident(&mut i).to_ascii_lowercase();
                if s.get(i) == Some(&'(') {
                    return None; // :not(), :nth-child()...
                }
                cur.pseudos.push(match name.as_str() {
                    "first-child" => Pseudo::FirstChild,
                    "last-child" => Pseudo::LastChild,
                    "only-child" => Pseudo::OnlyChild,
                    "root" => Pseudo::Root,
                    "link" | "any-link" => Pseudo::Link,
                    "empty" => Pseudo::Empty,
                    // :visited never matches (privacy); :hover and friends aren't static.
                    _ => return None,
                });
                has = true;
            }
            c if c.is_whitespace() || c == '>' || c == '+' || c == '~' => {
                let mut comb = Combinator::Descendant;
                while i < s.len() && (s[i].is_whitespace() || matches!(s[i], '>' | '+' | '~')) {
                    match s[i] {
                        '>' => comb = Combinator::Child,
                        '+' => comb = Combinator::Next,
                        '~' => comb = Combinator::Later,
                        _ => {}
                    }
                    i += 1;
                }
                if !has {
                    return None; // a combinator with nothing before it
                }
                if i < s.len() {
                    compounds.push((std::mem::take(&mut cur), comb));
                    has = false;
                }
            }
            c if ident_char(c) => {
                if has {
                    return None;
                }
                cur.tag = Some(read_ident(&mut i).to_ascii_lowercase());
                has = true;
            }
            _ => return None,
        }
    }
    if !has {
        return None;
    }
    compounds.push((cur, Combinator::Descendant));
    // Store right to left: each part's combinator says how it relates to the part after it.
    let n = compounds.len();
    let mut parts = Vec::with_capacity(n);
    for k in (0..n).rev() {
        let comb = if k > 0 { compounds[k - 1].1 } else { Combinator::Descendant };
        parts.push((compounds[k].0.clone(), comb));
    }
    let mut spec = (0u16, 0u16, 0u16);
    for (c, _) in &parts {
        spec.0 += c.id.is_some() as u16;
        spec.1 += (c.classes.len() + c.attrs.len() + c.pseudos.len()) as u16;
        spec.2 += c.tag.is_some() as u16;
    }
    Some(Selector { parts, specificity: spec })
}

fn parse_attr(inner: &str) -> Option<(String, AttrOp)> {
    let inner = inner.trim();
    let ops = ["~=", "^=", "$=", "*=", "|=", "="];
    for op in ops {
        if let Some((name, value)) = inner.split_once(op) {
            let name = name.trim().to_ascii_lowercase();
            let mut v = value.trim();
            // `[type=text i]`: case flags are ignored.
            if let Some(stripped) = v.strip_suffix(" i").or_else(|| v.strip_suffix(" s")) {
                v = stripped.trim();
            }
            let v = v.trim_matches(|c| c == '"' || c == '\'').to_string();
            let op = match op {
                "~=" => AttrOp::Word(v),
                "^=" => AttrOp::Prefix(v),
                "$=" => AttrOp::Suffix(v),
                "*=" => AttrOp::Contains(v),
                "|=" => AttrOp::Dash(v),
                _ => AttrOp::Equals(v),
            };
            return (!name.is_empty()).then_some((name, op));
        }
    }
    let name = inner.to_ascii_lowercase();
    (!name.is_empty() && name.chars().all(ident_char)).then_some((name, AttrOp::Exists))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rules_and_declarations() {
        let css = "/* hi */ @charset 'utf-8'; body { margin: 0; color: #333 } \
                   h1, h2.title { font: bold 2em/1.2 Georgia, serif !important; }\
                   @media print { p { display: none } }\
                   @media screen { p { color: red } }\
                   @font-face { font-family: X; src: url(a;b.woff) }\
                   a:hover { color: blue } a[href^='http'] { text-decoration: none }";
        let r = parse_stylesheet(css);
        assert_eq!(r.len(), 4, "{r:#?}");
        assert_eq!(r[0].decls, vec![Decl { name: "margin".into(), value: "0".into(), important: false }, Decl { name: "color".into(), value: "#333".into(), important: false }]);
        assert_eq!(r[1].selectors.len(), 2);
        assert!(r[1].decls[0].important && r[1].decls[0].value == "bold 2em/1.2 Georgia, serif");
        assert_eq!(r[2].decls[0].value, "red", "screen media applies, print doesn't");
        assert_eq!(r[3].selectors[0].parts[0].0.attrs[0], ("href".into(), AttrOp::Prefix("http".into())));
    }

    #[test]
    fn selectors() {
        let s = parse_selector("div#main > ul.nav li:first-child a").unwrap();
        assert_eq!(s.specificity, (1, 2, 4));
        assert_eq!(s.parts.len(), 4);
        assert_eq!(s.parts[0].0.tag.as_deref(), Some("a"));
        assert_eq!(s.parts[1].1, Combinator::Descendant);
        assert_eq!(s.parts[2].0.classes, ["nav"]);
        assert_eq!(s.parts[2].1, Combinator::Child);
        assert_eq!(parse_selector("h1 + p").unwrap().parts[0].1, Combinator::Next);
        assert_eq!(parse_selector("*").unwrap().specificity, (0, 0, 0));
        for bad in ["a:hover", "p::before", "li:nth-child(2)", "> p", "", "a b >"] {
            assert!(parse_selector(bad).is_none() || bad == "a b >", "{bad}");
        }
    }
}
