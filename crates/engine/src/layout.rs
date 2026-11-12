//! Layout: turns the styled DOM into boxes, places them, and paints them into
//! a display list.
//!
//! Block boxes stack vertically with margins (adjacent siblings' margins
//! collapse), padding, borders, `width`/`max-width` and auto-margin centring.
//! Inline content (text, `<br>`) is broken into lines: whitespace collapsing,
//! `pre`/`pre-wrap`/`nowrap`, word wrapping with long words split, line
//! heights and baselines from the fonts used, `text-align`. List items get
//! their markers. Blocks inside inline elements split the run around them.

use crate::fonts::{Font, Fonts};
use crate::style::{Align, Color, Display, Len, ListStyle, Space, Style, Transform};
use std::collections::HashMap;
use volant_core::html::{Data, Dom, NodeId};

/// Something to draw.
#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    Rect { x: f32, y: f32, w: f32, h: f32, color: Color },
    /// `y` is the baseline.
    Text { x: f32, y: f32, text: String, font: Font, color: Color, underline: bool, strike: bool },
}

/// A clickable area.
#[derive(Debug, Clone, PartialEq)]
pub struct Link {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub href: String,
}

/// A laid-out page.
#[derive(Debug, Clone, PartialEq)]
pub struct Page {
    pub width: f32,
    pub height: f32,
    /// The canvas colour (from `html` or `body`).
    pub background: Color,
    pub items: Vec<Item>,
    pub links: Vec<Link>,
}

struct BlockBox {
    node: NodeId,
    /// For list items: their number among their siblings.
    ordinal: usize,
    children: Vec<Child>,
}

enum Child {
    Block(BlockBox),
    /// A run of inline content (an anonymous block).
    Inline(Vec<Inline>),
}

enum Inline {
    /// `style` is the node whose computed style the text uses.
    Text { text: String, style: NodeId, link: Option<usize> },
    Break { style: NodeId },
}

struct Builder<'a> {
    dom: &'a Dom,
    styles: &'a [Style],
    hrefs: Vec<String>,
}

impl Builder<'_> {
    fn element(&self, id: NodeId) -> Option<&str> {
        self.dom.tag(id)
    }

    fn block(&mut self, id: NodeId, ordinal: usize) -> BlockBox {
        let mut b = BlockBox { node: id, ordinal, children: Vec::new() };
        let mut run = Vec::new();
        let mut item_no = 0;
        if let Some(start) = (self.element(id) == Some("ol")).then(|| self.dom.attr(id, "start").and_then(|s| s.trim().parse::<usize>().ok())).flatten() {
            item_no = start.saturating_sub(1);
        }
        for &c in self.dom.children(id) {
            self.child(c, &mut b.children, &mut run, None, &mut item_no);
        }
        flush(&mut b.children, &mut run);
        b
    }

    fn child(&mut self, id: NodeId, out: &mut Vec<Child>, run: &mut Vec<Inline>, link: Option<usize>, item_no: &mut usize) {
        match &self.dom.nodes[id as usize].data {
            Data::Text(t) => run.push(Inline::Text { text: t.clone(), style: id, link }),
            Data::Element { .. } => {
                let s = &self.styles[id as usize];
                match s.display {
                    Display::None => {}
                    Display::Block | Display::ListItem => {
                        flush(out, run);
                        if s.display == Display::ListItem {
                            *item_no += 1;
                        }
                        let b = self.block(id, *item_no);
                        out.push(Child::Block(b));
                    }
                    Display::Inline => {
                        if self.element(id) == Some("br") {
                            run.push(Inline::Break { style: id });
                            return;
                        }
                        let link = match (self.element(id), self.dom.attr(id, "href")) {
                            (Some("a"), Some(h)) => {
                                self.hrefs.push(h.to_string());
                                Some(self.hrefs.len() - 1)
                            }
                            _ => link,
                        };
                        for &c in self.dom.children(id) {
                            self.child(c, out, run, link, item_no);
                        }
                    }
                }
            }
            Data::Document => {}
        }
    }
}

