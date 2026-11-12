//! Computed styles: selector matching against the DOM, the cascade (user
//! agent sheet, author sheets, inline `style`, `!important`, specificity,
//! order), inheritance, and turning property values into numbers.

use crate::css::{self, AttrOp, Combinator, Compound, Decl, Pseudo, Rule};
use volant_core::html::{Data, Dom, NodeId};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const BLACK: Color = Color { r: 0, g: 0, b: 0, a: 255 };
    pub const TRANSPARENT: Color = Color { r: 0, g: 0, b: 0, a: 0 };
    pub fn rgb(r: u8, g: u8, b: u8) -> Color {
        Color { r, g, b, a: 255 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Len {
    Px(f32),
    /// Percent of the containing block's width.
    Pct(f32),
    Auto,
}

impl Len {
    pub fn resolve(self, of: f32) -> f32 {
        match self {
            Len::Px(v) => v,
            Len::Pct(p) => of * p / 100.0,
            Len::Auto => 0.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Display {
    Block,
    Inline,
    ListItem,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Family {
    Sans,
    Serif,
    Mono,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Space {
    Normal,
    /// Keep spaces and line breaks, don't wrap (`pre`).
    Pre,
    /// Keep spaces and line breaks, wrap (`pre-wrap`).
    PreWrap,
    NoWrap,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListStyle {
    Disc,
    Circle,
    Square,
    Decimal,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transform {
    None,
    Upper,
    Lower,
    Capitalize,
}

/// Every property the engine lays out with, resolved to numbers where it can be.
#[derive(Debug, Clone, PartialEq)]
pub struct Style {
    pub display: Display,
    pub color: Color,
    pub background: Color,
    pub font_size: f32,
    pub weight: u16,
    pub italic: bool,
    pub family: Family,
    /// Line height in px.
    pub line_height: f32,
    pub align: Align,
    pub space: Space,
    pub list: ListStyle,
    pub transform: Transform,
    pub underline: bool,
    pub strike: bool,
    /// top, right, bottom, left
    pub margin: [Len; 4],
    pub padding: [Len; 4],
    pub border_width: [f32; 4],
    pub border_color: [Color; 4],
    pub width: Len,
    pub max_width: Len,
    pub hidden: bool,
}

impl Default for Style {
    fn default() -> Self {
        Style {
            display: Display::Inline,
            color: Color::BLACK,
            background: Color::TRANSPARENT,
            font_size: 16.0,
            weight: 400,
            italic: false,
            family: Family::Sans,
            line_height: 16.0 * 1.25,
            align: Align::Left,
            space: Space::Normal,
            list: ListStyle::Disc,
            transform: Transform::None,
            underline: false,
            strike: false,
            margin: [Len::Px(0.0); 4],
            padding: [Len::Px(0.0); 4],
            border_width: [0.0; 4],
            border_color: [Color::BLACK; 4],
            width: Len::Auto,
            max_width: Len::Auto,
            hidden: false,
        }
    }
}

/// The user-agent stylesheet: HTML's defaults, the parts the engine draws.
pub const UA: &str = r#"
html, address, blockquote, body, center, dialog, div, figure, figcaption, footer, form, header, hr, legend, listing,
main, p, plaintext, pre, xmp, search, details, summary, dl, dt, dd, ol, ul, menu, dir, h1, h2, h3, h4, h5, h6, hgroup,
article, aside, nav, section, fieldset, table, caption, thead, tbody, tfoot, tr, td, th, picture, video, canvas { display: block }
li { display: list-item }
head, script, style, title, meta, link, base, template, noscript, area, datalist, param, source, track, [hidden],
img, svg, iframe, object, embed, audio, input, button, select, textarea, map { display: none }
body { margin: 8px }
p, dl, multicol { margin: 1em 0 }
blockquote, figure { margin: 1em 40px }
ul, ol, menu, dir { margin: 1em 0; padding-left: 40px }
ul ul, ol ul, ul ol, ol ol { margin-top: 0; margin-bottom: 0 }
ol { list-style-type: decimal }
dd { margin-left: 40px }
h1 { font-size: 2em; margin: .67em 0; font-weight: bold }
h2 { font-size: 1.5em; margin: .83em 0; font-weight: bold }
h3 { font-size: 1.17em; margin: 1em 0; font-weight: bold }
h4 { margin: 1.33em 0; font-weight: bold }
h5 { font-size: .83em; margin: 1.67em 0; font-weight: bold }
h6 { font-size: .67em; margin: 2.33em 0; font-weight: bold }
pre, listing, xmp, plaintext { white-space: pre; margin: 1em 0 }
code, kbd, samp, pre, tt, listing, xmp, plaintext { font-family: monospace }
b, strong, th { font-weight: bold }
i, em, cite, var, dfn, address { font-style: italic }
u, ins { text-decoration: underline }
s, strike, del { text-decoration: line-through }
small, sub, sup { font-size: smaller }
big { font-size: larger }
center { text-align: center }
a:link { color: #0000ee; text-decoration: underline }
hr { border-top: 1px solid #888; margin: .5em 0 }
mark { background-color: yellow; color: black }
"#;

pub fn parse_color(v: &str) -> Option<Color> {
    let v = v.trim().to_ascii_lowercase();
    if let Some(hex) = v.strip_prefix('#') {
        let d: Vec<u8> = hex.chars().map(|c| c.to_digit(16).map(|d| d as u8)).collect::<Option<_>>()?;
        return match d.len() {
            3 | 4 => Some(Color { r: d[0] * 17, g: d[1] * 17, b: d[2] * 17, a: d.get(3).map(|a| a * 17).unwrap_or(255) }),
            6 | 8 => Some(Color { r: d[0] << 4 | d[1], g: d[2] << 4 | d[3], b: d[4] << 4 | d[5], a: if d.len() == 8 { d[6] << 4 | d[7] } else { 255 } }),
            _ => None,
        };
    }
    if let Some(args) = v.strip_prefix("rgba(").or_else(|| v.strip_prefix("rgb(")).and_then(|a| a.strip_suffix(')')) {
        let parts: Vec<&str> = args.split([',', ' ', '/']).map(str::trim).filter(|p| !p.is_empty()).collect();
        let chan = |p: &str| -> Option<u8> {
            Some(match p.strip_suffix('%') {
                Some(n) => (n.parse::<f32>().ok()? * 2.55).round().clamp(0.0, 255.0) as u8,
                None => p.parse::<f32>().ok()?.round().clamp(0.0, 255.0) as u8,
            })
        };
        let alpha = |p: &str| -> Option<u8> {
            Some(match p.strip_suffix('%') {
                Some(n) => (n.parse::<f32>().ok()? * 2.55).round().clamp(0.0, 255.0) as u8,
                None => (p.parse::<f32>().ok()? * 255.0).round().clamp(0.0, 255.0) as u8,
            })
        };
        if parts.len() < 3 {
            return None;
        }
        return Some(Color { r: chan(parts[0])?, g: chan(parts[1])?, b: chan(parts[2])?, a: parts.get(3).map(|a| alpha(a)).unwrap_or(Some(255))? });
    }
    let named = |r, g, b| Some(Color::rgb(r, g, b));
    match v.as_str() {
        "transparent" => Some(Color::TRANSPARENT),
        "black" => named(0, 0, 0),
        "white" => named(255, 255, 255),
        "red" => named(255, 0, 0),
        "green" => named(0, 128, 0),
        "blue" => named(0, 0, 255),
        "gray" | "grey" => named(128, 128, 128),
        "darkgray" | "darkgrey" => named(169, 169, 169),
        "lightgray" | "lightgrey" => named(211, 211, 211),
        "silver" => named(192, 192, 192),
        "maroon" => named(128, 0, 0),
        "purple" => named(128, 0, 128),
        "fuchsia" | "magenta" => named(255, 0, 255),
        "lime" => named(0, 255, 0),
        "olive" => named(128, 128, 0),
        "yellow" => named(255, 255, 0),
        "navy" => named(0, 0, 128),
        "teal" => named(0, 128, 128),
        "aqua" | "cyan" => named(0, 255, 255),
        "orange" => named(255, 165, 0),
        "brown" => named(165, 42, 42),
        "pink" => named(255, 192, 203),
        "whitesmoke" => named(245, 245, 245),
        "gainsboro" => named(220, 220, 220),
        "dimgray" | "dimgrey" => named(105, 105, 105),
        _ => None,
    }
}

/// A length in px (or %/auto), with `em` relative to `em` and `rem` to `rem`.
pub fn parse_len(v: &str, em: f32, rem: f32) -> Option<Len> {
    let v = v.trim().to_ascii_lowercase();
    if v == "auto" {
        return Some(Len::Auto);
    }
    if v == "0" {
        return Some(Len::Px(0.0));
    }
    let split = v.find(|c: char| c.is_ascii_alphabetic() || c == '%').unwrap_or(v.len());
    let n: f32 = v[..split].parse().ok()?;
    Some(match &v[split..] {
        "px" => Len::Px(n),
        "em" => Len::Px(n * em),
        "rem" => Len::Px(n * rem),
        "%" => Len::Pct(n),
        "pt" => Len::Px(n * 4.0 / 3.0),
        "pc" => Len::Px(n * 16.0),
        "in" => Len::Px(n * 96.0),
        "cm" => Len::Px(n * 96.0 / 2.54),
        "mm" => Len::Px(n * 96.0 / 25.4),
        "ch" | "ex" => Len::Px(n * em * 0.5),
        "vw" | "vh" => return None,
        "" => return None, // unitless non-zero lengths are invalid
        _ => return None,
    })
}

fn family_of(v: &str) -> Option<Family> {
    for f in css_split_commas(v) {
        let f = f.trim().trim_matches(|c| c == '"' || c == '\'').to_ascii_lowercase();
        if f.contains("mono") || ["courier", "courier new", "menlo", "monaco", "consolas", "sf mono", "ui-monospace"].contains(&f.as_str()) {
            return Some(Family::Mono);
        }
        if f == "serif" || f == "ui-serif" || ["georgia", "times", "times new roman", "new york", "charter", "palatino", "garamond", "iowan old style"].contains(&f.as_str()) {
            return Some(Family::Serif);
        }
        if f == "sans-serif" || f == "system-ui" || f == "ui-sans-serif" || f == "-apple-system" || f == "blinkmacsystemfont" {
            return Some(Family::Sans);
        }
        if ["helvetica", "helvetica neue", "arial", "verdana", "roboto", "inter", "segoe ui", "sf pro text"].contains(&f.as_str()) {
            return Some(Family::Sans);
        }
    }
    None
}

fn css_split_commas(v: &str) -> Vec<&str> {
    v.split(',').collect()
}

fn font_size(v: &str, parent: f32, rem: f32) -> Option<f32> {
    Some(match v.trim().to_ascii_lowercase().as_str() {
        "xx-small" => 9.0,
        "x-small" => 10.0,
        "small" => 13.0,
        "medium" => 16.0,
        "large" => 18.0,
        "x-large" => 24.0,
        "xx-large" => 32.0,
        "smaller" => parent / 1.2,
        "larger" => parent * 1.2,
        other => parse_len(other, parent, rem)?.resolve(parent),
    })
}

fn weight(v: &str, parent: u16) -> Option<u16> {
    Some(match v.trim().to_ascii_lowercase().as_str() {
        "normal" => 400,
        "bold" => 700,
        "bolder" => (parent + 300).min(900),
        "lighter" => parent.saturating_sub(300).max(100),
        n => n.parse::<u16>().ok().filter(|w| (1..=1000).contains(w))?,
    })
}

fn line_height(v: &str, size: f32, rem: f32) -> Option<f32> {
    let v = v.trim();
    if v.eq_ignore_ascii_case("normal") {
        return Some(size * 1.25);
    }
    if let Ok(n) = v.parse::<f32>() {
        return Some(size * n);
    }
    match parse_len(v, size, rem)? {
        Len::Px(p) => Some(p),
        Len::Pct(p) => Some(size * p / 100.0),
        Len::Auto => None,
    }
}

/// `1px solid red` → (width, colour); `none` → 0.
fn border(v: &str, em: f32, rem: f32, current: Color) -> Option<(f32, Color)> {
    let mut width = 3.0; // `medium`
    let mut color = current;
    let mut none = false;
    for part in split_spaces(v) {
        let p = part.to_ascii_lowercase();
        match p.as_str() {
            "none" | "hidden" => none = true,
            "solid" | "dashed" | "dotted" | "double" | "groove" | "ridge" | "inset" | "outset" => {}
            "thin" => width = 1.0,
            "medium" => width = 3.0,
            "thick" => width = 5.0,
            _ => {
                if let Some(Len::Px(w)) = parse_len(&p, em, rem) {
                    width = w;
                } else if let Some(c) = parse_color(&p) {
                    color = c;
                }
            }
        }
    }
    Some(if none { (0.0, color) } else { (width, color) })
}

/// Splits a value on spaces outside parentheses (so `rgb(1, 2, 3)` stays whole).
fn split_spaces(v: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut depth = 0;
    for c in v.chars() {
        match c {
            '(' => {
                depth += 1;
                cur.push(c)
            }
            ')' => {
                depth -= 1;
                cur.push(c)
            }
            c if c.is_whitespace() && depth == 0 => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Expands 1–4 values to top, right, bottom, left.
fn four<T: Copy>(v: &[T]) -> Option<[T; 4]> {
    Some(match v.len() {
        1 => [v[0]; 4],
        2 => [v[0], v[1], v[0], v[1]],
        3 => [v[0], v[1], v[2], v[1]],
        4 => [v[0], v[1], v[2], v[3]],
        _ => return None,
    })
}

const SIDES: [&str; 4] = ["top", "right", "bottom", "left"];

/// Applies one declaration to `s` (whose inherited values come from `parent`).
fn apply(s: &mut Style, d: &Decl, parent: &Style, rem: f32) {
    let v = d.value.trim();
    let lower = v.to_ascii_lowercase();
    if lower == "inherit" {
        inherit_one(s, &d.name, parent);
        return;
    }
    let em = s.font_size;
    let len = |x: &str| parse_len(x, em, rem);
    match d.name.as_str() {
        "display" => {
            s.display = match lower.as_str() {
                "none" => Display::None,
                "inline" | "inline-block" | "inline-flex" | "inline-grid" | "contents" => Display::Inline,
                "list-item" => Display::ListItem,
                _ => Display::Block,
            }
        }
        "color" => {
            if let Some(c) = parse_color(v) {
                s.color = c
            }
        }
        "background-color" => {
            if let Some(c) = parse_color(v) {
                s.background = c
            }
        }
        "background" => {
            // The colour part only; images come with a later chunk.
            if lower == "none" {
                s.background = Color::TRANSPARENT;
            } else if let Some(c) = split_spaces(v).iter().rev().find_map(|p| parse_color(p)) {
                s.background = c;
            }
        }
        "font-size" => {
            if let Some(px) = font_size(v, parent.font_size, rem) {
                // Keep the line-height ratio when only the size changes.
                let ratio = s.line_height / s.font_size;
                s.font_size = px;
                s.line_height = px * ratio;
            }
        }
        "font-weight" => {
            if let Some(w) = weight(v, parent.weight) {
                s.weight = w
            }
        }
        "font-style" => s.italic = lower == "italic" || lower == "oblique",
        "font-family" => {
            if let Some(f) = family_of(v) {
                s.family = f
            }
        }
        "font" => {
            // [style] [weight] size[/line-height] family
            let parts = split_spaces(v);
            let Some(size_at) = parts.iter().position(|p| font_size(p.split('/').next().unwrap_or(""), parent.font_size, rem).is_some() && p.chars().next().map(|c| c.is_ascii_digit() || c == '.').unwrap_or(false))
            else {
                return;
            };
            s.italic = false;
            s.weight = 400;
            for p in &parts[..size_at] {
                match p.to_ascii_lowercase().as_str() {
                    "italic" | "oblique" => s.italic = true,
                    w => {
                        if let Some(w) = weight(w, parent.weight) {
                            s.weight = w
                        }
                    }
                }
            }
            let (size, lh) = parts[size_at].split_once('/').map(|(a, b)| (a, Some(b))).unwrap_or((&parts[size_at], None));
            if let Some(px) = font_size(size, parent.font_size, rem) {
                s.font_size = px;
                s.line_height = px * 1.25;
            }
            if let Some(lh) = lh.and_then(|l| line_height(l, s.font_size, rem)) {
                s.line_height = lh;
            }
            if let Some(f) = family_of(&parts[size_at + 1..].join(" ")) {
                s.family = f;
            }
        }
        "line-height" => {
            if let Some(lh) = line_height(v, s.font_size, rem) {
                s.line_height = lh
            }
        }
        "text-align" => {
            s.align = match lower.as_str() {
                "center" => Align::Center,
                "right" | "end" => Align::Right,
                _ => Align::Left,
            }
        }
        "white-space" => {
            s.space = match lower.as_str() {
                "pre" => Space::Pre,
                "pre-wrap" | "pre-line" | "break-spaces" => Space::PreWrap,
                "nowrap" => Space::NoWrap,
                _ => Space::Normal,
            }
        }
        "list-style-type" | "list-style" => {
            for p in lower.split_whitespace() {
                s.list = match p {
                    "disc" => ListStyle::Disc,
                    "circle" => ListStyle::Circle,
                    "square" => ListStyle::Square,
                    "decimal" | "decimal-leading-zero" | "lower-alpha" | "upper-alpha" | "lower-roman" | "upper-roman" | "lower-latin" | "upper-latin" => {
                        ListStyle::Decimal
                    }
                    "none" => ListStyle::None,
                    _ => continue,
                };
            }
        }
        "text-transform" => {
            s.transform = match lower.as_str() {
                "uppercase" => Transform::Upper,
                "lowercase" => Transform::Lower,
                "capitalize" => Transform::Capitalize,
                _ => Transform::None,
            }
        }
        "text-decoration" | "text-decoration-line" => {
            s.underline = lower.contains("underline");
            s.strike = lower.contains("line-through");
        }
        "visibility" => s.hidden = lower == "hidden" || lower == "collapse",
        "width" => {
            if let Some(l) = len(v) {
                s.width = l
            }
        }
        "max-width" => {
            s.max_width = if lower == "none" { Len::Auto } else { len(v).unwrap_or(s.max_width) };
        }
        "margin" | "padding" => {
            let vals: Option<Vec<Len>> = split_spaces(v).iter().map(|p| len(p)).collect();
            if let Some(four) = vals.as_deref().and_then(four) {
                if d.name == "margin" {
                    s.margin = four;
                } else {
                    s.padding = four;
                }
            }
        }
        "border" => {
            if let Some((w, c)) = border(v, em, rem, s.color) {
                s.border_width = [w; 4];
                s.border_color = [c; 4];
            }
        }
        "border-width" => {
            let vals: Option<Vec<f32>> = split_spaces(v).iter().map(|p| len(p).map(|l| l.resolve(0.0))).collect();
            if let Some(four) = vals.as_deref().and_then(four) {
                s.border_width = four;
            }
        }
        "border-color" => {
            let vals: Option<Vec<Color>> = split_spaces(v).iter().map(|p| parse_color(p)).collect();
            if let Some(four) = vals.as_deref().and_then(four) {
                s.border_color = four;
            }
        }
        "border-style" if lower == "none" || lower == "hidden" => s.border_width = [0.0; 4],
        name => {
            for (i, side) in SIDES.iter().enumerate() {
                if name == format!("margin-{side}") {
                    if let Some(l) = len(v) {
                        s.margin[i] = l;
                    }
                } else if name == format!("padding-{side}") {
                    if let Some(l) = len(v) {
                        s.padding[i] = l;
                    }
                } else if name == format!("border-{side}") {
                    if let Some((w, c)) = border(v, em, rem, s.color) {
                        s.border_width[i] = w;
                        s.border_color[i] = c;
                    }
                } else if name == format!("border-{side}-width") {
                    if let Some(l) = len(v) {
                        s.border_width[i] = l.resolve(0.0);
                    }
                } else if name == format!("border-{side}-color") {
                    if let Some(c) = parse_color(v) {
                        s.border_color[i] = c;
                    }
                }
            }
        }
    }
}

fn inherit_one(s: &mut Style, name: &str, p: &Style) {
    match name {
        "color" => s.color = p.color,
        "font-size" => s.font_size = p.font_size,
        "font-weight" => s.weight = p.weight,
        "font-family" => s.family = p.family,
        "line-height" => s.line_height = p.line_height,
        "background-color" | "background" => s.background = p.background,
        _ => {}
    }
}

/// What an element starts with before its own rules: inherited properties
/// from the parent, everything else at its initial value.
fn inherited(p: &Style) -> Style {
    Style {
        color: p.color,
        font_size: p.font_size,
        weight: p.weight,
        italic: p.italic,
        family: p.family,
        line_height: p.line_height,
        align: p.align,
        space: p.space,
        list: p.list,
        transform: p.transform,
        hidden: p.hidden,
        ..Style::default()
    }
}

// ----------------------------------------------------------- matching

fn element(dom: &Dom, id: NodeId) -> Option<(&str, &[(Box<str>, String)])> {
    match &dom.nodes[id as usize].data {
        Data::Element { name, attrs } => Some((name, attrs)),
        _ => None,
    }
}

fn attr<'a>(attrs: &'a [(Box<str>, String)], key: &str) -> Option<&'a str> {
    attrs.iter().find(|(k, _)| &**k == key).map(|(_, v)| v.as_str())
}

fn element_siblings(dom: &Dom, id: NodeId) -> Vec<NodeId> {
    let p = dom.parent(id);
    if p == id {
        return vec![id];
    }
    dom.children(p).iter().copied().filter(|&c| element(dom, c).is_some()).collect()
}

fn matches_compound(dom: &Dom, id: NodeId, c: &Compound) -> bool {
    let Some((tag, attrs)) = element(dom, id) else { return false };
    if let Some(t) = &c.tag {
        if t != tag {
            return false;
        }
    }
    if let Some(want) = &c.id {
        if attr(attrs, "id") != Some(want.as_str()) {
            return false;
        }
    }
    if !c.classes.is_empty() {
        let classes = attr(attrs, "class").unwrap_or("");
        if !c.classes.iter().all(|w| classes.split_ascii_whitespace().any(|x| x == w)) {
            return false;
        }
    }
    for (name, op) in &c.attrs {
        let Some(v) = attr(attrs, name) else { return false };
        let ok = match op {
            AttrOp::Exists => true,
            AttrOp::Equals(x) => v == x,
            AttrOp::Word(x) => v.split_ascii_whitespace().any(|w| w == x),
            AttrOp::Prefix(x) => !x.is_empty() && v.starts_with(x.as_str()),
            AttrOp::Suffix(x) => !x.is_empty() && v.ends_with(x.as_str()),
            AttrOp::Contains(x) => !x.is_empty() && v.contains(x.as_str()),
            AttrOp::Dash(x) => v == x || v.starts_with(&format!("{x}-")),
        };
        if !ok {
            return false;
        }
    }
    for p in &c.pseudos {
        let ok = match p {
            Pseudo::Root => tag == "html",
            Pseudo::Link => (tag == "a" || tag == "area") && attr(attrs, "href").is_some(),
            Pseudo::FirstChild => element_siblings(dom, id).first() == Some(&id),
            Pseudo::LastChild => element_siblings(dom, id).last() == Some(&id),
            Pseudo::OnlyChild => element_siblings(dom, id) == [id],
            Pseudo::Empty => dom.children(id).iter().all(|&k| match &dom.nodes[k as usize].data {
                Data::Text(t) => t.is_empty(),
                _ => false,
            }),
        };
        if !ok {
            return false;
        }
    }
    true
}

pub fn matches(dom: &Dom, id: NodeId, sel: &css::Selector) -> bool {
    matches_from(dom, id, &sel.parts, 0)
}

fn matches_from(dom: &Dom, id: NodeId, parts: &[(Compound, Combinator)], k: usize) -> bool {
    if !matches_compound(dom, id, &parts[k].0) {
        return false;
    }
    if k + 1 == parts.len() {
        return true;
    }
    let comb = parts[k].1;
    match comb {
        Combinator::Child | Combinator::Descendant => {
            let mut p = dom.parent(id);
            loop {
                if p == id || element(dom, p).is_none() {
                    return false;
                }
                if matches_from(dom, p, parts, k + 1) {
                    return true;
                }
                if comb == Combinator::Child {
                    return false;
                }
                let next = dom.parent(p);
                if next == p {
                    return false;
                }
                p = next;
            }
        }
        Combinator::Next | Combinator::Later => {
            let sibs = element_siblings(dom, id);
            let at = sibs.iter().position(|&s| s == id).unwrap_or(0);
            if comb == Combinator::Next {
                at > 0 && matches_from(dom, sibs[at - 1], parts, k + 1)
            } else {
                sibs[..at].iter().any(|&s| matches_from(dom, s, parts, k + 1))
            }
        }
    }
}

/// Styles for every node of `dom` (text nodes get their parent's).
pub fn compute(dom: &Dom, author_css: &[String]) -> Vec<Style> {
    // Origin 0: user agent; 1: author sheets (in order), then inline styles.
    let mut sheets: Vec<(u8, Vec<Rule>)> = vec![(0, css::parse_stylesheet(UA))];
    for c in author_css {
        sheets.push((1, css::parse_stylesheet(c)));
    }
    let mut styles: Vec<Style> = Vec::with_capacity(dom.nodes.len());
    let root_style = Style { display: Display::Block, ..Style::default() };
    let mut rem = 16.0;
    for id in 0..dom.nodes.len() as NodeId {
        let parent_id = dom.parent(id);
        let parent = if parent_id == id || (parent_id as usize) >= styles.len() { root_style.clone() } else { styles[parent_id as usize].clone() };
        let Some((tag, attrs)) = element(dom, id) else {
            styles.push(if id == 0 { root_style.clone() } else { parent });
            continue;
        };
        let mut s = inherited(&parent);
        // (important, origin, specificity, order) → declaration
        let mut found: Vec<((bool, u8, (u16, u16, u16), usize), &Decl)> = Vec::new();
        let mut order = 0usize;
        for (origin, rules) in &sheets {
            for r in rules {
                order += 1;
                let best = r.selectors.iter().filter(|sel| matches(dom, id, sel)).map(|sel| sel.specificity).max();
                if let Some(spec) = best {
                    for d in &r.decls {
                        found.push(((d.important, *origin, spec, order), d));
                    }
                }
            }
        }
        let inline = attr(attrs, "style").map(css::parse_declarations).unwrap_or_default();
        for d in &inline {
            order += 1;
            found.push(((d.important, 2, (u16::MAX, 0, 0), order), d));
        }
        found.sort_by_key(|(k, _)| *k);
        // font-size first, so em lengths in the same rule use the new size.
        for (_, d) in found.iter().filter(|(_, d)| d.name == "font-size" || d.name == "font") {
            apply(&mut s, d, &parent, rem);
        }
        for (_, d) in found.iter().filter(|(_, d)| d.name != "font-size" && d.name != "font") {
            apply(&mut s, d, &parent, rem);
        }
        // HTML's presentational hints that still matter.
        if tag == "font" {
            if let Some(c) = attr(attrs, "color").and_then(parse_color) {
                s.color = c;
            }
        }
        if attr(attrs, "align").map(|a| a.eq_ignore_ascii_case("center")).unwrap_or(false) {
            s.align = Align::Center;
        }
        if tag == "html" {
            rem = s.font_size;
        }
        styles.push(s);
    }
    styles
}

#[cfg(test)]
mod tests {
    use super::*;
    use volant_core::html;

    fn style_of(html: &str, css: &str, tag: &str) -> Style {
        let dom = html::parse(html);
        let styles = compute(&dom, &[css.to_string()]);
        let id = dom.find(tag).unwrap();
        styles[id as usize].clone()
    }

    #[test]
    fn values() {
        assert_eq!(parse_color("#0af"), Some(Color::rgb(0, 170, 255)));
        assert_eq!(parse_color("rgb(10, 20, 30)"), Some(Color::rgb(10, 20, 30)));
        assert_eq!(parse_color("rgba(0 0 0 / 50%)").unwrap().a, 128);
        assert_eq!(parse_color("#11223344").unwrap().a, 0x44);
        assert_eq!(parse_len("1.5em", 20.0, 16.0), Some(Len::Px(30.0)));
        assert_eq!(parse_len("2rem", 20.0, 16.0), Some(Len::Px(32.0)));
        assert_eq!(parse_len("50%", 20.0, 16.0), Some(Len::Pct(50.0)));
        assert_eq!(parse_len("12", 20.0, 16.0), None);
        assert_eq!(family_of("\"Helvetica Neue\", Arial, sans-serif"), Some(Family::Sans));
        assert_eq!(family_of("Charter, Georgia, serif"), Some(Family::Serif));
        assert_eq!(family_of("ui-monospace, Menlo"), Some(Family::Mono));
    }

    #[test]
    fn cascade_and_inheritance() {
        let html = r#"<html><body><div id="a" class="x"><p class="y" style="color: green">t <em>e</em></p></div></body></html>"#;
        let css = "div p { color: red; font-size: 20px } .x .y { color: blue !important } p { margin: 1em 2em } body { font-family: Georgia, serif }";
        let p = style_of(html, css, "p");
        assert_eq!(p.color, Color::rgb(0, 0, 255), "!important beats the inline style");
        assert_eq!(p.font_size, 20.0);
        assert_eq!(p.margin, [Len::Px(20.0), Len::Px(40.0), Len::Px(20.0), Len::Px(40.0)], "em uses the element's own new size");
        assert_eq!(p.family, Family::Serif, "inherited from body");
        assert_eq!(p.display, Display::Block);
        let em = style_of(html, css, "em");
        assert!(em.italic && em.color == Color::rgb(0, 0, 255) && em.display == Display::Inline);
        let h = style_of("<h1>x</h1>", "", "h1");
        assert_eq!((h.font_size, h.weight), (32.0, 700));
        assert_eq!(style_of("<p>x</p><p>y</p>", "p:first-child { color: red }", "p").color, Color::rgb(255, 0, 0));
        assert_eq!(style_of("<h2>x</h2><p>y</p>", "h2 + p { color: red }", "p").color, Color::rgb(255, 0, 0));
        assert_eq!(style_of("<ul><li>x</li></ul>", "", "li").display, Display::ListItem);
        assert_eq!(style_of("<pre>x</pre>", "", "pre").family, Family::Mono);
        let f = style_of("<p>x</p>", "p { font: italic bold 12px/30px Georgia, serif }", "p");
        assert_eq!((f.italic, f.weight, f.font_size, f.line_height, f.family), (true, 700, 12.0, 30.0, Family::Serif));
        let b = style_of("<p>x</p>", "p { border-left: 4px solid #ccc; border-top: none }", "p");
        assert_eq!((b.border_width, b.border_color[3]), ([0.0, 0.0, 0.0, 4.0], Color::rgb(204, 204, 204)));
        assert_eq!(style_of("<p>x</p>", "<style>", "p").display, Display::Block);
    }
}
