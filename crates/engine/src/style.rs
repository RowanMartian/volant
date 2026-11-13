//! Computed styles: selector matching against the DOM, the cascade (user
//! agent sheet, author sheets, inline `style`, `!important`, specificity,
//! order), inheritance, and turning property values into numbers.

use crate::css::{self, AttrOp, Combinator, Compound, Decl, Env, Pseudo, PseudoElement, Rule};
use std::collections::HashMap;
use std::rc::Rc;
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
    /// `calc()` of px and %: px + pct% of the containing block.
    Mix(f32, f32),
    Auto,
}

impl Len {
    pub fn resolve(self, of: f32) -> f32 {
        match self {
            Len::Px(v) => v,
            Len::Pct(p) => of * p / 100.0,
            Len::Mix(px, p) => px + of * p / 100.0,
            Len::Auto => 0.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Justify {
    Start,
    Center,
    End,
    Between,
    Around,
    Evenly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlignItems {
    Stretch,
    Start,
    Center,
    End,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Position {
    Static,
    Relative,
    Absolute,
    Fixed,
    /// In the flow, but held at its `top` (or `bottom`) inset while its parent scrolls past.
    Sticky,
}

/// One `grid-template-columns` track.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Track {
    Len(Len),
    Fr(f32),
    /// `repeat(auto-fill | auto-fit, minmax(min, 1fr))`: as many as fit, at least `min` px wide.
    AutoFill(f32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Display {
    Block,
    Inline,
    /// An atomic inline box: laid out as a block, placed on a line like an image.
    InlineBlock,
    ListItem,
    None,
    Flex,
    Grid,
    Table,
    TableRowGroup,
    TableRow,
    TableCell,
    TableCaption,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Float {
    None,
    Left,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Clear {
    None,
    Left,
    Right,
    Both,
}

/// An intrinsic size keyword.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sizing {
    Min,
    Max,
    Fit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VAlign {
    /// The baseline, raised by `Style::raise` (a length or percentage).
    Baseline,
    Super,
    Sub,
    Middle,
    /// Against the line box's top or bottom.
    Top,
    Bottom,
    /// Against the top or bottom of the parent's font.
    TextTop,
    TextBottom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Family {
    Sans,
    Serif,
    Mono,
    /// A page's web font (`@font-face`), by its number in [`crate::fonts::face`]'s table.
    Face(u16),
}

impl Family {
    /// Sans, Serif or Mono: a web font's fallback, or the family itself.
    pub fn generic(self) -> Family {
        match self {
            Family::Face(i) => crate::fonts::face_info(i).map(|(_, f)| f).unwrap_or(Family::Sans),
            f => f,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
}

/// `object-fit`: how an image fills its box.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fit {
    Fill,
    Contain,
    Cover,
    None,
    ScaleDown,
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
    pub max_height: Len,
    pub height: Len,
    pub hidden: bool,
    pub float: Float,
    pub clear: Clear,
    pub valign: VAlign,
    /// How far this text's baseline sits below its line's (negative: raised),
    /// from `vertical-align` on it and its inline ancestors.
    pub shift: f32,
    /// `border-radius` (a percentage is of the box).
    pub radius: Len,
    /// `box-sizing: border-box`: width and height include padding and borders.
    pub border_box: bool,
    /// `caption-side: bottom` (inherited).
    pub caption_bottom: bool,
    /// `border-spacing` (horizontal, vertical), inherited.
    pub border_spacing: (f32, f32),
    /// `border-collapse: collapse`, inherited.
    pub border_collapse: bool,
    /// `z-index` (`None` for `auto`).
    pub z_index: Option<i32>,
    /// How far `vertical-align: <length>` raises the box.
    pub raise: f32,
    /// `aspect-ratio` as width / height, and whether `auto` came with it
    /// (an image's own ratio then wins).
    pub aspect: Option<(f32, bool)>,
    /// `width: min-content | max-content | fit-content` (`width` is then auto).
    pub width_kw: Option<Sizing>,
    /// `contain: size` (or `strict`): the box sizes as if empty, or to `contain-intrinsic-size`.
    pub contain_size: bool,
    /// `contain-intrinsic-size` (width, height) in px.
    pub intrinsic_size: (Option<f32>, Option<f32>),
    /// `object-fit` and `object-position` (x, y) for images.
    pub object_fit: Fit,
    pub object_pos: (Len, Len),
    /// 0 (invisible) to 1.
    pub opacity: f32,
    /// Painted through a CSS mask (icons): the background alone would be a solid square.
    pub masked: bool,
    /// A table's `cellpadding`, passed down to its cells.
    pub cell_padding: Option<f32>,
    /// Whether CSS set the padding (then `cellpadding` doesn't apply).
    pub padding_set: bool,
    /// `overflow` other than `visible`: content is clipped to the padding box.
    pub overflow_clip: bool,
    /// `transform: translate(x, y)` (percentages of the box's own size).
    pub translate: Option<(Len, Len)>,
    pub min_width: Len,
    pub min_height: Len,
    /// Custom properties (`--name`), inherited.
    pub vars: Rc<HashMap<String, String>>,
    // Flexbox and grid containers.
    pub column: bool,
    pub reverse: bool,
    pub wrap: bool,
    pub justify: Justify,
    pub align_items: AlignItems,
    pub row_gap: f32,
    pub column_gap: f32,
    pub columns: Vec<Track>,
    // Flex and grid items.
    pub grow: f32,
    pub shrink: f32,
    pub basis: Len,
    pub order: i32,
    pub align_self: Option<AlignItems>,
    pub column_span: usize,
    /// `content` of a `::before` / `::after`.
    pub content: Option<String>,
    pub position: Position,
    /// top, right, bottom, left
    pub inset: [Len; 4],
    /// Clipped away entirely (`clip: rect(...)`, `clip-path: inset(50%)`): the
    /// "visually hidden" pattern.
    pub clipped: bool,
    /// `grid-template-areas`, row by row.
    pub areas: Vec<Vec<String>>,
    pub grid_area: Option<String>,
    /// `grid-column: start / end` (1-based lines; negative counts from the end; 0 = auto).
    pub grid_column: Option<(i32, i32)>,
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
            max_height: Len::Auto,
            height: Len::Auto,
            hidden: false,
            float: Float::None,
            clear: Clear::None,
            valign: VAlign::Baseline,
            shift: 0.0,
            radius: Len::Px(0.0),
            border_box: false,
            caption_bottom: false,
            border_spacing: (0.0, 0.0),
            border_collapse: false,
            z_index: None,
            aspect: None,
            width_kw: None,
            contain_size: false,
            intrinsic_size: (None, None),
            raise: 0.0,
            object_fit: Fit::Fill,
            object_pos: (Len::Pct(50.0), Len::Pct(50.0)),
            opacity: 1.0,
            masked: false,
            cell_padding: None,
            padding_set: false,
            overflow_clip: false,
            translate: None,
            min_width: Len::Auto,
            min_height: Len::Auto,
            vars: Rc::new(HashMap::new()),
            column: false,
            reverse: false,
            wrap: false,
            justify: Justify::Start,
            align_items: AlignItems::Stretch,
            row_gap: 0.0,
            column_gap: 0.0,
            columns: Vec::new(),
            grow: 0.0,
            shrink: 1.0,
            basis: Len::Auto,
            order: 0,
            align_self: None,
            column_span: 1,
            content: None,
            position: Position::Static,
            inset: [Len::Auto; 4],
            clipped: false,
            areas: Vec::new(),
            grid_area: None,
            grid_column: None,
        }
    }
}

/// The user-agent stylesheet: HTML's defaults, the parts the engine draws.
pub const UA: &str = r#"
html, address, blockquote, body, center, dialog, div, figure, figcaption, footer, form, header, hr, legend, listing,
main, p, plaintext, pre, xmp, search, details, summary, dl, dt, dd, ol, ul, menu, dir, h1, h2, h3, h4, h5, h6, hgroup,
article, aside, nav, section, fieldset, picture, video, canvas { display: block }
li { display: list-item }
table { display: table; border-spacing: 2px }
thead, tbody, tfoot { display: table-row-group }
tr { display: table-row }
td, th { display: table-cell; vertical-align: inherit }
thead, tbody, tfoot { vertical-align: middle }
tr { vertical-align: inherit }
th { font-weight: bold; text-align: center }
caption { display: table-caption; text-align: center }
colgroup, col { display: none }
head, script, style, title, meta, link, base, template, noscript, area, datalist, param, source, track, [hidden],
svg, iframe, object, embed, audio, map, dialog:not([open]), input[type=hidden], datalist, option, optgroup { display: none }
button { padding: 1px 6px; background: #e9e9ed; border-radius: 4px }
input, select, textarea { display: inline-block; border: 1px solid #8f8f9d; border-radius: 4px; padding: 2px 4px; background: #fff; color: #000; font-size: 13.3333px; white-space: pre; vertical-align: middle }
input, select { min-width: 150px }
select { white-space: nowrap }
textarea { min-width: 180px; min-height: 2.6em; white-space: pre-wrap; font-family: monospace }
input[type=submit], input[type=button], input[type=reset], input[type=image] { min-width: 0; background: #e9e9ed; padding: 1px 6px }
input[type=checkbox], input[type=radio] { min-width: 0; width: 13px; height: 13px; padding: 0; text-align: center; line-height: 13px; font-size: 11px; border-radius: 3px }
input[type=radio] { border-radius: 50% }
input[type=checkbox][checked], input[type=radio][checked] { background: #0a66d8; border-color: #0a66d8; color: #fff }
.-volant-placeholder { color: #757575 }
.-volant-caret { display: inline-block; width: 1.5px; height: 1.1em; margin-right: -1.5px; vertical-align: text-bottom; background: #0a66d8 }
input[type=range] { min-width: 120px; height: 4px; padding: 0; background: #ddd; border: 0; margin: 8px 0 }
progress, meter { display: inline-block; width: 160px; height: 8px; background: #ddd; border-radius: 4px }
sup { vertical-align: super }
sub { vertical-align: sub }
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
center table { text-align: left; margin-left: auto; margin-right: auto }
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
    if let Some(args) = v.strip_prefix("hsla(").or_else(|| v.strip_prefix("hsl(")).and_then(|a| a.strip_suffix(')')) {
        let parts: Vec<&str> = args.split([',', ' ', '/']).map(str::trim).filter(|p| !p.is_empty()).collect();
        if parts.len() < 3 {
            return None;
        }
        let h = parts[0];
        let turns = if let Some(n) = h.strip_suffix("deg") {
            n.parse::<f32>().ok()? / 360.0
        } else if let Some(n) = h.strip_suffix("grad") {
            n.parse::<f32>().ok()? / 400.0
        } else if let Some(n) = h.strip_suffix("rad") {
            n.parse::<f32>().ok()? / std::f32::consts::TAU
        } else if let Some(n) = h.strip_suffix("turn") {
            n.parse::<f32>().ok()?
        } else {
            h.parse::<f32>().ok()? / 360.0
        };
        let hue = turns.rem_euclid(1.0);
        let pct = |p: &str| p.trim_end_matches('%').parse::<f32>().ok().map(|n| (n / 100.0).clamp(0.0, 1.0));
        let (sat, light) = (pct(parts[1])?, pct(parts[2])?);
        let a = match parts.get(3) {
            Some(p) => match p.strip_suffix('%') {
                Some(n) => n.parse::<f32>().ok()? / 100.0,
                None => p.parse::<f32>().ok()?,
            },
            None => 1.0,
        };
        let q = if light < 0.5 { light * (1.0 + sat) } else { light + sat - light * sat };
        let p = 2.0 * light - q;
        let ch = |t: f32| {
            let t = t.rem_euclid(1.0);
            let v = if t < 1.0 / 6.0 { p + (q - p) * 6.0 * t } else if t < 0.5 { q } else if t < 2.0 / 3.0 { p + (q - p) * (2.0 / 3.0 - t) * 6.0 } else { p };
            (v * 255.0).round().clamp(0.0, 255.0) as u8
        };
        return Some(Color { r: ch(hue + 1.0 / 3.0), g: ch(hue), b: ch(hue - 1.0 / 3.0), a: (a.clamp(0.0, 1.0) * 255.0).round() as u8 });
    }
    if v == "transparent" {
        return Some(Color::TRANSPARENT);
    }
    NAMED.binary_search_by(|(n, _)| (*n).cmp(v.as_str())).ok().map(|i| {
        let c = NAMED[i].1;
        Color::rgb((c >> 16) as u8, (c >> 8) as u8, c as u8)
    })
}

/// The CSS named colours, sorted for binary search.
const NAMED: [(&str, u32); 148] = [
    ("aliceblue", 0xf0f8ff),
    ("antiquewhite", 0xfaebd7),
    ("aqua", 0x00ffff),
    ("aquamarine", 0x7fffd4),
    ("azure", 0xf0ffff),
    ("beige", 0xf5f5dc),
    ("bisque", 0xffe4c4),
    ("black", 0x000000),
    ("blanchedalmond", 0xffebcd),
    ("blue", 0x0000ff),
    ("blueviolet", 0x8a2be2),
    ("brown", 0xa52a2a),
    ("burlywood", 0xdeb887),
    ("cadetblue", 0x5f9ea0),
    ("chartreuse", 0x7fff00),
    ("chocolate", 0xd2691e),
    ("coral", 0xff7f50),
    ("cornflowerblue", 0x6495ed),
    ("cornsilk", 0xfff8dc),
    ("crimson", 0xdc143c),
    ("cyan", 0x00ffff),
    ("darkblue", 0x00008b),
    ("darkcyan", 0x008b8b),
    ("darkgoldenrod", 0xb8860b),
    ("darkgray", 0xa9a9a9),
    ("darkgreen", 0x006400),
    ("darkgrey", 0xa9a9a9),
    ("darkkhaki", 0xbdb76b),
    ("darkmagenta", 0x8b008b),
    ("darkolivegreen", 0x556b2f),
    ("darkorange", 0xff8c00),
    ("darkorchid", 0x9932cc),
    ("darkred", 0x8b0000),
    ("darksalmon", 0xe9967a),
    ("darkseagreen", 0x8fbc8f),
    ("darkslateblue", 0x483d8b),
    ("darkslategray", 0x2f4f4f),
    ("darkslategrey", 0x2f4f4f),
    ("darkturquoise", 0x00ced1),
    ("darkviolet", 0x9400d3),
    ("deeppink", 0xff1493),
    ("deepskyblue", 0x00bfff),
    ("dimgray", 0x696969),
    ("dimgrey", 0x696969),
    ("dodgerblue", 0x1e90ff),
    ("firebrick", 0xb22222),
    ("floralwhite", 0xfffaf0),
    ("forestgreen", 0x228b22),
    ("fuchsia", 0xff00ff),
    ("gainsboro", 0xdcdcdc),
    ("ghostwhite", 0xf8f8ff),
    ("gold", 0xffd700),
    ("goldenrod", 0xdaa520),
    ("gray", 0x808080),
    ("green", 0x008000),
    ("greenyellow", 0xadff2f),
    ("grey", 0x808080),
    ("honeydew", 0xf0fff0),
    ("hotpink", 0xff69b4),
    ("indianred", 0xcd5c5c),
    ("indigo", 0x4b0082),
    ("ivory", 0xfffff0),
    ("khaki", 0xf0e68c),
    ("lavender", 0xe6e6fa),
    ("lavenderblush", 0xfff0f5),
    ("lawngreen", 0x7cfc00),
    ("lemonchiffon", 0xfffacd),
    ("lightblue", 0xadd8e6),
    ("lightcoral", 0xf08080),
    ("lightcyan", 0xe0ffff),
    ("lightgoldenrodyellow", 0xfafad2),
    ("lightgray", 0xd3d3d3),
    ("lightgreen", 0x90ee90),
    ("lightgrey", 0xd3d3d3),
    ("lightpink", 0xffb6c1),
    ("lightsalmon", 0xffa07a),
    ("lightseagreen", 0x20b2aa),
    ("lightskyblue", 0x87cefa),
    ("lightslategray", 0x778899),
    ("lightslategrey", 0x778899),
    ("lightsteelblue", 0xb0c4de),
    ("lightyellow", 0xffffe0),
    ("lime", 0x00ff00),
    ("limegreen", 0x32cd32),
    ("linen", 0xfaf0e6),
    ("magenta", 0xff00ff),
    ("maroon", 0x800000),
    ("mediumaquamarine", 0x66cdaa),
    ("mediumblue", 0x0000cd),
    ("mediumorchid", 0xba55d3),
    ("mediumpurple", 0x9370db),
    ("mediumseagreen", 0x3cb371),
    ("mediumslateblue", 0x7b68ee),
    ("mediumspringgreen", 0x00fa9a),
    ("mediumturquoise", 0x48d1cc),
    ("mediumvioletred", 0xc71585),
    ("midnightblue", 0x191970),
    ("mintcream", 0xf5fffa),
    ("mistyrose", 0xffe4e1),
    ("moccasin", 0xffe4b5),
    ("navajowhite", 0xffdead),
    ("navy", 0x000080),
    ("oldlace", 0xfdf5e6),
    ("olive", 0x808000),
    ("olivedrab", 0x6b8e23),
    ("orange", 0xffa500),
    ("orangered", 0xff4500),
    ("orchid", 0xda70d6),
    ("palegoldenrod", 0xeee8aa),
    ("palegreen", 0x98fb98),
    ("paleturquoise", 0xafeeee),
    ("palevioletred", 0xdb7093),
    ("papayawhip", 0xffefd5),
    ("peachpuff", 0xffdab9),
    ("peru", 0xcd853f),
    ("pink", 0xffc0cb),
    ("plum", 0xdda0dd),
    ("powderblue", 0xb0e0e6),
    ("purple", 0x800080),
    ("rebeccapurple", 0x663399),
    ("red", 0xff0000),
    ("rosybrown", 0xbc8f8f),
    ("royalblue", 0x4169e1),
    ("saddlebrown", 0x8b4513),
    ("salmon", 0xfa8072),
    ("sandybrown", 0xf4a460),
    ("seagreen", 0x2e8b57),
    ("seashell", 0xfff5ee),
    ("sienna", 0xa0522d),
    ("silver", 0xc0c0c0),
    ("skyblue", 0x87ceeb),
    ("slateblue", 0x6a5acd),
    ("slategray", 0x708090),
    ("slategrey", 0x708090),
    ("snow", 0xfffafa),
    ("springgreen", 0x00ff7f),
    ("steelblue", 0x4682b4),
    ("tan", 0xd2b48c),
    ("teal", 0x008080),
    ("thistle", 0xd8bfd8),
    ("tomato", 0xff6347),
    ("turquoise", 0x40e0d0),
    ("violet", 0xee82ee),
    ("wheat", 0xf5deb3),
    ("white", 0xffffff),
    ("whitesmoke", 0xf5f5f5),
    ("yellow", 0xffff00),
    ("yellowgreen", 0x9acd32),
];

/// A length in px (or %/auto), with `em` relative to `em` and `rem` to `rem`.
pub fn parse_len(v: &str, em: f32, rem: f32) -> Option<Len> {
    let v = v.trim().to_ascii_lowercase();
    for f in ["calc(", "-webkit-calc("] {
        if let Some(inner) = v.strip_prefix(f).and_then(|r| r.strip_suffix(')')) {
            let (px, pct) = calc(inner, em, rem)?;
            return Some(if pct == 0.0 { Len::Px(px) } else if px == 0.0 { Len::Pct(pct) } else { Len::Mix(px, pct) });
        }
    }
    // min()/max()/clamp() of mixed units: the preferred value is a fair stand-in.
    if let Some(inner) = v.strip_prefix("clamp(").and_then(|r| r.strip_suffix(')')) {
        let parts = split_args(inner);
        return parts.get(1).and_then(|p| parse_len(p, em, rem));
    }
    for f in ["min(", "max("] {
        if let Some(inner) = v.strip_prefix(f).and_then(|r| r.strip_suffix(')')) {
            let vals: Vec<Len> = split_args(inner).iter().filter_map(|p| parse_len(p, em, rem)).collect();
            let all_px: Option<Vec<f32>> = vals.iter().map(|l| if let Len::Px(p) = l { Some(*p) } else { None }).collect();
            return match all_px {
                Some(px) if !px.is_empty() => Some(Len::Px(if f == "min(" { px.iter().cloned().fold(f32::MAX, f32::min) } else { px.iter().cloned().fold(f32::MIN, f32::max) })),
                _ => vals.into_iter().next(),
            };
        }
    }
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

/// Splits function arguments on top-level commas.
fn split_args(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0;
    let mut cur = String::new();
    for c in s.chars() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth == 0 => {
                out.push(std::mem::take(&mut cur).trim().to_string());
                continue;
            }
            _ => {}
        }
        cur.push(c);
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    out
}

/// `calc()`: sums and differences of lengths, scaled by plain numbers; as (px, %).
fn calc(expr: &str, em: f32, rem: f32) -> Option<(f32, f32)> {
    /// (px, %, is a plain number)
    type V = (f32, f32, bool);
    struct P<'a> {
        s: &'a [u8],
        i: usize,
        em: f32,
        rem: f32,
    }
    impl P<'_> {
        fn ws(&mut self) {
            while self.i < self.s.len() && self.s[self.i].is_ascii_whitespace() {
                self.i += 1;
            }
        }
        fn sum(&mut self) -> Option<V> {
            let mut v = self.product()?;
            loop {
                self.ws();
                let op = match self.s.get(self.i) {
                    Some(b'+') => 1.0,
                    Some(b'-') => -1.0,
                    _ => return Some(v),
                };
                self.i += 1;
                let r = self.product()?;
                v = (v.0 + op * r.0, v.1 + op * r.1, v.2 && r.2);
            }
        }
        fn product(&mut self) -> Option<V> {
            let mut v = self.unit()?;
            loop {
                self.ws();
                let op = match self.s.get(self.i) {
                    Some(b'*') => b'*',
                    Some(b'/') => b'/',
                    _ => return Some(v),
                };
                self.i += 1;
                let r = self.unit()?;
                v = match (op, v.2, r.2) {
                    (b'*', true, _) => (r.0 * v.0, r.1 * v.0, r.2),
                    (b'*', false, true) => (v.0 * r.0, v.1 * r.0, false),
                    (b'/', _, true) if r.0 != 0.0 => (v.0 / r.0, v.1 / r.0, v.2),
                    _ => return None,
                };
            }
        }
        fn unit(&mut self) -> Option<V> {
            self.ws();
            if self.s.get(self.i) == Some(&b'(') {
                self.i += 1;
                let v = self.sum()?;
                self.ws();
                if self.s.get(self.i) != Some(&b')') {
                    return None;
                }
                self.i += 1;
                return Some(v);
            }
            let start = self.i;
            if matches!(self.s.get(self.i), Some(b'+' | b'-')) {
                self.i += 1;
            }
            while self.i < self.s.len() && (self.s[self.i].is_ascii_digit() || self.s[self.i] == b'.') {
                self.i += 1;
            }
            let num: f32 = std::str::from_utf8(&self.s[start..self.i]).ok()?.parse().ok()?;
            let ustart = self.i;
            while self.i < self.s.len() && (self.s[self.i].is_ascii_alphabetic() || self.s[self.i] == b'%') {
                self.i += 1;
            }
            let unit = std::str::from_utf8(&self.s[ustart..self.i]).ok()?;
            if unit.is_empty() {
                return Some((num, 0.0, true));
            }
            match parse_len(&format!("{num}{unit}"), self.em, self.rem)? {
                Len::Px(p) => Some((p, 0.0, false)),
                Len::Pct(p) => Some((0.0, p, false)),
                _ => None,
            }
        }
    }
    let mut p = P { s: expr.as_bytes(), i: 0, em, rem };
    let v = p.sum()?;
    p.ws();
    (p.i == p.s.len()).then_some((v.0, v.1))
}

/// Replaces `var(--name, fallback)` with the custom property's value; `None`
/// if one is missing and has no fallback (the declaration is then ignored).
pub fn substitute(value: &str, vars: &HashMap<String, String>, depth: u32) -> Option<String> {
    if depth > 16 {
        return None;
    }
    let Some(at) = value.find("var(") else { return Some(value.to_string()) };
    let rest = &value[at + 4..];
    let mut level = 1;
    let mut end = None;
    for (i, c) in rest.char_indices() {
        match c {
            '(' => level += 1,
            ')' => {
                level -= 1;
                if level == 0 {
                    end = Some(i);
                    break;
                }
            }
            _ => {}
        }
    }
    let end = end?;
    let inner = &rest[..end];
    let (name, fallback) = match inner.split_once(',') {
        Some((n, f)) => (n.trim(), Some(f.trim())),
        None => (inner.trim(), None),
    };
    let found = match vars.get(name) {
        Some(v) => substitute(v, vars, depth + 1)?,
        None => substitute(fallback?, vars, depth + 1)?,
    };
    let tail = substitute(&rest[end + 1..], vars, depth + 1)?;
    Some(format!("{}{}{}", &value[..at], found, tail))
}

/// `content: "→ " attr(title) open-quote` → the text (icon-font glyphs dropped).
fn content_text(v: &str, attrs: &[(Box<str>, String)]) -> Option<String> {
    let v = v.trim();
    if v == "none" || v == "normal" || v.is_empty() {
        return None;
    }
    let mut out = String::new();
    let chars: Vec<char> = v.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '"' || c == '\'' {
            let q = c;
            i += 1;
            while i < chars.len() && chars[i] != q {
                if chars[i] == '\\' && i + 1 < chars.len() {
                    // \201C style escapes.
                    let hex: String = chars[i + 1..].iter().take_while(|c| c.is_ascii_hexdigit()).take(6).collect();
                    if !hex.is_empty() {
                        if let Some(ch) = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                            out.push(ch);
                        }
                        i += 1 + hex.len();
                        if chars.get(i) == Some(&' ') {
                            i += 1;
                        }
                        continue;
                    }
                    out.push(chars[i + 1]);
                    i += 2;
                    continue;
                }
                out.push(chars[i]);
                i += 1;
            }
            i += 1;
        } else if v[v.char_indices().nth(i).map(|x| x.0).unwrap_or(0)..].starts_with("attr(") {
            let rest: String = chars[i + 5..].iter().collect();
            let end = rest.find(')')?;
            let name = rest[..end].trim();
            out.push_str(attr(attrs, name).unwrap_or(""));
            i += 5 + end + 1;
        } else if v[v.char_indices().nth(i).map(|x| x.0).unwrap_or(0)..].starts_with("open-quote") {
            out.push('“');
            i += 10;
        } else if v[v.char_indices().nth(i).map(|x| x.0).unwrap_or(0)..].starts_with("close-quote") {
            out.push('”');
            i += 11;
        } else {
            i += 1;
        }
    }
    // Icon fonts put their glyphs in the Private Use Area; without the font they're boxes.
    let out: String = out.chars().filter(|c| !('\u{e000}'..='\u{f8ff}').contains(c)).collect();
    (!out.is_empty()).then_some(out)
}

fn family_of(v: &str) -> Option<Family> {
    let names = css_split_commas(v);
    for (k, f) in names.iter().enumerate() {
        let f = f.trim().trim_matches(|c| c == '"' || c == '\'').to_ascii_lowercase();
        // A web font the page loaded (or Ahem, the box font CSS tests take as installed):
        // it, with the rest of the list as its fallback.
        if crate::fonts::is_declared(&f) || f == "ahem" {
            let fallback = family_of(&names[k + 1..].join(",")).map(Family::generic).unwrap_or(Family::Sans);
            return Some(crate::fonts::face(&f, fallback));
        }
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
        Len::Mix(px, p) => Some(px + size * p / 100.0),
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
fn negative(l: Len) -> bool {
    matches!(l, Len::Px(v) | Len::Pct(v) if v < 0.0)
}

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

/// The quoted rows of `grid-template-areas`.
fn area_rows(v: &str) -> Vec<Vec<String>> {
    let mut out = Vec::new();
    let mut rest = v;
    while let Some(start) = rest.find(['"', '\'']) {
        let q = rest[start..].chars().next().unwrap_or('"');
        let after = &rest[start + 1..];
        let Some(end) = after.find(q) else { break };
        out.push(after[..end].split_whitespace().map(String::from).collect());
        rest = &after[end + 1..];
    }
    out
}

/// `grid-template-columns` → tracks (line names dropped, `repeat()` expanded).
fn tracks(v: &str, em: f32, rem: f32) -> Vec<Track> {
    let one = |t: &str| -> Option<Track> {
        let t = t.trim().to_ascii_lowercase();
        if let Some(fr) = t.strip_suffix("fr") {
            return fr.parse().ok().map(Track::Fr);
        }
        if let Some(inner) = t.strip_prefix("minmax(").and_then(|r| r.strip_suffix(')')) {
            let args = split_args(inner);
            let max = args.get(1)?.to_string();
            if max.ends_with("fr") {
                return max.trim_end_matches("fr").parse().ok().map(Track::Fr);
            }
            return parse_len(&max, em, rem).map(Track::Len).or_else(|| parse_len(args.first()?, em, rem).map(Track::Len));
        }
        if t.starts_with("fit-content") || t == "auto" || t == "min-content" || t == "max-content" {
            return Some(Track::Fr(1.0));
        }
        parse_len(&t, em, rem).map(Track::Len)
    };
    let mut out = Vec::new();
    for part in split_spaces(v) {
        if part.starts_with('[') {
            continue;
        }
        if let Some(inner) = part.strip_prefix("repeat(").and_then(|r| r.strip_suffix(')')) {
            let args = split_args(inner);
            let (Some(count), Some(what)) = (args.first(), args.get(1)) else { continue };
            if count == "auto-fill" || count == "auto-fit" {
                let min = what
                    .strip_prefix("minmax(")
                    .and_then(|r| r.strip_suffix(')'))
                    .and_then(|r| split_args(r).first().cloned())
                    .unwrap_or_else(|| what.clone());
                if let Some(Len::Px(px)) = parse_len(&min, em, rem) {
                    out.push(Track::AutoFill(px));
                }
                continue;
            }
            let n: usize = count.parse().unwrap_or(1).clamp(1, 64);
            let inner_tracks: Vec<Track> = split_spaces(what).iter().filter(|t| !t.starts_with('[')).filter_map(|t| one(t)).collect();
            for _ in 0..n {
                out.extend(inner_tracks.iter().copied());
            }
        } else if let Some(t) = one(&part) {
            out.push(t);
        }
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

/// Logical properties as physical ones, for horizontal, left-to-right text:
/// (name, [(physical name, which of the value's two parts)]).
fn logical(name: &str) -> Option<Vec<(String, usize)>> {
    let one = |p: &str| Some(vec![(p.to_string(), 0)]);
    match name {
        "inline-size" => return one("width"),
        "block-size" => return one("height"),
        "min-inline-size" => return one("min-width"),
        "max-inline-size" => return one("max-width"),
        "min-block-size" => return one("min-height"),
        "max-block-size" => return one("max-height"),
        _ => {}
    }
    let (head, rest) = ["margin", "padding", "inset", "border"].iter().find_map(|h| name.strip_prefix(h).map(|r| (*h, r)))?;
    let rest = rest.strip_prefix('-')?;
    let (axis, tail) = rest.split_once('-').unwrap_or((rest, ""));
    let (start, end) = match axis {
        "inline" => ("left", "right"),
        "block" => ("top", "bottom"),
        _ => return None,
    };
    let (sides, tail) = match tail.split_once('-') {
        Some(("start", t)) => (vec![(start, 0)], t),
        Some(("end", t)) => (vec![(end, 0)], t),
        _ if tail == "start" => (vec![(start, 0)], ""),
        _ if tail == "end" => (vec![(end, 0)], ""),
        // Both sides: `margin-inline: a b` is start then end; a border's value goes to both.
        _ if head == "border" => (vec![(start, 0), (end, 0)], tail),
        _ if tail.is_empty() => (vec![(start, 0), (end, 1)], ""),
        _ => return None,
    };
    let phys = |side: &str| match head {
        "inset" => side.to_string(),
        _ if tail.is_empty() => format!("{head}-{side}"),
        _ => format!("{head}-{side}-{tail}"),
    };
    Some(sides.into_iter().map(|(side, k)| (phys(side), k)).collect())
}

/// Applies one declaration to `s` (whose inherited values come from `parent`).
fn apply(s: &mut Style, d: &Decl, parent: &Style, rem: f32) {
    if let Some(targets) = logical(&d.name) {
        let parts = split_spaces(d.value.trim());
        let two = targets.iter().any(|t| t.1 == 1);
        for (name, k) in targets {
            let value = if two { parts.get(k).or(parts.first()).cloned().unwrap_or_default() } else { d.value.clone() };
            apply(s, &Decl { name, value, important: d.important }, parent, rem);
        }
        return;
    }
    let v = d.value.trim();
    let lower = v.to_ascii_lowercase();
    if lower == "inherit" {
        inherit_one(s, &d.name, parent);
        return;
    }
    let em = s.font_size;
    let len = |x: &str| parse_len(x, em, rem);
    if d.name.starts_with("padding") {
        s.padding_set = true;
    }
    match d.name.as_str() {
        "display" => {
            s.display = match lower.as_str() {
                "none" | "table-column" | "table-column-group" => Display::None,
                "inline" | "contents" => Display::Inline,
                "inline-block" => Display::InlineBlock,
                "list-item" => Display::ListItem,
                "flex" | "inline-flex" | "-webkit-box" | "-webkit-flex" => Display::Flex,
                "grid" | "inline-grid" => Display::Grid,
                "table" | "inline-table" => Display::Table,
                "table-row-group" | "table-header-group" | "table-footer-group" => Display::TableRowGroup,
                "table-row" => Display::TableRow,
                "table-cell" => Display::TableCell,
                "table-caption" => Display::TableCaption,
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
        "box-sizing" => s.border_box = lower == "border-box",
        "caption-side" => s.caption_bottom = lower == "bottom",
        "border-collapse" => s.border_collapse = lower == "collapse",
        "z-index" => s.z_index = lower.trim().parse::<i32>().ok(),
        "object-fit" => {
            s.object_fit = match lower.trim() {
                "contain" => Fit::Contain,
                "cover" => Fit::Cover,
                "none" => Fit::None,
                "scale-down" => Fit::ScaleDown,
                _ => Fit::Fill,
            }
        }
        "object-position" => {
            let (mut x, mut y) = (None, None);
            let mut rest = Vec::new();
            for p in split_spaces(&lower) {
                match p.as_str() {
                    "left" => x = Some(Len::Pct(0.0)),
                    "right" => x = Some(Len::Pct(100.0)),
                    "top" => y = Some(Len::Pct(0.0)),
                    "bottom" => y = Some(Len::Pct(100.0)),
                    "center" => rest.push(Len::Pct(50.0)),
                    p => rest.extend(len(p)),
                }
            }
            let mut rest = rest.into_iter();
            let x = x.or_else(|| rest.next()).unwrap_or(Len::Pct(50.0));
            let y = y.or_else(|| rest.next()).unwrap_or(Len::Pct(50.0));
            s.object_pos = (x, y);
        }
        "aspect-ratio" => {
            let auto = lower.split_whitespace().any(|w| w == "auto");
            let rest = lower.replace("auto", "");
            let mut nums = rest.split('/').map(|p| p.trim().parse::<f32>().ok());
            s.aspect = match (nums.next().flatten(), nums.next(), nums.next()) {
                (Some(w), None, None) if w > 0.0 => Some((w, auto)),
                (Some(w), Some(Some(h)), None) if w > 0.0 && h > 0.0 => Some((w / h, auto)),
                _ => None,
            };
        }
        "border-spacing" => {
            let parts: Vec<f32> = split_spaces(v).iter().filter_map(|p| len(p)).map(|l| l.resolve(0.0).max(0.0)).collect();
            match parts[..] {
                [a] => s.border_spacing = (a, a),
                [a, b] => s.border_spacing = (a, b),
                _ => {}
            }
        }
        "mask" | "mask-image" | "-webkit-mask" | "-webkit-mask-image" => s.masked = lower.contains("url(") || lower.contains("gradient("),
        "opacity" => {
            let t = lower.trim();
            let v = match t.strip_suffix('%') {
                Some(p) => p.trim().parse::<f32>().ok().map(|p| p / 100.0),
                None => t.parse::<f32>().ok(),
            };
            if let Some(v) = v {
                s.opacity = v.clamp(0.0, 1.0);
            }
        }
        "overflow" | "overflow-x" | "overflow-y" => {
            let clip = lower.split_whitespace().any(|w| matches!(w, "hidden" | "clip" | "auto" | "scroll"));
            if clip || d.name == "overflow" {
                s.overflow_clip = clip;
            }
        }
        "transform" => {
            s.translate = None;
            let mut dx = Len::Px(0.0);
            let mut dy = Len::Px(0.0);
            let mut any = false;
            let mut rest = lower.as_str();
            while let Some(open) = rest.find('(') {
                let fname = rest[..open].trim().trim_start_matches(|c: char| c == ',' || c.is_whitespace());
                let Some(close) = rest[open..].find(')') else { break };
                let args: Vec<&str> = rest[open + 1..open + close].split(|c: char| c == ',' || c.is_whitespace()).filter(|a| !a.is_empty()).collect();
                let l = |k: usize| args.get(k).and_then(|a| len(a));
                match fname {
                    "translate" | "translate3d" => {
                        dx = l(0).unwrap_or(Len::Px(0.0));
                        dy = l(1).unwrap_or(Len::Px(0.0));
                        any = true;
                    }
                    "translatex" => {
                        dx = l(0).unwrap_or(Len::Px(0.0));
                        any = true;
                    }
                    "translatey" => {
                        dy = l(0).unwrap_or(Len::Px(0.0));
                        any = true;
                    }
                    "matrix" if args.len() == 6 => {
                        dx = Len::Px(args[4].parse().unwrap_or(0.0));
                        dy = Len::Px(args[5].parse().unwrap_or(0.0));
                        any = true;
                    }
                    _ => {}
                }
                rest = &rest[open + close + 1..];
            }
            if any {
                s.translate = Some((dx, dy));
            }
        }
        "min-width" => {
            if let Some(l) = len(v) {
                s.min_width = l
            }
        }
        "min-height" => {
            if let Some(l) = len(v) {
                s.min_height = l
            }
        }
        "flex-direction" => {
            s.column = lower.starts_with("column");
            s.reverse = lower.ends_with("reverse");
        }
        "flex-wrap" => s.wrap = lower.starts_with("wrap"),
        "flex-flow" => {
            for p in lower.split_whitespace() {
                match p {
                    "row" | "row-reverse" | "column" | "column-reverse" => {
                        s.column = p.starts_with("column");
                        s.reverse = p.ends_with("reverse");
                    }
                    "wrap" | "wrap-reverse" => s.wrap = true,
                    "nowrap" => s.wrap = false,
                    _ => {}
                }
            }
        }
        "justify-content" => {
            s.justify = match lower.trim_start_matches("safe ").trim_start_matches("unsafe ") {
                "center" => Justify::Center,
                "flex-end" | "end" | "right" => Justify::End,
                "space-between" => Justify::Between,
                "space-around" => Justify::Around,
                "space-evenly" => Justify::Evenly,
                _ => Justify::Start,
            }
        }
        "align-items" | "align-self" => {
            let a = match lower.as_str() {
                "center" => Some(AlignItems::Center),
                "flex-start" | "start" | "self-start" | "baseline" | "first baseline" => Some(AlignItems::Start),
                "flex-end" | "end" | "self-end" => Some(AlignItems::End),
                "stretch" | "normal" => Some(AlignItems::Stretch),
                _ => None,
            };
            if let Some(a) = a {
                if d.name == "align-items" {
                    s.align_items = a;
                } else {
                    s.align_self = Some(a);
                }
            }
        }
        "gap" | "grid-gap" => {
            let parts = split_spaces(v);
            let r = parts.first().and_then(|p| len(p)).map(|l| l.resolve(0.0));
            let c = parts.get(1).and_then(|p| len(p)).map(|l| l.resolve(0.0)).or(r);
            if let (Some(r), Some(c)) = (r, c) {
                s.row_gap = r;
                s.column_gap = c;
            }
        }
        "row-gap" | "grid-row-gap" => {
            if let Some(l) = len(v) {
                s.row_gap = l.resolve(0.0)
            }
        }
        "column-gap" | "grid-column-gap" => {
            if let Some(l) = len(v) {
                s.column_gap = l.resolve(0.0)
            }
        }
        "flex-grow" => s.grow = v.parse().unwrap_or(s.grow),
        "flex-shrink" => s.shrink = v.parse().unwrap_or(s.shrink),
        "flex-basis" => {
            if let Some(l) = len(v).or((lower == "content").then_some(Len::Auto)) {
                s.basis = l
            }
        }
        "flex" => {
            // none | auto | <grow> [<shrink>] [<basis>]
            match lower.as_str() {
                "none" => (s.grow, s.shrink, s.basis) = (0.0, 0.0, Len::Auto),
                "auto" => (s.grow, s.shrink, s.basis) = (1.0, 1.0, Len::Auto),
                "initial" => (s.grow, s.shrink, s.basis) = (0.0, 1.0, Len::Auto),
                _ => {
                    let parts = split_spaces(v);
                    let nums: Vec<f32> = parts.iter().map_while(|p| p.parse::<f32>().ok()).collect();
                    s.grow = nums.first().copied().unwrap_or(1.0);
                    s.shrink = nums.get(1).copied().unwrap_or(1.0);
                    s.basis = parts.get(nums.len()).and_then(|p| len(p)).unwrap_or(if nums.is_empty() { Len::Auto } else { Len::Px(0.0) });
                    if nums.is_empty() && parts.len() == 1 {
                        s.grow = 1.0;
                    }
                }
            }
        }
        "order" => s.order = v.parse().unwrap_or(0),
        "grid-template-columns" => s.columns = tracks(v, em, rem),
        "grid-template-areas" => s.areas = area_rows(v),
        "grid-template" => {
            // rows / columns, with area strings among the rows.
            if let Some((rows, cols)) = v.rsplit_once('/') {
                s.columns = tracks(cols, em, rem);
                s.areas = area_rows(rows);
            } else {
                s.areas = area_rows(v);
            }
        }
        "grid-area" => {
            let name = v.split('/').next().unwrap_or("").trim();
            if !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_') && !name.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(true) {
                s.grid_area = Some(name.to_string());
            }
        }
        "grid-column" | "grid-column-end" | "grid-column-start" => {
            if let Some(n) = lower.split("span").nth(1).and_then(|n| n.trim().split(|c: char| !c.is_ascii_digit()).next()?.parse::<usize>().ok()) {
                s.column_span = n.clamp(1, 64);
            } else if d.name == "grid-column" {
                let mut parts = lower.split('/').map(|p| p.trim().parse::<i32>().ok());
                if let Some(Some(a)) = parts.next() {
                    let b = parts.next().flatten().unwrap_or(0);
                    s.grid_column = Some((a, b));
                }
            }
        }
        "position" => {
            s.position = match lower.as_str() {
                "relative" => Position::Relative,
                "sticky" | "-webkit-sticky" => Position::Sticky,
                "absolute" => Position::Absolute,
                "fixed" => Position::Fixed,
                _ => Position::Static,
            }
        }
        "top" | "right" | "bottom" | "left" => {
            let i = ["top", "right", "bottom", "left"].iter().position(|x| *x == d.name).unwrap_or(0);
            if let Some(l) = len(v) {
                s.inset[i] = l;
            }
        }
        "inset" => {
            let vals: Option<Vec<Len>> = split_spaces(v).iter().map(|p| len(p)).collect();
            if let Some(f) = vals.as_deref().and_then(four) {
                s.inset = f;
            }
        }
        "clip" => s.clipped = lower.starts_with("rect"),
        "clip-path" => s.clipped = lower.starts_with("inset(50%") || lower.starts_with("inset(100%") || lower.starts_with("circle(0"),
        "content" => s.content = Some(v.to_string()),
        "float" => {
            s.float = match lower.as_str() {
                "left" | "inline-start" => Float::Left,
                "right" | "inline-end" => Float::Right,
                _ => Float::None,
            }
        }
        "clear" => {
            s.clear = match lower.as_str() {
                "left" | "inline-start" => Clear::Left,
                "right" | "inline-end" => Clear::Right,
                "both" => Clear::Both,
                _ => Clear::None,
            }
        }
        "vertical-align" => {
            s.raise = 0.0;
            s.valign = match lower.as_str() {
                "super" => VAlign::Super,
                "sub" => VAlign::Sub,
                "top" => VAlign::Top,
                "bottom" => VAlign::Bottom,
                "text-top" => VAlign::TextTop,
                "text-bottom" => VAlign::TextBottom,
                "middle" => VAlign::Middle,
                other => {
                    // A length, or a percentage of the line height.
                    if let Some(l) = len(other) {
                        s.raise = l.resolve(s.line_height);
                    }
                    VAlign::Baseline
                }
            }
        }
        "border-radius" => {
            if let Some(l) = split_spaces(v).first().and_then(|p| len(p)) {
                s.radius = l;
            }
        }
        "height" => {
            if let Some(l) = len(v) {
                s.height = l
            }
        }
        "width" => {
            let kw = lower.trim_start_matches("-moz-").trim_start_matches("-webkit-");
            let kw = if kw == "min-content" {
                Some(Sizing::Min)
            } else if kw == "max-content" {
                Some(Sizing::Max)
            } else if kw == "fit-content" {
                Some(Sizing::Fit)
            } else {
                None
            };
            if kw.is_some() {
                s.width = Len::Auto;
                s.width_kw = kw;
            } else if let Some(l) = len(v) {
                s.width = l;
                s.width_kw = None;
            }
        }
        "contain" => {
            let words: Vec<&str> = lower.split_whitespace().collect();
            s.contain_size = words.iter().any(|w| matches!(*w, "size" | "strict"));
        }
        "contain-intrinsic-size" => {
            let px = |p: &str| match len(p) {
                Some(Len::Px(v)) if v >= 0.0 => Some(Some(v)),
                _ if p == "none" => Some(None),
                _ => None,
            };
            let parts: Vec<&str> = lower.split_whitespace().filter(|p| *p != "auto").collect();
            let vals: Option<Vec<Option<f32>>> = parts.iter().map(|p| px(p)).collect();
            match vals.as_deref() {
                Some([a]) => s.intrinsic_size = (*a, *a),
                Some([a, b]) => s.intrinsic_size = (*a, *b),
                _ => {}
            }
        }
        // (Negative sizes are invalid: the declaration is ignored.)
        "max-width" => {
            s.max_width = if lower == "none" { Len::Auto } else { len(v).filter(|l| !negative(*l)).unwrap_or(s.max_width) };
        }
        "max-height" => {
            s.max_height = if lower == "none" { Len::Auto } else { len(v).filter(|l| !negative(*l)).unwrap_or(s.max_height) };
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
        "vertical-align" => {
            s.valign = p.valign;
            s.raise = p.raise;
        }
        _ => {}
    }
}

/// What an element starts with before its own rules: inherited properties
/// from the parent, everything else at its initial value.
pub(crate) fn inherited(p: &Style) -> Style {
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
        vars: p.vars.clone(),
        caption_bottom: p.caption_bottom,
        border_spacing: p.border_spacing,
        border_collapse: p.border_collapse,
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

/// Whether an element is an HTML one, for type selectors' case: the HTML
/// parser's elements are (it lower-cases their names); `createElementNS` and
/// XML documents record a namespace.
fn is_html(dom: &Dom, id: NodeId) -> bool {
    dom.ns.get(&id).is_none_or(|(ns, _)| ns.as_deref() == Some("http://www.w3.org/1999/xhtml"))
}

fn matches_compound(dom: &Dom, id: NodeId, c: &Compound) -> bool {
    let Some((tag, attrs)) = element(dom, id) else { return false };
    if let Some(t) = &c.tag {
        if t != tag && !(t.eq_ignore_ascii_case(tag) && is_html(dom, id)) {
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
            Pseudo::Nth(a, b, from_end, of_type) => {
                let sibs = element_siblings(dom, id);
                let same: Vec<NodeId> = if *of_type { sibs.into_iter().filter(|&x| dom.tag(x) == Some(tag)).collect() } else { sibs };
                let at = same.iter().position(|&x| x == id).unwrap_or(0) as i32;
                let pos = if *from_end { same.len() as i32 - at } else { at + 1 };
                css::nth_matches(*a, *b, pos)
            }
            Pseudo::Not(list) => !list.iter().any(|c| matches_compound(dom, id, c)),
            Pseudo::Never => false,
            Pseudo::Hover => attr(attrs, css::STATE_HOVER).is_some(),
            Pseudo::Focus => attr(attrs, css::STATE_FOCUS).is_some(),
            Pseudo::FocusWithin => attr(attrs, css::STATE_FOCUS_WITHIN).is_some(),
            Pseudo::Target => attr(attrs, css::STATE_TARGET).is_some(),
            Pseudo::Checked => attr(attrs, "checked").is_some() || attr(attrs, "selected").is_some(),
            Pseudo::Disabled => attr(attrs, "disabled").is_some(),
            Pseudo::Enabled => attr(attrs, "disabled").is_none(),
            Pseudo::Root => tag == "html",
            Pseudo::Link => (tag == "a" || tag == "area") && attr(attrs, "href").is_some(),
            Pseudo::FirstChild => element_siblings(dom, id).first() == Some(&id),
            Pseudo::LastChild => element_siblings(dom, id).last() == Some(&id),
            Pseudo::OnlyChild => element_siblings(dom, id) == [id],
            Pseudo::Empty => dom.children(id).iter().all(|&k| match &dom.nodes[k as usize].data {
                Data::Text(t) => t.is_empty(),
                Data::Comment(_) | Data::Pi { .. } => true,
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

/// Every node's style (text nodes get their parent's), plus the styles of
/// `::before` / `::after` boxes, stored after the nodes' and found through
/// `before` / `after`.
pub struct Styled {
    pub styles: Vec<Style>,
    pub before: HashMap<NodeId, NodeId>,
    pub after: HashMap<NodeId, NodeId>,
    /// Block containers' `::first-letter` styles.
    pub first_letter: HashMap<NodeId, NodeId>,
}

/// Where to look a rule's selector up: by the subject's id, a class, its tag, or anywhere.
fn bucket(sel: &css::Selector) -> (u8, String) {
    let c = &sel.parts[0].0;
    if let Some(id) = &c.id {
        (0, id.clone())
    } else if let Some(cl) = c.classes.first() {
        (1, cl.clone())
    } else if let Some(t) = &c.tag {
        (2, t.to_ascii_lowercase())
    } else {
        (3, String::new())
    }
}

/// (origin, order, rule, selector)
type Entry = (u8, usize, usize, usize);

/// The selectors in these sheets (and the UA's) that depend on `:hover`,
/// so a page knows when the pointer moving needs a new layout.
pub fn hover_selectors(author_css: &[String], env: &Env) -> Vec<css::Selector> {
    fn has(c: &Compound) -> bool {
        c.pseudos.iter().any(|p| match p {
            Pseudo::Hover => true,
            Pseudo::Not(list) => list.iter().any(has),
            _ => false,
        })
    }
    let mut out = Vec::new();
    for text in std::iter::once(UA).chain(author_css.iter().map(String::as_str)) {
        if !text.contains(":hover") {
            continue;
        }
        for r in css::parse_stylesheet(text) {
            if r.media.iter().all(|m| css::media_applies(m, env)) {
                out.extend(r.selectors.into_iter().filter(|s| s.parts.iter().any(|(c, _)| has(c))));
            }
        }
    }
    out
}

pub fn compute(dom: &Dom, author_css: &[String], env: &Env) -> Styled {
    Cascade::new(author_css, env).compute(dom)
}

/// A document's style rules, parsed and indexed once so they can style the
/// whole tree or (for `getComputedStyle`) just one element and its ancestors.
pub struct Cascade {
    rules: Vec<Rule>,
    origins: Vec<u8>,
    index: HashMap<(u8, String), Vec<Entry>>,
}

impl Cascade {
pub fn new(author_css: &[String], env: &Env) -> Cascade {
    // Origin 0: user agent; 1: author sheets (in order), then inline styles.
    // Rules whose @media doesn't apply are dropped up front.
    let mut rules: Vec<Rule> = Vec::new();
    let mut origins: Vec<u8> = Vec::new();
    for (origin, text) in std::iter::once((0u8, UA)).chain(author_css.iter().map(|c| (1u8, c.as_str()))) {
        for r in css::parse_stylesheet(text) {
            if r.media.iter().all(|m| css::media_applies(m, env)) {
                rules.push(r);
                origins.push(origin);
            }
        }
    }
    let mut index: HashMap<(u8, String), Vec<Entry>> = HashMap::new();
    for (ri, r) in rules.iter().enumerate() {
        for (si, sel) in r.selectors.iter().enumerate() {
            index.entry(bucket(sel)).or_default().push((origins[ri], ri, ri, si));
        }
    }
    Cascade { rules, origins, index }
}

pub fn compute(&self, dom: &Dom) -> Styled {
    let mut styles: Vec<Style> = Vec::with_capacity(dom.nodes.len());
    let mut extra: Vec<(NodeId, PseudoElement, Style)> = Vec::new();
    let root_style = Style { display: Display::Block, ..Style::default() };
    let mut rem = 16.0;
    for id in 0..dom.nodes.len() as NodeId {
        let parent_id = dom.parent(id);
        let parent = if parent_id == id || (parent_id as usize) >= styles.len() { root_style.clone() } else { styles[parent_id as usize].clone() };
        let s = match element(dom, id) {
            Some((tag, attrs)) => self.element_style(dom, id, tag, attrs, &parent, &mut rem, &mut extra),
            None if id == 0 => root_style.clone(),
            None => parent,
        };
        styles.push(s);
    }
    // Pseudo-element styles go after the nodes'.
    let mut out = Styled { styles, before: HashMap::new(), after: HashMap::new(), first_letter: HashMap::new() };
    for (id, el, st) in extra {
        let k = out.styles.len() as NodeId;
        out.styles.push(st);
        match el {
            PseudoElement::Before => out.before.insert(id, k),
            PseudoElement::After => out.after.insert(id, k),
            PseudoElement::FirstLetter => out.first_letter.insert(id, k),
        };
    }
    out
}

/// One node's style, styling only it and its ancestors (those not already
/// in `memo`, which keeps every style worked out here for the next call).
pub fn style_of(&self, dom: &Dom, id: NodeId, memo: &mut HashMap<NodeId, Style>) -> Style {
    let mut chain = vec![id];
    let mut at = id;
    while !memo.contains_key(&at) {
        let p = dom.parent(at);
        if p == at {
            break;
        }
        chain.push(p);
        at = p;
    }
    let root_style = Style { display: Display::Block, ..Style::default() };
    let mut rem = 16.0;
    let mut extra = Vec::new();
    let mut parent: Option<Style> = None;
    for &n in chain.iter().rev() {
        let s = if let Some(s) = memo.get(&n) {
            s.clone()
        } else {
            let up = parent.clone().unwrap_or_else(|| root_style.clone());
            let s = match element(dom, n) {
                Some((tag, attrs)) => self.element_style(dom, n, tag, attrs, &up, &mut rem, &mut extra),
                None if n == 0 => root_style.clone(),
                None => up,
            };
            memo.insert(n, s.clone());
            s
        };
        if dom.tag(n) == Some("html") {
            rem = s.font_size;
        }
        parent = Some(s);
    }
    parent.unwrap_or(root_style)
}

#[allow(clippy::too_many_arguments)]
fn element_style(&self, dom: &Dom, id: NodeId, tag: &str, attrs: &[(Box<str>, String)], parent: &Style, rem_out: &mut f32, extra: &mut Vec<(NodeId, PseudoElement, Style)>) -> Style {
        let (rules, origins, index) = (&self.rules, &self.origins, &self.index);
        let empty = Vec::new();
        let mut rem = *rem_out;
        // Candidate rules: by id, each class, the tag, and the universal ones.
        let mut cands: Vec<&Entry> = Vec::new();
        if let Some(i) = attr(attrs, "id") {
            cands.extend(index.get(&(0, i.to_string())).unwrap_or(&empty));
        }
        for cl in attr(attrs, "class").unwrap_or("").split_ascii_whitespace() {
            cands.extend(index.get(&(1, cl.to_string())).unwrap_or(&empty));
        }
        cands.extend(index.get(&(2, tag.to_ascii_lowercase())).unwrap_or(&empty));
        cands.extend(index.get(&(3, String::new())).unwrap_or(&empty));
        cands.sort_by_key(|e| (e.1, e.3));
        cands.dedup();
        // (important, origin, specificity, order) → declaration, for the element and its pseudo-elements
        type Found<'a> = Vec<((bool, u8, (u16, u16, u16), usize), &'a Decl)>;
        let (mut found, mut before, mut after, mut first_letter): (Found, Found, Found, Found) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        // Group candidates by rule: a rule applies once, at its most specific matching selector.
        let mut per_rule: Vec<(usize, Option<PseudoElement>, (u16, u16, u16))> = Vec::new();
        for e in &cands {
            let sel = &rules[e.2].selectors[e.3];
            if matches(dom, id, sel) {
                match per_rule.iter_mut().find(|(r, el, _)| *r == e.2 && *el == sel.element) {
                    Some(x) => x.2 = x.2.max(sel.specificity),
                    None => per_rule.push((e.2, sel.element, sel.specificity)),
                }
            }
        }
        for (ri, el, spec) in per_rule {
            let target = match el {
                None => &mut found,
                Some(PseudoElement::Before) => &mut before,
                Some(PseudoElement::After) => &mut after,
                Some(PseudoElement::FirstLetter) => &mut first_letter,
            };
            for d in &rules[ri].decls {
                target.push(((d.important, origins[ri], spec, ri), d));
            }
        }
        let inline = attr(attrs, "style").map(css::parse_declarations).unwrap_or_default();
        for (k, d) in inline.iter().enumerate() {
            found.push(((d.important, 2, (u16::MAX, 0, 0), usize::MAX - inline.len() + k), d));
        }
        let mut s = inherited(&parent);
        cascade(&mut s, &mut found, &parent, rem);
        if s.clipped && matches!(s.position, Position::Absolute | Position::Fixed) {
            s.display = Display::None; // visually hidden: for screen readers only
        }
        // Flex and grid items are blocks (so vertical-align and the like don't apply).
        if matches!(parent.display, Display::Flex | Display::Grid) && matches!(s.display, Display::Inline | Display::InlineBlock) {
            s.display = Display::Block;
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
        // (A canvas's width and height are its bitmap's size, not CSS sizes.)
        if matches!(tag, "td" | "th" | "table" | "col" | "img" | "hr" | "iframe" | "video") && s.width == Len::Auto {
            if let Some(w) = attr(attrs, "width").map(str::trim) {
                if let Some(p) = w.strip_suffix('%').and_then(|p| p.trim().parse::<f32>().ok()) {
                    s.width = Len::Pct(p);
                } else if let Ok(px) = w.trim_end_matches("px").parse::<f32>() {
                    s.width = Len::Px(px);
                }
            }
        }
        if matches!(tag, "td" | "th" | "tr" | "table" | "img" | "iframe" | "video") && s.height == Len::Auto {
            if let Some(h) = attr(attrs, "height").map(str::trim) {
                if let Some(p) = h.strip_suffix('%').and_then(|p| p.trim().parse::<f32>().ok()) {
                    s.height = Len::Pct(p);
                } else if let Ok(px) = h.trim_end_matches("px").parse::<f32>() {
                    s.height = Len::Px(px);
                }
            }
        }
        // bgcolor and friends, from before CSS.
        if let Some(c) = attr(attrs, "bgcolor").and_then(parse_color) {
            if s.background.a == 0 {
                s.background = c;
            }
        }
        if tag == "body" {
            if let Some(c) = attr(attrs, "text").and_then(parse_color) {
                s.color = c;
            }
        }
        if tag == "table" {
            if let Some(px) = attr(attrs, "cellpadding").and_then(|v| v.trim().parse::<f32>().ok()) {
                s.cell_padding = Some(px);
            }
            if s.border_spacing == (2.0, 2.0) {
                if let Some(px) = attr(attrs, "cellspacing").and_then(|v| v.trim().trim_end_matches("px").parse::<f32>().ok()) {
                    s.border_spacing = (px.max(0.0), px.max(0.0));
                }
            }
        }
        if matches!(tag, "td" | "th") && !s.padding_set {
            // cellpadding comes down through the row (and row group) to the cells; 1px otherwise.
            s.padding = [Len::Px(parent.cell_padding.unwrap_or(1.0)); 4];
        }
        if matches!(tag, "tr" | "tbody" | "thead" | "tfoot") {
            s.cell_padding = parent.cell_padding;
        }
        if matches!(tag, "td" | "th" | "tr" | "div" | "p" | "table") {
            if let Some(a) = attr(attrs, "valign").map(|v| v.to_ascii_lowercase()) {
                if a == "middle" || a == "center" {
                    s.valign = VAlign::Middle;
                }
            }
        }
        // Raised or lowered text: its own vertical-align plus its inline ancestors'.
        let inherited_shift = if parent.display == Display::Inline { parent.shift } else { 0.0 };
        let own = match s.valign {
            VAlign::Super => -0.4 * parent.font_size,
            VAlign::Sub => 0.2 * parent.font_size,
            VAlign::Baseline => -s.raise,
            _ => 0.0,
        };
        s.shift = if s.display == Display::Inline { inherited_shift + own } else { 0.0 };
        if s.valign != VAlign::Baseline && matches!(tag, "sup" | "sub") && s.display == Display::Inline {
            // Superscripts don't make their line taller.
            s.line_height = s.line_height.min(parent.line_height);
        }
        // ::first-letter: for block containers; it can't be a block itself.
        if !first_letter.is_empty() && matches!(s.display, Display::Block | Display::ListItem | Display::InlineBlock | Display::TableCell | Display::TableCaption) {
            let mut p = inherited(&s);
            p.display = Display::Inline;
            cascade(&mut p, &mut first_letter, &s, rem);
            p.display = Display::Inline;
            p.shift = 0.0;
            extra.push((id, PseudoElement::FirstLetter, p));
        }
        for (el, list) in [(PseudoElement::Before, &mut before), (PseudoElement::After, &mut after)] {
            if list.is_empty() {
                continue;
            }
            let mut p = inherited(&s);
            p.display = Display::Inline;
            cascade(&mut p, list, &s, rem);
            if let Some(text) = p.content.as_deref().and_then(|c| content_text(c, attrs)) {
                p.content = Some(text);
                if p.display != Display::None {
                    if p.display != Display::Inline {
                        p.display = Display::Inline;
                    }
                    p.shift = if s.display == Display::Inline { s.shift } else { 0.0 };
                    extra.push((id, el, p));
                }
            }
        }
        *rem_out = rem;
        s
}
}

/// Applies matched declarations in cascade order: custom properties first,
/// then the font size (so `em` in the same rule uses it), then the rest, each
/// with `var()` substituted.
fn cascade(s: &mut Style, found: &mut [((bool, u8, (u16, u16, u16), usize), &Decl)], parent: &Style, rem: f32) {
    found.sort_by_key(|(k, _)| *k);
    let mut vars: Option<HashMap<String, String>> = None;
    for (_, d) in found.iter().filter(|(_, d)| d.name.starts_with("--")) {
        vars.get_or_insert_with(|| (*s.vars).clone()).insert(d.name.clone(), d.value.clone());
    }
    if let Some(v) = vars {
        s.vars = Rc::new(v);
    }
    let vars = s.vars.clone();
    let resolved = |d: &Decl| -> Option<Decl> {
        if d.value.contains("var(") {
            Some(Decl { name: d.name.clone(), value: substitute(&d.value, &vars, 0)?, important: d.important })
        } else {
            Some(d.clone())
        }
    };
    for (_, d) in found.iter().filter(|(_, d)| d.name == "font-size" || d.name == "font") {
        if let Some(d) = resolved(d) {
            apply(s, &d, parent, rem);
        }
    }
    for (_, d) in found.iter().filter(|(_, d)| d.name != "font-size" && d.name != "font" && !d.name.starts_with("--")) {
        if let Some(d) = resolved(d) {
            apply(s, &d, parent, rem);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use volant_core::html;

    fn style_of(html: &str, css: &str, tag: &str) -> Style {
        let dom = html::parse(html);
        let styled = compute(&dom, &[css.to_string()], &Env::default());
        let id = dom.find(tag).unwrap();
        styled.styles[id as usize].clone()
    }

    #[test]
    fn one_element_matches_whole_tree() {
        let dom = html::parse("<html style='font-size:20px'><body class=a><div id=x><p class='b c'>t<span>s</span></p><em>e</em></div></body></html>");
        let css = ".a p { color: red } #x > em { font-size: 2rem } .b.c span { font-weight: bold } div em + * { opacity: .5 }".to_string();
        let env = Env::default();
        let whole = compute(&dom, &[css.clone()], &env);
        let cascade = Cascade::new(&[css], &env);
        let mut memo = HashMap::new();
        for id in (0..dom.nodes.len() as NodeId).rev() {
            let one = cascade.style_of(&dom, id, &mut memo);
            let w = &whole.styles[id as usize];
            assert_eq!((one.color, one.font_size, one.weight, one.opacity, one.display), (w.color, w.font_size, w.weight, w.opacity, w.display), "node {id}");
        }
        let em = dom.find("em").unwrap();
        assert_eq!(cascade.style_of(&dom, em, &mut HashMap::new()).font_size, 40.0);
    }

    #[test]
    fn web_fonts() {
        let css = "@font-face { font-family: 'Brand Sans'; src: url(a.eot); src: url(/f/brand.woff2) format('woff2'), url(/f/brand.eot) format('embedded-opentype'), url(\"/f/brand.ttf\"); font-weight: 300 700 }\
                   @media screen { @font-face { font-family: Brand Sans; font-style: italic; src: url(i.woff) } }\
                   p { font-family: 'Brand Sans', Georgia, serif } h1 { font-family: \"Not Loaded\", monospace }";
        let faces = crate::css::font_faces(css);
        assert_eq!(faces.len(), 2);
        assert_eq!((faces[0].family.as_str(), faces[0].weight, faces[0].italic), ("brand sans", (300, 700), false));
        assert_eq!(faces[0].sources, vec!["/f/brand.woff2", "/f/brand.ttf"]);
        assert!(faces[1].italic);
        // Until its files load, the family falls back; then it's used, with its fallback kept.
        assert_eq!(style_of("<p>x</p>", css, "p").family, Family::Serif);
        crate::fonts::declare_face("Brand Sans");
        let f = style_of("<p>x</p>", css, "p").family;
        assert!(matches!(f, Family::Face(_)) && f.generic() == Family::Serif, "{f:?}");
        assert_eq!(crate::fonts::face_info(match f { Family::Face(i) => i, _ => 0 }).map(|(n, _)| n).as_deref(), Some("brand sans"));
        assert_eq!(style_of("<h1>x</h1>", css, "h1").family, Family::Mono);
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

    #[test]
    fn colours_logical_properties_and_sizing_keywords() {
        assert_eq!(parse_color("Coral"), Some(Color::rgb(255, 127, 80)));
        assert_eq!(parse_color("rebeccapurple"), Some(Color::rgb(102, 51, 153)));
        assert_eq!(parse_color("hsl(120, 100%, 25%)"), Some(Color::rgb(0, 128, 0)));
        assert_eq!(parse_color("hsl(0.5turn 100% 50% / 50%)").map(|c| (c.r, c.g, c.b, c.a)), Some((0, 255, 255, 128)));
        let p = style_of("<p>x</p>", "p { margin-inline: 1px 2px; padding-block-start: 3px; inline-size: 50px; border-inline-start: 4px solid red }", "p");
        assert_eq!((p.margin[3], p.margin[1], p.padding[0], p.width), (Len::Px(1.0), Len::Px(2.0), Len::Px(3.0), Len::Px(50.0)));
        assert_eq!((p.border_width[3], p.border_color[3]), (4.0, Color::rgb(255, 0, 0)));
        let c = style_of("<p>x</p>", "p { color: hsl(120/**/100%/**/25%); width: fit-content; contain: size; contain-intrinsic-size: 10px 20px }", "p");
        assert_eq!((c.color, c.width, c.width_kw), (Color::rgb(0, 128, 0), Len::Auto, Some(Sizing::Fit)), "a comment separates tokens");
        assert_eq!((c.contain_size, c.intrinsic_size), (true, (Some(10.0), Some(20.0))));
    }
}