/// Ends the current inline run as an anonymous block (unless it's only collapsible spaces).
fn flush(out: &mut Vec<Child>, run: &mut Vec<Inline>) {
    if run.is_empty() {
        return;
    }
    out.push(Child::Inline(std::mem::take(run)));
}

/// One unbreakable piece of a line.
struct Atom {
    text: String,
    w: f32,
    style: NodeId,
    link: Option<usize>,
    kind: Kind,
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Word,
    Space,
    Break,
}

struct Ctx<'a> {
    styles: &'a [Style],
    fonts: &'a dyn Fonts,
    hrefs: Vec<String>,
    items: Vec<Item>,
    links: Vec<Link>,
    cache: HashMap<((crate::style::Family, u32, u16, bool), String), f32>,
}

fn font_of(s: &Style) -> Font {
    Font { family: s.family, size: s.font_size, weight: s.weight, italic: s.italic }
}

fn transform(text: &str, t: Transform) -> String {
    match t {
        Transform::None => text.to_string(),
        Transform::Upper => text.to_uppercase(),
        Transform::Lower => text.to_lowercase(),
        Transform::Capitalize => {
            let mut out = String::with_capacity(text.len());
            let mut start = true;
            for c in text.chars() {
                if start && c.is_alphabetic() {
                    out.extend(c.to_uppercase());
                } else {
                    out.push(c);
                }
                start = c.is_whitespace();
            }
            out
        }
    }
}

/// Collapses two adjoining vertical margins.
fn collapse(a: f32, b: f32) -> f32 {
    if a >= 0.0 && b >= 0.0 {
        a.max(b)
    } else if a < 0.0 && b < 0.0 {
        a.min(b)
    } else {
        a + b
    }
}

fn vmargins(s: &Style, cb_w: f32) -> (f32, f32) {
    (s.margin[0].resolve(cb_w), s.margin[2].resolve(cb_w))
}

