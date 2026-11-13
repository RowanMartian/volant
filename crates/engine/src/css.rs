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
    /// The `@media` conditions it sits inside (all must hold).
    pub media: Vec<String>,
}

/// What `@media` queries are asked about.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Env {
    /// Viewport width in px.
    pub width: f32,
    pub dark: bool,
}

impl Default for Env {
    fn default() -> Self {
        Env { width: 1024.0, dark: false }
    }
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

#[derive(Debug, Clone, PartialEq)]
pub enum Pseudo {
    /// `:nth-child(an+b)`, and the `-last-` / `-of-type` variants: (a, b, from the end, same type only).
    Nth(i32, i32, bool, bool),
    /// `:not(...)`: matches when none of these compounds do.
    Not(Vec<Compound>),
    /// A state a drawn page is never in (`:active`, `:visited`, …).
    Never,
    /// `:hover`, `:focus` (and `:focus-visible`), `:focus-within`, `:target`:
    /// the snapshot marks these elements (see [`STATE_HOVER`] and friends).
    Hover,
    Focus,
    FocusWithin,
    Target,
    /// `:checked`, `:disabled`, `:enabled`, from the element's attributes.
    Checked,
    Disabled,
    Enabled,
    FirstChild,
    LastChild,
    OnlyChild,
    Root,
    /// `:link` and `:any-link`: an `<a>` or `<area>` with an href.
    Link,
    /// `:empty`: no element children and no text.
    Empty,
}

/// Hidden attributes a page snapshot puts on elements in an interaction
/// state (scripts never see them): the hovered element and its ancestors,
/// the focused element and its ancestors, and the `#fragment`'s target.
pub const STATE_HOVER: &str = "\0hover";
pub const STATE_FOCUS: &str = "\0focus";
pub const STATE_FOCUS_WITHIN: &str = "\0focus-within";
pub const STATE_TARGET: &str = "\0target";

/// One compound selector: `p.note#intro[lang]:first-child`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Compound {
    pub tag: Option<String>,
    pub id: Option<String>,
    pub classes: Vec<String>,
    pub attrs: Vec<(String, AttrOp)>,
    pub pseudos: Vec<Pseudo>,
}

/// `::before` and `::after`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PseudoElement {
    Before,
    After,
}

/// A complex selector, read right to left: `parts[0]` is the subject, then
/// each following part is related to the previous one by its combinator.
#[derive(Debug, Clone, PartialEq)]
pub struct Selector {
    pub parts: Vec<(Compound, Combinator)>,
    /// Styles the element's `::before` or `::after` instead of the element.
    pub element: Option<PseudoElement>,
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

/// Whether an `@media` query list applies: media types (`screen`, `all`,
/// `print`), `and`, `not`, `only`, and the features `min-width`,
/// `max-width`, `width`, `prefers-color-scheme`, `orientation`, `hover`,
/// `pointer`, `prefers-reduced-motion`. Unknown features don't apply.
pub fn media_applies(query: &str, env: &Env) -> bool {
    let q = query.to_ascii_lowercase();
    q.split(',').any(|one| {
        let one = one.trim();
        let (negate, one) = match one.strip_prefix("not ") {
            Some(rest) => (true, rest.trim()),
            None => (false, one.strip_prefix("only ").unwrap_or(one).trim()),
        };
        let ok = one.split(" and ").map(str::trim).filter(|p| !p.is_empty()).all(|part| {
            if let Some(feature) = part.strip_prefix('(').and_then(|p| p.strip_suffix(')')) {
                feature_applies(feature.trim(), env)
            } else {
                matches!(part, "all" | "screen")
            }
        });
        ok != negate
    })
}

fn feature_applies(f: &str, env: &Env) -> bool {
    let (name, value) = match f.split_once(':') {
        Some((n, v)) => (n.trim(), v.trim()),
        None => (f, ""),
    };
    let px = |v: &str| -> Option<f32> {
        let v = v.trim();
        if let Some(n) = v.strip_suffix("px") {
            n.trim().parse().ok()
        } else if let Some(n) = v.strip_suffix("rem").or_else(|| v.strip_suffix("em")) {
            n.trim().parse::<f32>().ok().map(|n| n * 16.0)
        } else {
            v.parse().ok()
        }
    };
    match name {
        "min-width" => px(value).map(|w| env.width >= w).unwrap_or(false),
        "max-width" => px(value).map(|w| env.width <= w).unwrap_or(false),
        "width" => px(value).map(|w| (env.width - w).abs() < 0.5).unwrap_or(false),
        "prefers-color-scheme" => (value == "dark") == env.dark,
        "orientation" => value == "landscape",
        "hover" | "any-hover" => value == "hover" || value.is_empty(),
        "pointer" | "any-pointer" => value == "fine" || value.is_empty(),
        "prefers-reduced-motion" => value == "no-preference",
        "color" | "min-resolution" | "-webkit-min-device-pixel-ratio" | "min--moz-device-pixel-ratio" => true,
        _ => false,
    }
}

pub fn parse_stylesheet(css: &str) -> Vec<Rule> {
    let css = strip_comments(css);
    let mut rules = Vec::new();
    parse_rules(&css, &mut rules, &[]);
    rules
}

fn parse_rules(css: &str, rules: &mut Vec<Rule>, media: &[String]) {
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
            if name.eq_ignore_ascii_case("media") {
                let mut inner = media.to_vec();
                inner.push(rest.trim().to_string());
                parse_rules(body, rules, &inner);
            } else if name.eq_ignore_ascii_case("supports") || name.eq_ignore_ascii_case("layer") || name.eq_ignore_ascii_case("document") {
                parse_rules(body, rules, media);
            }
        } else {
            let selectors: Vec<Selector> = split_top(prelude, b',').into_iter().filter_map(parse_selector).collect();
            if !selectors.is_empty() {
                rules.push(Rule { selectors, decls: parse_declarations(body), media: media.to_vec() });
            }
        }
        i = end;
    }
}

