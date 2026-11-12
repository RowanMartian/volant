//! Text selection over a laid-out page: which character a point is at, the
//! rectangles a selection covers, words and paragraphs, and the plain text to
//! copy (soft-wrapped lines rejoin, `<br>` stays a line break, blocks are
//! separated by a newline, or a blank line where there was a gap).

use crate::fonts::{Font, Fonts};
use crate::layout::{After, Item, Page};

/// A place in the page's text: a text item and a character offset in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Pos {
    pub item: usize,
    pub offset: usize,
}

/// A rectangle, in page coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Area {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

struct Run<'a> {
    x: f32,
    y: f32,
    text: &'a str,
    font: &'a Font,
    after: After,
}

fn run(page: &Page, i: usize) -> Option<Run<'_>> {
    match page.items.get(i)? {
        Item::Text { x, y, text, font, after, .. } => Some(Run { x: *x, y: *y, text, font, after: *after }),
        _ => None,
    }
}

fn prefix(text: &str, chars: usize) -> &str {
    match text.char_indices().nth(chars) {
        Some((i, _)) => &text[..i],
        None => text,
    }
}

fn char_len(text: &str) -> usize {
    text.chars().count()
}

/// The character boundary nearest `dx` px into `text`.
fn offset_at(fonts: &dyn Fonts, text: &str, font: &Font, dx: f32) -> usize {
    if dx <= 0.0 {
        return 0;
    }
    let mut prev = 0.0;
    for (k, (i, c)) in text.char_indices().enumerate() {
        let w = fonts.width(&text[..i + c.len_utf8()], font);
        if dx < (prev + w) / 2.0 {
            return k;
        }
        prev = w;
    }
    char_len(text)
}

/// The text position under (x, y): on the nearest line, the nearest character.
pub fn hit(page: &Page, fonts: &dyn Fonts, x: f32, y: f32) -> Option<Pos> {
    let mut best: Option<((f32, f32), usize)> = None;
    for i in 0..page.items.len() {
        let Some(r) = run(page, i) else { continue };
        let (top, bottom) = (r.y - fonts.ascent(r.font), r.y + fonts.descent(r.font));
        let dy = if y < top { top - y } else if y > bottom { y - bottom } else { 0.0 };
        let w = fonts.width(r.text, r.font);
        let dx = if x < r.x { r.x - x } else if x > r.x + w { x - r.x - w } else { 0.0 };
        let key = (dy, dx);
        if best.map(|(k, _)| key < k).unwrap_or(true) {
            best = Some((key, i));
        }
    }
    let (_, i) = best?;
    let r = run(page, i)?;
    Some(Pos { item: i, offset: offset_at(fonts, r.text, r.font, x - r.x) })
}

fn ordered(a: Pos, b: Pos) -> (Pos, Pos) {
    if a <= b {
        (a, b)
    } else {
        (b, a)
    }
}

/// The highlight rectangles for the text between `a` and `b`.
pub fn areas(page: &Page, fonts: &dyn Fonts, a: Pos, b: Pos) -> Vec<Area> {
    let (a, b) = ordered(a, b);
    let mut out = Vec::new();
    for i in a.item..=b.item.min(page.items.len().saturating_sub(1)) {
        let Some(r) = run(page, i) else { continue };
        let n = char_len(r.text);
        let start = if i == a.item { a.offset.min(n) } else { 0 };
        let end = if i == b.item { b.offset.min(n) } else { n };
        if start >= end {
            continue;
        }
        let x0 = r.x + fonts.width(prefix(r.text, start), r.font);
        let x1 = r.x + fonts.width(prefix(r.text, end), r.font);
        let (asc, desc) = (fonts.ascent(r.font), fonts.descent(r.font));
        out.push(Area { x: x0, y: r.y - asc, w: x1 - x0, h: asc + desc });
    }
    out
}

/// The plain text between `a` and `b`.
pub fn text(page: &Page, a: Pos, b: Pos) -> String {
    let (a, b) = ordered(a, b);
    let mut out = String::new();
    let mut prev: Option<Run> = None;
    for i in a.item..=b.item.min(page.items.len().saturating_sub(1)) {
        let Some(r) = run(page, i) else { continue };
        let n = char_len(r.text);
        let start = if i == a.item { a.offset.min(n) } else { 0 };
        let end = if i == b.item { b.offset.min(n) } else { n };
        if let Some(p) = &prev {
            match p.after {
                After::Inline => {}
                After::Space => out.push(' '),
                After::Wrap => {
                    if !out.ends_with(char::is_whitespace) && !out.ends_with('-') {
                        out.push(' ');
                    }
                }
                After::Break => out.push('\n'),
                // A gap between blocks (paragraphs) becomes a blank line.
                After::BlockEnd => out.push_str(if r.y - p.y > p.font.size * 2.4 { "\n\n" } else { "\n" }),
            }
        }
        let piece: String = r.text.chars().skip(start).take(end.saturating_sub(start)).collect();
        out.push_str(&piece);
        prev = Some(r);
    }
    out.trim().to_string()
}

