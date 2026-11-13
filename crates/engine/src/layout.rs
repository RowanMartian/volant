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
use crate::style::{self as css_style, BorderStyle, Align, Fit, AlignItems, Clear, Color, Display, Float, Justify, Len, ListStyle, Position, Sizing, Space, Style, AlignContent, Area, BgLayer, BgSize, BgPos, Rep, Track, Transform, VAlign};
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
    /// A background image: tiles `tw` × `th` from (`ox`, `oy`), every `sx` across and `sy`
    /// down (0: no repeat that way), clipped to the (rounded) rectangle.
    Tiled { x: f32, y: f32, w: f32, h: f32, radius: f32, src: String, ox: f32, oy: f32, tw: f32, th: f32, sx: f32, sy: f32 },
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
    /// Boxes that move as the page scrolls (`position: fixed` and `sticky`),
    /// drawn over the rest. Their items, hits and links are ranges of the lists above.
    pub layers: Vec<Layer>,
}

/// A fixed or sticky box: what of the page it is, and how it moves with scrolling.
#[derive(Debug, Clone, PartialEq)]
pub struct Layer {
    pub items: (usize, usize),
    pub hits: (usize, usize),
    pub links: (usize, usize),
    pub kind: LayerKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum LayerKind {
    /// Laid out at the top of the page; stays put in the window (at its `bottom` inset, if it has one instead of `top`).
    Fixed { bottom: Option<f32>, y: f32, h: f32 },
    /// Laid out in the flow at `y`; held at `top` (or `bottom`) from the window's edge
    /// while its parent (`limit`, top and bottom) is in view.
    Sticky { top: Option<f32>, bottom: Option<f32>, y: f32, h: f32, parent: NodeId, limit: (f32, f32) },
}

impl Page {
    /// How far a layer moves down when the page is scrolled to `scroll_y` in a window `view_h` tall.
    pub fn layer_offset(&self, l: &Layer, scroll_y: f32, view_h: f32) -> f32 {
        match l.kind {
            LayerKind::Fixed { bottom: None, .. } => scroll_y,
            LayerKind::Fixed { bottom: Some(b), y, h } => scroll_y + view_h - b - h - y,
            LayerKind::Sticky { top: Some(t), y, h, limit, .. } => (scroll_y + t - y).min(limit.1 - (y + h)).max(0.0),
            LayerKind::Sticky { bottom: Some(b), y, h, limit, .. } => (scroll_y + view_h - b - (y + h)).max(limit.0 - y).min(0.0),
            LayerKind::Sticky { .. } => 0.0,
        }
    }

    /// The innermost node drawn at (x, y) of the page as it shows when scrolled to
    /// `scroll_y` (fixed and sticky boxes first, as they're drawn on top).
    pub fn hit_scrolled(&self, x: f32, y: f32, scroll_y: f32, view_h: f32) -> Option<NodeId> {
        let smallest = |hs: &mut dyn Iterator<Item = (&Hit, f32)>| {
            hs.filter(|(h, dy)| x >= h.x && x < h.x + h.w && y >= h.y + dy && y < h.y + dy + h.h)
                .min_by(|a, b| (a.0.w * a.0.h).partial_cmp(&(b.0.w * b.0.h)).unwrap_or(std::cmp::Ordering::Equal))
                .map(|(h, _)| h.node)
        };
        for l in self.layers.iter().rev() {
            let dy = self.layer_offset(l, scroll_y, view_h);
            if let Some(n) = smallest(&mut self.hits[l.hits.0..l.hits.1].iter().map(|h| (h, dy))) {
                return Some(n);
            }
        }
        let layered = |i: usize| self.layers.iter().any(|l| i >= l.hits.0 && i < l.hits.1);
        smallest(&mut self.hits.iter().enumerate().filter(|(i, _)| !layered(*i)).map(|(_, h)| (h, 0.0)))
    }

    /// Is item `i` part of a fixed or sticky layer (drawn separately, on top)?
    pub fn in_layer(&self, i: usize) -> bool {
        self.layers.iter().any(|l| i >= l.items.0 && i < l.items.1)
    }

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
    /// Header rows first, footer rows last.
    rows: Vec<Row>,
    /// Each column's `table-column` element and its column group, where it has them.
    cols: Vec<(Option<NodeId>, Option<NodeId>)>,
}

struct Row {
    /// The row's element (`None` for an anonymous row).
    node: Option<NodeId>,
    /// Its row group's element: a rowspan stops at the end of the group.
    group: Option<NodeId>,
    cells: Vec<Cell>,
}

struct Cell {
    b: BlockBox,
    colspan: usize,
    rowspan: usize,
}

fn is_table_part(d: Display) -> bool {
    matches!(d, Display::TableRowGroup | Display::TableRow | Display::TableCell | Display::TableCaption | Display::TableColumn | Display::TableColumnGroup)
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
    /// An inline element going on after a block that split it (no start edge here).
    Resume(NodeId),
    Image { node: NodeId, img: Img, link: Option<usize> },
    /// An inline-block.
    Block(Box<BlockBox>, Option<usize>),
    Float(Box<BlockBox>),
    /// An absolutely positioned (or fixed) box, at its place in the flow.
    Abs(Box<BlockBox>),
}

struct Builder<'a> {
    dom: &'a Dom,
    styles: &'a [Style],
    before: &'a HashMap<NodeId, NodeId>,
    after: &'a HashMap<NodeId, NodeId>,
    first_letter: &'a HashMap<NodeId, NodeId>,
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
    s.background.a > 0 || s.padding.iter().any(|p| px(*p)) || px(s.margin[1]) || px(s.margin[3]) || s.border_width.iter().any(|w| *w > 0.0)
}