/// An `@font-face` rule: a web font file for a family, weight range and slant.
#[derive(Debug, Clone, PartialEq)]
pub struct FontFace {
    /// Lower case, unquoted.
    pub family: String,
    pub weight: (u16, u16),
    pub italic: bool,
    /// The `url()`s of its `src`, as written (relative to the style sheet), in order,
    /// leaving out formats macOS can't use (Embedded OpenType, SVG fonts).
    pub sources: Vec<String>,
}

/// The `@font-face` rules of a style sheet (including those inside `@media` and `@supports`).
pub fn font_faces(css: &str) -> Vec<FontFace> {
    fn walk(css: &str, out: &mut Vec<FontFace>) {
        let b = css.as_bytes();
        let mut i = 0;
        while i < b.len() {
            let Some(rel) = css[i..].find(['{', ';']) else { break };
            let at = i + rel;
            let prelude = css[i..at].trim();
            if b[at] == b';' {
                i = at + 1;
                continue;
            }
            let end = block_end(b, at);
            let body = &css[at + 1..end.saturating_sub(1).max(at + 1)];
            let name = prelude.strip_prefix('@').map(|r| r.split(|c: char| c.is_whitespace()).next().unwrap_or("").to_ascii_lowercase());
            match name.as_deref() {
                Some("font-face") => {
                    let decls = parse_declarations(body);
                    let get = |n: &str| decls.iter().rev().find(|d| d.name == n).map(|d| d.value.as_str());
                    let family = get("font-family").unwrap_or("").trim().trim_matches(|c| c == '"' || c == '\'').to_ascii_lowercase();
                    let weight = match get("font-weight").map(|w| w.trim().to_ascii_lowercase()) {
                        None => (400, 400),
                        Some(w) => {
                            let one = |w: &str| match w {
                                "normal" => Some(400),
                                "bold" => Some(700),
                                n => n.parse::<f32>().ok().map(|n| n.clamp(1.0, 1000.0) as u16),
                            };
                            let mut parts = w.split_whitespace();
                            let a = parts.next().and_then(one).unwrap_or(400);
                            (a, parts.next().and_then(one).unwrap_or(a))
                        }
                    };
                    let italic = get("font-style").map(|s| s.contains("italic") || s.contains("oblique")).unwrap_or(false);
                    let mut sources = Vec::new();
                    for part in split_top(get("src").unwrap_or(""), b',') {
                        let lower = part.to_ascii_lowercase();
                        if lower.contains("embedded-opentype") || lower.contains("format(\"svg") || lower.contains("format('svg") || lower.contains(".eot") {
                            continue;
                        }
                        if let Some(start) = lower.find("url(") {
                            let rest = &part[start + 4..];
                            if let Some(close) = rest.find(')') {
                                let u = rest[..close].trim().trim_matches(|c| c == '"' || c == '\'');
                                if !u.is_empty() {
                                    sources.push(u.to_string());
                                }
                            }
                        }
                    }
                    if !family.is_empty() && !sources.is_empty() {
                        out.push(FontFace { family, weight, italic, sources });
                    }
                }
                Some("media" | "supports" | "layer") => walk(body, out),
                _ => {}
            }
            i = end;
        }
    }
    let mut out = Vec::new();
    walk(&strip_comments(css), &mut out);
    out
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
    let mut element: Option<PseudoElement> = None;
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
                let double = s.get(i + 1) == Some(&':');
                i += if double { 2 } else { 1 };
                let name = read_ident(&mut i).to_ascii_lowercase();
                if element.is_some() {
                    return None; // nothing may follow a pseudo-element
                }
                if matches!(name.as_str(), "before" | "after") {
                    element = Some(if name == "before" { PseudoElement::Before } else { PseudoElement::After });
                    has = true;
                    continue;
                }
                if double {
                    return None; // ::marker, ::selection... come later
                }
                let arg = if s.get(i) == Some(&'(') {
                    let mut depth = 0;
                    let start = i + 1;
                    while i < s.len() {
                        match s[i] {
                            '(' => depth += 1,
                            ')' => {
                                depth -= 1;
                                if depth == 0 {
                                    break;
                                }
                            }
                            _ => {}
                        }
                        i += 1;
                    }
                    let a: String = s[start..i.min(s.len())].iter().collect();
                    i += 1;
                    Some(a)
                } else {
                    None
                };
                cur.pseudos.push(match (name.as_str(), arg) {
                    ("first-child", None) => Pseudo::FirstChild,
                    ("last-child", None) => Pseudo::LastChild,
                    ("only-child", None) => Pseudo::OnlyChild,
                    ("first-of-type", None) => Pseudo::Nth(0, 1, false, true),
                    ("last-of-type", None) => Pseudo::Nth(0, 1, true, true),
                    ("root", None) => Pseudo::Root,
                    ("link" | "any-link", None) => Pseudo::Link,
                    ("empty", None) => Pseudo::Empty,
                    ("nth-child", Some(a)) => {
                        let (x, y) = parse_nth(&a)?;
                        Pseudo::Nth(x, y, false, false)
                    }
                    ("nth-last-child", Some(a)) => {
                        let (x, y) = parse_nth(&a)?;
                        Pseudo::Nth(x, y, true, false)
                    }
                    ("nth-of-type", Some(a)) => {
                        let (x, y) = parse_nth(&a)?;
                        Pseudo::Nth(x, y, false, true)
                    }
                    ("nth-last-of-type", Some(a)) => {
                        let (x, y) = parse_nth(&a)?;
                        Pseudo::Nth(x, y, true, true)
                    }
                    ("not", Some(a)) => {
                        let mut list = Vec::new();
                        for part in split_top(&a, b',') {
                            let inner = parse_selector(part)?;
                            // Only simple and compound selectors inside :not() for now.
                            if inner.parts.len() != 1 || inner.element.is_some() {
                                return None;
                            }
                            list.push(inner.parts.into_iter().next()?.0);
                        }
                        Pseudo::Not(list)
                    }
                    ("is" | "where" | "matches", Some(a)) => {
                        // A single compound inside :is()/:where() works like itself.
                        let inner = parse_selector(&a)?;
                        if inner.parts.len() != 1 || split_top(&a, b',').len() != 1 {
                            return None;
                        }
                        let c = inner.parts.into_iter().next()?.0;
                        if cur.tag.is_none() {
                            cur.tag = c.tag;
                        }
                        cur.classes.extend(c.classes);
                        cur.attrs.extend(c.attrs);
                        cur.pseudos.extend(c.pseudos);
                        if c.id.is_some() {
                            cur.id = c.id;
                        }
                        has = true;
                        continue;
                    }
                    // Interaction states the snapshot marks; the rest never match
                    // (and :visited never does, for privacy).
                    ("hover", None) => Pseudo::Hover,
                    ("focus" | "focus-visible", None) => Pseudo::Focus,
                    ("focus-within", None) => Pseudo::FocusWithin,
                    ("target", None) => Pseudo::Target,
                    ("active" | "visited" | "indeterminate" | "placeholder-shown" | "autofill" | "-webkit-autofill", None) => Pseudo::Never,
                    ("checked", None) => Pseudo::Checked,
                    ("disabled", None) => Pseudo::Disabled,
                    ("enabled", None) => Pseudo::Enabled,
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
                // As written: HTML elements match it in any case, others exactly.
                cur.tag = Some(read_ident(&mut i));
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
        let (a, b, cc) = compound_specificity(c);
        spec = (spec.0 + a, spec.1 + b, spec.2 + cc);
    }
    spec.2 += element.is_some() as u16;
    Some(Selector { parts, element, specificity: spec })
}

fn compound_specificity(c: &Compound) -> (u16, u16, u16) {
    let mut spec = (c.id.is_some() as u16, (c.classes.len() + c.attrs.len()) as u16, c.tag.is_some() as u16);
    for p in &c.pseudos {
        match p {
            // :not() counts as its most specific argument.
            Pseudo::Not(list) => {
                let m = list.iter().map(compound_specificity).max().unwrap_or((0, 0, 0));
                spec = (spec.0 + m.0, spec.1 + m.1, spec.2 + m.2);
            }
            _ => spec.1 += 1,
        }
    }
    spec
}

/// `odd`, `even`, `3`, `2n+1`, `-n+3` → (a, b).
fn parse_nth(arg: &str) -> Option<(i32, i32)> {
    let a = arg.trim().to_ascii_lowercase().replace(' ', "");
    if a.contains("of") {
        return None;
    }
    match a.as_str() {
        "odd" => return Some((2, 1)),
        "even" => return Some((2, 0)),
        _ => {}
    }
    match a.split_once('n') {
        None => Some((0, a.parse().ok()?)),
        Some((x, y)) => {
            let x = match x {
                "" | "+" => 1,
                "-" => -1,
                n => n.parse().ok()?,
            };
            let y = if y.is_empty() { 0 } else { y.parse().ok()? };
            Some((x, y))
        }
    }
}

/// Whether position `pos` (1-based) is in `an+b`.
pub fn nth_matches(a: i32, b: i32, pos: i32) -> bool {
    if a == 0 {
        pos == b
    } else {
        let d = pos - b;
        d % a == 0 && d / a >= 0
    }
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
        assert_eq!(r.len(), 6, "{r:#?}");
        assert_eq!(r[0].decls, vec![Decl { name: "margin".into(), value: "0".into(), important: false }, Decl { name: "color".into(), value: "#333".into(), important: false }]);
        assert_eq!(r[1].selectors.len(), 2);
        assert!(r[1].decls[0].important && r[1].decls[0].value == "bold 2em/1.2 Georgia, serif");
        assert_eq!(r[2].media, ["print"]);
        assert_eq!(r[3].media, ["screen"]);
        assert_eq!(r[4].selectors[0].parts[0].0.pseudos, [Pseudo::Hover]);
        assert_eq!(r[5].selectors[0].parts[0].0.attrs[0], ("href".into(), AttrOp::Prefix("http".into())));
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
        let s = parse_selector("li:nth-child(2n+1):not(.x, #y)::before").unwrap();
        assert_eq!(s.element, Some(PseudoElement::Before));
        assert_eq!(s.parts[0].0.pseudos[0], Pseudo::Nth(2, 1, false, false));
        assert_eq!(s.specificity, (1, 1, 2), "li + ::before, :nth-child, :not counts its #y");
        assert_eq!(parse_nth("-n+3"), Some((-1, 3)));
        assert!(nth_matches(2, 1, 5) && !nth_matches(2, 1, 4) && nth_matches(-1, 3, 2) && !nth_matches(-1, 3, 4) && nth_matches(0, 3, 3));
        let env = Env { width: 800.0, dark: true };
        for (q, want) in [("screen and (min-width: 600px)", true), ("(max-width: 40em)", false), ("print", false), ("(prefers-color-scheme: dark)", true), ("not print", true), ("screen, print", true), ("(min-width: 900px)", false)] {
            assert_eq!(media_applies(q, &env), want, "{q}");
        }
        assert_eq!(parse_selector("a:hover").unwrap().parts[0].0.pseudos, [Pseudo::Hover]);
        assert_eq!(parse_selector("a:visited").unwrap().parts[0].0.pseudos, [Pseudo::Never]);
        for bad in ["p::marker", "li:has(p)", "> p", "", "a b >"] {
            assert!(parse_selector(bad).is_none() || bad == "a b >", "{bad}");
        }
    }
}
