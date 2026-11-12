//! Layout: turns the styled DOM into boxes, places them, and paints them into
//! a display list.
//!
//! * Block boxes stack vertically with margins (adjacent siblings' margins
//!   collapse), padding, borders, `width`/`max-width`/`height` and auto-margin
//!   centring. List items get their markers.
//! * Inline content is broken into lines: whitespace collapsing,
//!   `pre`/`pre-wrap`/`nowrap`, wrapping (long words split), baselines from
//!   font metrics, `vertical-align` (super/sub/middle), `text-align`. Inline
//!   elements with backgrounds or horizontal padding/borders/margins get real
//!   inline boxes, painted per line.
//! * Images are replaced elements, sized from CSS, `width`/`height`
//!   attributes and their intrinsic size (via [`Images`]), inline or block.
//! * Floats (`float: left|right`) sit at the side and lines wrap around them;
//!   `clear` moves a block below them. The root, floats and table cells
//!   contain their floats.
//! * Tables: rows (in row groups or not), cells with `colspan`, columns sized
//!   from their cells' minimum and maximum content widths, rows as tall as
//!   their tallest cell, captions.

use crate::fonts::{Font, Fonts};
use crate::style::{self as css_style, Align, AlignItems, Clear, Color, Display, Float, Justify, Len, ListStyle, Position, Space, Style, Track, Transform, VAlign};
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use volant_core::html::{Data, Dom, NodeId};

/// Something to draw.
#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    Rect { x: f32, y: f32, w: f32, h: f32, color: Color, radius: f32 },
    /// `y` is the baseline. `after` says what follows it, for copying text.
    Text { x: f32, y: f32, text: String, font: Font, color: Color, underline: bool, strike: bool, after: After },
    /// An image, scaled to fill the box.
    Image { x: f32, y: f32, w: f32, h: f32, src: String, radius: f32 },
    /// Clips what follows, up to the matching `Unclip`, to a (rounded) rectangle (`overflow`).
    Clip { x: f32, y: f32, w: f32, h: f32, radius: f32 },
    Unclip,
}

/// Where a node was drawn, for finding what a click hit.
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub node: NodeId,
}

/// What comes after a piece of text: more on its line, a wrapped line, a
/// forced break (`<br>`, a newline in `pre`), or the end of its block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum After {
    Inline,
    /// A space (after a list marker).
    Space,
    Wrap,
    Break,
    BlockEnd,
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
    /// The boxes of elements and runs of text, for hit testing.
    pub hits: Vec<Hit>,
}

impl Page {
    /// The innermost node drawn at (x, y): the smallest box containing the point.
    pub fn hit(&self, x: f32, y: f32) -> Option<NodeId> {
        self.hits
            .iter()
            .filter(|h| x >= h.x && x < h.x + h.w && y >= h.y && y < h.y + h.h)
            .min_by(|a, b| (a.w * a.h).partial_cmp(&(b.w * b.h)).unwrap_or(std::cmp::Ordering::Equal))
            .map(|h| h.node)
    }
}

/// Image sizes, as far as they're known (loaded, or still loading: `None`).
pub trait Images {
    /// Intrinsic (width, height) in px.
    fn size(&self, src: &str) -> Option<(f32, f32)>;
}

/// For when no images are loaded: only `width`/`height` attributes size them.
pub struct NoImages;

impl Images for NoImages {
    fn size(&self, _src: &str) -> Option<(f32, f32)> {
        None
    }
}

#[derive(Clone, Debug)]
struct Img {
    src: String,
    attr_w: Option<f32>,
    attr_h: Option<f32>,
}

struct BlockBox {
    node: NodeId,
    /// For list items: their number among their siblings.
    ordinal: usize,
    kind: BoxKind,
    children: Vec<Child>,
    /// Absolutely positioned children of a flex or grid container.
    abs: Vec<BlockBox>,
}

enum BoxKind {
    Flow,
    Image(Img),
    Table(Table),
    /// A flex container's items, in `order`.
    Flex(Vec<BlockBox>),
    /// A grid container's items, in `order`.
    Grid(Vec<BlockBox>),
}

struct Table {
    captions: Vec<BlockBox>,
    /// Each row's style node (if it had one) and its cells with their colspans.
    rows: Vec<(Option<NodeId>, Vec<(BlockBox, usize)>)>,
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
    /// The start and end of an inline element that draws a box.
    Open(NodeId),
    Close(NodeId),
    Image { node: NodeId, img: Img, link: Option<usize> },
    Float(Box<BlockBox>),
    /// An absolutely positioned (or fixed) box, at its place in the flow.
    Abs(Box<BlockBox>),
}

struct Builder<'a> {
    dom: &'a Dom,
    styles: &'a [Style],
    before: &'a HashMap<NodeId, NodeId>,
    after: &'a HashMap<NodeId, NodeId>,
    /// Styles for anonymous boxes (text directly in a flex or grid container), numbered after `styles`.
    anon: Vec<Style>,
    hrefs: Vec<String>,
}

fn px_attr(dom: &Dom, id: NodeId, k: &str) -> Option<f32> {
    dom.attr(id, k).and_then(|v| v.trim().trim_end_matches("px").parse::<f32>().ok()).filter(|v| *v > 0.0)
}

/// Whether an inline element draws anything of its own (so needs a box).
fn needs_box(s: &Style) -> bool {
    let px = |l: Len| matches!(l, Len::Px(v) if v != 0.0);
    s.background.a > 0 || px(s.padding[1]) || px(s.padding[3]) || px(s.margin[1]) || px(s.margin[3]) || s.border_width[1] > 0.0 || s.border_width[3] > 0.0
}

impl Builder<'_> {
    fn img(&self, id: NodeId) -> Img {
        Img { src: self.dom.attr(id, "src").unwrap_or("").to_string(), attr_w: px_attr(self.dom, id, "width"), attr_h: px_attr(self.dom, id, "height") }
    }

    fn style(&self, id: NodeId) -> &Style {
        let n = self.styles.len();
        if (id as usize) < n {
            &self.styles[id as usize]
        } else {
            &self.anon[id as usize - n]
        }
    }

    /// Generated text for an element's `::before` or `::after`.
    fn generated(&self, map: &HashMap<NodeId, NodeId>, id: NodeId, link: Option<usize>) -> Option<Inline> {
        let ps = *map.get(&id)?;
        let text = self.styles[ps as usize].content.clone()?;
        Some(Inline::Text { text, style: ps, link })
    }

    /// An anonymous block for text sitting directly in a flex or grid container.
    fn anonymous(&mut self, parent: NodeId, run: Vec<Inline>) -> BlockBox {
        let mut st = css_style::inherited(self.style(parent));
        st.display = Display::Block;
        self.anon.push(st);
        let id = (self.styles.len() + self.anon.len() - 1) as NodeId;
        BlockBox { node: id, ordinal: 0, kind: BoxKind::Flow, children: vec![Child::Inline(run)], abs: Vec::new() }
    }

    /// A flex or grid container's items (its element children, and runs of
    /// text between them), and its absolutely positioned children.
    fn items(&mut self, id: NodeId) -> (Vec<BlockBox>, Vec<BlockBox>) {
        let mut items = Vec::new();
        let mut abs = Vec::new();
        let mut run = Vec::new();
        let has_text = |run: &Vec<Inline>| run.iter().any(|i| matches!(i, Inline::Text { text, .. } if !text.trim().is_empty()));
        if let Some(g) = self.generated(self.before, id, None) {
            run.push(g);
        }
        for &c in self.dom.children(id) {
            match &self.dom.nodes[c as usize].data {
                Data::Text(t) => run.push(Inline::Text { text: t.clone(), style: c, link: None }),
                Data::Element { .. } => {
                    if self.styles[c as usize].display == Display::None {
                        continue;
                    }
                    if matches!(self.styles[c as usize].position, Position::Absolute | Position::Fixed) {
                        abs.push(self.block(c, 0));
                        continue;
                    }
                    if has_text(&run) {
                        let b = self.anonymous(id, std::mem::take(&mut run));
                        items.push(b);
                    }
                    run.clear();
                    items.push(self.block(c, 0));
                }
                Data::Document => {}
            }
        }
        if let Some(g) = self.generated(self.after, id, None) {
            run.push(g);
        }
        if has_text(&run) {
            let b = self.anonymous(id, run);
            items.push(b);
        }
        items.sort_by_key(|b| self.style(b.node).order);
        (items, abs)
    }

    fn block(&mut self, id: NodeId, ordinal: usize) -> BlockBox {
        if self.dom.tag(id) == Some("img") {
            return BlockBox { node: id, ordinal, kind: BoxKind::Image(self.img(id)), children: Vec::new(), abs: Vec::new() };
        }
        match self.styles[id as usize].display {
            Display::Flex => {
                let (items, abs) = self.items(id);
                return BlockBox { node: id, ordinal, kind: BoxKind::Flex(items), children: Vec::new(), abs };
            }
            Display::Grid => {
                let (items, abs) = self.items(id);
                return BlockBox { node: id, ordinal, kind: BoxKind::Grid(items), children: Vec::new(), abs };
            }
            _ => {}
        }
        if self.styles[id as usize].display == Display::Table {
            let mut t = Table { captions: Vec::new(), rows: Vec::new() };
            let mut pending = Vec::new();
            self.table_children(id, &mut t, &mut pending);
            if !pending.is_empty() {
                t.rows.push((None, pending));
            }
            return BlockBox { node: id, ordinal, kind: BoxKind::Table(t), children: Vec::new(), abs: Vec::new() };
        }
        let mut b = BlockBox { node: id, ordinal, kind: BoxKind::Flow, children: Vec::new(), abs: Vec::new() };
        let mut run = Vec::new();
        let mut item_no = 0;
        if let Some(start) = (self.dom.tag(id) == Some("ol")).then(|| self.dom.attr(id, "start").and_then(|s| s.trim().parse::<usize>().ok())).flatten() {
            item_no = start.saturating_sub(1);
        }
        if let Some(g) = self.generated(self.before, id, None) {
            run.push(g);
        }
        for &c in self.dom.children(id) {
            self.child(c, &mut b.children, &mut run, None, &mut item_no);
        }
        if let Some(g) = self.generated(self.after, id, None) {
            run.push(g);
        }
        flush(&mut b.children, &mut run);
        b
    }

    fn colspan(&self, id: NodeId) -> usize {
        self.dom.attr(id, "colspan").and_then(|v| v.trim().parse::<usize>().ok()).unwrap_or(1).clamp(1, 1000)
    }

    /// Rows and captions of a table, through row groups; stray cells form rows of their own.
    fn table_children(&mut self, id: NodeId, t: &mut Table, pending: &mut Vec<(BlockBox, usize)>) {
        for &c in self.dom.children(id) {
            if self.dom.tag(c).is_none() {
                continue;
            }
            match self.styles[c as usize].display {
                Display::None => {}
                Display::TableCaption => t.captions.push(self.block(c, 0)),
                Display::TableRowGroup => {
                    if !pending.is_empty() {
                        t.rows.push((None, std::mem::take(pending)));
                    }
                    self.table_children(c, t, pending);
                    if !pending.is_empty() {
                        t.rows.push((None, std::mem::take(pending)));
                    }
                }
                Display::TableRow => {
                    if !pending.is_empty() {
                        t.rows.push((None, std::mem::take(pending)));
                    }
                    let cells = self
                        .dom
                        .children(c)
                        .iter()
                        .copied()
                        .filter(|&cc| self.dom.tag(cc).is_some() && self.styles[cc as usize].display != Display::None)
                        .map(|cc| (self.block(cc, 0), self.colspan(cc)))
                        .collect();
                    t.rows.push((Some(c), cells));
                }
                _ => {
                    let span = self.colspan(c);
                    pending.push((self.block(c, 0), span));
                }
            }
        }
    }

    fn child(&mut self, id: NodeId, out: &mut Vec<Child>, run: &mut Vec<Inline>, link: Option<usize>, item_no: &mut usize) {
        match &self.dom.nodes[id as usize].data {
            Data::Text(t) => run.push(Inline::Text { text: t.clone(), style: id, link }),
            Data::Element { .. } => {
                let s = &self.styles[id as usize];
                if s.display == Display::None {
                    return;
                }
                let tag = self.dom.tag(id);
                if matches!(s.position, Position::Absolute | Position::Fixed) && !matches!(s.display, Display::TableRow | Display::TableCell | Display::TableRowGroup) {
                    let b = self.block(id, 0);
                    run.push(Inline::Abs(Box::new(b)));
                    return;
                }
                if s.float != Float::None && !matches!(s.display, Display::TableRow | Display::TableCell | Display::TableRowGroup) {
                    let b = self.block(id, 0);
                    run.push(Inline::Float(Box::new(b)));
                    return;
                }
                if tag == Some("img") && s.display == Display::Inline {
                    run.push(Inline::Image { node: id, img: self.img(id), link });
                    return;
                }
                match s.display {
                    Display::Inline => {
                        if tag == Some("br") {
                            run.push(Inline::Break { style: id });
                            return;
                        }
                        let link = match (tag, self.dom.attr(id, "href")) {
                            (Some("a"), Some(h)) => {
                                self.hrefs.push(h.to_string());
                                Some(self.hrefs.len() - 1)
                            }
                            _ => link,
                        };
                        let boxed = needs_box(s);
                        if boxed {
                            run.push(Inline::Open(id));
                        }
                        if let Some(g) = self.generated(self.before, id, link) {
                            run.push(g);
                        }
                        for &c in self.dom.children(id) {
                            self.child(c, out, run, link, item_no);
                        }
                        if let Some(g) = self.generated(self.after, id, link) {
                            run.push(g);
                        }
                        if boxed {
                            run.push(Inline::Close(id));
                        }
                    }
                    _ => {
                        flush(out, run);
                        if s.display == Display::ListItem {
                            *item_no += 1;
                        }
                        let b = self.block(id, *item_no);
                        out.push(Child::Block(b));
                    }
                }
            }
            Data::Document => {}
        }
    }
}