impl Ctx<'_> {
    fn measure(&mut self, text: &str, font: &Font) -> f32 {
        let key = (font.key(), text.to_string());
        if let Some(&w) = self.cache.get(&key) {
            return w;
        }
        let w = self.fonts.width(text, font);
        self.cache.insert(key, w);
        w
    }

    /// Lays out a block box at (x, y) inside a containing block `cb_w` wide;
    /// returns its border-box height. Its margins are the caller's business.
    fn block(&mut self, b: &BlockBox, x: f32, y: f32, cb_w: f32) -> f32 {
        let s = &self.styles[b.node as usize];
        let pad: [f32; 4] = std::array::from_fn(|i| s.padding[i].resolve(cb_w));
        let bw = s.border_width;
        let frame = pad[1] + pad[3] + bw[1] + bw[3];
        let (ml_auto, mr_auto) = (s.margin[3] == Len::Auto, s.margin[1] == Len::Auto);
        let mut ml = s.margin[3].resolve(cb_w);
        let mr = s.margin[1].resolve(cb_w);
        let mut content_w = match s.width {
            Len::Auto => cb_w - ml - mr - frame,
            w => w.resolve(cb_w),
        };
        if s.max_width != Len::Auto {
            content_w = content_w.min(s.max_width.resolve(cb_w));
        }
        content_w = content_w.max(0.0);
        let leftover = cb_w - content_w - frame;
        if ml_auto && mr_auto {
            ml = (leftover / 2.0).max(0.0);
        } else if ml_auto {
            ml = (leftover - mr).max(0.0);
        }
        let box_x = x + ml;
        let content_x = box_x + bw[3] + pad[3];
        let content_y = y + bw[0] + pad[0];
        let paint_at = self.items.len();
        let mut cursor = content_y;
        let mut prev_mb = 0.0;
        let mut first = true;
        for child in &b.children {
            match child {
                Child::Block(cb) => {
                    let (mt, mb) = vmargins(&self.styles[cb.node as usize], content_w);
                    let gap = if first { mt } else { collapse(prev_mb, mt) };
                    let h = self.block(cb, content_x, cursor + gap, content_w);
                    cursor += gap + h;
                    prev_mb = mb;
                }
                Child::Inline(run) => {
                    cursor += prev_mb;
                    prev_mb = 0.0;
                    cursor += self.inline(run, content_x, cursor, content_w, b.node);
                }
            }
            first = false;
        }
        cursor += prev_mb;
        let height = cursor - y + pad[2] + bw[2];
        let border_w = content_w + frame;
        // Background and borders go under the content: insert them before it.
        let mut under = Vec::new();
        if s.background.a > 0 {
            under.push(Item::Rect { x: box_x, y, w: border_w, h: height, color: s.background });
        }
        let sides = [(box_x, y, border_w, bw[0]), (box_x + border_w - bw[1], y, bw[1], height), (box_x, y + height - bw[2], border_w, bw[2]), (box_x, y, bw[3], height)];
        for (i, &(rx, ry, rw, rh)) in sides.iter().enumerate() {
            if bw[i] > 0.0 && s.border_color[i].a > 0 {
                under.push(Item::Rect { x: rx, y: ry, w: rw, h: rh, color: s.border_color[i] });
            }
        }
        if s.display == Display::ListItem && s.list != ListStyle::None && !s.hidden {
            let marker = match s.list {
                ListStyle::Disc => "•".to_string(),
                ListStyle::Circle => "◦".to_string(),
                ListStyle::Square => "▪".to_string(),
                ListStyle::Decimal => format!("{}.", b.ordinal),
                ListStyle::None => String::new(),
            };
            let font = font_of(s);
            let w = self.measure(&marker, &font);
            let (asc, desc) = (self.fonts.ascent(&font), self.fonts.descent(&font));
            let baseline = content_y + (s.line_height - asc - desc) / 2.0 + asc;
            under.push(Item::Text { x: content_x - w - font.size * 0.5, y: baseline, text: marker, font, color: s.color, underline: false, strike: false });
        }
        self.items.splice(paint_at..paint_at, under);
        height
    }

    /// Breaks a run of inline content into lines at (x, y), `width` wide; returns the height used.
    fn inline(&mut self, run: &[Inline], x: f32, y: f32, width: f32, block: NodeId) -> f32 {
        let bs = self.styles[block as usize].clone();
        // 1. Atoms: words, collapsible spaces, forced breaks.
        let mut atoms: Vec<Atom> = Vec::new();
        let mut last_space = true; // drop leading spaces
        for item in run {
            match item {
                Inline::Break { style } => {
                    atoms.push(Atom { text: String::new(), w: 0.0, style: *style, link: None, kind: Kind::Break });
                    last_space = true;
                }
                Inline::Text { text, style, link } => {
                    let s = self.styles[*style as usize].clone();
                    let font = font_of(&s);
                    let text = transform(text, s.transform);
                    let keep = matches!(s.space, Space::Pre | Space::PreWrap);
                    let mut word = String::new();
                    let push_word = |ctx: &mut Self, word: &mut String, atoms: &mut Vec<Atom>| {
                        if !word.is_empty() {
                            let w = ctx.measure(word, &font);
                            atoms.push(Atom { text: std::mem::take(word), w, style: *style, link: *link, kind: Kind::Word });
                        }
                    };
                    for c in text.chars() {
                        if keep && c == '\n' {
                            push_word(self, &mut word, &mut atoms);
                            atoms.push(Atom { text: String::new(), w: 0.0, style: *style, link: None, kind: Kind::Break });
                        } else if c.is_whitespace() && c != '\u{a0}' {
                            push_word(self, &mut word, &mut atoms);
                            if keep {
                                let sp = if c == '\t' { "        " } else { " " };
                                let w = self.measure(sp, &font);
                                atoms.push(Atom { text: sp.into(), w, style: *style, link: *link, kind: Kind::Space });
                            } else if !last_space {
                                let w = self.measure(" ", &font);
                                atoms.push(Atom { text: " ".into(), w, style: *style, link: *link, kind: Kind::Space });
                                last_space = true;
                            }
                        } else {
                            word.push(c);
                            last_space = false;
                        }
                    }
                    push_word(self, &mut word, &mut atoms);
                }
            }
        }
        // Nothing but collapsible spaces: no line at all.
        if atoms.iter().all(|a| a.kind == Kind::Space) && !atoms.iter().any(|a| matches!(self.styles[a.style as usize].space, Space::Pre | Space::PreWrap)) {
            return 0.0;
        }
        // 2. Lines.
        let mut lines: Vec<Vec<Atom>> = vec![Vec::new()];
        let mut line_w = 0.0;
        for a in atoms {
            let wraps = matches!(self.styles[a.style as usize].space, Space::Normal | Space::PreWrap);
            let collapsible = self.styles[a.style as usize].space == Space::Normal || self.styles[a.style as usize].space == Space::NoWrap;
            match a.kind {
                Kind::Break => {
                    let line = lines.last_mut().expect("a line");
                    while line.last().map(|x| x.kind == Kind::Space && self.styles[x.style as usize].space == Space::Normal).unwrap_or(false) {
                        line.pop();
                    }
                    line.push(a);
                    lines.push(Vec::new());
                    line_w = 0.0;
                    continue;
                }
                Kind::Space if collapsible && lines.last().map(|l| l.is_empty()).unwrap_or(true) => continue,
                _ => {}
            }
            if wraps && line_w + a.w > width && lines.last().map(|l| !l.is_empty()).unwrap_or(false) {
                lines.push(Vec::new());
                line_w = 0.0;
                if a.kind == Kind::Space && collapsible {
                    continue;
                }
            }
            if a.kind == Kind::Word && a.w > width && wraps {
                // A word longer than the line (a URL): break it where it must.
                let s = self.styles[a.style as usize].clone();
                let font = font_of(&s);
                let mut piece = String::new();
                for c in a.text.chars() {
                    let mut next = piece.clone();
                    next.push(c);
                    if !piece.is_empty() && line_w + self.measure(&next, &font) > width {
                        let w = self.measure(&piece, &font);
                        lines.last_mut().expect("a line").push(Atom { text: std::mem::take(&mut piece), w, style: a.style, link: a.link, kind: Kind::Word });
                        lines.push(Vec::new());
                        line_w = 0.0;
                    }
                    piece.push(c);
                }
                let w = self.measure(&piece, &font);
                line_w += w;
                lines.last_mut().expect("a line").push(Atom { text: piece, w, style: a.style, link: a.link, kind: Kind::Word });
                continue;
            }
            line_w += a.w;
            lines.last_mut().expect("a line").push(a);
        }
        // A trailing forced break leaves an empty last line that shouldn't count.
        if lines.len() > 1 && lines.last().map(|l| l.is_empty()).unwrap_or(false) {
            lines.pop();
        }
        // 3. Place and paint each line.
        let mut top = y;
        for mut line in lines {
            while line.last().map(|a| a.kind == Kind::Space && self.styles[a.style as usize].space == Space::Normal).unwrap_or(false) {
                line.pop();
            }
            let block_font = font_of(&bs);
            let mut above = (bs.line_height - self.fonts.ascent(&block_font) - self.fonts.descent(&block_font)) / 2.0 + self.fonts.ascent(&block_font);
            let mut below = bs.line_height - above;
            for a in &line {
                let s = &self.styles[a.style as usize];
                let f = font_of(s);
                let (asc, desc) = (self.fonts.ascent(&f), self.fonts.descent(&f));
                let half = (s.line_height - asc - desc) / 2.0;
                above = above.max(half + asc);
                below = below.max(half + desc);
            }
            let used: f32 = line.iter().map(|a| a.w).sum();
            let mut cx = x + match bs.align {
                Align::Left => 0.0,
                Align::Center => ((width - used) / 2.0).max(0.0),
                Align::Right => (width - used).max(0.0),
            };
            let baseline = top + above;
            let line_h = above + below;
            // Runs of atoms that share a style and link draw as one piece of text.
            let mut i = 0;
            while i < line.len() {
                let (style, link) = (line[i].style, line[i].link);
                let mut text = String::new();
                let mut w = 0.0;
                while i < line.len() && line[i].style == style && line[i].link == link && line[i].kind != Kind::Break {
                    text.push_str(&line[i].text);
                    w += line[i].w;
                    i += 1;
                }
                if text.is_empty() {
                    i += 1;
                    continue;
                }
                let s = &self.styles[style as usize];
                let font = font_of(s);
                if s.background.a > 0 {
                    let pad = font.size * 0.15;
                    self.items.push(Item::Rect { x: cx, y: baseline - self.fonts.ascent(&font) - pad, w, h: font.size + 2.0 * pad, color: s.background });
                }
                if !s.hidden {
                    self.items.push(Item::Text { x: cx, y: baseline, text, font, color: s.color, underline: s.underline, strike: s.strike });
                }
                if let Some(l) = link {
                    self.links.push(Link { x: cx, y: top, w, h: line_h, href: self.hrefs[l].clone() });
                }
                cx += w;
            }
            top += line_h;
        }
        top - y
    }
}

