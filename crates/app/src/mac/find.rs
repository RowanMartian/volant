//! Find on page (⌘F): a small filled bar in the page's top-right corner.

use super::theme;
use super::views::{self, label, set_text, Hit, IconButton, Kind, Weight};
use super::{ns, rect, Browser};
use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{NSEventModifierFlags, NSFocusRingType, NSTextField};
use objc2_web_kit::{WKFindConfiguration, WKFindResult};
use std::ptr::NonNull;

pub struct FindBar {
    panel: Retained<Hit>,
    pub field: Retained<NSTextField>,
    pub status: Retained<NSTextField>,
    prev: IconButton,
    next: IconButton,
    close: IconButton,
    open: bool,
}

const W: f64 = 330.0;
const H: f64 = 40.0;

impl FindBar {
    pub fn new(mtm: MainThreadMarker) -> Self {
        let panel = Hit::new(Kind::Panel, 0, rect(0.0, 0.0, W, H), false, mtm);
        if let Some(l) = panel.layer() {
            l.setCornerRadius(12.0);
        }
        panel.setHidden(true);
        let field = NSTextField::initWithFrame(NSTextField::alloc(mtm), rect(14.0, 11.0, 170.0, 19.0));
        field.setBezeled(false);
        field.setDrawsBackground(false);
        field.setFocusRingType(NSFocusRingType::None);
        field.setFont(Some(&views::font(13.0, Weight::Regular)));
        field.setPlaceholderString(Some(&ns("Find on page")));
        if let Some(cell) = field.cell() {
            cell.setUsesSingleLineMode(true);
            cell.setScrollable(true);
        }
        let status = label("", 11.5, Weight::Medium, theme::current().text3, mtm);
        status.setFrame(rect(186.0, 12.0, 60.0, 16.0));
        status.setAlignment(objc2_app_kit::NSTextAlignment::Right);
        let prev = IconButton::new(Kind::FindPrev, "chevron.up", 12.0, rect(250.0, 8.0, 24.0, 24.0), "Previous (⇧⌘G)", mtm);
        let next = IconButton::new(Kind::FindNext, "chevron.down", 12.0, rect(274.0, 8.0, 24.0, 24.0), "Next (⌘G)", mtm);
        let close = IconButton::new(Kind::FindClose, "xmark", 11.0, rect(300.0, 8.0, 24.0, 24.0), "Done (Esc)", mtm);
        panel.addSubview(&field);
        panel.addSubview(&status);
        for b in [&prev, &next, &close] {
            panel.addSubview(&b.hit);
        }
        Self { panel, field, status, prev, next, close, open: false }
    }
}

pub fn attach(b: &mut Browser) {
    unsafe { b.find.field.setDelegate(Some(ProtocolObject::from_ref(&*b.delegate))) };
    b.ui.root.addSubview(&b.find.panel);
}

pub fn is_field(b: &Browser, ptr: usize) -> bool {
    Retained::as_ptr(&b.find.field) as usize == ptr
}

pub fn layout(b: &Browser) {
    let c = b.ui.content.frame();
    b.find.panel.setFrame(rect(c.origin.x + c.size.width - W - 12.0, c.origin.y + 12.0, W, H));
}

pub fn repaint(b: &Browser) {
    let t = theme::current();
    let f = &b.find;
    f.panel.set_fill(Some(t.fill));
    f.field.setTextColor(Some(&theme::ns(t.text)));
    views::recolor(&f.status, t.text3);
    for btn in [&f.prev, &f.next, &f.close] {
        views::tint(&btn.img, t.text2);
        btn.hit.restyle();
    }
}

pub fn open(b: &mut Browser) {
    if super::command::is_open(b) {
        return;
    }
    b.find.open = true;
    b.find.panel.setHidden(false);
    b.ui.root.addSubview(&b.find.panel);
    layout(b);
    b.window.makeFirstResponder(Some(&b.find.field));
    if let Some(ed) = b.find.field.currentEditor() {
        unsafe { ed.selectAll(None) };
    }
}

pub fn close(b: &mut Browser) {
    if !b.find.open {
        return;
    }
    b.find.open = false;
    b.find.panel.setHidden(true);
    set_text(&b.find.status, "");
    super::engine_view::clear_find(b);
    if let Some(w) = b.tabs.active.and_then(|id| b.webs.get(&id)) {
        unsafe { w.web.evaluateJavaScript_completionHandler(&ns("getSelection().removeAllRanges()"), None) };
        b.window.makeFirstResponder(Some(&w.web));
    }
}

/// The tab or page changed under the find bar.
pub fn page_changed(b: &mut Browser) {
    if b.find.open {
        close(b);
    }
}

pub fn changed(b: &mut Browser) {
    search(b, false, true);
}

pub fn step(b: &mut Browser, backwards: bool) {
    if !b.find.open {
        open(b);
        return;
    }
    search(b, backwards, false);
}

fn search(b: &mut Browser, backwards: bool, fresh: bool) {
    let text = b.find.field.stringValue().to_string();
    // Reader drawn by Volant's own engine: search what it laid out.
    if super::engine_view::is_active(b) {
        if text.is_empty() {
            set_text(&b.find.status, "");
            super::engine_view::clear_find(b);
            return;
        }
        let (current, total) = super::engine_view::find(b, &text, backwards, fresh);
        set_text(&b.find.status, &if total == 0 { "Not found".to_string() } else { format!("{current} of {total}") });
        return;
    }
    let Some(w) = b.tabs.active.and_then(|id| b.webs.get(&id)) else { return };
    if text.is_empty() {
        set_text(&b.find.status, "");
        unsafe { w.web.evaluateJavaScript_completionHandler(&ns("getSelection().removeAllRanges()"), None) };
        return;
    }
    if fresh {
        // Start from the top of the page for a new search term.
        unsafe { w.web.evaluateJavaScript_completionHandler(&ns("getSelection().removeAllRanges()"), None) };
    }
    let cfg = unsafe { WKFindConfiguration::new(b.mtm) };
    unsafe {
        cfg.setBackwards(backwards);
        cfg.setWraps(true);
        cfg.setCaseSensitive(false);
    }
    let done = RcBlock::new(|result: NonNull<WKFindResult>| {
        let found = unsafe { result.as_ref().matchFound() };
        super::with(move |b| set_text(&b.find.status, if found { "" } else { "Not found" }));
    });
    unsafe { w.web.findString_withConfiguration_completionHandler(&ns(&text), Some(&cfg), &done) };
}

pub fn key(b: &mut Browser, selector: &str) -> bool {
    match selector {
        "insertNewline:" => {
            let shift = objc2_app_kit::NSApplication::sharedApplication(b.mtm)
                .currentEvent()
                .map(|e| e.modifierFlags().contains(NSEventModifierFlags::Shift))
                .unwrap_or(false);
            search(b, shift, false);
            true
        }
        "cancelOperation:" => {
            close(b);
            true
        }
        _ => false,
    }
}