/// Ends the current inline run as an anonymous block.
fn flush(out: &mut Vec<Child>, run: &mut Vec<Inline>) {
    if !run.is_empty() {
        out.push(Child::Inline(std::mem::take(run)));
    }
}

/// Slack when deciding whether something fits: sizes that should match
/// exactly (a column made as wide as its widest word) can differ by rounding.
const FIT: f32 = 0.01;

#[derive(Clone, Copy, PartialEq, Debug)]
enum Kind {
    Word,
    Space,
    Break,
    Open,
    Close,
    Image,
    Float,
    Abs,
}

/// One unbreakable piece of a line.
struct Atom<'r> {
    kind: Kind,
    text: String,
    /// Advance width, including an image's or edge's margins.
    w: f32,
    style: NodeId,
    link: Option<usize>,
    /// The inline boxes this atom is inside, outermost first.
    inside: Rc<[NodeId]>,
    /// Images: (left margin, width, height, src).
    image: Option<(f32, f32, f32, &'r str)>,
    float: Option<&'r BlockBox>,
}

impl Atom<'_> {
    fn content(&self) -> bool {
        matches!(self.kind, Kind::Word | Kind::Space | Kind::Image)
    }
}

#[derive(Clone, Copy, Debug)]
struct FloatBox {
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    left: bool,
}

#[derive(Clone, Copy, Default)]
struct Opts {
    /// The smallest border-box height (table cells stretch to their row).
    min_h: f32,
    /// A border-box width set by a flex or grid container; margins are then the caller's.
    width: Option<f32>,
    /// A block formatting context root: contains its floats.
    bfc: bool,
}

type FontKey = (crate::style::Family, u32, u16, bool);

struct Ctx<'a> {
    styles: &'a [Style],
    fonts: &'a dyn Fonts,
    images: &'a dyn Images,
    hrefs: Vec<String>,
    items: Vec<Item>,
    links: Vec<Link>,
    hits: Vec<Hit>,
    cache: HashMap<(FontKey, String), f32>,
    floats: Vec<FloatBox>,
    /// Containing blocks for absolutely positioned boxes: (x, y, width) of each
    /// positioned ancestor's padding box, the viewport first.
    cbs: Vec<(f32, f32, f32)>,
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

/// A length that only counts when it's absolute (for intrinsic sizes).
fn px_only(l: Len) -> f32 {
    match l {
        Len::Px(v) => v,
        _ => 0.0,
    }
}

impl<'r> Ctx<'_> {
    fn measure(&mut self, text: &str, font: &Font) -> f32 {
        let key = (font.key(), text.to_string());
        if let Some(&w) = self.cache.get(&key) {
            return w;
        }
        let w = self.fonts.width(text, font);
        self.cache.insert(key, w);
        w
    }

    /// An image's size in px: CSS width/height, then attributes, then its
    /// intrinsic size, keeping the aspect ratio; `max-width` scales it down.
    fn replaced_size(&self, node: NodeId, img: &Img, cb_w: f32) -> Option<(f32, f32)> {
        let s = &self.styles[node as usize];
        let intrinsic = if img.src.is_empty() { None } else { self.images.size(&img.src) };
        let ratio = intrinsic.filter(|(w, _)| *w > 0.0).map(|(w, h)| h / w).or_else(|| Some(img.attr_h? / img.attr_w?));
        let mut w = match s.width {
            Len::Px(v) => Some(v),
            Len::Pct(p) if cb_w > 0.0 => Some(cb_w * p / 100.0),
            _ => None,
        };
        let mut h = match s.height {
            Len::Px(v) => Some(v),
            _ => None,
        };
        if w.is_none() && h.is_none() {
            w = img.attr_w.or(intrinsic.map(|i| i.0));
            if img.attr_w.is_none() || img.attr_h.is_some() {
                h = img.attr_h;
            }
        }
        if let (Some(wv), None) = (w, h) {
            h = ratio.map(|r| wv * r).or(intrinsic.map(|i| i.1));
        }
        if let (None, Some(hv)) = (w, h) {
            w = ratio.filter(|r| *r > 0.0).map(|r| hv / r).or(intrinsic.map(|i| i.0));
        }
        let (mut w, mut h) = (w?, h?);
        let max = match s.max_width {
            Len::Px(v) => Some(v),
            Len::Pct(p) if cb_w > 0.0 => Some(cb_w * p / 100.0),
            _ => None,
        };
        if let Some(m) = max {
            if w > m && w > 0.0 {
                h *= m / w;
                w = m;
            }
        }
        Some((w.max(0.0), h.max(0.0)))
    }

    // ------------------------------------------------------------ floats

    /// The left and right edges of a line `h` tall at `top`, around the floats.
    fn span(&self, top: f32, h: f32, x: f32, width: f32) -> (f32, f32) {
        let (mut l, mut r) = (x, x + width);
        for f in &self.floats {
            if f.y0 < top + h && f.y1 > top {
                if f.left {
                    l = l.max(f.x1);
                } else {
                    r = r.min(f.x0);
                }
            }
        }
        (l, r.max(l))
    }

    /// The lowest bottom among floats overlapping a line at `top`, if any do.
    fn next_float_bottom(&self, top: f32, h: f32) -> Option<f32> {
        self.floats.iter().filter(|f| f.y0 < top + h && f.y1 > top).map(|f| f.y1).fold(None, |a: Option<f32>, b| Some(a.map_or(b, |a| a.min(b))))
    }

    fn clear_to(&self, c: Clear) -> f32 {
        self.floats
            .iter()
            .filter(|f| matches!((c, f.left), (Clear::Both, _) | (Clear::Left, true) | (Clear::Right, false)))
            .map(|f| f.y1)
            .fold(f32::MIN, f32::max)
    }

    /// Places a float at `top` (or lower, if it doesn't fit beside the floats already there).
    fn place_float(&mut self, b: &BlockBox, mut top: f32, x: f32, width: f32) {
        let s = self.styles[b.node as usize].clone();
        let (ml, mr) = (s.margin[3].resolve(width), s.margin[1].resolve(width));
        let (mt, mb) = vmargins(&s, width);
        let frame = s.padding[1].resolve(width) + s.padding[3].resolve(width) + s.border_width[1] + s.border_width[3];
        let border_w = match &b.kind {
            BoxKind::Image(img) => self.replaced_size(b.node, img, width).map(|(w, _)| w).unwrap_or(0.0) + frame,
            _ => match s.width {
                Len::Auto => {
                    let (min, max) = self.intrinsic(b);
                    let (l, r) = self.span(top, 1.0, x, width);
                    max.min((r - l - ml - mr).max(min))
                }
                w => w.resolve(width) + frame,
            },
        };
        let outer = border_w + ml + mr;
        let (mut l, mut r) = self.span(top, 1.0, x, width);
        for _ in 0..64 {
            if r - l + FIT >= outer {
                break;
            }
            match self.next_float_bottom(top, 1.0) {
                Some(bottom) => {
                    top = bottom;
                    (l, r) = self.span(top, 1.0, x, width);
                }
                None => break,
            }
        }
        let left = s.float != Float::Right;
        let fx = if left { l } else { r - outer };
        let h = self.block(b, fx, top + mt, outer, Opts { bfc: true, ..Default::default() });
        self.floats.push(FloatBox { x0: fx, y0: top, x1: fx + outer, y1: top + mt + h + mb, left });
    }

    // ------------------------------------------------------------ intrinsic sizes

