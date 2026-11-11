//! The command bar (⌘T / ⌘L): one field for addresses, searches, history and open tabs.
//! A flat panel over a dimmed window. Inline completion fills in the site
//! you're most likely typing.

use super::theme::{self, Rgba};
use super::ui::SiteIcon;
use super::views::{self, label, set_text, symbol, Hit, Kind, Weight};
use super::{actions, ns, rect, Browser};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{NSEventModifierFlags, NSFocusRingType, NSImageView, NSTextField};
use objc2_foundation::NSRange;
use std::cell::Cell;
use volant_core::tabs::TabId;
use volant_core::{fuzzy, omnibox, search};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Load in the tab in front.
    Current,
    /// Open a new tab.
    NewTab,
}

#[derive(Clone, Debug)]
enum Sugg {
    Go(String),
    Search(String),
    Page { url: String, title: String },
    Tab { id: TabId, url: String, title: String },
}

struct Row {
    hit: Retained<Hit>,
    icon: SiteIcon,
    glyph: Retained<NSImageView>,
    title: Retained<NSTextField>,
    sub: Retained<NSTextField>,
    /// Which SF Symbol the glyph shows, to skip reloading it.
    glyph_name: Cell<&'static str>,
}

pub struct CommandBar {
    overlay: Retained<Hit>,
    panel: Retained<Hit>,
    pub field: Retained<NSTextField>,
    lens: Retained<NSImageView>,
    rows: Vec<Row>,
    /// How many rows are in use.
    shown: usize,
    suggs: Vec<Sugg>,
    selected: usize,
    mode: Mode,
    open: bool,
    /// What was actually typed, without the inline completion.
    typed: String,
}

const MAX_ROWS: usize = 7;
const ROW_H: f64 = 46.0;
const HEAD_H: f64 = 58.0;

impl CommandBar {
    pub fn new(mtm: MainThreadMarker) -> Self {
        let overlay = Hit::new(Kind::Scrim, 0, rect(0.0, 0.0, 100.0, 100.0), false, mtm);
        if let Some(l) = overlay.layer() {
            l.setCornerRadius(0.0);
        }
        overlay.setHidden(true);
        let panel = Hit::new(Kind::Panel, 0, rect(0.0, 0.0, 600.0, HEAD_H), false, mtm);
        if let Some(l) = panel.layer() {
            l.setCornerRadius(16.0);
            l.setMasksToBounds(true);
        }
        overlay.addSubview(&panel);
        let field = NSTextField::initWithFrame(NSTextField::alloc(mtm), rect(52.0, 16.0, 500.0, 28.0));
        field.setBezeled(false);
        field.setDrawsBackground(false);
        field.setFocusRingType(NSFocusRingType::None);
        field.setFont(Some(&views::font(19.0, Weight::Regular)));
        field.setPlaceholderString(Some(&ns("Search or enter an address")));
        if let Some(cell) = field.cell() {
            cell.setUsesSingleLineMode(true);
            cell.setScrollable(true);
        }
        panel.addSubview(&field);
        let lens = views::image_view(symbol("magnifyingglass", 16.0, Weight::Medium).as_deref(), rect(20.0, 19.0, 20.0, 20.0), None, mtm);
        panel.addSubview(&lens);
        let rows: Vec<Row> = (0..MAX_ROWS).map(|i| make_row(i, mtm)).collect();
        for r in &rows {
            panel.addSubview(&r.hit);
        }
        Self { overlay, panel, field, lens, rows, shown: 0, suggs: Vec::new(), selected: 0, mode: Mode::Current, open: false, typed: String::new() }
    }
}

pub fn attach(b: &mut Browser) {
    unsafe { b.command.field.setDelegate(Some(ProtocolObject::from_ref(&*b.delegate))) };
    b.ui.root.addSubview(&b.command.overlay);
}

pub fn is_open(b: &Browser) -> bool {
    b.command.open
}

pub fn is_field(b: &Browser, ptr: usize) -> bool {
    Retained::as_ptr(&b.command.field) as usize == ptr
}