fn word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '\'' || c == '’'
}

/// The word at `p` (or the single character, if it isn't in a word).
pub fn word(page: &Page, p: Pos) -> (Pos, Pos) {
    let Some(r) = run(page, p.item) else { return (p, p) };
    let chars: Vec<char> = r.text.chars().collect();
    let at = p.offset.min(chars.len());
    // Clicking just after a word's last letter still means that word.
    let probe = if at < chars.len() && word_char(chars[at]) { at } else { at.saturating_sub(1) };
    if chars.get(probe).map(|c| !word_char(*c)).unwrap_or(true) {
        return (Pos { item: p.item, offset: at }, Pos { item: p.item, offset: (at + 1).min(chars.len()) });
    }
    let mut s = probe;
    while s > 0 && word_char(chars[s - 1]) {
        s -= 1;
    }
    let mut e = probe;
    while e < chars.len() && word_char(chars[e]) {
        e += 1;
    }
    (Pos { item: p.item, offset: s }, Pos { item: p.item, offset: e })
}

/// The whole block of text (paragraph, list item, cell) that `p` is in.
pub fn paragraph(page: &Page, p: Pos) -> (Pos, Pos) {
    let mut first = p.item;
    let mut k = p.item;
    while k > 0 {
        k -= 1;
        match run(page, k) {
            Some(r) if r.after == After::BlockEnd => break,
            Some(_) => first = k,
            None => {}
        }
    }
    let mut last = p.item;
    for k in p.item..page.items.len() {
        if let Some(r) = run(page, k) {
            last = k;
            if r.after == After::BlockEnd {
                break;
            }
        }
    }
    let end = run(page, last).map(|r| char_len(r.text)).unwrap_or(0);
    (Pos { item: first, offset: 0 }, Pos { item: last, offset: end })
}

/// All the text on the page.
pub fn all(page: &Page) -> Option<(Pos, Pos)> {
    let first = (0..page.items.len()).find(|&i| run(page, i).is_some())?;
    let last = (0..page.items.len()).rev().find(|&i| run(page, i).is_some())?;
    Some((Pos { item: first, offset: 0 }, Pos { item: last, offset: char_len(run(page, last)?.text) }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{render, ApproxFonts, NoImages};

    fn page(body: &str, width: f32) -> Page {
        let html = format!("<html><body>{body}</body></html>");
        render(&html, "body { margin: 0; font: 10px/20px sans-serif } p { margin: 0 0 20px }", width, &ApproxFonts, &NoImages)
    }

    #[test]
    fn copies_like_a_reader_expects() {
        let p = page("<p>one two three four five six</p><p>Second <b>bold</b> line<br>after break</p><ul><li>alpha<li>beta</ul>", 90.0);
        let (a, b) = all(&p).unwrap();
        assert_eq!(text(&p, a, b), "one two three four five six\n\nSecond bold line\nafter break\n\n• alpha\n• beta");
        // A word too long for its line is split on screen but copies whole.
        let p = page("<p>Supercalifragilistic</p>", 40.0);
        let (a, b) = all(&p).unwrap();
        assert_eq!(text(&p, a, b), "Supercalifragilistic");
    }

    #[test]
    fn hits_words_and_areas() {
        let p = page("<p>Hello world</p>", 500.0);
        // "Hello " is 7.22+5.56+2.22+2.22+5.56+2.78 = 25.56 px; a click at 25.5 is past the space's middle.
        let at = hit(&p, &ApproxFonts, 25.5, 13.0).unwrap();
        assert_eq!(at.offset, 6);
        let (s, e) = word(&p, at);
        assert_eq!(text(&p, s, e), "world");
        let (s2, e2) = word(&p, Pos { item: at.item, offset: 11 });
        assert_eq!(text(&p, s2, e2), "world", "just after the last letter");
        let r = areas(&p, &ApproxFonts, s, e);
        assert_eq!(r.len(), 1);
        assert!((r[0].x - 25.56).abs() < 0.01 && r[0].w > 20.0 && (r[0].h - 10.0).abs() < 0.01);
        // Far below the text: the nearest line, the nearest end.
        assert_eq!(hit(&p, &ApproxFonts, 999.0, 500.0).unwrap().offset, 11);
        // Backwards selections read the same.
        assert_eq!(text(&p, e, s), "world");
    }

    #[test]
    fn paragraphs() {
        let p = page("<p>first para wraps around here</p><p>second</p>", 60.0);
        let at = hit(&p, &ApproxFonts, 5.0, 33.0).unwrap(); // the second line of the first paragraph
        let (s, e) = paragraph(&p, at);
        assert_eq!(text(&p, s, e), "first para wraps around here");
    }
}