    /// (min-content, max-content) border-box widths.
    fn intrinsic(&mut self, b: &BlockBox) -> (f32, f32) {
        let s = self.styles[b.node as usize].clone();
        let frame = px_only(s.padding[1]) + px_only(s.padding[3]) + s.border_width[1] + s.border_width[3];
        if let Len::Px(w) = s.width {
            return (w + frame, w + frame);
        }
        let (min, max) = match &b.kind {
            BoxKind::Image(img) => {
                let w = self.replaced_size(b.node, img, 0.0).map(|(w, _)| w).unwrap_or(0.0);
                (w, w)
            }
            BoxKind::Table(t) => {
                let (mins, maxs) = self.columns(t);
                (mins.iter().sum(), maxs.iter().sum())
            }
            BoxKind::Flex(items) | BoxKind::Grid(items) => {
                let row = matches!(b.kind, BoxKind::Grid(_)) || !s.column;
                let (mut mn, mut mx) = (0f32, 0f32);
                for it in items {
                    let is = &self.styles[it.node as usize];
                    let m = px_only(is.margin[1]) + px_only(is.margin[3]);
                    let (a, z) = self.intrinsic(it);
                    mn = mn.max(a + m);
                    mx = if row { mx + z + m } else { mx.max(z + m) };
                }
                if row && items.len() > 1 {
                    mx += s.column_gap * (items.len() - 1) as f32;
                }
                (mn, mx)
            }
            BoxKind::Flow => {
                let (mut mn, mut mx) = (0f32, 0f32);
                for c in &b.children {
                    match c {
                        Child::Block(cb) => {
                            let cs = &self.styles[cb.node as usize];
                            let m = px_only(cs.margin[1]) + px_only(cs.margin[3]);
                            let (a, z) = self.intrinsic(cb);
                            mn = mn.max(a + m);
                            mx = mx.max(z + m);
                        }
                        Child::Inline(run) => {
                            let atoms = self.atoms(run, 0.0);
                            let mut line = 0f32;
                            for a in &atoms {
                                match a.kind {
                                    Kind::Break => {
                                        mx = mx.max(line);
                                        line = 0.0;
                                    }
                                    Kind::Float => {
                                        let (fa, fz) = self.intrinsic(a.float.expect("float atom"));
                                        mn = mn.max(fa);
                                        line += fz;
                                    }
                                    _ => {
                                        if a.kind != Kind::Space {
                                            mn = mn.max(a.w);
                                        }
                                        line += a.w;
                                    }
                                }
                            }
                            mx = mx.max(line);
                        }
                    }
                }
                (mn, mx)
            }
        };
        (min + frame, max.max(min) + frame)
    }

    /// Each column's minimum and maximum width.
    fn columns(&mut self, t: &Table) -> (Vec<f32>, Vec<f32>) {
        let n = t.rows.iter().map(|(_, cells)| cells.iter().map(|c| c.1).sum::<usize>()).max().unwrap_or(0);
        let (mut mins, mut maxs) = (vec![0f32; n], vec![0f32; n]);
        // Single-column cells first, then spanning cells spread what's missing.
        for pass in 0..2 {
            for (_, cells) in &t.rows {
                let mut col = 0;
                for (cell, span) in cells {
                    let span = (*span).min(n - col);
                    if (pass == 0) == (span == 1) {
                        let (a, z) = self.intrinsic(cell);
                        let have_min: f32 = mins[col..col + span].iter().sum();
                        let have_max: f32 = maxs[col..col + span].iter().sum();
                        for k in col..col + span {
                            mins[k] += ((a - have_min) / span as f32).max(0.0);
                            maxs[k] += ((z - have_max) / span as f32).max(0.0);
                        }
                    }
                    col += span;
                }
            }
        }
        for k in 0..n {
            maxs[k] = maxs[k].max(mins[k]);
        }
        (mins, maxs)
    }

    // ------------------------------------------------------------ blocks