pub fn open(b: &mut Browser, mode: Mode, text: &str) {
    super::find::close(b);
    let c = &mut b.command;
    c.mode = mode;
    c.open = true;
    c.typed = text.to_string();
    c.selected = 0;
    c.field.setStringValue(&ns(text));
    c.overlay.setHidden(false);
    // Build the history index now, so typing never waits for it.
    b.history.warm(super::now());
    // Re-adding moves it above the page.
    b.ui.root.addSubview(&b.command.overlay);
    rebuild(b);
    layout(b);
    b.window.makeFirstResponder(Some(&b.command.field));
    if let Some(ed) = b.command.field.currentEditor() {
        unsafe { ed.selectAll(None) };
    }
}

pub fn close(b: &mut Browser) {
    if !b.command.open {
        return;
    }
    b.command.open = false;
    b.command.overlay.setHidden(true);
    // Give the index's memory back until next time.
    b.history.cool();
    if let Some(w) = b.tabs.active.and_then(|id| b.webs.get(&id)) {
        b.window.makeFirstResponder(Some(&w.web));
    }
}

pub fn layout(b: &Browser) {
    let c = &b.command;
    let size = b.ui.root.frame().size;
    c.overlay.setFrame(rect(0.0, 0.0, size.width, size.height));
    let content = b.ui.content.frame();
    let pw = (content.size.width - 48.0).clamp(320.0, 640.0);
    let x = content.origin.x + ((content.size.width - pw) / 2.0).round();
    let y = content.origin.y + (content.size.height * 0.13).round().max(48.0);
    let n = c.shown as f64;
    let h = HEAD_H + if n > 0.0 { n * ROW_H + 8.0 } else { 0.0 };
    c.panel.setFrame(rect(x, y, pw, h));
    c.field.setFrame(rect(52.0, 16.0, pw - 70.0, 28.0));
    for (i, r) in c.rows.iter().enumerate().take(c.shown) {
        let w = pw - 16.0;
        r.hit.setFrame(rect(8.0, HEAD_H + i as f64 * ROW_H, w, ROW_H - 2.0));
        r.title.setFrame(rect(48.0, 5.0, w - 60.0, 19.0));
        r.sub.setFrame(rect(48.0, 23.0, w - 60.0, 16.0));
    }
}

pub fn repaint(b: &Browser) {
    let t = theme::current();
    let c = &b.command;
    c.overlay.set_fill(Some(t.scrim));
    c.panel.set_fill(Some(t.panel));
    c.field.setTextColor(Some(&theme::ns(t.text)));
    views::tint(&c.lens, t.text3);
    restyle_rows(b);
}

fn restyle_rows(b: &Browser) {
    let t = theme::current();
    let c = &b.command;
    for (i, r) in c.rows.iter().enumerate() {
        let on = i == c.selected && i < c.shown;
        r.hit.set_fill(Some(if on { t.accent_soft } else { Rgba(0.0, 0.0, 0.0, 0.0) }));
        views::recolor(&r.title, t.text);
        views::recolor(&r.sub, if on { t.accent } else { t.text2 });
        views::tint(&r.glyph, if on { t.accent } else { t.text2 });
    }
}

/// The text field changed: refresh suggestions and maybe complete inline.
pub fn changed(b: &mut Browser) {
    let text = b.command.field.stringValue().to_string();
    let grew = text.len() > b.command.typed.len() && text.starts_with(&b.command.typed);
    b.command.typed = text.clone();
    if grew && !text.ends_with(' ') {
        if let Some(full) = b.history.complete(&text, super::now()) {
            // Keep exactly what was typed and select the rest, so typing on replaces it.
            let low = text.to_ascii_lowercase();
            let low = low.strip_prefix("https://").or_else(|| low.strip_prefix("http://")).unwrap_or(&low);
            let low = low.strip_prefix("www.").unwrap_or(low);
            if full.len() > low.len() && full.is_char_boundary(low.len()) {
                let rest = &full[low.len()..];
                let shown = format!("{text}{rest}");
                b.command.field.setStringValue(&ns(&shown));
                if let Some(ed) = b.command.field.currentEditor() {
                    let start = text.encode_utf16().count();
                    ed.setSelectedRange(NSRange::new(start, rest.encode_utf16().count()));
                }
            }
        }
    }
    b.command.selected = 0;
    rebuild(b);
    layout(b);
}