/// Lays out `dom` (styled by `styles`) for a viewport `width` px wide.
pub fn layout(dom: &Dom, styles: &[Style], width: f32, fonts: &dyn Fonts) -> Page {
    let mut builder = Builder { dom, styles, hrefs: Vec::new() };
    let root = builder.block(0, 0);
    let mut ctx = Ctx { styles, fonts, hrefs: builder.hrefs, items: Vec::new(), links: Vec::new(), cache: HashMap::new() };
    // The canvas takes the root's background, or else the body's.
    let bg_of = |tag: &str| dom.find(tag).map(|id| styles[id as usize].background).filter(|c| c.a > 0);
    let background = bg_of("html").or_else(|| bg_of("body")).unwrap_or(Color::rgb(255, 255, 255));
    let mut height = 0.0;
    let mut prev_mb = 0.0;
    for (k, child) in root.children.iter().enumerate() {
        match child {
            Child::Block(b) => {
                let (mt, mb) = vmargins(&styles[b.node as usize], width);
                let gap = if k == 0 { mt } else { collapse(prev_mb, mt) };
                height += gap + ctx.block(b, 0.0, height + gap, width);
                prev_mb = mb;
            }
            Child::Inline(run) => {
                height += prev_mb;
                prev_mb = 0.0;
                height += ctx.inline(run, 0.0, height, width, 0);
            }
        }
    }
    height += prev_mb;
    Page { width, height, background, items: ctx.items, links: ctx.links }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fonts::ApproxFonts;
    use crate::style;
    use volant_core::html;

    fn page(html: &str, css: &str, width: f32) -> Page {
        let html = format!("<html><body>{html}</body></html>");
        let dom = html::parse(&html);
        let styles = style::compute(&dom, &[css.to_string()]);
        layout(&dom, &styles, width, &ApproxFonts)
    }

    fn texts(p: &Page) -> Vec<(String, f32, f32)> {
        p.items
            .iter()
            .filter_map(|i| match i {
                Item::Text { x, y, text, .. } => Some((text.clone(), *x, *y)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn wraps_and_stacks() {
        let css = "body { margin: 0; font: 10px/20px sans-serif } p { margin: 10px 0 }";
        let p = page("<p>aaa bbb ccc</p><p>ddd</p>", css, 45.0);
        let t = texts(&p);
        // "aaa bbb" is 5.56*3*2 + 2.78 = 36.14 px; with " ccc" it would pass 45.
        assert_eq!(t.iter().map(|x| x.0.as_str()).collect::<Vec<_>>(), ["aaa bbb", "ccc", "ddd"]);
        assert_eq!(t[0].2, 10.0 + 5.0 + 8.0, "first baseline: margin, half-leading (20 - 10) / 2, ascent 0.8 em");
        assert_eq!(t[1].2 - t[0].2, 20.0, "line height");
        // Second paragraph: 10 (collapsed margins) below the first's two lines.
        assert_eq!(t[2].2, 10.0 + 40.0 + 10.0 + 13.0);
        assert_eq!(p.height, 10.0 + 40.0 + 10.0 + 20.0 + 10.0);
    }

    #[test]
    fn whitespace_inline_and_links() {
        let css = "body { margin: 0; font-size: 10px }";
        let p = page("<p>  Hello\n  <b>bold</b>   <a href='/x'>link</a> <br>next </p>", css, 500.0);
        let t = texts(&p);
        assert_eq!(t.iter().map(|x| x.0.as_str()).collect::<Vec<_>>(), ["Hello ", "bold", " ", "link", "next"]);
        assert_eq!(p.links.len(), 1);
        assert_eq!(p.links[0].href, "/x");
        assert!(t[4].2 > t[0].2, "<br> starts a new line");
        let bold = p.items.iter().find_map(|i| match i {
            Item::Text { text, font, .. } if text == "bold" => Some(*font),
            _ => None,
        });
        assert_eq!(bold.unwrap().weight, 700);
    }

    #[test]
    fn pre_center_lists_boxes() {
        let css = "body { margin: 0; font-size: 10px } div { width: 100px; margin: 0 auto; padding: 5px; background: #eee; border-left: 2px solid red }";
        let p = page("<div>x</div>", css, 300.0);
        let bg = p.items.iter().find_map(|i| match i {
            Item::Rect { x, w, color, .. } if color.r == 238 => Some((*x, *w)),
            _ => None,
        });
        assert_eq!(bg, Some((300.0 - 112.0, 112.0 / 1.0)).map(|(l, w)| (l / 2.0, w)), "centred: (300 - 112) / 2");
        assert!(p.items.iter().any(|i| matches!(i, Item::Rect { color, w, .. } if color.r == 255 && *w == 2.0)));
        let t = texts(&page("<pre>a  b\nc</pre>", css, 300.0));
        assert_eq!(t.iter().map(|x| x.0.as_str()).collect::<Vec<_>>(), ["a  b", "c"]);
        let t = texts(&page("<ol start=3><li>one<li>two</ol><ul><li>dot</ul>", css, 300.0));
        assert_eq!(t.iter().map(|x| x.0.as_str()).collect::<Vec<_>>(), ["3.", "one", "4.", "two", "•", "dot"]);
        let t = texts(&page("<p style='text-align:center'>hi</p>", "body{margin:0;font-size:10px}", 100.0));
        assert!((t[0].1 - (100.0 - 7.78) / 2.0).abs() < 0.01);
        let t = texts(&page("<p>https://example.com/a/very/long/address/that/cannot/fit</p>", css, 60.0));
        assert!(t.len() > 3 && t.iter().all(|(s, ..)| ApproxFonts.width(s, &Font { family: style::Family::Sans, size: 10.0, weight: 400, italic: false }) <= 60.0));
        let t = texts(&page("<p>a<span style='display:none'>hidden</span><script>x()</script></p>", css, 100.0));
        assert_eq!(t.iter().map(|x| x.0.as_str()).collect::<Vec<_>>(), ["a"]);
    }
}