    /// Lays out a block box at (x, y) inside a containing block `cb_w` wide;
    /// returns its border-box height. Its vertical margins are the caller's business.
    fn block(&mut self, b: &BlockBox, x: f32, y: f32, cb_w: f32, opts: Opts) -> f32 {
        let s = self.styles[b.node as usize].clone();
        let pad: [f32; 4] = std::array::from_fn(|i| s.padding[i].resolve(cb_w));
        let bw = s.border_width;
        let frame = pad[1] + pad[3] + bw[1] + bw[3];
        let forced = opts.width;
        let (ml_auto, mr_auto) = (s.margin[3] == Len::Auto && forced.is_none(), s.margin[1] == Len::Auto && forced.is_none());
        let mut ml = if forced.is_some() { 0.0 } else { s.margin[3].resolve(cb_w) };
        let mr = if forced.is_some() { 0.0 } else { s.margin[1].resolve(cb_w) };
        // With box-sizing: border-box, specified sizes include padding and borders.
        let inner = |l: Len| -> f32 {
            let v = l.resolve(cb_w);
            if s.border_box {
                (v - frame).max(0.0)
            } else {
                v
            }
        };
        let replaced = match &b.kind {
            BoxKind::Image(img) => Some(self.replaced_size(b.node, img, cb_w - ml - mr - frame).unwrap_or((0.0, 0.0))),
            _ => None,
        };
        let table_w = match &b.kind {
            BoxKind::Table(t) => Some(self.table_width(t, &s, cb_w - ml - mr - frame)),
            _ => None,
        };
        let mut content_w = if let Some(f) = forced.filter(|_| replaced.is_none()) {
            f - frame
        } else if let Some((w, _)) = replaced {
            w
        } else if let Some(w) = table_w {
            w
        } else {
            match s.width {
                Len::Auto => cb_w - ml - mr - frame,
                w => inner(w),
            }
        };
        if forced.is_none() {
            if s.max_width != Len::Auto && replaced.is_none() {
                content_w = content_w.min(inner(s.max_width));
            }
            if s.min_width != Len::Auto {
                content_w = content_w.max(inner(s.min_width));
            }
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
        let links_at = self.links.len();
        let hits_at = self.hits.len();
        let positioned = s.position != Position::Static;
        if positioned {
            self.cbs.push((box_x + bw[3], y + bw[0], content_w + pad[1] + pad[3]));
        }
        let saved = opts.bfc.then(|| std::mem::take(&mut self.floats));
        let mut cursor = content_y;
        match &b.kind {
            BoxKind::Image(img) => {
                let (w, h) = replaced.unwrap_or((0.0, 0.0));
                if w > 0.0 && h > 0.0 && !img.src.is_empty() && !s.hidden {
                    self.items.push(Item::Image { x: content_x, y: content_y, w, h, src: img.src.clone(), radius: s.radius });
                }
                cursor += h;
            }
            BoxKind::Table(t) => cursor += self.table(t, content_x, content_y, content_w),
            BoxKind::Flex(items) => cursor += self.flex(items, &s, content_x, content_y, content_w),
            BoxKind::Grid(items) => cursor += self.grid(items, &s, content_x, content_y, content_w),
            BoxKind::Flow => {}
        }
        for a in &b.abs {
            self.place_abs(a, content_x, content_y);
        }
        match &b.kind {
            BoxKind::Image(_) | BoxKind::Table(_) | BoxKind::Flex(_) | BoxKind::Grid(_) => {}
            BoxKind::Flow => {
                let mut prev_mb = 0.0;
                let mut first = true;
                for child in &b.children {
                    match child {
                        Child::Block(cb) => {
                            let cs = &self.styles[cb.node as usize];
                            let (mt, mb) = vmargins(cs, content_w);
                            let clear = cs.clear;
                            let gap = if first { mt } else { collapse(prev_mb, mt) };
                            let mut at = cursor + gap;
                            if clear != Clear::None {
                                at = at.max(self.clear_to(clear));
                            }
                            let h = self.block(cb, content_x, at, content_w, Opts::default());
                            cursor = at + h;
                            prev_mb = mb;
                        }
                        Child::Inline(run) => {
                            let h = self.inline(run, content_x, cursor + prev_mb, content_w, b.node);
                            // A run that made no lines (only floats, or spaces) doesn't
                            // separate the margins around it.
                            if h > 0.0 {
                                cursor += prev_mb + h;
                                prev_mb = 0.0;
                            }
                        }
                    }
                    first = false;
                }
                cursor += prev_mb;
            }
        }
        let vframe = pad[0] + pad[2] + bw[0] + bw[2];
        if let Len::Px(h) = s.height {
            if replaced.is_none() {
                cursor = content_y + if s.border_box { (h - vframe).max(0.0) } else { h };
            }
        }
        if let Len::Px(h) = s.min_height {
            cursor = cursor.max(content_y + if s.border_box { (h - vframe).max(0.0) } else { h });
        }
        if let Some(saved) = saved {
            // A formatting-context root grows to hold its floats.
            let bottom = self.floats.iter().map(|f| f.y1).fold(cursor, f32::max);
            cursor = bottom;
            self.floats = saved;
        }
        let height = (cursor - y + pad[2] + bw[2]).max(opts.min_h);
        let border_w = content_w + frame;
        // Background and borders go under the content: insert them before it.
        let mut under = Vec::new();
        if s.background.a > 0 {
            under.push(Item::Rect { x: box_x, y, w: border_w, h: height, color: s.background, radius: s.radius });
        }
        let sides = [(box_x, y, border_w, bw[0]), (box_x + border_w - bw[1], y, bw[1], height), (box_x, y + height - bw[2], border_w, bw[2]), (box_x, y, bw[3], height)];
        for (i, &(rx, ry, rw, rh)) in sides.iter().enumerate() {
            if bw[i] > 0.0 && s.border_color[i].a > 0 {
                under.push(Item::Rect { x: rx, y: ry, w: rw, h: rh, color: s.border_color[i], radius: 0.0 });
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
            let font = font_of(&s);
            let w = self.measure(&marker, &font);
            let (asc, desc) = (self.fonts.ascent(&font), self.fonts.descent(&font));
            let baseline = content_y + (s.line_height - asc - desc) / 2.0 + asc;
            under.push(Item::Text { x: content_x - w - font.size * 0.5, y: baseline, text: marker, font, color: s.color, underline: false, strike: false, after: After::Space });
        }
        let n_under = under.len();
        self.items.splice(paint_at..paint_at, under);
        if positioned {
            self.cbs.pop();
        }
        self.hits.push(Hit { x: box_x, y, w: border_w, h: height, node: b.node });
        // overflow: the content (not the box's own background and borders) is clipped to the padding box.
        if s.overflow_clip && self.items.len() > paint_at + n_under {
            let (cx, cy) = (box_x + bw[3], y + bw[0]);
            let (cw, ch) = ((border_w - bw[1] - bw[3]).max(0.0), (height - bw[0] - bw[2]).max(0.0));
            self.items.insert(paint_at + n_under, Item::Clip { x: cx, y: cy, w: cw, h: ch, radius: s.radius });
            self.items.push(Item::Unclip);
        }
        if s.opacity < 1.0 {
            if s.opacity <= 0.01 {
                self.items.truncate(paint_at);
                self.links.truncate(links_at);
            } else {
                for it in &mut self.items[paint_at..] {
                    if let Item::Rect { color, .. } | Item::Text { color, .. } = it {
                        color.a = (color.a as f32 * s.opacity).round() as u8;
                    }
                }
            }
        }
        // position: relative and transform: translate move the box (and everything in it) after layout.
        let (mut dx, mut dy) = (0.0, 0.0);
        if s.position == Position::Relative {
            dx += if s.inset[3] != Len::Auto { s.inset[3].resolve(cb_w) } else if s.inset[1] != Len::Auto { -s.inset[1].resolve(cb_w) } else { 0.0 };
            dy += if s.inset[0] != Len::Auto { s.inset[0].resolve(cb_w) } else if s.inset[2] != Len::Auto { -s.inset[2].resolve(cb_w) } else { 0.0 };
        }
        if let Some((tx, ty)) = s.translate {
            dx += tx.resolve(border_w);
            dy += ty.resolve(height);
        }
        if dx != 0.0 || dy != 0.0 {
            self.shift(paint_at, links_at, hits_at, dx, dy);
        }
        height
    }

    /// Moves everything drawn since the given indices.
    fn shift(&mut self, items_at: usize, links_at: usize, hits_at: usize, dx: f32, dy: f32) {
        for it in &mut self.items[items_at..] {
            match it {
                Item::Rect { x, y, .. } | Item::Text { x, y, .. } | Item::Image { x, y, .. } | Item::Clip { x, y, .. } => {
                    *x += dx;
                    *y += dy;
                }
                Item::Unclip => {}
            }
        }
        for l in &mut self.links[links_at..] {
            l.x += dx;
            l.y += dy;
        }
        for h in &mut self.hits[hits_at..] {
            h.x += dx;
            h.y += dy;
        }
    }

    /// Places an absolutely positioned (or fixed) box against its containing
    /// block; `(sx, sy)` is where it would have been in the flow.
    fn place_abs(&mut self, b: &BlockBox, sx: f32, sy: f32) {
        let st = self.styles[b.node as usize].clone();
        let (cx, cy, cw) = if st.position == Position::Fixed { self.cbs[0] } else { *self.cbs.last().expect("the viewport") };
        let inset = |i: usize| (st.inset[i] != Len::Auto).then(|| st.inset[i].resolve(cw));
        let (top, right, left) = (inset(0), inset(1), inset(3));
        let (ml, mr) = (px_only(st.margin[3]), px_only(st.margin[1]));
        let mt = px_only(st.margin[0]);
        let frame = st.padding[1].resolve(cw) + st.padding[3].resolve(cw) + st.border_width[1] + st.border_width[3];
        let border_w = match (st.width, left, right) {
            (Len::Auto, Some(l), Some(r)) => (cw - l - r - ml - mr).max(0.0),
            (Len::Auto, ..) => {
                let (_, max) = self.intrinsic(b);
                max.min((cw - ml - mr).max(0.0))
            }
            (w, ..) => w.resolve(cw) + if st.border_box { 0.0 } else { frame },
        };
        let x = left.map(|l| cx + l + ml).or(right.map(|r| cx + cw - r - mr - border_w)).unwrap_or(sx + ml);
        let y = top.map(|t| cy + t + mt).unwrap_or(sy + mt);
        let floats = std::mem::take(&mut self.floats);
        self.block(b, x, y, border_w, Opts { width: Some(border_w), bfc: true, ..Default::default() });
        self.floats = floats;
    }

    /// The content width a table takes inside `avail`.
    fn table_width(&mut self, t: &Table, s: &Style, avail: f32) -> f32 {
        let (mins, maxs) = self.columns(t);
        let (mut sum_min, sum_max): (f32, f32) = (mins.iter().sum(), maxs.iter().sum());
        // At least as wide as a caption's longest word.
        for cap in &t.captions {
            sum_min = sum_min.max(self.intrinsic(cap).0);
        }
        match s.width {
            Len::Auto => sum_max.min(avail).max(sum_min),
            w => w.resolve(avail).max(sum_min),
        }
    }

    /// A box's height when laid out `w` wide (border box), without painting anything.
    fn measure_block(&mut self, b: &BlockBox, w: f32) -> f32 {
        let (ni, nl) = (self.items.len(), self.links.len());
        let floats = self.floats.clone();
        let h = self.block(b, 0.0, 0.0, w, Opts { bfc: true, width: Some(w), ..Default::default() });
        self.items.truncate(ni);
        self.links.truncate(nl);
        self.floats = floats;
        h
    }

    /// Lays out a table's captions and rows in a box `width` wide; returns the height.
    fn table(&mut self, t: &Table, x: f32, y: f32, width: f32) -> f32 {
        let (mins, maxs) = self.columns(t);
        let (sum_min, sum_max): (f32, f32) = (mins.iter().sum(), maxs.iter().sum());
        let n = mins.len();
        let widths: Vec<f32> = if n == 0 {
            Vec::new()
        } else if width >= sum_max {
            // Room to spare: share it out in proportion to each column's content.
            (0..n).map(|k| if sum_max > 0.0 { maxs[k] + (width - sum_max) * maxs[k] / sum_max } else { width / n as f32 }).collect()
        } else {
            let spread = sum_max - sum_min;
            (0..n).map(|k| mins[k] + if spread > 0.0 { (width - sum_min).max(0.0) * (maxs[k] - mins[k]) / spread } else { 0.0 }).collect()
        };
        let mut cursor = y;
        let place_captions = |me: &mut Self, cursor: &mut f32, bottom: bool| {
            for cap in t.captions.iter().filter(|c| me.styles[c.node as usize].caption_bottom == bottom) {
                let (mt, mb) = vmargins(&me.styles[cap.node as usize], width);
                *cursor += mt;
                *cursor += me.block(cap, x, *cursor, width, Opts::default());
                *cursor += mb;
            }
        };
        place_captions(self, &mut cursor, false);
        for (row, cells) in &t.rows {
            let mut row_h = 0f32;
            let mut col = 0;
            let mut placed = Vec::new();
            for (cell, span) in cells {
                let span = (*span).min(n.saturating_sub(col)).max(1);
                let cx = x + widths[..col.min(n)].iter().sum::<f32>();
                let cw: f32 = widths[col.min(n)..(col + span).min(n)].iter().sum();
                row_h = row_h.max(self.measure_block(cell, cw));
                placed.push((cell, cx, cw));
                col += span;
            }
            let row_bg = row.map(|r| self.styles[r as usize].background).filter(|c| c.a > 0);
            if let Some(color) = row_bg {
                self.items.push(Item::Rect { x, y: cursor, w: width, h: row_h, color, radius: 0.0 });
            }
            for (cell, cx, cw) in placed {
                self.block(cell, cx, cursor, cw, Opts { min_h: row_h, bfc: true, ..Default::default() });
            }
            cursor += row_h;
        }
        place_captions(self, &mut cursor, true);
        cursor - y
    }

    // ------------------------------------------------------------ flexbox and grid

    /// Lays out a flex container's items in a content box `width` wide; returns the height used.
    fn flex(&mut self, items: &[BlockBox], s: &Style, x: f32, y: f32, width: f32) -> f32 {
        if items.is_empty() {
            return 0.0;
        }
        if s.column {
            return self.flex_column(items, s, x, y, width);
        }
        struct It<'b> {
            b: &'b BlockBox,
            base: f32,
            min: f32,
            size: f32,
            ml: f32,
            mr: f32,
            auto_l: bool,
            auto_r: bool,
            mt: f32,
            mb: f32,
            grow: f32,
            shrink: f32,
            stretch: bool,
            align: AlignItems,
        }
        let mut its: Vec<It> = Vec::new();
        for b in items {
            let st = self.styles[b.node as usize].clone();
            let frame = st.padding[1].resolve(width) + st.padding[3].resolve(width) + st.border_width[1] + st.border_width[3];
            let outer = |l: Len| l.resolve(width) + if st.border_box { 0.0 } else { frame };
            let (imin, imax) = self.intrinsic(b);
            let specified = match (st.basis, st.width) {
                (Len::Auto, Len::Auto) => None,
                (Len::Auto, w) => Some(outer(w)),
                (basis, _) => Some(outer(basis)),
            };
            let mut base = specified.unwrap_or(imax);
            if st.max_width != Len::Auto {
                base = base.min(outer(st.max_width));
            }
            let min = match st.min_width {
                Len::Auto => {
                    if specified.is_some() {
                        imin.min(base)
                    } else {
                        imin
                    }
                }
                m => outer(m),
            };
            let (mt, mb) = vmargins(&st, width);
            its.push(It {
                b,
                base,
                min,
                size: base.max(min),
                ml: st.margin[3].resolve(width),
                mr: st.margin[1].resolve(width),
                auto_l: st.margin[3] == Len::Auto,
                auto_r: st.margin[1] == Len::Auto,
                mt,
                mb,
                grow: st.grow,
                shrink: st.shrink,
                stretch: st.height == Len::Auto,
                align: st.align_self.unwrap_or(s.align_items),
            });
        }
        // Lines: one, or as many as wrapping needs.
        let mut lines: Vec<Vec<It>> = Vec::new();
        let mut cur: Vec<It> = Vec::new();
        let mut used = 0.0;
        for it in its {
            let outer = it.size + it.ml + it.mr;
            if s.wrap && !cur.is_empty() && used + s.column_gap + outer > width + FIT {
                lines.push(std::mem::take(&mut cur));
                used = 0.0;
            }
            used += outer + if cur.is_empty() { 0.0 } else { s.column_gap };
            cur.push(it);
        }
        lines.push(cur);
        let mut top = y;
        for (li, mut line) in lines.into_iter().enumerate() {
            if s.reverse {
                line.reverse();
            }
            let n = line.len();
            let gaps = s.column_gap * (n.saturating_sub(1)) as f32;
            let outer = |l: &[It]| l.iter().map(|i| i.size + i.ml + i.mr).sum::<f32>();
            let mut free = width - outer(&line) - gaps;
            if free > 0.0 {
                let total: f32 = line.iter().map(|i| i.grow).sum();
                if total > 0.0 {
                    for i in line.iter_mut() {
                        i.size += free * i.grow / total;
                    }
                    free = 0.0;
                }
            } else if free < 0.0 {
                // Shrink in proportion to shrink × base, never below the minimum.
                let mut frozen = vec![false; n];
                for _ in 0..n {
                    let weights: f32 = line.iter().zip(&frozen).filter(|(_, f)| !**f).map(|(i, _)| i.shrink * i.base).sum();
                    if weights <= 0.0 || free >= -FIT {
                        break;
                    }
                    let over = -free;
                    for (k, i) in line.iter_mut().enumerate() {
                        if frozen[k] {
                            continue;
                        }
                        i.size -= over * i.shrink * i.base / weights;
                        if i.size <= i.min {
                            i.size = i.min;
                            frozen[k] = true;
                        }
                    }
                    free = width - outer(&line) - gaps;
                }
            }
            // Auto margins take the free space first; then justify-content.
            let autos = line.iter().map(|i| i.auto_l as usize + i.auto_r as usize).sum::<usize>();
            let auto_each = if autos > 0 && free > 0.0 { free / autos as f32 } else { 0.0 };
            let free = if autos > 0 { 0.0 } else { free.max(0.0) };
            let nf = n as f32;
            let justify = if s.reverse && s.justify == Justify::Start { Justify::End } else { s.justify };
            let (start, between) = match justify {
                Justify::Start => (0.0, 0.0),
                Justify::End => (free, 0.0),
                Justify::Center => (free / 2.0, 0.0),
                Justify::Between => (0.0, if n > 1 { free / (nf - 1.0) } else { 0.0 }),
                Justify::Around => (free / nf / 2.0, free / nf),
                Justify::Evenly => (free / (nf + 1.0), free / (nf + 1.0)),
            };
            // Cross size: each item's height at its width; the line is as tall as the tallest.
            let heights: Vec<f32> = line.iter().map(|i| i.size).collect::<Vec<_>>().into_iter().zip(line.iter().map(|i| i.b)).map(|(w, b)| self.measure_block(b, w)).collect();
            let line_h = line.iter().zip(&heights).map(|(i, h)| h + i.mt + i.mb).fold(0.0, f32::max);
            if li > 0 {
                top += s.row_gap;
            }
            let mut cx = x + start;
            for (i, h) in line.iter().zip(heights) {
                cx += i.ml + if i.auto_l { auto_each } else { 0.0 };
                let room = line_h - i.mt - i.mb;
                let (dy, min_h) = match i.align {
                    AlignItems::Stretch if i.stretch => (0.0, room),
                    AlignItems::Center => ((room - h) / 2.0, 0.0),
                    AlignItems::End => (room - h, 0.0),
                    _ => (0.0, 0.0),
                };
                self.block(i.b, cx, top + i.mt + dy, i.size, Opts { width: Some(i.size), min_h, bfc: true });
                cx += i.size + i.mr + if i.auto_r { auto_each } else { 0.0 } + s.column_gap + between;
            }
            top += line_h;
        }
        top - y
    }

    /// A column flex container: items stack, each as wide as `align` says.
    fn flex_column(&mut self, items: &[BlockBox], s: &Style, x: f32, y: f32, width: f32) -> f32 {
        let mut order: Vec<&BlockBox> = items.iter().collect();
        if s.reverse {
            order.reverse();
        }
        let mut top = y;
        for (k, b) in order.into_iter().enumerate() {
            if k > 0 {
                top += s.row_gap;
            }
            let st = self.styles[b.node as usize].clone();
            let (mt, mb) = vmargins(&st, width);
            let align = st.align_self.unwrap_or(s.align_items);
            let (ml, mr) = (st.margin[3].resolve(width), st.margin[1].resolve(width));
            let avail = (width - ml - mr).max(0.0);
            let h = if st.width != Len::Auto || align == AlignItems::Stretch {
                self.block(b, x, top + mt, width, Opts { bfc: true, ..Default::default() })
            } else {
                let (_, max) = self.intrinsic(b);
                let w = max.min(avail);
                let dx = match align {
                    AlignItems::Center => (avail - w) / 2.0,
                    AlignItems::End => avail - w,
                    _ => 0.0,
                };
                self.block(b, x + ml + dx, top + mt, w, Opts { width: Some(w), bfc: true, ..Default::default() })
            };
            top += mt + h + mb;
        }
        top - y
    }

    /// Lays out a grid container's items into its columns, row by row.
    fn grid(&mut self, items: &[BlockBox], s: &Style, x: f32, y: f32, width: f32) -> f32 {
        let gap = s.column_gap;
        let mut tracks = s.columns.clone();
        if let Some(min) = tracks.iter().find_map(|t| if let Track::AutoFill(m) = t { Some(*m) } else { None }) {
            let n = (((width + gap) / (min + gap)).floor() as usize).clamp(1, 64);
            tracks = vec![Track::Fr(1.0); n];
        }
        let area_cols = s.areas.iter().map(|r| r.len()).max().unwrap_or(0);
        while tracks.len() < area_cols.max(1) {
            tracks.push(Track::Fr(1.0));
        }
        let n = tracks.len();
        let avail = (width - gap * (n - 1) as f32).max(0.0);
        let fixed: f32 = tracks.iter().map(|t| if let Track::Len(l) = t { l.resolve(width) } else { 0.0 }).sum();
        let frs: f32 = tracks.iter().map(|t| if let Track::Fr(f) = t { *f } else { 0.0 }).sum();
        let cols: Vec<f32> = tracks
            .iter()
            .map(|t| match t {
                Track::Len(l) => l.resolve(width),
                Track::Fr(f) => ((avail - fixed).max(0.0) * f / frs.max(f32::MIN_POSITIVE)).max(0.0),
                Track::AutoFill(_) => 0.0,
            })
            .collect();
        let col_x = |c: usize| -> f32 { cols[..c].iter().sum::<f32>() + gap * c as f32 };
        let span_w = |c: usize, span: usize| -> f32 { cols[c..c + span].iter().sum::<f32>() + gap * (span - 1) as f32 };
        // Placement: named areas and explicit columns first, then everything
        // else in the first free cells, row by row.
        let mut taken: Vec<Vec<bool>> = Vec::new();
        let occupy = |taken: &mut Vec<Vec<bool>>, r: usize, c: usize, span: usize| {
            while taken.len() <= r {
                taken.push(vec![false; n]);
            }
            for k in c..(c + span).min(n) {
                taken[r][k] = true;
            }
        };
        let free = |taken: &Vec<Vec<bool>>, r: usize, c: usize, span: usize| -> bool { taken.get(r).map(|row| row[c..(c + span).min(n)].iter().all(|t| !t)).unwrap_or(true) };
        let mut placed_at: Vec<Option<(usize, usize, usize)>> = vec![None; items.len()];
        for (k, b) in items.iter().enumerate() {
            let st = &self.styles[b.node as usize];
            if let Some(name) = &st.grid_area {
                let cells: Vec<(usize, usize)> =
                    s.areas.iter().enumerate().flat_map(|(r, row)| row.iter().enumerate().filter(|(_, x)| *x == name).map(move |(c, _)| (r, c))).collect();
                if let (Some(&(r0, _)), Some(c0), Some(c1)) = (cells.first(), cells.iter().map(|x| x.1).min(), cells.iter().map(|x| x.1).max()) {
                    for &(r, c) in &cells {
                        occupy(&mut taken, r, c, 1);
                    }
                    placed_at[k] = Some((r0, c0, c1 - c0 + 1));
                }
            }
        }
        for (k, b) in items.iter().enumerate() {
            if placed_at[k].is_some() {
                continue;
            }
            let st = &self.styles[b.node as usize];
            if let Some((a, e)) = st.grid_column {
                let line = |v: i32| if v > 0 { v } else { n as i32 + 2 + v };
                let start = line(a).clamp(1, n as i32);
                let end = if e == 0 { start + st.column_span as i32 } else { line(e) };
                let (c, span) = ((start - 1) as usize, ((end - start).max(1) as usize).min(n - (start - 1) as usize));
                let mut r = 0;
                while !free(&taken, r, c, span) {
                    r += 1;
                }
                occupy(&mut taken, r, c, span);
                placed_at[k] = Some((r, c, span));
            }
        }
        let (mut r, mut c) = (0usize, 0usize);
        for (k, b) in items.iter().enumerate() {
            if placed_at[k].is_some() {
                continue;
            }
            let span = self.styles[b.node as usize].column_span.min(n);
            loop {
                if c + span > n {
                    r += 1;
                    c = 0;
                }
                if free(&taken, r, c, span) {
                    break;
                }
                c += 1;
            }
            occupy(&mut taken, r, c, span);
            placed_at[k] = Some((r, c, span));
            c += span;
        }
        let nrows = placed_at.iter().flatten().map(|p| p.0 + 1).max().unwrap_or(0);
        let mut rows: Vec<Vec<(&BlockBox, usize, usize)>> = vec![Vec::new(); nrows];
        for (k, b) in items.iter().enumerate() {
            if let Some((r, c, span)) = placed_at[k] {
                rows[r].push((b, c, span));
            }
        }
        let mut top = y;
        let mut first_row = true;
        for row in rows.iter().filter(|r| !r.is_empty()) {
            if !first_row {
                top += s.row_gap;
            }
            first_row = false;
            let mut placed = Vec::new();
            let mut row_h = 0f32;
            for &(b, c, span) in row {
                let st = self.styles[b.node as usize].clone();
                let w = span_w(c, span);
                let (ml, mr) = (st.margin[3].resolve(w), st.margin[1].resolve(w));
                let (mt, mb) = vmargins(&st, w);
                let iw = (w - ml - mr).max(0.0);
                let h = self.measure_block(b, iw);
                row_h = row_h.max(h + mt + mb);
                placed.push((b, x + col_x(c) + ml, iw, mt, mb, st.height == Len::Auto && st.align_self.unwrap_or(s.align_items) == AlignItems::Stretch));
            }
            for (b, bx, iw, mt, mb, stretch) in placed {
                let min_h = if stretch { row_h - mt - mb } else { 0.0 };
                self.block(b, bx, top + mt, iw, Opts { width: Some(iw), min_h, bfc: true });
            }
            top += row_h;
        }
        top - y
    }

    // ------------------------------------------------------------ inline content

    /// Words, spaces, breaks, box edges, images and floats, in order.
    fn atoms(&mut self, run: &'r [Inline], cb_w: f32) -> Vec<Atom<'r>> {
        let mut atoms: Vec<Atom<'r>> = Vec::new();
        let mut stack: Vec<NodeId> = Vec::new();
        let mut inside: Rc<[NodeId]> = Rc::from(Vec::new());
        let mut last_space = true; // drop leading spaces
        let atom = |kind: Kind, text: String, w: f32, style: NodeId, link: Option<usize>, inside: &Rc<[NodeId]>| Atom {
            kind,
            text,
            w,
            style,
            link,
            inside: inside.clone(),
            image: None,
            float: None,
        };
        for item in run {
            match item {
                Inline::Break { style } => {
                    atoms.push(atom(Kind::Break, String::new(), 0.0, *style, None, &inside));
                    last_space = true;
                }
                Inline::Open(node) => {
                    stack.push(*node);
                    inside = Rc::from(stack.clone());
                    let s = &self.styles[*node as usize];
                    let w = s.margin[3].resolve(cb_w) + s.border_width[3] + s.padding[3].resolve(cb_w);
                    atoms.push(atom(Kind::Open, String::new(), w, *node, None, &inside));
                }
                Inline::Close(node) => {
                    let s = &self.styles[*node as usize];
                    let w = s.margin[1].resolve(cb_w) + s.border_width[1] + s.padding[1].resolve(cb_w);
                    atoms.push(atom(Kind::Close, String::new(), w, *node, None, &inside));
                    stack.pop();
                    inside = Rc::from(stack.clone());
                }
                Inline::Image { node, img, link } => {
                    if let Some((w, h)) = self.replaced_size(*node, img, cb_w) {
                        let s = &self.styles[*node as usize];
                        let (ml, mr) = (s.margin[3].resolve(cb_w), s.margin[1].resolve(cb_w));
                        let mut a = atom(Kind::Image, String::new(), ml + w + mr, *node, *link, &inside);
                        a.image = Some((ml, w, h, img.src.as_str()));
                        atoms.push(a);
                        last_space = false;
                    }
                }
                Inline::Float(b) => {
                    let mut a = atom(Kind::Float, String::new(), 0.0, b.node, None, &inside);
                    a.float = Some(b);
                    atoms.push(a);
                }
                Inline::Abs(b) => {
                    let mut a = atom(Kind::Abs, String::new(), 0.0, b.node, None, &inside);
                    a.float = Some(b);
                    atoms.push(a);
                }
                Inline::Text { text, style, link } => {
                    let s = self.styles[*style as usize].clone();
                    let font = font_of(&s);
                    let text = transform(text, s.transform);
                    let keep = matches!(s.space, Space::Pre | Space::PreWrap);
                    let mut word = String::new();
                    for c in text.chars().chain(std::iter::once('\u{0}')) {
                        let end = c == '\u{0}';
                        let ws = !end && c.is_whitespace() && c != '\u{a0}';
                        if (end || ws || (keep && c == '\n')) && !word.is_empty() {
                            let w = self.measure(&word, &font);
                            atoms.push(atom(Kind::Word, std::mem::take(&mut word), w, *style, *link, &inside));
                        }
                        if end {
                            break;
                        }
                        if keep && c == '\n' {
                            atoms.push(atom(Kind::Break, String::new(), 0.0, *style, None, &inside));
                        } else if ws {
                            if keep {
                                let sp = if c == '\t' { "        " } else { " " };
                                let w = self.measure(sp, &font);
                                atoms.push(atom(Kind::Space, sp.into(), w, *style, *link, &inside));
                            } else if !last_space {
                                let w = self.measure(" ", &font);
                                atoms.push(atom(Kind::Space, " ".into(), w, *style, *link, &inside));
                                last_space = true;
                            }
                        } else {
                            word.push(c);
                            last_space = false;
                        }
                    }
                }
            }
        }
        atoms
    }

    fn collapsible(&self, a: &Atom) -> bool {
        a.kind == Kind::Space && matches!(self.styles[a.style as usize].space, Space::Normal | Space::NoWrap)
    }

    /// Breaks a run of inline content into lines at (x, y), `width` wide (less
    /// any floats beside them); returns the height used.
    fn inline(&mut self, run: &'r [Inline], x: f32, y: f32, width: f32, block: NodeId) -> f32 {
        let bs = self.styles[block as usize].clone();
        let atoms = self.atoms(run, width);
        let pre = |s: &Style| matches!(s.space, Space::Pre | Space::PreWrap);
        if !atoms.iter().any(|a| matches!(a.kind, Kind::Word | Kind::Image | Kind::Break | Kind::Float | Kind::Abs) || (a.kind == Kind::Space && pre(&self.styles[a.style as usize]))) {
            return 0.0;
        }
        let mut queue: VecDeque<Atom<'r>> = atoms.into();
        let mut top = y;
        while !queue.is_empty() {
            let est = bs.line_height;
            // Too narrow beside the floats for the next word or image: go below them.
            let (mut left, mut right) = self.span(top, est, x, width);
            for _ in 0..64 {
                let need = queue.iter().find(|a| matches!(a.kind, Kind::Word | Kind::Image)).map(|a| a.w).unwrap_or(0.0);
                if need <= right - left + FIT {
                    break;
                }
                match self.next_float_bottom(top, est) {
                    Some(b) if b > top => {
                        top = b;
                        (left, right) = self.span(top, est, x, width);
                    }
                    _ => break,
                }
            }
            let mut line: Vec<Atom<'r>> = Vec::new();
            let mut line_w = 0.0;
            let mut split = false;
            while let Some(a) = queue.pop_front() {
                let space = self.styles[a.style as usize].space;
                let wraps = matches!(space, Space::Normal | Space::PreWrap);
                match a.kind {
                    Kind::Float => {
                        self.place_float(a.float.expect("float atom"), top, x, width);
                        (left, right) = self.span(top, est, x, width);
                        continue;
                    }
                    Kind::Abs => {
                        self.place_abs(a.float.expect("abs atom"), left + line_w, top);
                        continue;
                    }
                    Kind::Break => {
                        line.push(a);
                        break;
                    }
                    Kind::Space if self.collapsible(&a) && !line.iter().any(|l| l.content()) => continue,
                    Kind::Close => {
                        line_w += a.w;
                        line.push(a);
                        continue;
                    }
                    _ => {}
                }
                let avail = right - left;
                let has_content = line.iter().any(|l| l.content());
                if wraps && has_content && line_w + a.w > avail + FIT {
                    if !self.collapsible(&a) {
                        queue.push_front(a);
                    }
                    // An opening edge belongs with the word after it.
                    while line.last().map(|l| l.kind == Kind::Open).unwrap_or(false) {
                        queue.push_front(line.pop().expect("checked"));
                    }
                    break;
                }
                if a.kind == Kind::Word && wraps && line_w + a.w > avail + FIT && !has_content {
                    // A word longer than the line (a URL): break it where it must.
                    let font = font_of(&self.styles[a.style as usize].clone());
                    let mut piece = String::new();
                    let mut rest = a.text.chars().peekable();
                    while let Some(&c) = rest.peek() {
                        let mut next = piece.clone();
                        next.push(c);
                        if !piece.is_empty() && line_w + self.measure(&next, &font) > avail + FIT {
                            break;
                        }
                        piece = next;
                        rest.next();
                    }
                    let tail: String = rest.collect();
                    let pw = self.measure(&piece, &font);
                    let mut head = Atom { text: piece, w: pw, ..a };
                    if !tail.is_empty() {
                        let tw = self.measure(&tail, &font);
                        let more = Atom { kind: Kind::Word, text: tail, w: tw, style: head.style, link: head.link, inside: head.inside.clone(), image: None, float: None };
                        queue.push_front(more);
                        split = true;
                        head.kind = Kind::Word;
                        line.push(head);
                        break;
                    }
                    line_w += pw;
                    line.push(head);
                    continue;
                }
                line_w += a.w;
                line.push(a);
            }
            if line.iter().any(|a| a.content() || a.kind == Kind::Break) {
                let end = if line.last().map(|a| a.kind == Kind::Break).unwrap_or(false) {
                    After::Break
                } else if split {
                    After::Inline // a word broken across lines rejoins as one
                } else if queue.iter().any(|a| a.content() || a.kind == Kind::Break) {
                    After::Wrap
                } else {
                    After::BlockEnd
                };
                top += self.paint_line(line, top, left, right - left, &bs, end);
            }
        }
        top - y
    }

    /// Paints one line box; returns its height.
    fn paint_line(&mut self, mut line: Vec<Atom<'r>>, top: f32, left: f32, avail: f32, bs: &Style, end: After) -> f32 {
        // Trailing collapsible spaces go (also before closing edges and a break).
        loop {
            let last = line.iter().rposition(|a| !matches!(a.kind, Kind::Close | Kind::Break));
            match last {
                Some(i) if self.collapsible(&line[i]) => {
                    line.remove(i);
                }
                _ => break,
            }
        }
        let bf = font_of(bs);
        let (ba, bd) = (self.fonts.ascent(&bf), self.fonts.descent(&bf));
        let mut above = (bs.line_height - ba - bd) / 2.0 + ba;
        let mut below = bs.line_height - above;
        let mid = bf.size * 0.25; // about half an x-height
        for a in &line {
            let s = &self.styles[a.style as usize];
            match a.kind {
                Kind::Word | Kind::Space => {
                    let f = font_of(s);
                    let (asc, desc) = (self.fonts.ascent(&f), self.fonts.descent(&f));
                    let half = (s.line_height - asc - desc) / 2.0;
                    above = above.max(half + asc - s.shift);
                    below = below.max(half + desc + s.shift);
                }
                Kind::Image => {
                    let h = a.image.map(|i| i.2).unwrap_or(0.0);
                    if s.valign == VAlign::Middle {
                        above = above.max(h / 2.0 + mid);
                        below = below.max(h / 2.0 - mid);
                    } else {
                        above = above.max(h - s.shift);
                        below = below.max(s.shift);
                    }
                }
                _ => {}
            }
        }
        let used: f32 = line.iter().map(|a| a.w).sum();
        let dx = match bs.align {
            Align::Left => 0.0,
            Align::Center => ((avail - used) / 2.0).max(0.0),
            Align::Right => (avail - used).max(0.0),
        };
        let baseline = top + above;
        let line_h = above + below;
        let mut xs = Vec::with_capacity(line.len());
        let mut cx = left + dx;
        for a in &line {
            xs.push(cx);
            cx += a.w;
        }
        // Inline box backgrounds, outermost first, under this line's content.
        let mut boxes: Vec<(usize, NodeId, usize, usize)> = Vec::new(); // (depth, node, first, last)
        for (i, a) in line.iter().enumerate() {
            for (depth, &e) in a.inside.iter().enumerate() {
                match boxes.iter_mut().find(|b| b.1 == e) {
                    Some(b) => b.3 = i,
                    None => boxes.push((depth, e, i, i)),
                }
            }
        }
        boxes.sort_by_key(|b| b.0);
        for (_, e, i0, i1) in boxes {
            let s = self.styles[e as usize].clone();
            let x0 = xs[i0] + if line[i0].kind == Kind::Open && line[i0].style == e { px_only(s.margin[3]) } else { 0.0 };
            let x1 = xs[i1] + line[i1].w - if line[i1].kind == Kind::Close && line[i1].style == e { px_only(s.margin[1]) } else { 0.0 };
            let f = font_of(&s);
            let y0 = baseline + s.shift - self.fonts.ascent(&f) - px_only(s.padding[0]) - s.border_width[0];
            let y1 = baseline + s.shift + self.fonts.descent(&f) + px_only(s.padding[2]) + s.border_width[2];
            self.hits.push(Hit { x: x0, y: y0.min(top), w: (x1 - x0).max(0.0), h: (y1 - y0).max(line_h), node: e });
            if s.background.a == 0 {
                continue;
            }
            self.items.push(Item::Rect { x: x0, y: y0, w: (x1 - x0).max(0.0), h: y1 - y0, color: s.background, radius: s.radius });
        }
        // Text (atoms sharing a style and link draw as one run) and images.
        let mut last_text = None;
        let mut i = 0;
        while i < line.len() {
            let a = &line[i];
            match a.kind {
                Kind::Image => {
                    let s = &self.styles[a.style as usize];
                    if let Some((ml, w, h, src)) = a.image {
                        let y = if s.valign == VAlign::Middle { baseline - mid - h / 2.0 } else { baseline + s.shift - h };
                        if !s.hidden && !src.is_empty() {
                            self.items.push(Item::Image { x: xs[i] + ml, y, w, h, src: src.to_string(), radius: s.radius });
                        }
                        self.hits.push(Hit { x: xs[i] + ml, y, w, h, node: a.style });
                        if let Some(l) = a.link {
                            self.links.push(Link { x: xs[i] + ml, y, w, h, href: self.hrefs[l].clone() });
                        }
                    }
                    i += 1;
                }
                Kind::Word | Kind::Space => {
                    let (style, link, start) = (a.style, a.link, i);
                    let mut text = String::new();
                    let mut w = 0.0;
                    while i < line.len() && matches!(line[i].kind, Kind::Word | Kind::Space) && line[i].style == style && line[i].link == link {
                        text.push_str(&line[i].text);
                        w += line[i].w;
                        i += 1;
                    }
                    let s = &self.styles[style as usize];
                    let font = font_of(s);
                    if !s.hidden {
                        last_text = Some(self.items.len());
                        self.items.push(Item::Text { x: xs[start], y: baseline + s.shift, text, font, color: s.color, underline: s.underline, strike: s.strike, after: After::Inline });
                    }
                    self.hits.push(Hit { x: xs[start], y: top, w, h: line_h, node: style });
                    if let Some(l) = link {
                        self.links.push(Link { x: xs[start], y: top, w, h: line_h, href: self.hrefs[l].clone() });
                    }
                }
                _ => i += 1,
            }
        }
        if let Some(Item::Text { after, .. }) = last_text.and_then(|k| self.items.get_mut(k)) {
            *after = end;
        }
        line_h
    }
}

/// Lays out `dom` (styled by `styles`) for a viewport `width` px wide.
pub fn layout(dom: &Dom, styled: &css_style::Styled, width: f32, fonts: &dyn Fonts, images: &dyn Images) -> Page {
    let styles = &styled.styles;
    let mut builder = Builder { dom, styles, before: &styled.before, after: &styled.after, anon: Vec::new(), hrefs: Vec::new() };
    let root = builder.block(0, 0);
    let mut all: Vec<Style> = styles.clone();
    all.extend(std::mem::take(&mut builder.anon));
    let mut ctx = Ctx { styles: &all, fonts, images, hrefs: builder.hrefs, items: Vec::new(), links: Vec::new(), hits: Vec::new(), cache: HashMap::new(), floats: Vec::new(), cbs: vec![(0.0, 0.0, width)] };
    // The canvas takes the root's background, or else the body's.
    let bg_of = |tag: &str| dom.find(tag).map(|id| styles[id as usize].background).filter(|c| c.a > 0);
    let background = bg_of("html").or_else(|| bg_of("body")).unwrap_or(Color::rgb(255, 255, 255));
    let height = ctx.block(&root, 0.0, 0.0, width, Opts { bfc: true, ..Default::default() });
    Page { width, height, background, items: ctx.items, links: ctx.links, hits: ctx.hits }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fonts::ApproxFonts;
    use crate::style;
    use volant_core::html;

    struct FixedImages;
    impl Images for FixedImages {
        fn size(&self, src: &str) -> Option<(f32, f32)> {
            (src == "wide.png").then_some((800.0, 400.0))
        }
    }

    fn page(html: &str, css: &str, width: f32) -> Page {
        let html = format!("<html><body>{html}</body></html>");
        let dom = html::parse(&html);
        let styled = style::compute(&dom, &[css.to_string()], &crate::css::Env { width, dark: false });
        layout(&dom, &styled, width, &ApproxFonts, &FixedImages)
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

    fn words(p: &Page) -> Vec<String> {
        texts(p).into_iter().map(|t| t.0).collect()
    }

    fn images(p: &Page) -> Vec<(f32, f32, f32, f32)> {
        p.items
            .iter()
            .filter_map(|i| match i {
                Item::Image { x, y, w, h, .. } => Some((*x, *y, *w, *h)),
                _ => None,
            })
            .collect()
    }

    const BASE: &str = "body { margin: 0; font: 10px/20px sans-serif } p { margin: 10px 0 }";

    #[test]
    fn wraps_and_stacks() {
        let p = page("<p>aaa bbb ccc</p><p>ddd</p>", BASE, 45.0);
        let t = texts(&p);
        // "aaa bbb" is 5.56*3*2 + 2.78 = 36.14 px; with " ccc" it would pass 45.
        assert_eq!(words(&p), ["aaa bbb", "ccc", "ddd"]);
        assert_eq!(t[0].2, 10.0 + 5.0 + 8.0, "first baseline: margin, half-leading (20 - 10) / 2, ascent 0.8 em");
        assert_eq!(t[1].2 - t[0].2, 20.0, "line height");
        assert_eq!(t[2].2, 10.0 + 40.0 + 10.0 + 13.0, "collapsed margins between the paragraphs");
        assert_eq!(p.height, 10.0 + 40.0 + 10.0 + 20.0 + 10.0);
    }

    #[test]
    fn whitespace_inline_and_links() {
        let p = page("<p>  Hello\n  <b>bold</b>   <a href='/x'>link</a> <br>next </p>", "body { margin: 0; font-size: 10px }", 500.0);
        let t = texts(&p);
        assert_eq!(words(&p), ["Hello ", "bold", " ", "link", "next"]);
        assert_eq!(p.links.len(), 1);
        assert_eq!(p.links[0].href, "/x");
        assert!(t[4].2 > t[0].2, "<br> starts a new line");
    }

    #[test]
    fn boxes_lists_pre_centre() {
        let css = "body { margin: 0; font-size: 10px } div { width: 100px; margin: 0 auto; padding: 5px; background: #eee; border-left: 2px solid red; border-radius: 6px }";
        let p = page("<div>x</div>", css, 300.0);
        let bg = p.items.iter().find_map(|i| match i {
            Item::Rect { x, w, color, radius, .. } if color.r == 238 => Some((*x, *w, *radius)),
            _ => None,
        });
        assert_eq!(bg, Some((94.0, 112.0, 6.0)), "centred: (300 - 112) / 2");
        assert_eq!(words(&page("<pre>a  b\nc</pre>", css, 300.0)), ["a  b", "c"]);
        assert_eq!(words(&page("<ol start=3><li>one<li>two</ol><ul><li>dot</ul>", css, 300.0)), ["3.", "one", "4.", "two", "•", "dot"]);
        let t = texts(&page("<p style='text-align:center'>hi</p>", "body{margin:0;font-size:10px}", 100.0));
        assert!((t[0].1 - (100.0 - 7.78) / 2.0).abs() < 0.01);
        let long = page("<p>https://example.com/a/very/long/address/that/cannot/fit</p>", css, 60.0);
        let f = Font { family: style::Family::Sans, size: 10.0, weight: 400, italic: false };
        assert!(texts(&long).len() > 3 && texts(&long).iter().all(|(s, ..)| ApproxFonts.width(s, &f) <= 60.0));
        assert_eq!(words(&page("<p>a<span style='display:none'>hidden</span><script>x()</script></p>", css, 100.0)), ["a"]);
    }

    #[test]
    fn inline_boxes_and_superscripts() {
        let css = format!("{BASE} code {{ background: #eee; padding: 0 4px; border-radius: 3px }}");
        let p = page("<p>run <code>ls -l</code> now<sup>2</sup></p>", &css, 500.0);
        let t = texts(&p);
        let x_of = |w: &str| t.iter().find(|x| x.0.contains(w)).unwrap().1;
        let run_w = ApproxFonts.width("run ", &Font { family: style::Family::Sans, size: 10.0, weight: 400, italic: false });
        // The code text starts after its 4px left padding; its background starts at the edge.
        assert!((x_of("ls") - (run_w + 4.0)).abs() < 0.01);
        let bg = p.items.iter().find_map(|i| match i {
            Item::Rect { x, w, radius, .. } => Some((*x, *w, *radius)),
            _ => None,
        });
        let code_w = ApproxFonts.width("ls -l", &Font { family: style::Family::Mono, size: 10.0, weight: 400, italic: false });
        let (bx, bw, br) = bg.unwrap();
        assert!((bx - run_w).abs() < 0.01 && (bw - (code_w + 8.0)).abs() < 0.01 && br == 3.0, "{bg:?}");
        assert!((x_of(" now") - (run_w + code_w + 8.0)).abs() < 0.01, "text resumes after the right padding");
        let base = t.iter().find(|x| x.0 == " now").unwrap().2;
        let sup = t.iter().find(|x| x.0 == "2").unwrap().2;
        assert!(sup < base - 3.0, "superscripts sit higher: {sup} vs {base}");
    }

    #[test]
    fn images_inline_block_and_scaled() {
        // Block image, scaled to the column by max-width, centred by auto margins.
        let css = format!("{BASE} img.b {{ display: block; max-width: 100%; margin: 0 auto }}");
        let p = page("<img class=b src='wide.png'><p>after</p>", &css, 400.0);
        assert_eq!(images(&p), [(0.0, 0.0, 400.0, 200.0)]);
        assert!(texts(&p)[0].2 > 200.0);
        // Inline image from attributes alone (not loaded): sits on the baseline, takes its space.
        let p = page("<p>a <img src='later.png' width=30 height=40> b</p>", BASE, 400.0);
        let (x, y, w, h) = images(&p)[0];
        let base = texts(&p)[0].2;
        assert_eq!((w, h), (30.0, 40.0));
        assert!((y + h - base).abs() < 0.01 && x > 0.0);
        // Unknown size and nothing to go on: nothing drawn, nothing reserved.
        assert!(images(&page("<p><img src='unknown.png'></p>", BASE, 400.0)).is_empty());
        // Only a width attribute: the height follows the intrinsic ratio.
        assert_eq!(images(&page("<p><img src='wide.png' width=100></p>", BASE, 400.0))[0].3, 50.0);
    }

    #[test]
    fn floats_wrap_text() {
        let css = format!("{BASE} .f {{ float: right; width: 50px; height: 60px; background: #ccc }} .c {{ clear: both }}");
        let p = page("<div class=f></div><p>one two three four five six seven eight</p><div class=c>after</div>", &css, 150.0);
        let t = texts(&p);
        // Lines beside the float are at most 100px wide; the float's box is at the right.
        let float = p.items.iter().find_map(|i| match i {
            Item::Rect { x, y, w, h, .. } => Some((*x, *y, *w, *h)),
            _ => None,
        });
        assert_eq!(float, Some((100.0, 0.0, 50.0, 60.0)));
        let f = Font { family: style::Family::Sans, size: 10.0, weight: 400, italic: false };
        for (s, x, y) in &t {
            if *y < 60.0 {
                assert!(x + ApproxFonts.width(s, &f) <= 100.01, "{s} overlaps the float");
            }
        }
        let after = t.iter().find(|x| x.0 == "after").unwrap();
        assert!(after.2 > 60.0, "clear: both goes below the float");
        // A floated image inside a paragraph, left: text starts beside it.
        let p = page("<p><img src='wide.png' width=40 style='float:left'>text</p>", BASE, 300.0);
        assert_eq!(images(&p)[0].0, 0.0);
        assert!(texts(&p)[0].1 >= 40.0);
    }

    #[test]
    fn tables() {
        let css = format!("{BASE} td, th {{ padding: 2px }} table {{ background: #fff }}");
        let html = "<table><caption>Cap</caption><tr><th>Name</th><th>Speed</th></tr>\
                    <tr><td>Comet</td><td>740 km/h</td></tr><tr><td colspan=2>Both columns</td></tr></table>";
        let p = page(html, &css, 500.0);
        let t = texts(&p);
        let get = |w: &str| t.iter().find(|x| x.0 == w).cloned().unwrap();
        // Columns line up and the second starts where the first ends.
        // First column starts at the cell padding; headers are centred within their columns.
        assert_eq!(get("Comet").1, 2.0);
        assert!(get("Name").1 >= 2.0 && get("Name").1 < get("Speed").1);
        assert!(get("Speed").1 > get("Name").1 + 20.0);
        assert!((get("740 km/h").1 - get("Speed").1).abs() < 25.0, "same column");
        assert!(get("Cap").2 < get("Name").2 && get("Name").2 < get("Comet").2 && get("Comet").2 < get("Both columns").2);
        // caption-side: bottom puts the caption under the rows (Wikipedia's figures).
        let below = page("<table class=b><caption>Under</caption><tr><td>Cell</td></tr></table>", &format!("{css} .b {{ caption-side: bottom }}"), 500.0);
        let tb = texts(&below);
        assert!(tb.iter().find(|x| x.0 == "Under").unwrap().2 > tb.iter().find(|x| x.0 == "Cell").unwrap().2);
        // A narrow container wraps cell text instead of overflowing.
        let narrow = page("<table><tr><td>alpha beta gamma delta</td><td>x</td></tr></table>", &css, 60.0);
        assert!(texts(&narrow).len() > 2);
    }

    #[test]
    fn opacity_overflow_transform() {
        let css = format!("{BASE} .clip {{ height: 20px; overflow: hidden }} .ghost {{ opacity: 0 }} .half {{ opacity: .5; color: #000 }} .moved {{ transform: translate(10px, 50%); height: 40px }}");
        let p = page("<div class=clip>a<br>b<br>c</div><p class=ghost>gone</p><p class=half>faint</p><div class=moved>m</div>", &css, 300.0);
        // The clipped box's content is wrapped in Clip/Unclip.
        let clip = p.items.iter().position(|i| matches!(i, Item::Clip { h, .. } if *h == 20.0)).expect("a clip");
        let unclip = p.items.iter().position(|i| matches!(i, Item::Unclip)).expect("an unclip");
        assert!(clip < unclip);
        let t = texts(&p);
        assert!(!t.iter().any(|x| x.0 == "gone"), "opacity: 0 draws nothing");
        let faint = p.items.iter().find_map(|i| match i {
            Item::Text { text, color, .. } if text == "faint" => Some(color.a),
            _ => None,
        });
        assert_eq!(faint, Some(128));
        let m = t.iter().find(|x| x.0 == "m").unwrap();
        assert_eq!(m.1, 10.0, "translateX");
        assert!(p.hit(m.1 + 1.0, m.2 - 2.0).is_some());
    }

    fn rects(p: &Page) -> Vec<(f32, f32, f32, f32)> {
        p.items
            .iter()
            .filter_map(|i| match i {
                Item::Rect { x, y, w, h, .. } => Some((*x, *y, *w, *h)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn flex_rows() {
        let css = format!("{BASE} .row {{ display: flex; gap: 10px }} .row > div {{ background: #eee; padding: 0 }} .g {{ flex: 1 }} .push {{ margin-left: auto }}");
        // Grow fills the rest: 300 - fixed item widths - gaps.
        let p = page("<div class=row><div>aa</div><div class=g>grow</div><div>bb</div></div>", &css, 300.0);
        let r = rects(&p);
        let aa = ApproxFonts.width("aa", &Font { family: style::Family::Sans, size: 10.0, weight: 400, italic: false });
        assert_eq!(r.len(), 3);
        assert!((r[0].0 - 0.0).abs() < 0.01 && (r[0].2 - aa).abs() < 0.01);
        assert!((r[1].0 - (aa + 10.0)).abs() < 0.01);
        assert!((r[2].0 + r[2].2 - 300.0).abs() < 0.01, "last item ends at the right edge: {r:?}");
        assert!(r.iter().all(|x| x.1 == 0.0), "one line");
        // An auto margin pushes the rest to the end.
        let p = page("<div class=row><div>logo</div><div class=push>a</div><div>b</div></div>", &css, 300.0);
        let r = rects(&p);
        assert!((r[2].0 + r[2].2 - 300.0).abs() < 0.01 && r[1].0 > 200.0);
        // justify-content: space-between and align-items: center.
        let css2 = format!("{BASE} .row {{ display: flex; justify-content: space-between; align-items: center }} .tall {{ height: 60px }}");
        let p = page("<div class=row><div class=tall>t</div><div>a</div></div>", &css2, 300.0);
        let t = texts(&p);
        let a = t.iter().find(|x| x.0 == "a").unwrap();
        assert!(a.1 > 290.0 && a.2 > 30.0, "a sits at the end, centred against the 60px item: {a:?}");
        // Wrapping onto a second line.
        let css3 = format!("{BASE} .row {{ display: flex; flex-wrap: wrap; row-gap: 5px }} .row > div {{ width: 120px; background: #eee }}");
        let r = rects(&page("<div class=row><div>1</div><div>2</div><div>3</div></div>", &css3, 300.0));
        assert_eq!(r.iter().map(|x| (x.0, x.1)).collect::<Vec<_>>(), [(0.0, 0.0), (120.0, 0.0), (0.0, 25.0)]);
        // Columns stack; text directly in a container becomes its own item.
        let css4 = format!("{BASE} .col {{ display: flex; flex-direction: column; align-items: center; gap: 4px }}");
        let t = texts(&page("<div class=col>loose text<span>item</span></div>", &css4, 300.0));
        assert_eq!(t.len(), 2);
        assert!(t[1].2 > t[0].2 + 20.0 && t[1].1 > 100.0, "{t:?}");
    }

    #[test]
    fn grids() {
        let css = format!("{BASE} .g {{ display: grid; grid-template-columns: repeat(3, 1fr); gap: 10px }} .g > div {{ background: #eee }} .wide {{ grid-column: span 2 }}");
        let r = rects(&page("<div class=g><div>1</div><div>2</div><div>3</div><div class=wide>4</div><div>5</div></div>", &css, 320.0));
        let xs: Vec<(f32, f32)> = r.iter().map(|x| (x.0, x.2)).collect();
        assert_eq!(xs, [(0.0, 100.0), (110.0, 100.0), (220.0, 100.0), (0.0, 210.0), (220.0, 100.0)]);
        assert!(r[3].1 > r[0].1, "second row");
        let css2 = format!("{BASE} .g {{ display: grid; grid-template-columns: repeat(auto-fill, minmax(100px, 1fr)) }} .g > div {{ background: #eee }}");
        let r = rects(&page("<div class=g><div>1</div><div>2</div><div>3</div></div>", &css2, 250.0));
        assert_eq!(r.iter().map(|x| (x.0, x.1, x.2)).collect::<Vec<_>>(), [(0.0, 0.0, 125.0), (125.0, 0.0, 125.0), (0.0, 20.0, 125.0)]);
    }

    #[test]
    fn modern_css() {
        // ::before / ::after content, :nth-child, :not, var(), calc(), box-sizing, @media.
        let css = format!(
            "{BASE} :root {{ --accent: #c00; --pad: 6px }} li:nth-child(2n) {{ color: var(--accent) }} li:not(.x)::before {{ content: '→ ' }} \
             a::after {{ content: ' (' attr(href) ')' }} .b {{ box-sizing: border-box; width: calc(50% + 20px); padding: var(--pad); background: #eee }} \
             @media (max-width: 500px) {{ .m {{ display: none }} }} @media (min-width: 501px) {{ .w {{ display: none }} }}"
        );
        let p = page("<ul><li class=x>one</li><li>two</li></ul><a href='/h'>link</a><div class=b>b</div><p class=m>narrow-only hidden</p><p class=w>wide hidden</p>", &css, 400.0);
        let w = words(&p);
        assert!(w.contains(&"→ ".to_string()) && w.iter().filter(|x| x.as_str() == "→ ").count() == 1, "{w:?}");
        assert!(w.contains(&" (/h)".to_string()), "{w:?}");
        let two = p.items.iter().find_map(|i| match i {
            Item::Text { text, color, .. } if text == "two" => Some(*color),
            _ => None,
        });
        assert_eq!(two, Some(Color::rgb(204, 0, 0)), "var() and :nth-child(2n)");
        let bw = p.items.iter().find_map(|i| match i {
            Item::Rect { w, color, .. } if color.r == 238 => Some(*w),
            _ => None,
        });
        assert_eq!(bw, Some(220.0), "calc(50% + 20px) as a border box");
        assert!(w.contains(&"wide hidden".to_string()) && !w.contains(&"narrow-only hidden".to_string()), "media queries: {w:?}");
    }


    #[test]
    fn positioning_and_areas() {
        let css = format!(
            "{BASE} .box {{ position: relative; width: 200px; height: 100px; background: #eee }} \
             .badge {{ position: absolute; top: 5px; right: 10px; width: 30px; background: #c00 }} \
             .nudge {{ position: relative; left: 7px; top: 3px }} \
             .sr {{ position: absolute; clip: rect(1px, 1px, 1px, 1px); width: 1px; height: 1px }} \
             .g {{ display: grid; grid-template-columns: 50px 1fr; grid-template-areas: 'head head' 'side main' }} \
             .h {{ grid-area: head }} .s {{ grid-area: side }} .m {{ grid-area: main }}"
        );
        let p = page("<div class=box><span class=badge>!</span>in box</div><p class=nudge>moved</p><a class=sr href=#c>Skip</a>", &css, 400.0);
        let badge = p.items.iter().find_map(|i| match i {
            Item::Rect { x, y, w, color, .. } if color.r == 204 => Some((*x, *y, *w)),
            _ => None,
        });
        assert_eq!(badge, Some((160.0, 5.0, 30.0)), "top: 5px, right: 10px inside the 200px box");
        let t = texts(&p);
        let moved = t.iter().find(|x| x.0 == "moved").unwrap();
        assert_eq!(moved.1, 7.0, "relative: left 7px");
        assert!(!t.iter().any(|x| x.0 == "Skip"), "visually hidden");
        // Areas: the header spans both columns; main sits in the second column of row 2.
        let p = page("<div class=g><div class=m>main</div><div class=s>side</div><div class=h>head</div></div>", &css, 400.0);
        let t = texts(&p);
        let at = |w: &str| t.iter().find(|x| x.0 == w).map(|x| (x.1, x.2)).unwrap();
        assert_eq!(at("head").0, 0.0);
        assert_eq!(at("side").0, 0.0);
        assert_eq!(at("main").0, 50.0);
        assert!(at("main").1 > at("head").1 && (at("main").1 - at("side").1).abs() < 0.01);
    }

}