fn full_text(b: &Browser) -> String {
    b.command.field.stringValue().to_string()
}

fn suggestions(b: &Browser) -> Vec<Sugg> {
    let typed = b.command.typed.trim().to_string();
    let full = full_text(b).trim().to_string();
    let now = super::now();
    let mut out: Vec<Sugg> = Vec::new();
    if typed.is_empty() {
        // Nothing typed: your most-visited places.
        for (_, url, title) in b.history.top_sites(5, now) {
            out.push(Sugg::Page { url, title });
        }
        return out;
    }
    let primary_url = omnibox::as_url(&full);
    match &primary_url {
        Some(u) => out.push(Sugg::Go(u.clone())),
        None => out.push(Sugg::Search(typed.clone())),
    }
    let q = fuzzy::fold(&typed);
    let words: Vec<&str> = q.split_whitespace().collect();
    let mut tab_hits: Vec<(u32, &volant_core::tabs::Tab)> = b
        .tabs
        .list
        .iter()
        .filter(|t| Some(t.id) != b.tabs.active)
        .filter_map(|t| fuzzy::score(&words, &fuzzy::fold(&omnibox::display(&t.url)), &fuzzy::fold(&t.title)).map(|s| (s, t)))
        .collect();
    tab_hits.sort_by(|a, b| b.0.cmp(&a.0));
    for (_, t) in tab_hits.into_iter().take(2) {
        out.push(Sugg::Tab { id: t.id, url: t.url.clone(), title: t.title.clone() });
    }
    let skip = |u: &str, out: &[Sugg]| {
        out.iter().any(|s| match s {
            Sugg::Go(x) | Sugg::Page { url: x, .. } | Sugg::Tab { url: x, .. } => x.trim_end_matches('/') == u.trim_end_matches('/'),
            _ => false,
        })
    };
    for p in b.history.query(&typed, 10, now) {
        if out.len() >= MAX_ROWS - 1 {
            break;
        }
        if !skip(&p.url, &out) {
            out.push(Sugg::Page { url: p.url.clone(), title: p.title.clone() });
        }
    }
    if primary_url.is_some() && out.len() < MAX_ROWS {
        out.push(Sugg::Search(typed));
    }
    out
}

fn make_row(i: usize, mtm: MainThreadMarker) -> Row {
    let t = theme::current();
    let hit = Hit::new(Kind::Suggestion, i as u64, rect(8.0, 0.0, 400.0, ROW_H - 2.0), false, mtm);
    if let Some(l) = hit.layer() {
        l.setCornerRadius(10.0);
    }
    let icon = SiteIcon::new(14.0, 11.0, 22.0, mtm);
    let glyph = views::image_view(None, rect(15.0, 12.0, 20.0, 20.0), Some(t.text2), mtm);
    let title = label("", 13.5, Weight::Medium, t.text, mtm);
    let sub = label("", 12.0, Weight::Regular, t.text2, mtm);
    hit.addSubview(&icon.tile);
    hit.addSubview(&glyph);
    hit.addSubview(&title);
    hit.addSubview(&sub);
    hit.setHidden(true);
    Row { hit, icon, glyph, title, sub, glyph_name: Cell::new("") }
}