impl Builder<'_> {
    fn img(&self, id: NodeId) -> Img {
        if self.dom.tag(id) == Some("canvas") {
            // A canvas is a picture of what the page drew (300 x 150 unless sized).
            return Img { src: crate::canvas::src_for(id), attr_w: px_attr(self.dom, id, "width").or(Some(300.0)), attr_h: px_attr(self.dom, id, "height").or(Some(150.0)) };
        }
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

    /// A flex or grid container's `::before` or `::after`: an item of its own,
    /// or (with `display: contents`) text joining the run beside it.
    fn generated_item(&mut self, map: &HashMap<NodeId, NodeId>, id: NodeId, items: &mut Vec<BlockBox>, run: &mut Vec<Inline>) {
        let Some(&ps) = map.get(&id) else { return };
        let Some(pieces) = self.pseudo_inline(ps, None) else { return };
        if self.styles[ps as usize].contents {
            run.extend(pieces);
            return;
        }
        if run.iter().any(|i| matches!(i, Inline::Text { text, .. } if !text.trim().is_empty())) {
            let b = self.anonymous(id, std::mem::take(run));
            items.push(b);
        }
        run.clear();
        let text = pieces.into_iter().filter(|p| matches!(p, Inline::Text { .. })).collect();
        items.push(BlockBox { node: ps, ordinal: 0, kind: BoxKind::Flow, children: vec![Child::Inline(text)], abs: Vec::new() });
    }

    /// An element's `::before` or `::after`: inline pieces in the run, or a block of its own
    /// (which ends the run) when it's block-level.
    fn push_generated(&self, map: &HashMap<NodeId, NodeId>, id: NodeId, link: Option<usize>, out: &mut Vec<Child>, run: &mut Vec<Inline>) {
        let Some(&ps) = map.get(&id) else { return };
        let Some(pieces) = self.pseudo_inline(ps, link) else { return };
        // (A flex or grid container with only text in it lays out like a block.)
        let s = &self.styles[ps as usize];
        if matches!(s.display, Display::Block | Display::ListItem | Display::Flex | Display::Grid) && !s.contents {
            let text = pieces.into_iter().filter(|p| matches!(p, Inline::Text { .. })).collect();
            let b = BlockBox { node: ps, ordinal: 0, kind: BoxKind::Flow, children: vec![Child::Inline(text)], abs: Vec::new() };
            if s.float != Float::None {
                run.push(Inline::Float(Box::new(b)));
            } else {
                flush(out, run);
                out.push(Child::Block(b));
            }
        } else {
            run.extend(pieces);
        }
    }

    /// A `::before` or `::after`'s content as inline pieces: its text, in a box when it draws one.
    fn pseudo_inline(&self, ps: NodeId, link: Option<usize>) -> Option<Vec<Inline>> {
        let s = &self.styles[ps as usize];
        if matches!(s.display, Display::None | Display::TableColumn | Display::TableColumnGroup) {
            return None;
        }
        let text = Inline::Text { text: s.content.clone()?, style: ps, link };
        Some(if needs_box(s) && !s.contents { vec![Inline::Open(ps), text, Inline::Close(ps)] } else { vec![text] })
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
        self.generated_item(self.before, id, &mut items, &mut run);
        for c in self.kids(id) {
            if let Some(g) = self.pseudo_text(c, None) {
                run.extend(g);
                continue;
            }
            match &self.dom.nodes[c as usize].data {
                Data::Text(t) => run.push(Inline::Text { text: t.clone(), style: c, link: None }),
                Data::Comment(_) | Data::Doctype { .. } | Data::Pi { .. } => {}
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
        self.generated_item(self.after, id, &mut items, &mut run);
        if has_text(&run) {
            let b = self.anonymous(id, run);
            items.push(b);
        }
        items.sort_by_key(|b| self.style(b.node).order);
        (items, abs)
    }

    fn block(&mut self, id: NodeId, ordinal: usize) -> BlockBox {
        if matches!(self.dom.tag(id), Some("img" | "canvas")) {
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
            let kids = self.kids(id);
            return self.table_box(id, &kids, ordinal);
        }
        let mut b = BlockBox { node: id, ordinal, kind: BoxKind::Flow, children: Vec::new(), abs: Vec::new() };
        let mut run = Vec::new();
        let mut item_no = 0;
        if let Some(start) = (self.dom.tag(id) == Some("ol")).then(|| self.dom.attr(id, "start").and_then(|s| s.trim().parse::<usize>().ok())).flatten() {
            item_no = start.saturating_sub(1);
        }
        self.push_generated(self.before, id, None, &mut b.children, &mut run);
        let kids = self.kids(id);
        self.flow_into(id, &kids, &mut b.children, &mut run, None, &mut item_no, false);
        self.push_generated(self.after, id, None, &mut b.children, &mut run);
        flush(&mut b.children, &mut run);
        if let Some(&ps) = self.first_letter.get(&id) {
            first_letter(&mut b.children, ps, needs_box(&self.styles[ps as usize]));
        }
        b
    }

    /// A span attribute (`colspan`, `rowspan`, `span`): 1 unless it says more.
    fn span_attr(&self, id: NodeId, k: &str) -> usize {
        match self.dom.attr(id, k).and_then(|v| v.trim().parse::<usize>().ok()) {
            // rowspan="0": to the end of the row group.
            Some(0) if k == "rowspan" => 65534,
            Some(n) => n.clamp(1, if k == "rowspan" { 65534 } else { 1000 }),
            None => 1,
        }
    }

    /// The children of `id`, with those of `display: contents` elements in their place.
    fn kids(&self, id: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        for &c in self.dom.children(id) {
            if self.styles[c as usize].contents && self.styles[c as usize].display != Display::None && matches!(self.dom.nodes[c as usize].data, Data::Element { .. }) {
                // (Its ::before and ::after stay, around its children.)
                out.extend(self.before.get(&c).copied());
                out.extend(self.kids(c));
                out.extend(self.after.get(&c).copied());
            } else {
                out.push(c);
            }
        }
        out
    }

    /// The text of a `::before` or `::after` that stands among its element's
    /// siblings' children (when the element is `display: contents`).
    fn pseudo_text(&self, id: NodeId, link: Option<usize>) -> Option<Vec<Inline>> {
        if (id as usize) < self.dom.nodes.len() || (id as usize) >= self.styles.len() {
            return None;
        }
        self.pseudo_inline(id, link)
    }

    /// An element's display (`None` for text and the like).
    fn display_of(&self, id: NodeId) -> Option<Display> {
        matches!(self.dom.nodes.get(id as usize).map(|n| &n.data), Some(Data::Element { .. })).then(|| self.styles[id as usize].display)
    }

    /// Whether `id` makes no box: collapsible white space, a comment, `display: none`.
    fn ignorable(&self, id: NodeId) -> bool {
        let Some(n) = self.dom.nodes.get(id as usize) else { return false };
        match &n.data {
            Data::Text(t) => matches!(self.styles[id as usize].space, Space::Normal | Space::NoWrap) && t.trim_matches([' ', '\t', '\n', '\r', '\x0c']).is_empty(),
            Data::Element { .. } => self.styles[id as usize].display == Display::None,
            _ => true,
        }
    }

    /// Whether `id` makes no box inside a table, row group or row: white space there
    /// is dropped whatever `white-space` says.
    fn blank(&self, id: NodeId) -> bool {
        let Some(n) = self.dom.nodes.get(id as usize) else { return false };
        match &n.data {
            Data::Text(t) => t.trim_matches([' ', '\t', '\n', '\r', '\x0c']).is_empty(),
            _ => self.ignorable(id),
        }
    }

    /// The children of a table, row group or row that make boxes: white space
    /// goes when it's first, last, or between two table parts (rows, cells...).
    fn proper(&self, kids: &[NodeId]) -> Vec<NodeId> {
        let k: Vec<NodeId> = kids.iter().copied().filter(|&c| matches!(self.dom.nodes.get(c as usize).map(|n| &n.data), Some(Data::Text(_))) || !self.ignorable(c)).collect();
        let white = |c: NodeId| matches!(self.dom.nodes.get(c as usize).map(|n| &n.data), Some(Data::Text(t)) if t.trim_matches([' ', '\t', '\n', '\r', '\x0c']).is_empty());
        (0..k.len())
            .filter(|&i| !(white(k[i]) && (i == 0 || self.table_part(k[i - 1])) && (i + 1 == k.len() || self.table_part(k[i + 1]))))
            .map(|i| k[i])
            .collect()
    }

    /// A row, cell or the like.
    fn table_part(&self, id: NodeId) -> bool {
        self.display_of(id).is_some_and(is_table_part) && !matches!(self.dom.tag(id), Some("img" | "canvas"))
    }

    /// A style for an anonymous box inside `parent`.
    fn anon_style(&mut self, parent: NodeId, display: Display, inline_level: bool) -> NodeId {
        let mut st = css_style::inherited(self.style(parent));
        st.display = display;
        st.inline_level = inline_level;
        self.anon.push(st);
        (self.styles.len() + self.anon.len() - 1) as NodeId
    }

    /// The boxes for `kids` of `parent`; a run of rows, cells and the like outside
    /// a table gets an anonymous table around it (inline-level in an inline).
    #[allow(clippy::too_many_arguments)]
    fn flow_into(&mut self, parent: NodeId, kids: &[NodeId], out: &mut Vec<Child>, run: &mut Vec<Inline>, link: Option<usize>, item_no: &mut usize, inline: bool) {
        let mut k = 0;
        while k < kids.len() {
            if !self.table_part(kids[k]) {
                self.child(kids[k], out, run, link, item_no);
                k += 1;
                continue;
            }
            let (mut j, mut last) = (k, k);
            while j < kids.len() && (self.table_part(kids[j]) || self.ignorable(kids[j])) {
                if self.table_part(kids[j]) {
                    last = j;
                }
                j += 1;
            }
            let parts: Vec<NodeId> = kids[k..=last].iter().copied().filter(|&c| self.table_part(c)).collect();
            let t = self.anon_style(parent, Display::Table, inline);
            let b = self.table_box(t, &parts, 0);
            if inline {
                run.push(Inline::Block(Box::new(b), link));
            } else {
                flush(out, run);
                out.push(Child::Block(b));
            }
            k = last + 1;
        }
    }

    /// A table box for `kids` (the children of `id`): captions, columns and rows,
    /// with anonymous rows and cells around anything else.
    fn table_box(&mut self, id: NodeId, kids: &[NodeId], ordinal: usize) -> BlockBox {
        let kids = self.proper(kids);
        let mut t = Table { captions: Vec::new(), rows: Vec::new(), cols: Vec::new() };
        let (mut head, mut foot): (Option<Vec<Row>>, Option<Vec<Row>>) = (None, None);
        let mut stray = Vec::new();
        for &c in &kids {
            let d = self.display_of(c);
            if matches!(d, Some(Display::TableCaption | Display::TableColumn | Display::TableColumnGroup | Display::TableRowGroup | Display::TableRow)) {
                let run = std::mem::take(&mut stray);
                t.rows.extend(self.row(id, None, None, &run));
            }
            match d {
                Some(Display::None) => {}
                Some(Display::TableCaption) => {
                    let b = self.block(c, 0);
                    t.captions.push(b);
                }
                Some(Display::TableColumnGroup) => {
                    let cols: Vec<NodeId> = self.kids(c).into_iter().filter(|&k| self.display_of(k) == Some(Display::TableColumn)).collect();
                    if cols.is_empty() {
                        for _ in 0..self.span_attr(c, "span") {
                            t.cols.push((None, Some(c)));
                        }
                    }
                    for k in cols {
                        for _ in 0..self.span_attr(k, "span") {
                            t.cols.push((Some(k), Some(c)));
                        }
                    }
                }
                Some(Display::TableColumn) => {
                    for _ in 0..self.span_attr(c, "span") {
                        t.cols.push((Some(c), None));
                    }
                }
                Some(Display::TableRowGroup) => {
                    let rows = self.group_rows(c);
                    match self.styles[c as usize].table_section {
                        1 if head.is_none() => head = Some(rows),
                        2 if foot.is_none() => foot = Some(rows),
                        _ => t.rows.extend(rows),
                    }
                }
                Some(Display::TableRow) => {
                    let cells = self.kids(c);
                    t.rows.extend(self.row(c, Some(c), None, &cells));
                }
                _ => {
                    stray.push(c);
                }
            }
        }
        t.rows.extend(self.row(id, None, None, &stray));
        if let Some(h) = head {
            t.rows.splice(0..0, h);
        }
        t.rows.extend(foot.unwrap_or_default());
        BlockBox { node: id, ordinal, kind: BoxKind::Table(t), children: Vec::new(), abs: Vec::new() }
    }

    /// A row group's rows; anything else in it goes in anonymous rows.
    fn group_rows(&mut self, group: NodeId) -> Vec<Row> {
        let mut rows = Vec::new();
        let mut stray = Vec::new();
        let kids = self.kids(group);
        for c in self.proper(&kids) {
            if self.display_of(c) == Some(Display::TableRow) {
                let run = std::mem::take(&mut stray);
                rows.extend(self.row(group, None, Some(group), &run));
                let cells = self.kids(c);
                rows.extend(self.row(c, Some(c), Some(group), &cells));
            } else {
                stray.push(c);
            }
        }
        rows.extend(self.row(group, None, Some(group), &stray));
        rows
    }

    /// A row of `kids`: cells, and anonymous cells around anything else. An
    /// anonymous row (`node` none) of nothing but white space is no row at all.
    fn row(&mut self, parent: NodeId, node: Option<NodeId>, group: Option<NodeId>, kids: &[NodeId]) -> Option<Row> {
        if node.is_none() && kids.iter().all(|&k| self.blank(k)) {
            return None;
        }
        let mut cells = Vec::new();
        let mut other = Vec::new();
        for k in self.proper(kids) {
            if self.display_of(k) == Some(Display::TableCell) {
                self.anon_cell(parent, &mut other, &mut cells);
                let b = self.block(k, 0);
                cells.push(Cell { b, colspan: self.span_attr(k, "colspan"), rowspan: self.span_attr(k, "rowspan") });
            } else {
                other.push(k);
            }
        }
        self.anon_cell(parent, &mut other, &mut cells);
        Some(Row { node, group, cells })
    }

    /// An anonymous cell around `other` (content in a row that isn't a cell), if there's any.
    fn anon_cell(&mut self, parent: NodeId, other: &mut Vec<NodeId>, cells: &mut Vec<Cell>) {
        if other.iter().all(|&k| self.blank(k)) {
            other.clear();
        }
        if other.is_empty() {
            return;
        }
        let kids = std::mem::take(other);
        let id = self.anon_style(parent, Display::TableCell, false);
        let mut b = BlockBox { node: id, ordinal: 0, kind: BoxKind::Flow, children: Vec::new(), abs: Vec::new() };
        let (mut run, mut n) = (Vec::new(), 0);
        self.flow_into(id, &kids, &mut b.children, &mut run, None, &mut n, false);
        flush(&mut b.children, &mut run);
        cells.push(Cell { b, colspan: 1, rowspan: 1 });
    }

    fn child(&mut self, id: NodeId, out: &mut Vec<Child>, run: &mut Vec<Inline>, link: Option<usize>, item_no: &mut usize) {
        if let Some(g) = self.pseudo_text(id, link) {
            run.extend(g);
            return;
        }
        match &self.dom.nodes[id as usize].data {
            Data::Text(t) => run.push(Inline::Text { text: t.clone(), style: id, link }),
            Data::Comment(_) | Data::Doctype { .. } | Data::Pi { .. } => {}
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
                if matches!(tag, Some("img" | "canvas")) && matches!(s.display, Display::Inline | Display::InlineBlock) {
                    run.push(Inline::Image { node: id, img: self.img(id), link });
                    return;
                }
                let atomic = s.display == Display::InlineBlock || (s.inline_level && matches!(s.display, Display::Table | Display::Flex | Display::Grid));
                match s.display {
                    _ if atomic => {
                        let link = match (tag, self.dom.attr(id, "href")) {
                            (Some("a"), Some(h)) => {
                                self.hrefs.push(h.to_string());
                                Some(self.hrefs.len() - 1)
                            }
                            _ => link,
                        };
                        let b = self.block(id, 0);
                        run.push(Inline::Block(Box::new(b), link));
                    }
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
                        self.push_generated(self.before, id, link, out, run);
                        let kids = self.kids(id);
                        self.flow_into(id, &kids, out, run, link, item_no, true);
                        self.push_generated(self.after, id, link, out, run);
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
/// Gives the first letter of the first line (with the punctuation around it) the
/// `::first-letter` style `ps`; looks into a first child block. True once it is done
/// (or there's no letter to style: the line starts with something else).
fn first_letter(children: &mut [Child], ps: NodeId, boxed: bool) -> bool {
    for child in children.iter_mut() {
        match child {
            Child::Block(b) => {
                // The first line is inside it (if it holds lines at all).
                if matches!(b.kind, BoxKind::Flow) {
                    first_letter(&mut b.children, ps, boxed);
                }
                return true;
            }
            Child::Inline(run) => {
                for k in 0..run.len() {
                    match &run[k] {
                        Inline::Open(_) | Inline::Close(_) | Inline::Resume(_) | Inline::Float(_) | Inline::Abs(_) => continue,
                        Inline::Text { text, .. } if text.trim().is_empty() => continue,
                        Inline::Text { text, style, link } => {
                            let (style, link) = (*style, *link);
                            // Collapsible spaces come before it; any other space means there's no first letter.
                            let lead = text.len() - text.trim_start_matches([' ', '\t', '\n', '\r', '\x0c']).len();
                            let rest = &text[lead..];
                            let Some(end) = first_letter_len(rest) else { return true };
                            let (head, letter, tail) = (text[..lead].to_string(), rest[..end].to_string(), rest[end..].to_string());
                            let mut parts = Vec::new();
                            if !head.is_empty() {
                                parts.push(Inline::Text { text: head, style, link });
                            }
                            if boxed {
                                parts.push(Inline::Open(ps));
                            }
                            parts.push(Inline::Text { text: letter, style: ps, link });
                            if boxed {
                                parts.push(Inline::Close(ps));
                            }
                            if !tail.is_empty() {
                                parts.push(Inline::Text { text: tail, style, link });
                            }
                            run.splice(k..k + 1, parts);
                            return true;
                        }
                        _ => return true,
                    }
                }
            }
        }
    }
    false
}

/// The length of the first letter at the start of `s`, with the punctuation
/// before and after it; `None` when `s` doesn't start with one.
fn first_letter_len(s: &str) -> Option<usize> {
    let mut chars = s.char_indices().skip_while(|&(_, c)| crate::punct::is_punct(c));
    let (i, c) = chars.next().filter(|(_, c)| !c.is_whitespace())?;
    let after = chars.take_while(|&(_, c)| crate::punct::is_punct(c)).last();
    Some(after.map_or(i + c.len_utf8(), |(j, d)| j + d.len_utf8()))
}

/// The corner radius of a w x h box.
/// A rectangle (x, y, w, h).
type Rect4 = (f32, f32, f32, f32);

/// A box's border, padding or content area, from its border box.
fn area_rect(area: Area, border: Rect4, bw: [f32; 4], pad: [f32; 4]) -> Rect4 {
    let inset = |r: Rect4, e: [f32; 4]| (r.0 + e[3], r.1 + e[0], (r.2 - e[1] - e[3]).max(0.0), (r.3 - e[0] - e[2]).max(0.0));
    match area {
        Area::Border => border,
        Area::Padding => inset(border, bw),
        Area::Content => inset(inset(border, bw), pad),
    }
}

/// One background layer as tiles: sized and placed in its origin area (`origin`),
/// repeated (or not) and clipped to `clip`. `natural` is the image's own size.
fn bg_tiles(layer: &BgLayer, natural: (f32, f32), origin: Rect4, clip: Rect4, radius: f32) -> Option<Item> {
    let (iw, ih) = natural;
    let (ox, oy, ow, oh) = origin;
    if iw <= 0.0 || ih <= 0.0 || clip.2 <= 0.0 || clip.3 <= 0.0 {
        return None;
    }
    let (mut tw, mut th) = match layer.size {
        BgSize::Cover | BgSize::Contain => {
            let (sx, sy) = (ow / iw, oh / ih);
            let k = if layer.size == BgSize::Cover { sx.max(sy) } else { sx.min(sy) };
            (iw * k, ih * k)
        }
        BgSize::Len(a, b) => {
            let w = (a != Len::Auto).then(|| a.resolve(ow));
            let h = (b != Len::Auto).then(|| b.resolve(oh));
            match (w, h) {
                (Some(w), Some(h)) => (w, h),
                (Some(w), None) => (w, w * ih / iw),
                (None, Some(h)) => (h * iw / ih, h),
                (None, None) => (iw, ih),
            }
        }
    };
    // round: as many whole tiles as fit (an auto other side keeps the proportions).
    let auto_h = matches!(layer.size, BgSize::Len(_, Len::Auto));
    let auto_w = matches!(layer.size, BgSize::Len(Len::Auto, _));
    if layer.repeat.0 == Rep::Round && tw > 0.0 {
        let n = (ow / tw).round().max(1.0);
        let nw = ow / n;
        if auto_h && layer.repeat.1 != Rep::Round {
            th *= nw / tw;
        }
        tw = nw;
    }
    if layer.repeat.1 == Rep::Round && th > 0.0 {
        let n = (oh / th).round().max(1.0);
        let nh = oh / n;
        if auto_w && layer.repeat.0 != Rep::Round {
            tw *= nh / th;
        }
        th = nh;
    }
    if tw <= 0.0 || th <= 0.0 {
        return None;
    }
    // Each axis: where the first tile goes, and the step between tiles (0: just the one).
    let axis = |rep: Rep, (end, off): BgPos, o: f32, size: f32, tile: f32| -> Option<(f32, f32)> {
        let placed = || {
            let d = off.resolve(size - tile);
            if end {
                o + size - tile - d
            } else {
                o + d
            }
        };
        match rep {
            Rep::NoRepeat => Some((placed(), 0.0)),
            Rep::Repeat | Rep::Round => Some((placed(), tile)),
            Rep::Space => {
                let n = (size / tile + 1e-4).floor();
                if n >= 2.0 {
                    Some((o, tile + (size - n * tile) / (n - 1.0)))
                } else if n >= 1.0 {
                    Some((placed(), 0.0))
                } else {
                    None
                }
            }
        }
    };
    let (x0, sx) = axis(layer.repeat.0, layer.pos[0], ox, ow, tw)?;
    let (y0, sy) = axis(layer.repeat.1, layer.pos[1], oy, oh, th)?;
    Some(Item::Tiled { x: clip.0, y: clip.1, w: clip.2, h: clip.3, radius, src: layer.image.to_string(), ox: x0, oy: y0, tw, th, sx, sy })
}

fn radius(s: &Style, w: f32, h: f32) -> f32 {
    // A percentage is of the box (of its shorter side: corners are circular here); corners can't overlap.
    let side = w.min(h).max(0.0);
    s.radius.resolve(side).clamp(0.0, side / 2.0)
}

/// An inline image's padding plus border on each side (top, right, bottom, left).
fn image_frame(s: &Style) -> [f32; 4] {
    std::array::from_fn(|i| px_only(s.padding[i]) + s.border_width[i])
}

fn flush(out: &mut Vec<Child>, run: &mut Vec<Inline>) {
    if !run.is_empty() {
        // Inline elements still open (a block splits them) go on in the next run.
        let mut open = Vec::new();
        for i in run.iter() {
            match i {
                Inline::Open(n) | Inline::Resume(n) => open.push(*n),
                Inline::Close(_) => {
                    open.pop();
                }
                _ => {}
            }
        }
        out.push(Child::Inline(std::mem::take(run)));
        run.extend(open.into_iter().map(Inline::Resume));
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
    /// An inline-block (geometry in `image`, the box in `float`).
    Block,
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
    /// How far an inline-block reaches below the baseline (from its margin box's bottom).
    drop: f32,
    /// An inline-block's top margin (its height includes both vertical margins).
    mt: f32,
}

impl Atom<'_> {
    fn content(&self) -> bool {
        matches!(self.kind, Kind::Word | Kind::Space | Kind::Image | Kind::Block)
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
    /// A border-box height set by a flex container.
    height: Option<f32>,
    /// Measuring the content: the box's own height, min-height and max-height don't apply.
    content: bool,
    /// How far down the content starts (a table cell's `vertical-align`).
    shift: f32,
    /// In a block's flow: its margins can collapse with its children's.
    collapse: bool,
}

/// Adjoining margins as one: the largest positive one and the most negative.
#[derive(Clone, Copy, Default)]
struct Margin(f32, f32);

impl Margin {
    fn of(m: f32) -> Margin {
        Margin(m.max(0.0), m.min(0.0))
    }
    fn and(self, o: Margin) -> Margin {
        Margin(self.0.max(o.0), self.1.min(o.1))
    }
    fn px(self) -> f32 {
        self.0 + self.1
    }
}

/// A run that makes no line: collapsible spaces, floats and positioned boxes.
fn blank_run(run: &[Inline], styles: &[Style]) -> bool {
    run.iter().all(|i| match i {
        Inline::Text { text, style, .. } => matches!(styles[*style as usize].space, Space::Normal | Space::NoWrap) && text.trim_matches([' ', '\t', '\n', '\r', '\x0c']).is_empty(),
        Inline::Float(_) | Inline::Abs(_) => true,
        _ => false,
    })
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
    layers: Vec<Layer>,
    /// Positioned boxes, to be painted in stacking order at the end.
    segs: Vec<Seg>,
    pending: Vec<Pending>,
    /// The element whose background is the canvas's (the root's, or else the body's).
    canvas_node: NodeId,
    /// Collapsed-border tables' cells: the border widths they lay out with (halves of the shared edges).
    collapsed: HashMap<NodeId, [f32; 4]>,
    /// Each line box's baseline as it's laid out, with how deep in inline-blocks it is.
    baselines: Vec<(u32, f32)>,
    line_depth: u32,
    /// A cell whose own width `intrinsic` looks past (to find its content's).
    width_free: Option<NodeId>,
    /// The viewport's height (`background-attachment: fixed` places against the viewport).
    view_h: f32,
    /// The root element (it starts the root stacking context).
    root_el: NodeId,
    /// The containing block's content height, when it is fixed (for percentage heights).
    avail_h: Option<f32>,
    dom: &'a Dom,
}

/// An absolutely positioned box placed by `bottom`, waiting for its containing block's height.
struct Pending {
    /// The depth of its containing block in `cbs`.
    depth: usize,
    items: (usize, usize),
    links: (usize, usize),
    hits: (usize, usize),
    /// Where its margin box's bottom must go, up from the containing block's bottom.
    bottom: f32,
    /// Where its border box was laid out, and its height.
    y: f32,
    h: f32,
}

/// A positioned box's run of items: the first `bg` are its own background and
/// borders. `ctx` when it starts a stacking context of its own.
#[derive(Debug, Clone, Copy)]
struct Seg {
    items: (usize, usize),
    bg: usize,
    z: Option<i32>,
    ctx: bool,
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
                    // Title case: the digraphs have their own (ǅ, ǈ, ǋ, ǲ), not their capitals.
                    match c as u32 {
                        0x1c4..=0x1c6 => out.push('\u{1c5}'),
                        0x1c7..=0x1c9 => out.push('\u{1c8}'),
                        0x1ca..=0x1cc => out.push('\u{1cb}'),
                        0x1f1..=0x1f3 => out.push('\u{1f2}'),
                        _ => out.extend(c.to_uppercase()),
                    }
                } else {
                    out.push(c);
                }
                start = c.is_whitespace();
            }
            out
        }
    }
}

/// A column flex item.
struct ColItem<'b> {
    b: &'b BlockBox,
    /// (x, containing width, forced border-box width)
    at: (f32, f32, Option<f32>),
    /// The width it asks of a line when columns wrap.
    cross: f32,
    size: f32,
    base: f32,
    min: f32,
    max: f32,
    grow: f32,
    shrink: f32,
    mt: f32,
    mb: f32,
    auto_t: bool,
    auto_b: bool,
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

    /// An image drawn in a w x h box, as `object-fit` and `object-position` place it
    /// (clipped to the box where it spills out).
    fn image_items(&self, s: &Style, x: f32, y: f32, w: f32, h: f32, src: &str) -> Vec<Item> {
        let natural = if s.object_fit == Fit::Fill || src.starts_with("canvas:") { None } else { self.images.size(src).filter(|(nw, nh)| *nw > 0.0 && *nh > 0.0) };
        let Some((nw, nh)) = natural else {
            return vec![Item::Image { x, y, w, h, src: src.to_string(), radius: radius(s, w, h) }];
        };
        let contain = (w / nw).min(h / nh);
        let k = match s.object_fit {
            Fit::Contain => contain,
            Fit::Cover => (w / nw).max(h / nh),
            Fit::None => 1.0,
            Fit::ScaleDown => contain.min(1.0),
            Fit::Fill => unreachable!(),
        };
        let (dw, dh) = (nw * k, nh * k);
        let at = |l: Len, free: f32| match l {
            Len::Pct(p) => free * p / 100.0,
            l => l.resolve(0.0),
        };
        let (dx, dy) = (x + at(s.object_pos.0, w - dw), y + at(s.object_pos.1, h - dh));
        if dx >= x - FIT && dy >= y - FIT && dx + dw <= x + w + FIT && dy + dh <= y + h + FIT {
            return vec![Item::Image { x: dx, y: dy, w: dw, h: dh, src: src.to_string(), radius: if s.object_fit == Fit::Contain { 0.0 } else { radius(s, w, h) } }];
        }
        vec![Item::Clip { x, y, w, h, radius: radius(s, w, h) }, Item::Image { x: dx, y: dy, w: dw, h: dh, src: src.to_string(), radius: 0.0 }, Item::Unclip]
    }

    /// The border-box width `aspect-ratio` gives a box with an auto width and a fixed height.
    fn ratio_width(&self, s: &Style, frame: f32) -> Option<f32> {
        let (r, auto) = s.aspect?;
        let Len::Px(h) = s.height else { return None };
        if s.width != Len::Auto {
            return None;
        }
        // The ratio holds between the sizes box-sizing names (the content box when `auto` comes with it).
        if s.border_box && !auto {
            return Some(h * r);
        }
        let vframe = px_only(s.padding[0]) + px_only(s.padding[2]) + s.border_width[0] + s.border_width[2];
        let content_h = if s.border_box { (h - vframe).max(0.0) } else { h };
        Some(content_h * r + frame)
    }

    /// An image's size in px: CSS width/height, then attributes, then its
    /// intrinsic size, keeping the aspect ratio; `max-width` scales it down.
    fn replaced_size(&self, node: NodeId, img: &Img, cb_w: f32) -> Option<(f32, f32)> {
        let s = &self.styles[node as usize];
        let intrinsic = if img.src.is_empty() || img.src.starts_with("canvas:") { None } else { self.images.size(&img.src) };
        let mut ratio = intrinsic.filter(|(w, _)| *w > 0.0).map(|(w, h)| h / w).or_else(|| Some(img.attr_h? / img.attr_w?));
        // aspect-ratio, unless it says `auto` and the image has a ratio of its own.
        let mut forced_ratio = false;
        if let Some((r, auto)) = s.aspect {
            if !auto || ratio.is_none() {
                ratio = Some(1.0 / r);
                forced_ratio = true;
            }
        }
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
            // The natural width, and the height from aspect-ratio when it overrides the natural ratio.
            if !forced_ratio && (img.attr_w.is_none() || img.attr_h.is_some()) {
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
        if let Len::Px(m) = s.max_height {
            if h > m && h > 0.0 {
                w *= m / h;
                h = m;
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
        let (bw, own) = self.borders(b);
        let pad = own || !matches!(b.kind, BoxKind::Table(_));
        let frame = if pad { px_only(s.padding[1]) + px_only(s.padding[3]) } else { 0.0 } + bw[1] + bw[3];
        if let Len::Px(w) = s.width {
            if self.width_free != Some(b.node) {
                return (w + frame, w + frame);
            }
        }
        if s.contain_size {
            let w = s.intrinsic_size.0.unwrap_or(0.0) + frame;
            return (w, w);
        }
        let ratio_w = self.ratio_width(&s, frame);
        let (min, max) = match &b.kind {
            BoxKind::Image(img) => {
                let w = self.replaced_size(b.node, img, 0.0).map(|(w, _)| w).unwrap_or(0.0);
                (w, w)
            }
            BoxKind::Table(t) => {
                let (mins, maxs) = self.columns(t, &s);
                let sp = spacing(&s).0 * if mins.is_empty() { 0.0 } else { mins.len() as f32 + 1.0 };
                let (mut mn, mut mx) = (mins.iter().sum::<f32>() + sp, maxs.iter().sum::<f32>() + sp);
                for cap in &t.captions {
                    let (a, z) = self.intrinsic(cap);
                    mn = mn.max(a);
                    mx = mx.max(z);
                }
                (mn, mx)
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
                            // (Collapsible spaces at the end of a line hang: they don't count.)
                            let (mut line, mut spaces) = (0f32, 0f32);
                            // (Words joined by spaces that don't wrap, nowrap or pre, make one unbreakable piece.)
                            let (mut word, mut joined) = (0f32, 0f32);
                            for a in &atoms {
                                let keep = matches!(self.styles[a.style as usize].space, Space::NoWrap | Space::Pre);
                                match a.kind {
                                    Kind::Word if keep => {
                                        word += joined + a.w;
                                        joined = 0.0;
                                        mn = mn.max(word);
                                    }
                                    Kind::Word => {
                                        word = a.w;
                                        joined = 0.0;
                                    }
                                    Kind::Space if keep => joined += a.w,
                                    Kind::Open | Kind::Close => {}
                                    _ => (word, joined) = (0.0, 0.0),
                                }
                                match a.kind {
                                    Kind::Break => {
                                        mx = mx.max(line);
                                        line = 0.0;
                                        spaces = 0.0;
                                    }
                                    Kind::Float => {
                                        let (fa, fz) = self.intrinsic(a.float.expect("float atom"));
                                        mn = mn.max(fa);
                                        line += fz;
                                    }
                                    Kind::Space if !matches!(self.styles[a.style as usize].space, Space::Pre | Space::PreWrap) => spaces += a.w,
                                    _ => {
                                        if a.kind != Kind::Space {
                                            mn = mn.max(a.w);
                                        }
                                        line += spaces + a.w;
                                        spaces = 0.0;
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
        if let Some(w) = ratio_w {
            // Not narrower than its content (the automatic minimum size).
            let w = if s.overflow_clip || s.min_width != Len::Auto { w } else { w.max(min + frame) };
            return (w, w);
        }
        let (min, max) = (min + frame, max.max(min) + frame);
        match s.width_kw {
            Some(Sizing::Min) => (min, min),
            Some(Sizing::Max) => (max, max),
            _ => (min, max),
        }
    }

    /// The border widths a box lays out with, and whether it paints them itself
    /// (not so a collapsed-border table and its cells: the table paints the edges).
    fn borders(&mut self, b: &BlockBox) -> ([f32; 4], bool) {
        if let Some(w) = self.collapsed.get(&b.node) {
            return (*w, false);
        }
        let s = &self.styles[b.node as usize];
        match &b.kind {
            BoxKind::Table(t) if s.border_collapse => {
                let s = s.clone();
                let edges = self.collapse(t, &s);
                let (nr, cells) = (t.rows.len(), &edges.cells);
                // Each cell lays out with the half of each edge on its side of the grid line.
                for (row, ce) in t.rows.iter().zip(cells) {
                    for (cell, e) in row.cells.iter().zip(ce) {
                        let w = [e[0].0 - (e[0].0 / 2.0).floor(), (e[1].0 / 2.0).floor(), (e[2].0 / 2.0).floor(), e[3].0 - (e[3].0 / 2.0).floor()];
                        self.collapsed.insert(cell.b.node, w);
                    }
                }
                // The table's border is the other half of the outer edges: the first row's left and right, the top of the first row, the bottom of the last.
                let first = cells.first().filter(|_| nr > 0);
                let left = first.and_then(|c| c.first()).map_or(0.0, |e| e[3].0);
                let right = first.and_then(|c| c.last()).map_or(0.0, |e| e[1].0);
                let top = first.map_or(0.0, |c| c.iter().map(|e| e[0].0).fold(0.0, f32::max));
                let bottom = edges.bottom;
                let w = [(top / 2.0).floor(), right - (right / 2.0).floor(), bottom - (bottom / 2.0).floor(), (left / 2.0).floor()];
                self.collapsed.insert(b.node, w);
                (w, false)
            }
            _ => (s.border_width, true),
        }
    }

    /// The collapsed borders of a table's cells (CSS 2 17.6.2): each edge goes to the
    /// widest, then most prominent, of the borders meeting there; `hidden` wins outright.
    fn collapse(&self, t: &Table, ts: &Style) -> Collapsed {
        let (n, places) = place(t);
        let nr = t.rows.len();
        // Which cell covers each slot of the grid.
        let mut owner: Vec<Vec<Option<(usize, usize)>>> = vec![vec![None; n]; nr];
        for (ri, at) in places.iter().enumerate() {
            for (k, &(col, span, rows)) in at.iter().enumerate() {
                for line in &mut owner[ri..ri + rows] {
                    for slot in &mut line[col..col + span] {
                        *slot = Some((ri, k));
                    }
                }
            }
        }
        let side = |node: NodeId, i: usize| -> Side {
            let st = &self.styles[node as usize];
            (if st.border_style[i].visible() { st.border_spec[i] } else { 0.0 }, st.border_style[i], st.border_color[i])
        };
        let cell_node = |(ri, k): (usize, usize)| t.rows[ri].cells[k].b.node;
        let first_of_group = |ri: usize| ri == 0 || t.rows[ri - 1].group != t.rows[ri].group;
        let last_of_group = |ri: usize| ri + 1 == nr || t.rows[ri + 1].group != t.rows[ri].group;
        let table = |i: usize| (ts.border_spec[i] * ts.border_style[i].visible() as u8 as f32, ts.border_style[i], ts.border_color[i]);
        let mut cells = Vec::with_capacity(nr);
        let mut bottom = 0f32;
        for (ri, (row, at)) in t.rows.iter().zip(&places).enumerate() {
            let mut out = Vec::with_capacity(at.len());
            for (k, &(col, span, rows)) in at.iter().enumerate() {
                let me = row.cells[k].b.node;
                let last_row = ri + rows - 1;
                let (cols, end) = (col..col + span, col + span);
                let mut top: Vec<Side> = Vec::new();
                if ri > 0 {
                    top.extend(cols.clone().filter_map(|c| owner[ri - 1][c]).map(|o| side(cell_node(o), 2)));
                }
                top.push(side(me, 0));
                if ri > 0 {
                    top.extend(t.rows[ri - 1].node.map(|r| side(r, 2)));
                }
                top.extend(row.node.map(|r| side(r, 0)));
                if first_of_group(ri) {
                    if ri > 0 {
                        top.extend(t.rows[ri - 1].group.map(|g| side(g, 2)));
                    }
                    top.extend(row.group.map(|g| side(g, 0)));
                }
                if ri == 0 {
                    top.extend(cols.clone().filter_map(|c| t.cols.get(c)).flat_map(|c| [c.0, c.1]).flatten().map(|c| side(c, 0)));
                    top.push(table(0));
                }
                let mut bot: Vec<Side> = vec![side(me, 2)];
                if last_row + 1 < nr {
                    bot.extend(cols.clone().filter_map(|c| owner[last_row + 1][c]).map(|o| side(cell_node(o), 0)));
                }
                bot.extend(t.rows[last_row].node.map(|r| side(r, 2)));
                if last_row + 1 < nr {
                    bot.extend(t.rows[last_row + 1].node.map(|r| side(r, 0)));
                }
                if last_of_group(last_row) {
                    bot.extend(t.rows[last_row].group.map(|g| side(g, 2)));
                    if last_row + 1 < nr {
                        bot.extend(t.rows[last_row + 1].group.map(|g| side(g, 0)));
                    }
                }
                if last_row + 1 == nr {
                    bot.extend(cols.clone().filter_map(|c| t.cols.get(c)).flat_map(|c| [c.0, c.1]).flatten().map(|c| side(c, 2)));
                    bot.push(table(2));
                }
                let lines = ri..ri + rows;
                let mut left: Vec<Side> = Vec::new();
                if col > 0 {
                    left.extend(lines.clone().filter_map(|r| owner[r][col - 1]).map(|o| side(cell_node(o), 1)));
                }
                left.push(side(me, 3));
                if col > 0 {
                    left.extend(t.cols.get(col - 1).and_then(|c| c.0).map(|c| side(c, 1)));
                }
                left.extend(t.cols.get(col).and_then(|c| c.0).map(|c| side(c, 3)));
                let group_at = |c: usize| t.cols.get(c).and_then(|x| x.1);
                if col == 0 || group_at(col - 1) != group_at(col) {
                    if col > 0 {
                        left.extend(group_at(col - 1).map(|g| side(g, 1)));
                    }
                    left.extend(group_at(col).map(|g| side(g, 3)));
                }
                if col == 0 {
                    left.extend(row.node.map(|r| side(r, 3)));
                    left.extend(row.group.map(|g| side(g, 3)));
                    left.push(table(3));
                }
                let mut right: Vec<Side> = vec![side(me, 1)];
                if end < n {
                    right.extend(lines.clone().filter_map(|r| owner[r][end]).map(|o| side(cell_node(o), 3)));
                }
                right.extend(t.cols.get(end - 1).and_then(|c| c.0).map(|c| side(c, 1)));
                right.extend(t.cols.get(end).and_then(|c| c.0).map(|c| side(c, 3)));
                if end == n || group_at(end - 1) != group_at(end) {
                    right.extend(group_at(end - 1).map(|g| side(g, 1)));
                    right.extend(group_at(end).map(|g| side(g, 3)));
                }
                if end == n {
                    right.extend(row.node.map(|r| side(r, 1)));
                    right.extend(row.group.map(|g| side(g, 1)));
                    right.push(table(1));
                }
                let e = [winner(&top), winner(&right), winner(&bot), winner(&left)];
                if last_row + 1 == nr {
                    bottom = bottom.max(e[2].0);
                }
                out.push(e);
            }
            cells.push(out);
        }
        Collapsed { cells, bottom, owner }
    }

    /// Paints a collapsed-border table's edges, centred on the grid lines.
    fn paint_collapsed(&mut self, t: &Table, s: &Style, col_x: &[f32], row_y: &[f32]) {
        let edges = self.collapse(t, s);
        let (n, places) = place(t);
        let nr = t.rows.len();
        let before = |w: f32| (w / 2.0).floor();
        for (ri, at) in places.iter().enumerate() {
            for (k, &(col, span, rows)) in at.iter().enumerate() {
                let [top, right, bottom, left] = edges.cells[ri][k];
                let (x0, x1, y0, y1) = (col_x[col], col_x[col + span], row_y[ri], row_y[ri + rows]);
                let (l0, r1) = (x0 - before(left.0), x1 - before(right.0) + right.0);
                let (t0, b1) = (y0 - before(top.0), y1 - before(bottom.0) + bottom.0);
                let mut edge = |e: Side, x: f32, y: f32, w: f32, h: f32| {
                    if e.0 > 0.0 && e.2.a > 0 && w > 0.0 && h > 0.0 {
                        self.items.push(Item::Rect { x, y, w, h, color: e.2, radius: 0.0 });
                    }
                };
                // Each cell draws its top and left edges; its right and bottom ones where no cell does.
                edge(top, l0, t0, r1 - l0, top.0);
                edge(left, l0, t0, left.0, b1 - t0);
                if col + span == n || edges.owner[ri][col + span].is_none() {
                    edge(right, r1 - right.0, t0, right.0, b1 - t0);
                }
                if ri + rows == nr || edges.owner[ri + rows][col].is_none() {
                    edge(bottom, l0, b1 - bottom.0, r1 - l0, bottom.0);
                }
            }
        }
    }

    /// Each column's minimum and maximum width (fixed layout: both its width).
    fn columns(&mut self, t: &Table, s: &Style) -> (Vec<f32>, Vec<f32>) {
        let (n, places) = place(t);
        if fixed_layout(s) {
            let w = self.fixed_columns(t, &places, n, None, spacing(s).0);
            return (w.clone(), w);
        }
        let (mut mins, mut maxs) = (vec![0f32; n], vec![0f32; n]);
        // Columns with a width of their own.
        for (k, (col, group)) in t.cols.iter().enumerate() {
            if let Some(Len::Px(w)) = [*col, *group].into_iter().flatten().map(|c| self.styles[c as usize].width).find(|w| *w != Len::Auto) {
                mins[k] = w;
                maxs[k] = w;
            }
        }
        // Single-column cells first, then spanning cells spread what's missing.
        for pass in 0..2 {
            for (row, at) in t.rows.iter().zip(&places) {
                for (cell, &(col, span, _)) in row.cells.iter().zip(at) {
                    if (pass == 0) == (span == 1) {
                        let (a, z) = self.cell_widths(&cell.b);
                        let have_min: f32 = mins[col..col + span].iter().sum();
                        let have_max: f32 = maxs[col..col + span].iter().sum();
                        for k in col..col + span {
                            mins[k] += ((a - have_min) / span as f32).max(0.0);
                            maxs[k] += ((z - have_max) / span as f32).max(0.0);
                        }
                    }
                }
            }
        }
        for k in 0..n {
            maxs[k] = maxs[k].max(mins[k]);
        }
        (mins, maxs)
    }

    /// A cell's minimum and maximum width: a width of its own, but never narrower than its content.
    fn cell_widths(&mut self, b: &BlockBox) -> (f32, f32) {
        let s = &self.styles[b.node as usize];
        let (a, z) = self.intrinsic(b);
        if !matches!(s.width, Len::Px(_)) {
            return (a, z);
        }
        let w = a;
        self.width_free = Some(b.node);
        let (content_min, _) = self.intrinsic(b);
        self.width_free = None;
        (w.max(content_min), w.max(content_min))
    }

    /// Column widths under `table-layout: fixed`: from the columns' widths, then the
    /// first row's cells; the other columns share what's left of `width` (when known).
    fn fixed_columns(&self, t: &Table, places: &[Vec<(usize, usize, usize)>], n: usize, width: Option<f32>, hs: f32) -> Vec<f32> {
        let base = width.unwrap_or(0.0);
        let mut w: Vec<Option<f32>> = vec![None; n];
        for (k, (col, group)) in t.cols.iter().enumerate() {
            if let Some(l) = [*col, *group].into_iter().flatten().map(|c| self.styles[c as usize].width).find(|w| *w != Len::Auto) {
                w[k] = Some(l.resolve(base));
            }
        }
        if let (Some(row), Some(at)) = (t.rows.first(), places.first()) {
            for (cell, &(col, span, _)) in row.cells.iter().zip(at) {
                let st = &self.styles[cell.b.node as usize];
                if st.width == Len::Auto || w[col..col + span].iter().any(|x| x.is_some()) {
                    continue;
                }
                let bw = self.collapsed.get(&cell.b.node).copied().unwrap_or(st.border_width);
                let frame = px_only(st.padding[1]) + px_only(st.padding[3]) + bw[1] + bw[3];
                let outer = st.width.resolve(base) + if st.border_box { 0.0 } else { frame };
                let each = ((outer - hs * (span as f32 - 1.0)) / span as f32).max(0.0);
                for x in &mut w[col..col + span] {
                    *x = Some(each);
                }
            }
        }
        let fixed: f32 = w.iter().flatten().sum();
        let free = w.iter().filter(|x| x.is_none()).count();
        let rest = width.map_or(0.0, |wd| (wd - fixed).max(0.0));
        let mut out: Vec<f32> = w.iter().map(|x| x.unwrap_or(rest / free.max(1) as f32)).collect();
        // Every column has a width and there's room over: they share it.
        if let Some(wd) = width.filter(|wd| free == 0 && n > 0 && *wd > fixed) {
            for x in &mut out {
                *x += if fixed > 0.0 { (wd - fixed) * *x / fixed } else { (wd - fixed) / n as f32 };
            }
        }
        out
    }

    // ------------------------------------------------------------ blocks

    /// Whether a box's content is its own: its children's margins don't collapse with its own.
    fn own_context(&self, b: &BlockBox) -> bool {
        let s = &self.styles[b.node as usize];
        !matches!(b.kind, BoxKind::Flow)
            || b.node == self.root_el
            || s.flow_root
            || s.overflow_clip
            || !matches!(s.display, Display::Block | Display::ListItem)
            || s.float != Float::None
            || matches!(s.position, Position::Absolute | Position::Fixed)
    }

    /// The children whose margins meet the box's top (or bottom) one: the margins of the empty
    /// blocks at that end, and the first (last) block with something in it, unless a line comes first.
    fn edge_block<'b>(&self, b: &'b BlockBox, last: bool, cb_w: f32) -> (Margin, Option<&'b BlockBox>) {
        let kids: Box<dyn Iterator<Item = &Child>> = if last { Box::new(b.children.iter().rev()) } else { Box::new(b.children.iter()) };
        let mut m = Margin::default();
        for c in kids {
            match c {
                Child::Block(cb) if self.empty(cb) => m = m.and(self.edge_margin(cb, cb_w, false)).and(self.edge_margin(cb, cb_w, true)),
                // (Margins after an empty block with clearance stay inside: they meet a cleared margin.)
                Child::Block(cb) if last && self.cleared_empty(cb) => return (Margin::default(), None),
                // (Clearance keeps a child's top margin from joining its parent's.)
                Child::Block(cb) if !last && self.styles[cb.node as usize].clear != Clear::None => return (m, None),
                Child::Block(cb) => return (m, Some(cb)),
                Child::Inline(run) if blank_run(run, self.styles) => {}
                Child::Inline(_) => return (m, None),
            }
        }
        (m, None)
    }

    /// A block with nothing in it, which its margins collapse through.
    fn empty(&self, b: &BlockBox) -> bool {
        self.styles[b.node as usize].clear == Clear::None && self.empty_shape(b)
    }

    /// An empty block with clearance.
    fn cleared_empty(&self, b: &BlockBox) -> bool {
        self.styles[b.node as usize].clear != Clear::None && self.empty_shape(b)
    }

    fn empty_shape(&self, b: &BlockBox) -> bool {
        let s = &self.styles[b.node as usize];
        self.margins_through(b, false)
            && self.margins_through(b, true)
            && !(s.display == Display::ListItem && s.list != ListStyle::None)
            && s.aspect.is_none()
            && !s.contain_size
            && b.children.iter().all(|c| match c {
                Child::Block(cb) => self.empty(cb),
                Child::Inline(run) => blank_run(run, self.styles),
            })
    }

    /// Whether the top (or bottom) margin of a box in a flow adjoins its first (last) child's.
    fn margins_through(&self, b: &BlockBox, bottom: bool) -> bool {
        let s = &self.styles[b.node as usize];
        let i = if bottom { 2 } else { 0 };
        let zero = |l: Len| l.resolve(1.0) == 0.0;
        !self.own_context(b) && s.border_width[i] == 0.0 && zero(s.padding[i]) && (!bottom || (s.height == Len::Auto && s.max_height == Len::Auto && (s.min_height == Len::Auto || zero(s.min_height))))
    }

    /// A box's top (or bottom) margin collapsed with its descendants' that adjoin it.
    fn edge_margin(&self, b: &BlockBox, cb_w: f32, bottom: bool) -> Margin {
        let s = &self.styles[b.node as usize];
        let m = Margin::of(s.margin[if bottom { 2 } else { 0 }].resolve(cb_w));
        if !self.margins_through(b, bottom) {
            return m;
        }
        if bottom && b.children.iter().rev().find_map(|c| match c {
            Child::Block(cb) if self.empty(cb) => None,
            Child::Block(cb) => Some(self.cleared_empty(cb)),
            Child::Inline(run) if blank_run(run, self.styles) => None,
            Child::Inline(_) => Some(false),
        }) == Some(true)
        {
            return m;
        }
        let (lead, c) = self.edge_block(b, bottom, cb_w);
        c.map_or(m.and(lead), |c| m.and(lead).and(self.edge_margin(c, cb_w, bottom)))
    }

    /// Lays out a block box at (x, y) inside a containing block `cb_w` wide;
    /// returns its border-box height. Its vertical margins are the caller's business.
    fn block(&mut self, b: &BlockBox, x: f32, y: f32, cb_w: f32, opts: Opts) -> f32 {
        let s = self.styles[b.node as usize].clone();
        // (A table whose borders collapse has no padding; it and its cells take half of each shared border.)
        let (bw, own_borders) = self.borders(b);
        let collapsed_table = !own_borders && matches!(b.kind, BoxKind::Table(_));
        let pad: [f32; 4] = std::array::from_fn(|i| if collapsed_table { 0.0 } else { s.padding[i].resolve(cb_w) });
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
            BoxKind::Table(t) => Some(self.table_width(b.node, t, &s, cb_w - ml - mr - frame)),
            _ => None,
        };
        let mut content_w = if let Some(f) = forced.filter(|_| replaced.is_none()) {
            f - frame
        } else if let Some((w, _)) = replaced {
            w
        } else if let Some(w) = table_w {
            w
        } else {
            match (s.width, s.width_kw) {
                (Len::Auto, Some(kw)) => {
                    let (min, max) = self.intrinsic(b);
                    match kw {
                        Sizing::Min => min - frame,
                        Sizing::Max => max - frame,
                        Sizing::Fit => max.min(cb_w - ml - mr).max(min) - frame,
                        Sizing::Stretch => cb_w - ml - mr - frame,
                    }
                }
                (Len::Auto, None) => match self.ratio_width(&s, frame) {
                    Some(_) => self.intrinsic(b).0 - frame,
                    None => cb_w - ml - mr - frame,
                },
                (w, _) => inner(w),
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
        // A table is never narrower than its columns need.
        if let BoxKind::Table(t) = &b.kind {
            content_w = content_w.max(self.table_min(t, &s));
        }
        content_w = content_w.max(0.0);
        let leftover = cb_w - content_w - frame;
        if ml_auto && mr_auto {
            ml = (leftover / 2.0).max(0.0);
        } else if ml_auto {
            ml = (leftover - mr).max(0.0);
        }
        let box_x = x + ml;
        // A table's captions sit outside its border box, above or below it.
        let outer_y = y;
        let mut cap_top = 0.0;
        if let BoxKind::Table(t) = &b.kind {
            for cap in t.captions.iter().filter(|c| !self.styles[c.node as usize].caption_bottom) {
                let (mt, mb) = vmargins(&self.styles[cap.node as usize], content_w + frame);
                cap_top += mt + self.measure_block(cap, content_w + frame) + mb;
            }
        }
        let y = y + cap_top;
        let content_x = box_x + bw[3] + pad[3];
        let top_y = y + bw[0] + pad[0];
        let content_y = top_y + opts.shift;
        let paint_at = self.items.len();
        let links_at = self.links.len();
        let hits_at = self.hits.len();
        let positioned = s.position != Position::Static;
        if positioned {
            self.cbs.push((box_x + bw[3], y + bw[0], content_w + pad[1] + pad[3]));
        }
        // Percentage heights resolve against the containing block's height, when that is fixed.
        let parent_h = self.avail_h;
        let vframe = pad[0] + pad[2] + bw[0] + bw[2];
        let resolve_h = |l: Len| match l {
            Len::Px(v) => Some(v),
            Len::Pct(p) => parent_h.map(|h| h * p / 100.0),
            _ => None,
        };
        let to_content = |h: f32| if s.border_box { (h - vframe).max(0.0) } else { h };
        // (A flex container's word on the height wins; it's a border-box height.)
        let fixed_h = match opts.height {
            Some(h) => Some(if s.border_box { h } else { (h - vframe).max(0.0) }),
            None if opts.content => None,
            None => resolve_h(s.height),
        }
        .filter(|_| replaced.is_none());
        // The height this box's children see: its own, when fixed (a cell's grows, so it isn't).
        // The document's box passes on the viewport's: the root element's containing block.
        self.avail_h = if b.node == 0 { parent_h } else { fixed_h.filter(|_| !matches!(s.display, Display::Table | Display::TableCell)).map(|h| {
            let mut c = to_content(h);
            if let Some(m) = resolve_h(s.max_height) {
                c = c.min(to_content(m));
            }
            if let Some(m) = resolve_h(s.min_height) {
                c = c.max(to_content(m));
            }
            c
        }) };
        // The height a table's rows share: its own, within its min and max heights.
        let table_h = fixed_h.map_or(0.0, |h| {
            let mut c = to_content(h);
            if let Some(m) = resolve_h(s.max_height) {
                c = c.min(to_content(m));
            }
            resolve_h(s.min_height).map_or(c, |m| c.max(to_content(m)))
        });
        // A box stretched to a known height (a flex item, a cell in its row) is that tall for its children.
        if self.avail_h.is_none() && s.height == Len::Auto && opts.min_h > 0.0 && replaced.is_none() {
            self.avail_h = Some((opts.min_h - vframe).max(0.0));
        }
        let saved = opts.bfc.then(|| std::mem::take(&mut self.floats));
        let mut cursor = content_y;
        match &b.kind {
            BoxKind::Image(img) => {
                let (w, h) = replaced.unwrap_or((0.0, 0.0));
                if w > 0.0 && h > 0.0 && !img.src.is_empty() && !s.hidden {
                    let drawn = self.image_items(&s, content_x, content_y, w, h, &img.src);
                    self.items.extend(drawn);
                }
                cursor += h;
            }
            BoxKind::Table(t) => cursor += self.table(t, &s, content_x, content_y, content_w, table_h),
            BoxKind::Flex(items) | BoxKind::Grid(items) => {
                // (Percentage gaps are of the content box: the row gap of its height, when known.)
                let gs;
                let s = if s.gap_pct != [0.0; 2] {
                    let mut g = s.clone();
                    g.row_gap += self.avail_h.unwrap_or(0.0) * g.gap_pct[0] / 100.0;
                    g.column_gap += content_w * g.gap_pct[1] / 100.0;
                    gs = g;
                    &gs
                } else {
                    &s
                };
                cursor += if matches!(b.kind, BoxKind::Flex(_)) { self.flex(items, s, content_x, content_y, content_w) } else { self.grid(items, s, content_x, content_y, content_w) };
            }
            BoxKind::Flow => {}
        }
        for a in &b.abs {
            self.place_abs(a, content_x, content_y);
        }
        match &b.kind {
            BoxKind::Image(_) | BoxKind::Table(_) | BoxKind::Flex(_) | BoxKind::Grid(_) => {}
            BoxKind::Flow => {
                // The first child's top margin and the last one's bottom can be this box's own
                // (its caller has put them outside it); between children, margins collapse.
                // While `leading`, children's margins are this box's top one (its caller has put
                // them outside it); with `trailing`, the last ones are its bottom margin.
                let mut leading = opts.collapse && self.margins_through(b, false);
                let trailing = opts.collapse && self.margins_through(b, true);
                let mut prev = Margin::default();
                let mut tail_cleared = false;
                for child in &b.children {
                    match child {
                        Child::Block(cb) if self.empty(cb) => {
                            if !leading {
                                prev = prev.and(self.edge_margin(cb, content_w, false)).and(self.edge_margin(cb, content_w, true));
                            }
                            self.block(cb, content_x, cursor + prev.px(), content_w, Opts { collapse: true, ..Default::default() });
                        }
                        Child::Block(cb) => {
                            let clear = self.styles[cb.node as usize].clear;
                            let gap = if leading && clear == Clear::None { 0.0 } else { prev.and(self.edge_margin(cb, content_w, false)).px() };
                            leading = false;
                            let mut at = cursor + gap;
                            if clear != Clear::None {
                                at = at.max(self.clear_to(clear));
                            }
                            let h = self.block(cb, content_x, at, content_w, Opts { collapse: true, ..Default::default() });
                            cursor = at + h;
                            prev = self.edge_margin(cb, content_w, true);
                            tail_cleared = self.cleared_empty(cb);
                        }
                        Child::Inline(run) => {
                            let h = self.inline(run, content_x, cursor + prev.px(), content_w, b.node);
                            // A run that made no lines (only floats, or spaces) doesn't
                            // separate the margins around it.
                            if h > 0.0 {
                                cursor += prev.px() + h;
                                prev = Margin::default();
                                leading = false;
                                tail_cleared = false;
                            }
                        }
                    }
                }
                if !trailing || tail_cleared {
                    cursor += prev.px();
                }
                // An empty list item still has a line, for its marker.
                if s.display == Display::ListItem && s.list != ListStyle::None && b.children.is_empty() {
                    cursor += s.line_height;
                }
            }
        }
        self.avail_h = parent_h;
        // Size containment: the box is as tall as contain-intrinsic-size says, whatever is in it.
        if s.contain_size && replaced.is_none() {
            cursor = content_y + s.intrinsic_size.1.unwrap_or(0.0);
        }
        if let Some(h) = fixed_h {
            let to = top_y + to_content(h);
            // A table's or a cell's height is a minimum: they grow to fit their content.
            cursor = if matches!(s.display, Display::Table | Display::TableCell) { cursor.max(to) } else { to };
        }
        // aspect-ratio gives an auto height from the width; the content can still make it taller.
        if let (Len::Auto, Some((r, auto)), None) = (s.height, s.aspect, replaced) {
            let h = if s.border_box && !auto { ((content_w + frame) / r - vframe).max(0.0) } else { content_w / r };
            cursor = if s.overflow_clip || s.min_height != Len::Auto { content_y + h } else { cursor.max(content_y + h) };
        }
        // max-height (a percentage of an unknown height doesn't apply), then min-height, which wins.
        // (A flex container has applied them already.)
        if let (Some(h), None, None, false) = (resolve_h(s.max_height), replaced, opts.height, opts.content) {
            cursor = cursor.min(content_y + to_content(h));
        }
        if let (Some(h), None, false) = (resolve_h(s.min_height), opts.height, opts.content) {
            cursor = cursor.max(content_y + to_content(h));
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
        // (The canvas takes the root's background, or the body's.)
        if b.node != self.canvas_node && !s.masked {
            let border = (box_x, y, border_w, height);
            let r = radius(&s, border_w, height);
            if s.background.a > 0 {
                let area = s.bg.as_ref().map_or(Area::Border, |g| g.color_clip());
                let (cx, cy, cw, ch) = area_rect(area, border, bw, pad);
                let inner = if area == Area::Border { r } else { (r - bw[0].max(bw[3])).max(0.0) };
                under.push(Item::Rect { x: cx, y: cy, w: cw, h: ch, color: s.background, radius: inner });
            }
            under.extend(self.bg_items(&s, border, bw, pad, None, r));
        }
        if own_borders {
            border_items((box_x, y, border_w, height), bw, &s, [true; 4], &mut under);
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
        self.layers_moved(paint_at, n_under);
        // The captions (while a positioned table is still their containing block).
        let mut total = height;
        if let BoxKind::Table(t) = &b.kind {
            if !t.captions.is_empty() {
                let mut at = outer_y;
                self.place_captions(t, box_x, &mut at, border_w, false);
                let mut at = y + height;
                self.place_captions(t, box_x, &mut at, border_w, true);
                total = at - outer_y;
            }
        }
        if positioned {
            // Boxes placed by `bottom` go up from this box's padding box's bottom.
            let depth = self.cbs.len();
            let cb_bottom = y + height - bw[2];
            let mut k = 0;
            while k < self.pending.len() {
                if self.pending[k].depth == depth {
                    let p = self.pending.remove(k);
                    self.shift_range(p.items, p.links, p.hits, cb_bottom - p.bottom - p.h - p.y);
                } else {
                    k += 1;
                }
            }
            self.cbs.pop();
        }
        self.hits.push(Hit { x: box_x, y, w: border_w, h: height, node: b.node });
        // overflow: the content (not the box's own background and borders) is clipped to the padding box.
        if s.overflow_clip && self.items.len() > paint_at + n_under {
            let (cx, cy) = (box_x + bw[3], y + bw[0]);
            let (cw, ch) = ((border_w - bw[1] - bw[3]).max(0.0), (height - bw[0] - bw[2]).max(0.0));
            self.items.insert(paint_at + n_under, Item::Clip { x: cx, y: cy, w: cw, h: ch, radius: radius(&s, border_w, height) });
            self.layers_moved(paint_at + n_under, 1);
            self.items.push(Item::Unclip);
        }
        let height = total;
        if s.opacity < 1.0 {
            if s.opacity <= 0.01 {
                self.items.truncate(paint_at);
                self.layers.retain(|l| l.items.0 < paint_at);
                self.segs.retain(|g| g.items.0 < paint_at);
                self.pending.retain(|p| p.items.0 < paint_at);
                self.links.truncate(links_at);
            } else {
                for it in &mut self.items[paint_at..] {
                    if let Item::Rect { color, .. } | Item::Text { color, .. } = it {
                        color.a = (color.a as f32 * s.opacity).round() as u8;
                    }
                }
            }
        }
        // Positioned boxes, flex and grid items with a z-index, and the root element are painted in stacking order.
        let flex_item = s.z_index.is_some() && self.dom.nodes.get(b.node as usize).and_then(|n| self.styles.get(n.parent as usize)).is_some_and(|p| matches!(p.display, Display::Flex | Display::Grid));
        let root = b.node == self.root_el;
        if (positioned || flex_item || root) && self.items.len() > paint_at {
            let ctx = s.z_index.is_some() || root || matches!(s.position, Position::Fixed | Position::Sticky);
            let z = if root { Some(0) } else { s.z_index };
            self.segs.push(Seg { items: (paint_at, self.items.len()), bg: n_under.min(self.items.len() - paint_at), z, ctx });
        }
        // position: relative and transform: translate move the box (and everything in it) after layout.
        let (mut dx, mut dy) = (0.0, 0.0);
        if s.position == Position::Relative {
            dx += if s.inset[3] != Len::Auto { s.inset[3].resolve(cb_w) } else if s.inset[1] != Len::Auto { -s.inset[1].resolve(cb_w) } else { 0.0 };
            dy += if s.inset[0] != Len::Auto { s.inset[0].resolve(cb_w) } else if s.inset[2] != Len::Auto { -s.inset[2].resolve(cb_w) } else { 0.0 };
        }
        for (tx, ty) in [s.translate, s.translate_own].into_iter().flatten() {
            dx += tx.resolve(border_w);
            dy += ty.resolve(height);
        }
        if dx != 0.0 || dy != 0.0 {
            self.shift(paint_at, links_at, hits_at, dx, dy);
        }
        // position: sticky: it moves with scrolling, within its parent.
        if s.position == Position::Sticky && (s.inset[0] != Len::Auto || s.inset[2] != Len::Auto) {
            let inset = |i: usize| (s.inset[i] != Len::Auto).then(|| s.inset[i].resolve(cb_w));
            let parent = self.dom.nodes.get(b.node as usize).map(|n| n.parent).unwrap_or(0);
            let kind = LayerKind::Sticky { top: inset(0), bottom: if inset(0).is_none() { inset(2) } else { None }, y, h: height, parent, limit: (0.0, f32::MAX) };
            self.add_layer(paint_at, hits_at, links_at, kind);
        }
        height
    }

    /// Records a fixed or sticky layer over everything drawn since the given indices
    /// (taking in any layers inside it).
    fn add_layer(&mut self, items_at: usize, hits_at: usize, links_at: usize, kind: LayerKind) {
        let (ni, nh, nl) = (self.items.len(), self.hits.len(), self.links.len());
        if ni == items_at {
            return;
        }
        self.layers.retain(|l| !(l.items.0 >= items_at && l.items.1 <= ni));
        self.layers.push(Layer { items: (items_at, ni), hits: (hits_at, nh), links: (links_at, nl), kind });
    }

    /// `n` items were inserted at `at`: layers after it move along.
    fn layers_moved(&mut self, at: usize, n: usize) {
        for l in &mut self.layers {
            if l.items.0 >= at {
                l.items.0 += n;
                l.items.1 += n;
            }
        }
        for g in &mut self.segs {
            if g.items.0 >= at {
                g.items.0 += n;
                g.items.1 += n;
            } else if g.items.1 > at {
                g.items.1 += n;
            }
        }
        for p in &mut self.pending {
            if p.items.0 >= at {
                p.items.0 += n;
                p.items.1 += n;
            } else if p.items.1 > at {
                p.items.1 += n;
            }
        }
    }

    /// A box's background image layers, bottom first. `canvas` paints them over the whole canvas instead.
    fn bg_items(&self, s: &Style, border: Rect4, bw: [f32; 4], pad: [f32; 4], canvas: Option<Rect4>, r: f32) -> Vec<Item> {
        let Some(bg) = &s.bg else { return Vec::new() };
        let mut out = Vec::new();
        for layer in bg.layers() {
            if layer.image.is_empty() {
                continue;
            }
            let Some(natural) = self.images.size(layer.image) else { continue };
            let origin = if layer.fixed { (0.0, 0.0, self.cbs[0].2, self.view_h) } else { area_rect(layer.origin, border, bw, pad) };
            let (clip, radius) = match canvas {
                Some(c) => (c, 0.0),
                None => {
                    let c = area_rect(layer.clip, border, bw, pad);
                    (c, if layer.clip == Area::Border { r } else { (r - bw[0].max(bw[3])).max(0.0) })
                }
            };
            out.extend(bg_tiles(&layer, natural, origin, clip, radius));
        }
        out
    }

    /// Moves the items, links and hits in the given ranges down by `dy`.
    fn shift_range(&mut self, items: (usize, usize), links: (usize, usize), hits: (usize, usize), dy: f32) {
        for it in &mut self.items[items.0..items.1] {
            if let Item::Rect { y, .. } | Item::Text { y, .. } | Item::Image { y, .. } | Item::Clip { y, .. } = it {
                *y += dy;
            } else if let Item::Tiled { y, oy, .. } = it {
                *y += dy;
                *oy += dy;
            }
        }
        for l in &mut self.links[links.0..links.1] {
            l.y += dy;
        }
        for h in &mut self.hits[hits.0..hits.1] {
            h.y += dy;
        }
        for l in &mut self.layers {
            if l.items.0 >= items.0 && l.items.1 <= items.1 {
                let (LayerKind::Sticky { y, .. } | LayerKind::Fixed { y, .. }) = &mut l.kind;
                *y += dy;
            }
        }
    }

    /// Moves everything drawn since the given indices.
    fn shift(&mut self, items_at: usize, links_at: usize, hits_at: usize, dx: f32, dy: f32) {
        for it in &mut self.items[items_at..] {
            match it {
                Item::Rect { x, y, .. } | Item::Text { x, y, .. } | Item::Image { x, y, .. } | Item::Clip { x, y, .. } => {
                    *x += dx;
                    *y += dy;
                }
                Item::Tiled { x, y, ox, oy, .. } => {
                    *x += dx;
                    *y += dy;
                    *ox += dx;
                    *oy += dy;
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
        for l in &mut self.layers {
            if l.items.0 >= items_at {
                let (LayerKind::Sticky { y, .. } | LayerKind::Fixed { y, .. }) = &mut l.kind;
                *y += dy;
            }
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
            // (A table never stretches: it shrinks to fit even between both insets.)
            (Len::Auto, Some(l), Some(r)) if matches!(b.kind, BoxKind::Table(_)) => {
                let (min, max) = self.intrinsic(b);
                max.min((cw - l - r - ml - mr).max(0.0)).max(min)
            }
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
        let (items_at, hits_at, links_at) = (self.items.len(), self.hits.len(), self.links.len());
        // (The containing block's height isn't known here: percentage heights stay auto.)
        let avail_h = self.avail_h.take();
        let h = self.block(b, x, y, border_w, Opts { width: Some(border_w), bfc: true, ..Default::default() });
        self.avail_h = avail_h;
        self.floats = floats;
        // With `top: auto`, `bottom` places it once the containing block's height is known.
        let depth = self.cbs.len();
        if let (None, false, true, true) = (top, st.position == Position::Fixed, st.inset[2] != Len::Auto, depth > 1) {
            let bottom = st.inset[2].resolve(cw) + px_only(st.margin[2]);
            let span = |a: usize, b: usize| (a, b);
            self.pending.push(Pending { depth, items: span(items_at, self.items.len()), links: span(links_at, self.links.len()), hits: span(hits_at, self.hits.len()), bottom, y, h });
        }
        // position: fixed stays where it is in the window as the page scrolls.
        if st.position == Position::Fixed {
            let bottom = (top.is_none()).then(|| (st.inset[2] != Len::Auto).then(|| st.inset[2].resolve(cw))).flatten();
            self.add_layer(items_at, hits_at, links_at, LayerKind::Fixed { bottom, y, h });
        }
    }

    /// The content width a table takes inside `avail`.
    /// The narrowest a table's content box can be.
    fn table_min(&mut self, t: &Table, s: &Style) -> f32 {
        let (mins, _) = self.columns(t, s);
        let sp = spacing(s).0 * if mins.is_empty() { 0.0 } else { mins.len() as f32 + 1.0 };
        let caps = t.captions.iter().map(|c| self.intrinsic(c).0).fold(0.0, f32::max);
        (mins.iter().sum::<f32>() + sp).max(caps)
    }

    fn table_width(&mut self, node: NodeId, t: &Table, s: &Style, avail: f32) -> f32 {
        let (mins, maxs) = self.columns(t, s);
        let (mut sum_min, sum_max): (f32, f32) = (mins.iter().sum(), maxs.iter().sum());
        // At least as wide as a caption's longest word.
        for cap in &t.captions {
            sum_min = sum_min.max(self.intrinsic(cap).0);
        }
        let sp = spacing(s).0 * if mins.is_empty() { 0.0 } else { mins.len() as f32 + 1.0 };
        // (box-sizing: border-box, as <table> has: the width includes the borders and padding.)
        let frame = match self.collapsed.get(&node) {
            _ if !s.border_box => 0.0,
            Some(bw) => bw[1] + bw[3],
            None => px_only(s.padding[1]) + px_only(s.padding[3]) + s.border_width[1] + s.border_width[3],
        };
        match s.width {
            Len::Auto if s.width_kw == Some(Sizing::Stretch) => avail.max(sum_min + sp),
            Len::Auto => (sum_max + sp).min(avail).max(sum_min + sp),
            w => (w.resolve(avail + frame) - frame).max(sum_min + sp),
        }
    }

    /// A box's height when laid out `w` wide (border box), without painting anything.
    fn measure_block(&mut self, b: &BlockBox, w: f32) -> f32 {
        self.measure_with(b, w, false)
    }

    /// The border-box height of `b`'s content alone, laid out `w` wide.
    fn measure_content(&mut self, b: &BlockBox, w: f32) -> f32 {
        self.measure_with(b, w, true)
    }

    fn measure_with(&mut self, b: &BlockBox, w: f32, content: bool) -> f32 {
        self.measure_lines(b, w, content).0
    }

    /// A box's border-box height laid out `w` wide, and the first and last baselines of the text in it.
    fn measure_lines(&mut self, b: &BlockBox, w: f32, content: bool) -> (f32, Option<(f32, f32)>) {
        let (ni, nl, nh, nb) = (self.items.len(), self.links.len(), self.hits.len(), self.baselines.len());
        let floats = self.floats.clone();
        let h = self.block(b, 0.0, 0.0, w, Opts { bfc: true, width: Some(w), content, ..Default::default() });
        let mut lines: Option<(f32, f32)> = None;
        for &(d, y) in &self.baselines[nb..] {
            if d == self.line_depth {
                lines = Some(lines.map_or((y, y), |(a, _)| (a, y)));
            }
        }
        self.baselines.truncate(nb);
        self.items.truncate(ni);
        self.layers.retain(|l| l.items.0 < ni);
        self.segs.retain(|g| g.items.0 < ni);
        self.pending.retain(|p| p.items.0 < ni);
        self.links.truncate(nl);
        self.hits.truncate(nh);
        self.floats = floats;
        (h, lines)
    }

    /// Lays out a table's captions and rows in a box `width` wide, `border-spacing`
    /// `(hs, vs)` around and between its cells; returns the height.
    fn table(&mut self, t: &Table, s: &Style, x: f32, y: f32, width: f32, min_h: f32) -> f32 {
        let (hs, vs) = spacing(s);
        let (n, places) = place(t);
        // The columns share what the spacing leaves.
        let table_w = width;
        let width = if n == 0 { width } else { (width - hs * (n as f32 + 1.0)).max(0.0) };
        let widths: Vec<f32> = if n == 0 {
            Vec::new()
        } else if fixed_layout(s) {
            self.fixed_columns(t, &places, n, Some(width), hs)
        } else {
            let (mins, maxs) = self.columns(t, s);
            let (sum_min, sum_max): (f32, f32) = (mins.iter().sum(), maxs.iter().sum());
            if width >= sum_max {
                // Room to spare: share it out in proportion to each column's content.
                (0..n).map(|k| if sum_max > 0.0 { maxs[k] + (width - sum_max) * maxs[k] / sum_max } else { width / n as f32 }).collect()
            } else {
                let spread = sum_max - sum_min;
                (0..n).map(|k| mins[k] + if spread > 0.0 { (width - sum_min).max(0.0) * (maxs[k] - mins[k]) / spread } else { 0.0 }).collect()
            }
        };
        // Each column's left edge.
        let mut col_x = Vec::with_capacity(n + 1);
        let mut at = x + hs;
        for w in &widths {
            col_x.push(at);
            at += w + hs;
        }
        col_x.push(at);
        let span_w = |col: usize, span: usize| -> f32 { widths[col..col + span].iter().sum::<f32>() + hs * (span as f32 - 1.0) };
        let mut cursor = y;
        if !t.rows.is_empty() {
            cursor += vs;
        }
        // Row heights: the row's own, its cells'; a cell spanning rows makes the last of them taller if it must.
        let nr = t.rows.len();
        let mut heights = vec![0f32; nr];
        // (Each cell's height, its content's height, and for a baseline-aligned cell, its first baseline.)
        let mut measured: Vec<Vec<(f32, f32, Option<f32>)>> = Vec::with_capacity(nr);
        let mut row_base = vec![0f32; nr];
        for (ri, (row, at)) in t.rows.iter().zip(&places).enumerate() {
            if let Some(Len::Px(h)) = row.node.map(|r| self.styles[r as usize].height) {
                heights[ri] = h;
            }
            let mut m = Vec::new();
            for (cell, &(col, span, rows)) in row.cells.iter().zip(at) {
                let cw = span_w(col, span);
                let (h, lines) = self.measure_lines(&cell.b, cw, false);
                let cs = &self.styles[cell.b.node as usize];
                let valign = cs.valign;
                // (A cell with no line in it sits at the top, out of the row's baseline.)
                let base = lines.filter(|_| !matches!(valign, VAlign::Top | VAlign::Middle | VAlign::Bottom)).map(|l| l.0);
                let content_h = if matches!(valign, VAlign::Middle | VAlign::Bottom) { self.measure_content(&cell.b, cw) } else { 0.0 };
                if rows == 1 {
                    heights[ri] = heights[ri].max(h);
                }
                if let Some(b) = base {
                    row_base[ri] = row_base[ri].max(b);
                }
                m.push((h, content_h, base));
            }
            measured.push(m);
        }
        // Baseline-aligned cells line up their first lines, which can make the row taller.
        for (ri, m) in measured.iter().enumerate() {
            for (k, &(h, _, base)) in m.iter().enumerate() {
                if let (Some(b), 1) = (base, places[ri][k].2) {
                    heights[ri] = heights[ri].max(row_base[ri] - b + h);
                }
            }
        }
        for (ri, at) in places.iter().enumerate() {
            for (k, &(_, _, rows)) in at.iter().enumerate() {
                if rows > 1 {
                    let have: f32 = heights[ri..ri + rows].iter().sum::<f32>() + vs * (rows as f32 - 1.0);
                    let need = measured[ri][k].0;
                    if need > have {
                        heights[ri + rows - 1] += need - have;
                    }
                }
            }
        }
        // A table taller than its rows shares the extra height out, in proportion to their heights.
        let rows_h: f32 = heights.iter().map(|h| h + vs).sum();
        let extra = min_h - (cursor - y) - rows_h;
        if extra > 0.0 && nr > 0 {
            let total: f32 = heights.iter().sum();
            for h in &mut heights {
                *h += if total > 0.0 { extra * *h / total } else { extra / nr as f32 };
            }
        }
        let mut row_y = Vec::with_capacity(nr + 1);
        for h in &heights {
            row_y.push(cursor);
            cursor += h + vs;
        }
        row_y.push(cursor);
        let span_h = |ri: usize, rows: usize| -> f32 { row_y[ri + rows] - vs - row_y[ri] };
        // Where each row group and column group reaches, for their backgrounds.
        let group_rows = |g: NodeId| -> (usize, usize) {
            let first = t.rows.iter().position(|r| r.group == Some(g)).unwrap_or(0);
            let last = t.rows.iter().rposition(|r| r.group == Some(g)).unwrap_or(first);
            (first, last + 1)
        };
        let col_group = |g: NodeId| -> (usize, usize) {
            let first = t.cols.iter().position(|c| c.1 == Some(g)).unwrap_or(0);
            let last = t.cols.iter().rposition(|c| c.1 == Some(g)).unwrap_or(first);
            (first, (last + 1).min(n))
        };
        let (top, bottom) = (row_y[0], row_y[nr] - if nr > 0 { vs } else { 0.0 });
        for (ri, (row, at)) in t.rows.iter().zip(&places).enumerate() {
            for (k, (cell, &(col, span, rows))) in row.cells.iter().zip(at).enumerate() {
                let (cx, cy, cw, ch) = (col_x[col], row_y[ri], span_w(col, span), span_h(ri, rows));
                let cs = &self.styles[cell.b.node as usize];
                if cs.empty_cells_hide && !s.border_collapse && is_empty_cell(&cell.b) {
                    continue;
                }
                // Behind the cell: its column group's, column's, row group's and row's backgrounds.
                let (colgroup, column) = t.cols.get(col).map_or((None, None), |c| (c.1, c.0));
                let layers = [
                    colgroup.map(|g| {
                        let (a, z) = col_group(g);
                        (g, (col_x[a], top, col_x[z.max(a + 1).min(n)] - hs - col_x[a], bottom - top))
                    }),
                    column.map(|c| (c, (col_x[col], top, widths[col], bottom - top))),
                    row.group.map(|g| {
                        let (a, z) = group_rows(g);
                        (g, (x + hs, row_y[a], table_w - 2.0 * hs, row_y[z] - vs - row_y[a]))
                    }),
                    row.node.map(|r| (r, (x + hs, cy, table_w - 2.0 * hs, heights[ri]))),
                ];
                for (node, area) in layers.into_iter().flatten() {
                    let ls = &self.styles[node as usize];
                    if ls.background.a > 0 {
                        self.items.push(Item::Rect { x: cx, y: cy, w: cw, h: ch, color: ls.background, radius: 0.0 });
                    }
                    let drawn = self.bg_items(ls, area, [0.0; 4], [0.0; 4], Some((cx, cy, cw, ch)), 0.0);
                    self.items.extend(drawn);
                }
                // vertical-align: middle or bottom moves the content down within the row.
                let (_, content_h, base) = measured[ri][k];
                let shift = match self.styles[cell.b.node as usize].valign {
                    VAlign::Middle => (ch - content_h) / 2.0,
                    VAlign::Bottom => ch - content_h,
                    _ => base.map_or(0.0, |b| row_base[ri] - b),
                }
                .max(0.0);
                self.block(&cell.b, cx, cy, cw, Opts { min_h: ch, bfc: true, shift, ..Default::default() });
            }
        }
        if s.border_collapse && nr > 0 {
            self.paint_collapsed(t, s, &col_x, &row_y);
        }
        cursor - y
    }

    /// Lays out a table's captions on one side of it, `w` wide, from `cursor` down.
    fn place_captions(&mut self, t: &Table, x: f32, cursor: &mut f32, w: f32, bottom: bool) {
        for cap in t.captions.iter().filter(|c| self.styles[c.node as usize].caption_bottom == bottom) {
            let (mt, mb) = vmargins(&self.styles[cap.node as usize], w);
            *cursor += mt;
            *cursor += self.block(cap, x, *cursor, w, Opts::default());
            *cursor += mb;
        }
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
                // (Auto margins across keep an item from stretching.)
                stretch: st.height == Len::Auto && st.margin[0] != Len::Auto && st.margin[2] != Len::Auto,
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
        let mut prepared = Vec::new();
        for mut line in lines {
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
            let mut line_h = line.iter().zip(&heights).map(|(i, h)| h + i.mt + i.mb).fold(0.0, f32::max);
            if !s.wrap {
                // A single line is as tall as the container, when that's known, or within its min and max heights.
                line_h = match self.avail_h {
                    Some(h) => h,
                    None => {
                        let vframe = px_only(s.padding[0]) + px_only(s.padding[2]) + s.border_width[0] + s.border_width[2];
                        let content = |l: Len| if let Len::Px(v) = l { Some(if s.border_box { (v - vframe).max(0.0) } else { v }) } else { None };
                        let h = content(s.max_height).map_or(line_h, |m| line_h.min(m));
                        content(s.min_height).map_or(h, |m| h.max(m))
                    }
                };
            }
            prepared.push((line, start, between, auto_each, heights, line_h));
        }
        // align-content shares the container's spare height among wrapped lines.
        let nl = prepared.len();
        let spare = match self.avail_h {
            Some(h) if s.wrap => h - prepared.iter().map(|p| p.5).sum::<f32>() - s.row_gap * nl.saturating_sub(1) as f32,
            _ => 0.0,
        };
        let nf = nl as f32;
        let (mut top, gap_extra, grow_each) = match (s.align_content, spare > 0.0) {
            (_, false) => (y, 0.0, 0.0),
            (AlignContent::Stretch, true) => (y, 0.0, spare / nf),
            (AlignContent::Start, true) => (y, 0.0, 0.0),
            (AlignContent::End, true) => (y + spare, 0.0, 0.0),
            (AlignContent::Center, true) => (y + spare / 2.0, 0.0, 0.0),
            (AlignContent::Between, true) => (y, if nl > 1 { spare / (nf - 1.0) } else { 0.0 }, 0.0),
            (AlignContent::Around, true) => (y + spare / nf / 2.0, spare / nf, 0.0),
            (AlignContent::Evenly, true) => (y + spare / (nf + 1.0), spare / (nf + 1.0), 0.0),
        };
        for (li, (line, start, between, auto_each, heights, line_h)) in prepared.into_iter().enumerate() {
            let line_h = line_h + grow_each;
            if li > 0 {
                top += s.row_gap + gap_extra;
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
                self.block(i.b, cx, top + i.mt + dy, i.size, Opts { width: Some(i.size), min_h, bfc: true, ..Default::default() });
                cx += i.size + i.mr + if i.auto_r { auto_each } else { 0.0 } + s.column_gap + between;
            }
            top += line_h;
        }
        // (With a known height, the lines may not fill it; the container is that tall anyway.)
        top - y
    }

    /// A column flex container: items stack, each as wide as `align` says. When the
    /// container's height is fixed, they grow or shrink to fill it, auto margins
    /// take what's left, then justify-content places them.
    fn flex_column(&mut self, items: &[BlockBox], s: &Style, x: f32, y: f32, width: f32) -> f32 {
        let mut order: Vec<&BlockBox> = items.iter().collect();
        if s.reverse {
            order.reverse();
        }
        let its = self.column_items(&order, s, x, width);
        let Some(r) = self.avail_h.filter(|_| s.wrap && its.len() > 1) else {
            return self.column_line(its, s, y);
        };
        // flex-wrap: items go into columns of the container's height.
        let mut lines: Vec<Vec<usize>> = Vec::new();
        let mut used = 0.0;
        for (k, i) in its.iter().enumerate() {
            let o = i.size + i.mt + i.mb;
            match lines.last_mut() {
                Some(l) if used + s.row_gap + o <= r + FIT => {
                    l.push(k);
                    used += s.row_gap + o;
                }
                _ => {
                    lines.push(vec![k]);
                    used = o;
                }
            }
        }
        if lines.len() == 1 {
            return self.column_line(its, s, y);
        }
        // Each line is as wide as its widest item; spare width is shared out (align-content: normal).
        let mut cross: Vec<f32> = lines.iter().map(|l| l.iter().map(|&k| its[k].cross).fold(0.0, f32::max)).collect();
        let spare = width - cross.iter().sum::<f32>() - s.column_gap * (lines.len() - 1) as f32;
        if spare > 0.0 {
            let each = spare / lines.len() as f32;
            cross.iter_mut().for_each(|c| *c += each);
        }
        let (mut lx, mut h) = (x, 0.0f32);
        for (l, c) in lines.iter().zip(cross) {
            let order: Vec<&BlockBox> = l.iter().map(|&k| its[k].b).collect();
            let line = self.column_items(&order, s, lx, c);
            h = h.max(self.column_line(line, s, y));
            lx += c + s.column_gap;
        }
        h
    }

    /// The items of one column of a column flex container, with their hypothetical sizes.
    fn column_items<'b>(&mut self, order: &[&'b BlockBox], s: &Style, x: f32, width: f32) -> Vec<ColItem<'b>> {
        let room = self.avail_h;
        let mut its = Vec::new();
        for &b in order {
            let st = self.styles[b.node as usize].clone();
            let (mt, mb) = vmargins(&st, width);
            let align = st.align_self.unwrap_or(s.align_items);
            let (ml, mr) = (st.margin[3].resolve(width), st.margin[1].resolve(width));
            let avail = (width - ml - mr).max(0.0);
            let fixed = st.width != Len::Auto && st.margin[1] != Len::Auto && st.margin[3] != Len::Auto;
            let at = if fixed && matches!(align, AlignItems::Center | AlignItems::End) {
                // A fixed-width item is aligned in the cross axis too, overflowing both sides when centred.
                let hframe = st.padding[1].resolve(width) + st.padding[3].resolve(width) + st.border_width[1] + st.border_width[3];
                let w = st.width.resolve(width) + if st.border_box { 0.0 } else { hframe };
                let dx = if align == AlignItems::Center { (avail - w) / 2.0 } else { avail - w };
                (x + dx, width, None)
            } else if st.width != Len::Auto || align == AlignItems::Stretch {
                (x, width, None)
            } else {
                let (_, max) = self.intrinsic(b);
                let w = max.min(avail);
                let dx = match align {
                    AlignItems::Center => (avail - w) / 2.0,
                    AlignItems::End => avail - w,
                    _ => 0.0,
                };
                (x + ml + dx, w, Some(w))
            };
            // The width the item asks of a line when columns wrap.
            let cross = if !s.wrap {
                width
            } else if let Some(w) = at.2 {
                w + ml + mr
            } else if st.width != Len::Auto {
                let hframe = st.padding[1].resolve(width) + st.padding[3].resolve(width) + st.border_width[1] + st.border_width[3];
                st.width.resolve(width) + if st.border_box { 0.0 } else { hframe } + ml + mr
            } else {
                self.intrinsic(b).1.min(avail) + ml + mr
            };
            let vframe = px_only(st.padding[0]) + px_only(st.padding[2]) + st.border_width[0] + st.border_width[2];
            let border = |h: f32| if st.border_box { h } else { h + vframe };
            let resolve = |l: Len| match l {
                Len::Px(v) => Some(v),
                Len::Pct(p) => room.map(|r| r * p / 100.0),
                _ => None,
            };
            // The flex basis: flex-basis, else the height, else the content's height.
            let measured = |me: &mut Self| me.measure_block(b, at.2.unwrap_or((width - ml - mr).max(0.0)));
            let me_content = |me: &mut Self| me.measure_content(b, at.2.unwrap_or((width - ml - mr).max(0.0)));
            let explicit = resolve(st.basis).or(resolve(st.height)).map(border);
            let base = match explicit {
                Some(h) => h,
                None => measured(self),
            };
            // Not below min-height, or (with none set, unless it clips) its content, or its
            // height if that's smaller (the automatic minimum size).
            let specified = resolve(st.height).map(border);
            let min = match resolve(st.min_height) {
                Some(m) => border(m),
                None if st.overflow_clip => 0.0,
                None => match (specified, explicit) {
                    (Some(h), _) => h.min(me_content(self)),
                    (None, Some(_)) => measured(self),
                    (None, None) => base,
                },
            };
            let max = resolve(st.max_height).map(border).unwrap_or(f32::INFINITY);
            its.push(ColItem { b, at, cross, size: base.clamp(min, max.max(min)), base, min, max, grow: st.grow, shrink: st.shrink, mt, mb, auto_t: st.margin[0] == Len::Auto, auto_b: st.margin[2] == Len::Auto });
        }
        its
    }

    /// Flexes one column of items into the container's height and lays them out.
    fn column_line(&mut self, mut its: Vec<ColItem>, s: &Style, y: f32) -> f32 {
        let room = self.avail_h;
        let n = its.len();
        let gaps = s.row_gap * n.saturating_sub(1) as f32;
        let outer = |its: &[ColItem]| its.iter().map(|i| i.size + i.mt + i.mb).sum::<f32>();
        let mut free = room.map(|r| r - outer(&its) - gaps).unwrap_or(0.0);
        if free > 0.0 {
            // Grow, in proportion to flex-grow, up to max-height.
            let mut frozen = vec![false; n];
            for _ in 0..n {
                let total: f32 = its.iter().zip(&frozen).filter(|(_, f)| !**f).map(|(i, _)| i.grow).sum();
                if total <= 0.0 || free <= FIT {
                    break;
                }
                let share = free;
                for (k, i) in its.iter_mut().enumerate() {
                    if !frozen[k] {
                        i.size += share * i.grow / total;
                        if i.size >= i.max {
                            i.size = i.max;
                            frozen[k] = true;
                        }
                    }
                }
                free = room.unwrap_or(0.0) - outer(&its) - gaps;
            }
        } else if free < 0.0 {
            // Shrink in proportion to flex-shrink × base, never below the minimum.
            let mut frozen = vec![false; n];
            for _ in 0..n {
                let weights: f32 = its.iter().zip(&frozen).filter(|(_, f)| !**f).map(|(i, _)| i.shrink * i.base).sum();
                if weights <= 0.0 || free >= -FIT {
                    break;
                }
                let over = -free;
                for (k, i) in its.iter_mut().enumerate() {
                    if !frozen[k] {
                        i.size -= over * i.shrink * i.base / weights;
                        if i.size <= i.min {
                            i.size = i.min;
                            frozen[k] = true;
                        }
                    }
                }
                free = room.unwrap_or(0.0) - outer(&its) - gaps;
            }
        }
        // Auto margins take the free space first; then justify-content.
        let free = free.max(0.0);
        let autos = its.iter().map(|i| i.auto_t as usize + i.auto_b as usize).sum::<usize>();
        let auto_each = if autos > 0 { free / autos as f32 } else { 0.0 };
        let free = if autos > 0 { 0.0 } else { free };
        let nf = n as f32;
        let (start, between) = match s.justify {
            Justify::Start => (0.0, 0.0),
            Justify::End => (free, 0.0),
            Justify::Center => (free / 2.0, 0.0),
            Justify::Between => (0.0, if n > 1 { free / (nf - 1.0) } else { 0.0 }),
            Justify::Around => (free / nf / 2.0, free / nf),
            Justify::Evenly => (free / (nf + 1.0), free / (nf + 1.0)),
        };
        let mut top = y + start;
        for (k, i) in its.iter().enumerate() {
            if k > 0 {
                top += s.row_gap + between;
            }
            top += i.mt + if i.auto_t { auto_each } else { 0.0 };
            // With a fixed container height the flexed size is the item's height.
            let height = room.map(|_| i.size);
            let (bx, cw, forced) = i.at;
            let h = self.block(i.b, bx, top, cw, Opts { width: forced, bfc: true, height, ..Default::default() });
            top += h + i.mb + if i.auto_b { auto_each } else { 0.0 };
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
                self.block(b, bx, top + mt, iw, Opts { width: Some(iw), min_h, bfc: true, ..Default::default() });
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
            drop: 0.0,
            mt: 0.0,
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
                Inline::Resume(node) => {
                    stack.push(*node);
                    inside = Rc::from(stack.clone());
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
                        // The atom holds the border box; the picture is inset by the padding and borders.
                        let [ft, fr, fb, fl] = image_frame(s);
                        let (w, h) = (w + fl + fr, h + ft + fb);
                        let mut a = atom(Kind::Image, String::new(), ml + w + mr, *node, *link, &inside);
                        a.image = Some((ml, w, h, img.src.as_str()));
                        atoms.push(a);
                        last_space = false;
                    }
                }
                Inline::Block(b, link) => {
                    let s = self.styles[b.node as usize].clone();
                    let (ml, mr) = (s.margin[3].resolve(cb_w), s.margin[1].resolve(cb_w));
                    let (bws, own) = self.borders(b);
                    let own = own || !matches!(b.kind, BoxKind::Table(_));
                    let pad: [f32; 4] = std::array::from_fn(|i| if own { s.padding[i].resolve(cb_w) } else { 0.0 });
                    let frame = pad[1] + pad[3] + bws[1] + bws[3];
                    let border_box = |l: Len| if s.border_box { l.resolve(cb_w) } else { l.resolve(cb_w) + frame };
                    let mut w = if s.width != Len::Auto {
                        border_box(s.width)
                    } else {
                        let (mn, mx) = self.intrinsic(b);
                        if cb_w > 0.0 { mx.min(cb_w - ml - mr).max(mn) } else { mx }
                    };
                    if let Len::Px(_) | Len::Pct(_) | Len::Mix(..) = s.max_width {
                        w = w.min(border_box(s.max_width)).max(0.0);
                    }
                    if s.min_width != Len::Auto {
                        w = w.max(border_box(s.min_width));
                    }
                    let (h, lines) = self.measure_lines(b, w, false);
                    // Its baseline: an inline-table's first row's, an inline-block's last line's
                    // (its bottom edge when it clips or has no lines).
                    let table = matches!(b.kind, BoxKind::Table(_));
                    let (mt, mb) = vmargins(&s, cb_w);
                    let drop = match lines {
                        Some((first, _)) if table => (h - first).max(0.0) + mb,
                        Some((_, last)) if !s.overflow_clip => (h - last).max(0.0) + mb,
                        _ => 0.0,
                    };
                    let mut a = atom(Kind::Block, String::new(), ml + w + mr, b.node, *link, &inside);
                    a.image = Some((ml, w, mt + h + mb, ""));
                    a.float = Some(b);
                    a.drop = drop;
                    a.mt = mt;
                    atoms.push(a);
                    last_space = false;
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
        if !atoms.iter().any(|a| matches!(a.kind, Kind::Word | Kind::Image | Kind::Block | Kind::Break | Kind::Float | Kind::Abs) || (a.kind == Kind::Space && pre(&self.styles[a.style as usize]))) {
            return 0.0;
        }
        let mut queue: VecDeque<Atom<'r>> = atoms.into();
        let mut top = y;
        while !queue.is_empty() {
            let est = bs.line_height;
            // Too narrow beside the floats for the next word or image: go below them.
            let (mut left, mut right) = self.span(top, est, x, width);
            for _ in 0..64 {
                let need = queue.iter().find(|a| matches!(a.kind, Kind::Word | Kind::Image | Kind::Block)).map(|a| a.w).unwrap_or(0.0);
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
            // A space that hung past the end leaves a break opportunity before what follows.
            let mut hung = false;
            while let Some(a) = queue.pop_front() {
                let space = self.styles[a.style as usize].space;
                let wraps = matches!(space, Space::Normal | Space::PreWrap) || hung;
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
                    // A collapsible space hangs past the end: what follows decides whether the line breaks.
                    if self.collapsible(&a) {
                        hung = true;
                        continue;
                    }
                    queue.push_front(a);
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
                        let more = Atom { kind: Kind::Word, text: tail, w: tw, style: head.style, link: head.link, inside: head.inside.clone(), image: None, float: None, drop: 0.0, mt: 0.0 };
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
                if a.kind != Kind::Open {
                    hung = false;
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
                Kind::Image | Kind::Block => {
                    let h = a.image.map(|i| i.2).unwrap_or(0.0);
                    match s.valign {
                        VAlign::Middle => {
                            above = above.max(h / 2.0 + mid);
                            below = below.max(h / 2.0 - mid);
                        }
                        VAlign::Top | VAlign::Bottom => {} // placed once the rest has set the line's height
                        VAlign::TextTop => {
                            above = above.max(ba);
                            below = below.max(h - ba);
                        }
                        VAlign::TextBottom => {
                            above = above.max(h - bd);
                            below = below.max(bd);
                        }
                        _ => {
                            above = above.max(h - a.drop - s.shift);
                            below = below.max(a.drop + s.shift);
                        }
                    }
                }
                _ => {}
            }
        }
        // Boxes aligned to the line's top (bottom) make it taller downwards (upwards) when they don't fit.
        for a in &line {
            if matches!(a.kind, Kind::Image | Kind::Block) {
                let h = a.image.map(|i| i.2).unwrap_or(0.0);
                match self.styles[a.style as usize].valign {
                    VAlign::Top => below = below.max(h - above),
                    VAlign::Bottom => above = above.max(h - below),
                    _ => {}
                }
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
        self.baselines.push((self.line_depth, baseline));
        // Where an image or inline-block `h` tall goes.
        let box_y = |s: &Style, h: f32, drop: f32| match s.valign {
            VAlign::Middle => baseline - mid - h / 2.0,
            VAlign::Top => top,
            VAlign::Bottom => top + line_h - h,
            VAlign::TextTop => baseline - ba,
            VAlign::TextBottom => baseline + bd - h,
            // (An inline-block's vertical-align: <length> raises it; an inline's shift has that already.)
            _ => baseline + s.shift + drop - h - if s.display == Display::Inline { 0.0 } else { s.raise },
        };
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
            if s.masked || s.hidden {
                continue;
            }
            if s.background.a > 0 {
                self.items.push(Item::Rect { x: x0, y: y0, w: (x1 - x0).max(0.0), h: y1 - y0, color: s.background, radius: radius(&s, x1 - x0, y1 - y0) });
            }
            // The left and right borders only where the box starts and ends (it may span lines).
            let opens = line[i0].kind == Kind::Open && line[i0].style == e;
            let closes = line[i1].kind == Kind::Close && line[i1].style == e;
            border_items((x0, y0, (x1 - x0).max(0.0), y1 - y0), s.border_width, &s, [true, closes, true, opens], &mut self.items);
        }
        // Text (atoms sharing a style and link draw as one run) and images.
        let mut last_text = None;
        let mut i = 0;
        while i < line.len() {
            let a = &line[i];
            match a.kind {
                Kind::Block => {
                    let s = self.styles[a.style as usize].clone();
                    if let (Some((ml, w, h, _)), Some(b)) = (a.image, a.float) {
                        let y = box_y(&s, h, a.drop) + a.mt;
                        let (x, link) = (xs[i] + ml, a.link);
                        self.line_depth += 1;
                        self.block(b, x, y, w, Opts { width: Some(w), bfc: true, ..Default::default() });
                        self.line_depth -= 1;
                        if let Some(l) = link {
                            self.links.push(Link { x, y, w, h, href: self.hrefs[l].clone() });
                        }
                    }
                    i += 1;
                }
                Kind::Image => {
                    let s = &self.styles[a.style as usize];
                    if let Some((ml, w, h, src)) = a.image {
                        // position: relative moves it from where the line put it.
                        let (mut dx, mut dy) = (0.0, 0.0);
                        if s.position == Position::Relative {
                            let cw = self.cbs.last().map_or(0.0, |c| c.2);
                            dx = if s.inset[3] != Len::Auto { s.inset[3].resolve(cw) } else if s.inset[1] != Len::Auto { -s.inset[1].resolve(cw) } else { 0.0 };
                            dy = if s.inset[0] != Len::Auto { s.inset[0].resolve(cw) } else if s.inset[2] != Len::Auto { -s.inset[2].resolve(cw) } else { 0.0 };
                        }
                        let y = box_y(s, h, 0.0) + dy;
                        let x = xs[i] + ml + dx;
                        let [ft, fr, fb, fl] = image_frame(s);
                        if !s.hidden {
                            if s.background.a > 0 {
                                self.items.push(Item::Rect { x, y, w, h, color: s.background, radius: radius(s, w, h) });
                            }
                            border_items((x, y, w, h), s.border_width, s, [true; 4], &mut self.items);
                        }
                        if !s.hidden && !src.is_empty() {
                            let drawn = self.image_items(s, x + fl, y + ft, (w - fl - fr).max(0.0), (h - ft - fb).max(0.0), src);
                            self.items.extend(drawn);
                        }
                        self.hits.push(Hit { x, y, w, h, node: a.style });
                        if let Some(l) = a.link {
                            self.links.push(Link { x, y, w, h, href: self.hrefs[l].clone() });
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

/// How far down the canvas's background images reach, at least (the window may be taller than the page).
const CANVAS_H: f32 = 4096.0;

/// Lays out `dom` (styled by `styles`) for a viewport `width` px wide.
pub fn layout(dom: &Dom, styled: &css_style::Styled, width: f32, view_h: f32, fonts: &dyn Fonts, images: &dyn Images) -> Page {
    let styles = &styled.styles;
    let mut builder = Builder { dom, styles, before: &styled.before, after: &styled.after, first_letter: &styled.first_letter, anon: Vec::new(), hrefs: Vec::new() };
    let root = builder.block(0, 0);
    let mut all: Vec<Style> = styles.clone();
    all.extend(std::mem::take(&mut builder.anon));
    // The body's overflow goes to the viewport when the root's is visible: the body itself doesn't clip.
    if let (Some(h), Some(b)) = (dom.find("html"), dom.find("body")) {
        if !all[h as usize].overflow_clip && all[b as usize].overflow_clip {
            all[b as usize].overflow_clip = false;
        }
    }
    // The canvas takes the root's background, or else the body's.
    let painted = |id: NodeId| styles[id as usize].background.a > 0 || styles[id as usize].bg.as_ref().is_some_and(|g| g.images.iter().any(|i| !i.is_empty()));
    let canvas_node = match (dom.find("html"), dom.find("body")) {
        (Some(h), _) if painted(h) => h,
        (_, Some(b)) if painted(b) => b,
        _ => NodeId::MAX,
    };
    let mut ctx = Ctx { styles: &all, fonts, images, hrefs: builder.hrefs, items: Vec::new(), links: Vec::new(), hits: Vec::new(), cache: HashMap::new(), floats: Vec::new(), cbs: vec![(0.0, 0.0, width)], layers: Vec::new(), segs: Vec::new(), pending: Vec::new(), canvas_node, collapsed: HashMap::new(), baselines: Vec::new(), line_depth: 0, width_free: None, view_h, root_el: dom.find("html").unwrap_or(NodeId::MAX), avail_h: (view_h > 0.0).then_some(view_h), dom };
    // The canvas takes the root's background, or else the body's.
    let background = styles.get(canvas_node as usize).map(|s| s.background).filter(|c| c.a > 0).unwrap_or(Color::rgb(255, 255, 255));
    let height = ctx.block(&root, 0.0, 0.0, width, Opts { bfc: true, ..Default::default() });
    // Its background images cover the canvas, placed against the root element's box.
    if let (Some(cs), Some(root_hit)) = (styles.get(canvas_node as usize), ctx.hits.iter().find(|h| h.node == ctx.root_el).cloned()) {
        let rs = &styles[ctx.root_el as usize];
        let pad: [f32; 4] = std::array::from_fn(|i| rs.padding[i].resolve(width));
        let canvas = (0.0, 0.0, width, height.max(view_h).max(CANVAS_H));
        let items = ctx.bg_items(cs, (root_hit.x, root_hit.y, root_hit.w, root_hit.h), rs.border_width, pad, Some(canvas), 0.0);
        let n = items.len();
        if n > 0 {
            ctx.items.splice(0..0, items);
            ctx.layers_moved(0, n);
        }
    }
    // A sticky box stays within its parent's box.
    let mut layers = ctx.layers;
    for l in &mut layers {
        if let LayerKind::Sticky { parent, limit, .. } = &mut l.kind {
            if let Some(p) = ctx.hits.iter().rev().find(|h| h.node == *parent) {
                *limit = (p.y, p.y + p.h);
            } else {
                *limit = (0.0, height);
            }
        }
    }
    let items = stack(ctx.items, &ctx.segs, &mut layers);
    layers.sort_by_key(|l| l.items.0);
    Page { width, height, background, items, links: ctx.links, hits: ctx.hits, layers }
}

/// Where each table cell sits: the number of columns, and for each row its cells'
/// (first column, columns spanned, rows spanned), past cells from rows above that span down.
fn place(t: &Table) -> (usize, Vec<Vec<(usize, usize, usize)>>) {
    let nr = t.rows.len();
    // The last row of each row's group: rowspans stop there.
    let mut group_end = vec![0; nr];
    for r in (0..nr).rev() {
        group_end[r] = if r + 1 < nr && t.rows[r + 1].group == t.rows[r].group { group_end[r + 1] } else { r };
    }
    let mut taken: Vec<Vec<bool>> = vec![Vec::new(); nr];
    let mut out = Vec::with_capacity(nr);
    let mut n = t.cols.len();
    for (ri, row) in t.rows.iter().enumerate() {
        let mut col = 0;
        let mut placed = Vec::with_capacity(row.cells.len());
        for cell in &row.cells {
            while taken[ri].get(col).copied().unwrap_or(false) {
                col += 1;
            }
            let rows = cell.rowspan.min(group_end[ri] - ri + 1);
            for line in &mut taken[ri..ri + rows] {
                if line.len() < col + cell.colspan {
                    line.resize(col + cell.colspan, false);
                }
                line[col..col + cell.colspan].fill(true);
            }
            placed.push((col, cell.colspan, rows));
            col += cell.colspan;
            n = n.max(col);
        }
        out.push(placed);
    }
    (n, out)
}

/// Blink's darker shade of a colour, for the shadowed sides of inset, outset, groove and ridge borders.
fn dark(c: Color) -> Color {
    if (c.r, c.g, c.b) == (0, 0, 0) {
        return Color { r: 84, g: 84, b: 84, a: c.a };
    }
    let v = c.r.max(c.g).max(c.b) as f32 / 255.0;
    let m = if v == 0.0 { 0.0 } else { ((v - 0.33) / v).max(0.0) };
    let f = |x: u8| (x as f32 * m).round().min(255.0) as u8;
    Color { r: f(c.r), g: f(c.g), b: f(c.b), a: c.a }
}

/// A box's borders as rectangles, side by side in their styles; `sides` says which to draw.
fn border_items((x, y, w, h): (f32, f32, f32, f32), bw: [f32; 4], s: &Style, sides: [bool; 4], out: &mut Vec<Item>) {
    // The strip of side `i` from `a` to `b` of its width in from the outer edge (0..1).
    let strip = |i: usize, a: f32, b: f32| -> (f32, f32, f32, f32) {
        let (t0, r0, b0, l0) = (bw[0] * a, bw[1] * a, bw[2] * a, bw[3] * a);
        match i {
            0 => (x + l0, y + bw[0] * a, w - l0 - r0, bw[0] * (b - a)),
            1 => (x + w - bw[1] * b, y + t0, bw[1] * (b - a), h - t0 - b0),
            2 => (x + l0, y + h - bw[2] * b, w - l0 - r0, bw[2] * (b - a)),
            _ => (x + bw[3] * a, y + t0, bw[3] * (b - a), h - t0 - b0),
        }
    };
    for i in 0..4 {
        let (width, style, color) = (bw[i], s.border_style[i], s.border_color[i]);
        if !sides[i] || width <= 0.0 || color.a == 0 || !style.visible() {
            continue;
        }
        // Inset's top and left are in shadow, and outset's bottom and right; groove and ridge are half of each.
        let shade = |inset: bool| if inset == (i == 0 || i == 3) { dark(color) } else { color };
        let mut rect = |(rx, ry, rw, rh): (f32, f32, f32, f32), c: Color| {
            if rw > 0.0 && rh > 0.0 {
                out.push(Item::Rect { x: rx, y: ry, w: rw, h: rh, color: c, radius: 0.0 });
            }
        };
        match style {
            BorderStyle::Double if width >= 3.0 => {
                let third = ((width + 1.0) / 3.0).floor() / width;
                rect(strip(i, 0.0, third), color);
                rect(strip(i, 1.0 - third, 1.0), color);
            }
            BorderStyle::Inset => rect(strip(i, 0.0, 1.0), shade(true)),
            BorderStyle::Outset => rect(strip(i, 0.0, 1.0), shade(false)),
            BorderStyle::Groove | BorderStyle::Ridge => {
                let groove = style == BorderStyle::Groove;
                rect(strip(i, 0.0, 0.5), shade(groove));
                rect(strip(i, 0.5, 1.0), shade(!groove));
            }
            BorderStyle::Dotted | BorderStyle::Dashed => {
                // Dots as wide as the border (round once it's 3px), dashes three times as long,
                // spaced evenly so each side starts and ends with one.
                let (rx, ry, rw, rh) = strip(i, 0.0, 1.0);
                let along = if i % 2 == 0 { rw } else { rh };
                let dash = if style == BorderStyle::Dotted { width } else { (3.0 * width).min(along) };
                let n = (((along + dash) / (2.0 * dash)).round() as usize).max(1);
                let gap = if n > 1 { (along - n as f32 * dash) / (n - 1) as f32 } else { 0.0 };
                let radius = if style == BorderStyle::Dotted && width >= 3.0 { width / 2.0 } else { 0.0 };
                for k in 0..n.min(10_000) {
                    let at = k as f32 * (dash + gap);
                    let (dx, dy, dw, dh) = if i % 2 == 0 { (rx + at, ry, dash, rh) } else { (rx, ry + at, rw, dash) };
                    if dw > 0.0 && dh > 0.0 {
                        out.push(Item::Rect { x: dx, y: dy, w: dw, h: dh, color, radius });
                    }
                }
            }
            _ => rect(strip(i, 0.0, 1.0), color),
        }
    }
}

/// A border side: (width, style, colour).
type Side = (f32, BorderStyle, Color);

/// A collapsed-border table's resolved edges: each cell's (top, right, bottom, left),
/// the widest bottom edge, and which cell covers each grid slot.
struct Collapsed {
    cells: Vec<Vec<[Side; 4]>>,
    bottom: f32,
    owner: Vec<Vec<Option<(usize, usize)>>>,
}

/// The border that wins where several meet: `hidden` beats all, then the widest,
/// then by style; on a tie the earlier (the one nearer the top left, or the cell) wins.
fn winner(cands: &[Side]) -> Side {
    let none = (0.0, BorderStyle::None, Color { r: 0, g: 0, b: 0, a: 0 });
    if cands.iter().any(|c| c.1 == BorderStyle::Hidden) {
        return none;
    }
    let mut best = none;
    for &c in cands {
        if c.1.visible() && (best.1 == BorderStyle::None || c.0 > best.0 || (c.0 == best.0 && c.1 > best.1)) {
            best = c;
        }
    }
    best
}

/// `table-layout: fixed` applies (the table has a width).
fn fixed_layout(s: &Style) -> bool {
    s.table_fixed && (s.width != Len::Auto || s.width_kw == Some(Sizing::Stretch))
}

/// A cell with nothing in it (for `empty-cells: hide`).
fn is_empty_cell(b: &BlockBox) -> bool {
    b.children.iter().all(|c| match c {
        Child::Inline(run) => run.iter().all(|i| match i {
            Inline::Text { text, .. } => text.trim_matches([' ', '\t', '\n', '\r', '\x0c']).is_empty(),
            Inline::Open(_) | Inline::Close(_) | Inline::Resume(_) => true,
            _ => false,
        }),
        Child::Block(_) => false,
    })
}

/// A table's `border-spacing` (none when its borders collapse).
fn spacing(s: &Style) -> (f32, f32) {
    if s.border_collapse {
        (0.0, 0.0)
    } else {
        s.border_spacing
    }
}

/// Puts the display list in CSS painting order (CSS 2 appendix E): in each
/// stacking context, its background, then children with negative `z-index`,
/// then its flow, then positioned boxes (`z-index: auto` or 0) in tree order,
/// then positive `z-index`. Boxes moved out of a clipped run keep its clip.
/// Layers' item ranges are updated to match.
fn stack(items: Vec<Item>, segs: &[Seg], layers: &mut [Layer]) -> Vec<Item> {
    if segs.is_empty() {
        return items;
    }
    let n = segs.len();
    // segs[n] is the page itself.
    let mut all: Vec<Seg> = segs.to_vec();
    all.push(Seg { items: (0, items.len()), bg: 0, z: Some(0), ctx: true });
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by_key(|&i| (all[i].items.0, std::cmp::Reverse(all[i].items.1)));
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); n + 1];
    let mut open = vec![n];
    // The clips in force where each box starts.
    let mut clips: Vec<Vec<usize>> = vec![Vec::new(); n + 1];
    let mut clip_stack: Vec<usize> = Vec::new();
    let mut at = 0;
    for &i in &order {
        let (s, e) = all[i].items;
        while at < s {
            match items[at] {
                Item::Clip { .. } => clip_stack.push(at),
                Item::Unclip => {
                    clip_stack.pop();
                }
                _ => {}
            }
            at += 1;
        }
        clips[i] = clip_stack.clone();
        while let Some(&top) = open.last() {
            let (ts, te) = all[top].items;
            if ts <= s && e <= te {
                break;
            }
            open.pop();
        }
        children[*open.last().unwrap_or(&n)].push(i);
        open.push(i);
    }
    struct Painter<'a> {
        items: &'a [Item],
        all: &'a [Seg],
        children: &'a [Vec<usize>],
        clips: &'a [Vec<usize>],
        out: Vec<Item>,
        placed: Vec<Option<(usize, usize)>>,
    }
    impl Painter<'_> {
        /// A box's items less those of the positioned boxes inside it, skipping the first `skip`.
        fn own(&mut self, i: usize, skip: usize) {
            let (s, e) = self.all[i].items;
            let mut k = s + skip;
            for &c in &self.children[i] {
                let (cs, ce) = self.all[c].items;
                if cs > k {
                    self.out.extend_from_slice(&self.items[k..cs]);
                }
                k = k.max(ce);
            }
            if e > k {
                self.out.extend_from_slice(&self.items[k..e]);
            }
        }
        fn gather(&self, i: usize, list: &mut Vec<usize>) {
            for &c in &self.children[i] {
                list.push(c);
                if !self.all[c].ctx {
                    self.gather(c, list);
                }
            }
        }
        /// A stacking context.
        fn context(&mut self, i: usize) {
            let start = self.out.len();
            let Seg { items: (s, e), bg, .. } = self.all[i];
            let bg = bg.min(e - s);
            self.out.extend_from_slice(&self.items[s..s + bg]);
            let mut list = Vec::new();
            self.gather(i, &mut list);
            let z = |c: usize| self.all[c].z.filter(|_| self.all[c].ctx).unwrap_or(0);
            let mut neg: Vec<usize> = list.iter().copied().filter(|&c| z(c) < 0).collect();
            neg.sort_by_key(|&c| z(c));
            let mut pos: Vec<usize> = list.iter().copied().filter(|&c| z(c) > 0).collect();
            pos.sort_by_key(|&c| z(c));
            for c in neg {
                self.entry(i, c);
            }
            self.own(i, bg);
            for c in list.iter().copied().filter(|&c| z(c) == 0) {
                self.entry(i, c);
            }
            for c in pos {
                self.entry(i, c);
            }
            self.placed[i] = Some((start, self.out.len()));
        }
        /// A positioned box painted as part of context `ctx`, inside the clips it had there.
        fn entry(&mut self, ctx: usize, c: usize) {
            let from = self.all[ctx].items.0;
            let wrap: Vec<usize> = self.clips[c].iter().copied().filter(|&k| k >= from && (ctx == self.all.len() - 1 || k > from)).collect();
            for &k in &wrap {
                self.out.push(self.items[k].clone());
            }
            if self.all[c].ctx {
                self.context(c);
            } else {
                self.own(c, 0);
            }
            for _ in &wrap {
                self.out.push(Item::Unclip);
            }
        }
    }
    let mut p = Painter { items: &items, all: &all, children: &children, clips: &clips, out: Vec::with_capacity(items.len()), placed: vec![None; n + 1] };
    p.context(n);
    let (out, placed) = (p.out, p.placed);
    for l in layers.iter_mut() {
        if let Some(i) = (0..n).find(|&i| all[i].items == l.items) {
            if let Some(range) = placed[i] {
                l.items = range;
            }
        }
    }
    out
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
        let styled = style::compute(&dom, &[css.to_string()], &crate::css::Env { width, height: 600.0, dark: false });
        layout(&dom, &styled, width, 600.0, &ApproxFonts, &FixedImages)
    }

    #[test]
    fn fixed_and_sticky_layers() {
        let p = page(
            "<header class=bar>Top bar</header><section class=s><h2 class=st>Heading</h2><p class=tall>a</p></section><p class=tall>b</p><div class=banner>Cookies</div>",
            "body { margin: 0 } .bar { position: fixed; top: 0; left: 0; right: 0; height: 40px; background: #eee }\
             .s { padding: 0 } .st { position: sticky; top: 10px; margin: 0; height: 30px; background: #ddd } .tall { height: 1000px; margin: 0 }\
             .banner { position: fixed; bottom: 0; left: 0; height: 50px; background: #ccc }",
            800.0,
        );
        assert_eq!(p.layers.len(), 3, "{:?}", p.layers);
        let kinds: Vec<&LayerKind> = p.layers.iter().map(|l| &l.kind).collect();
        let bar = p.layers.iter().find(|l| matches!(l.kind, LayerKind::Fixed { bottom: None, .. })).unwrap();
        let banner = p.layers.iter().find(|l| matches!(l.kind, LayerKind::Fixed { bottom: Some(_), .. })).unwrap();
        let sticky = p.layers.iter().find(|l| matches!(l.kind, LayerKind::Sticky { .. })).expect("sticky");
        // The bar follows the window; the banner sits at its bottom edge.
        assert_eq!(p.layer_offset(bar, 500.0, 600.0), 500.0);
        let LayerKind::Fixed { y, h, .. } = banner.kind else { unreachable!() };
        assert_eq!(y + p.layer_offset(banner, 500.0, 600.0) + h, 1100.0, "{kinds:?}");
        // The heading sticks 10px from the top while its section is in view, then leaves with it.
        let LayerKind::Sticky { y, h, limit, .. } = sticky.kind else { unreachable!() };
        assert_eq!((y, h), (0.0, 30.0));
        assert_eq!(p.layer_offset(sticky, 0.0, 600.0), 10.0);
        assert_eq!(p.layer_offset(sticky, 400.0, 600.0), 410.0);
        assert_eq!(p.layer_offset(sticky, 2000.0, 600.0), limit.1 - 30.0);
        // Its items are drawn as a layer, and clicks find it where it shows.
        assert!(p.in_layer(sticky.items.0));
        let at = p.hit_scrolled(5.0, 400.0 + 15.0, 400.0, 600.0);
        assert!(at.is_some() && p.hits[sticky.hits.0..sticky.hits.1].iter().any(|h| Some(h.node) == at), "{at:?}");
        // Without the layers, that point is the tall paragraph's.
        assert!(!p.hits[sticky.hits.0..sticky.hits.1].iter().any(|h| Some(h.node) == p.hit(5.0, 415.0)));
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
        // (The paragraph's top margin collapses through the body, so the body's content starts at 10.)
        assert_eq!(float, Some((100.0, 10.0, 50.0, 60.0)));
        let f = Font { family: style::Family::Sans, size: 10.0, weight: 400, italic: false };
        for (s, x, y) in &t {
            if *y < 70.0 {
                assert!(x + ApproxFonts.width(s, &f) <= 100.01, "{s} overlaps the float");
            }
        }
        let after = t.iter().find(|x| x.0 == "after").unwrap();
        assert!(after.2 > 70.0, "clear: both goes below the float");
        // A floated image inside a paragraph, left: text starts beside it.
        let p = page("<p><img src='wide.png' width=40 style='float:left'>text</p>", BASE, 300.0);
        assert_eq!(images(&p)[0].0, 0.0);
        assert!(texts(&p)[0].1 >= 40.0);
    }

    #[test]
    fn stacking_first_letter_and_ratio() {
        let colors = |p: &Page| -> Vec<Color> { p.items.iter().filter_map(|i| if let Item::Rect { color, .. } = i { Some(*color) } else { None }).collect() };
        let (red, green, blue) = (Color::rgb(255, 0, 0), Color::rgb(0, 128, 0), Color::rgb(0, 0, 255));
        // Higher z-index paints later, whatever the tree order; positioned boxes paint over the flow after them.
        let p = page(
            "<div style='position:relative; z-index:2; background:green; height:10px'></div>\
             <div style='position:relative; z-index:1; background:red; height:10px'></div>\
             <div style='position:relative; background:blue; height:10px'></div><p style='background:#fff'>x</p>",
            BASE,
            100.0,
        );
        let c = colors(&p);
        let at = |k: Color| c.iter().position(|&x| x == k).unwrap();
        assert!(at(red) < at(green) && at(blue) < at(red), "{c:?}");
        assert!(at(Color::rgb(255, 255, 255)) < at(blue), "in-flow content goes under positioned boxes");
        // ::first-letter takes the punctuation around the letter.
        let p = page("<p>\u{201c}Hello, world</p>", &format!("{BASE} p::first-letter {{ color: red }}"), 300.0);
        let first = p.items.iter().find_map(|i| if let Item::Text { text, color, .. } = i { Some((text.clone(), *color)) } else { None }).unwrap();
        assert_eq!(first, ("\u{201c}H".to_string(), red));
        // aspect-ratio: an auto height from the width, and an auto width from a fixed height.
        let p = page("<div style='width:100px; aspect-ratio:2/1; background:red'></div><div style='height:30px; aspect-ratio:2; background:blue; float:left'></div>", BASE, 300.0);
        let rects: Vec<(f32, f32)> = p.items.iter().filter_map(|i| if let Item::Rect { w, h, .. } = i { Some((*w, *h)) } else { None }).collect();
        assert_eq!(rects, vec![(100.0, 50.0), (60.0, 30.0)]);
        // vertical-align: top hangs an image from the line's top, and the line is just as tall;
        // an inline image's padding sits around the picture.
        let p = page("<div>x<img src='wide.png' width=40 height=40 style='vertical-align:top; padding-left:5px'></div><p>after</p>", BASE, 300.0);
        let (ix, iy, _, ih) = images(&p)[0];
        assert_eq!((iy, ih), (0.0, 40.0));
        assert_eq!(ix, ApproxFonts.width("x", &Font { family: style::Family::Sans, size: 10.0, weight: 400, italic: false }) + 5.0);
        assert_eq!(texts(&p).iter().find(|t| t.0 == "after").unwrap().2 - 10.0 - 40.0 < 20.0, true, "the line is 40px tall");
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
        // First column starts after the border spacing (2px) and cell padding; headers are centred within their columns.
        assert_eq!(get("Comet").1, 4.0);
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