/// Fills the (reused) rows with fresh suggestions. Rows are created once,
/// so a keystroke only changes text and images, never the view tree.
fn rebuild(b: &mut Browser) {
    let suggs = suggestions(b);
    let engine = search::engine(&b.settings.engine).name;
    for (i, row) in b.command.rows.iter().enumerate() {
        let Some(s) = suggs.get(i) else {
            row.hit.setHidden(true);
            continue;
        };
        let (title, sub, url, sym): (String, String, Option<&str>, &'static str) = match s {
            Sugg::Go(u) => (omnibox::display(u), "Open website".into(), Some(u), ""),
            Sugg::Search(q) => (q.clone(), format!("Search {engine}"), None, "magnifyingglass"),
            Sugg::Page { url, title } => {
                let searched = search::query_of(url).is_some();
                (
                    if title.is_empty() { omnibox::display(url) } else { title.clone() },
                    omnibox::display(url),
                    if searched { None } else { Some(url) },
                    if searched { "clock" } else { "" },
                )
            }
            Sugg::Tab { url, title, .. } => (
                if title.is_empty() { omnibox::display(url) } else { title.clone() },
                format!("Switch to tab  ·  {}", omnibox::display(url)),
                Some(url),
                "",
            ),
        };
        match url {
            Some(u) if sym.is_empty() => {
                row.icon.set(b, u);
                row.icon.tile.setHidden(false);
                row.glyph.setHidden(true);
            }
            _ => {
                row.icon.tile.setHidden(true);
                row.glyph.setHidden(false);
                if row.glyph_name.get() != sym {
                    row.glyph.setImage(symbol(sym, 15.0, Weight::Medium).as_deref());
                    row.glyph_name.set(sym);
                }
            }
        }
        set_text(&row.title, &title);
        set_text(&row.sub, &sub);
        row.hit.setHidden(false);
    }
    b.command.shown = suggs.len().min(b.command.rows.len());
    b.command.suggs = suggs;
    if b.command.selected >= b.command.shown {
        b.command.selected = 0;
    }
    restyle_rows(b);
}

/// Arrow keys, Return and Escape while the field has focus. Returns true if handled.
pub fn key(b: &mut Browser, selector: &str) -> bool {
    let n = b.command.shown;
    match selector {
        "moveDown:" | "moveDownAndModifySelection:" if n > 0 => {
            b.command.selected = (b.command.selected + 1) % n;
            restyle_rows(b);
            true
        }
        "moveUp:" | "moveUpAndModifySelection:" if n > 0 => {
            b.command.selected = (b.command.selected + n - 1) % n;
            restyle_rows(b);
            true
        }
        "insertNewline:" | "insertNewlineIgnoringFieldEditor:" => {
            let cmd = objc2_app_kit::NSApplication::sharedApplication(b.mtm)
                .currentEvent()
                .map(|e| e.modifierFlags().contains(NSEventModifierFlags::Command))
                .unwrap_or(false);
            if n == 0 {
                let text = full_text(b);
                if !text.trim().is_empty() {
                    go(b, &omnibox::resolve(&text, &b.settings.engine), cmd);
                }
            } else {
                choose(b, b.command.selected, cmd);
            }
            true
        }
        "cancelOperation:" => {
            close(b);
            true
        }
        _ => false,
    }
}

fn go(b: &mut Browser, url: &str, new_tab: bool) {
    let mode = b.command.mode;
    close(b);
    if new_tab || mode == Mode::NewTab {
        actions::open_tab(b, url, true);
    } else {
        actions::navigate(b, url);
    }
}

pub fn choose(b: &mut Browser, i: usize, new_tab: bool) {
    let Some(s) = b.command.suggs.get(i).cloned() else { return };
    match s {
        Sugg::Go(u) => go(b, &u, new_tab),
        Sugg::Page { url, .. } => go(b, &url, new_tab),
        Sugg::Search(q) => {
            let u = search::search_url(&b.settings.engine, &q);
            go(b, &u, new_tab);
        }
        Sugg::Tab { id, .. } => {
            close(b);
            // A blank new tab you leave to switch elsewhere isn't worth keeping.
            let leave = b.tabs.active.filter(|&a| b.tabs.get(a).map(|t| t.url == actions::NEWTAB).unwrap_or(false));
            actions::select(b, id);
            if let Some(a) = leave {
                actions::close_tab(b, a);
                actions::select(b, id);
            }
        }
    }
}
