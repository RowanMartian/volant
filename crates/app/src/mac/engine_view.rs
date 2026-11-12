//! Volant's own engine on screen (experimental, off by default): with
//! Settings → Experimental → "Draw Reader with Volant's own engine", Reader
//! view is laid out by volant-engine and drawn natively by this view, instead
//! of by WebKit.
//!
//! Reader itself works exactly as before: the article still loads as the
//! `volant://reader` page, so history, Back and the Reader toolbar behave the
//! same. This view sits over that page's content, just under its toolbar, so
//! the size and font buttons keep working (they change settings this view
//! follows). Turning the setting off simply hides it.

use super::{actions, ns, rect, theme, with, Browser};
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{define_class, msg_send, AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSAttributedStringNSStringDrawing, NSBezierPath, NSColor, NSCursor, NSEvent, NSFont, NSFontAttributeName, NSFontDescriptorSymbolicTraits,
    NSFontDescriptorSystemDesignSerif, NSFontWeightBold, NSFontWeightMedium, NSFontWeightRegular, NSFontWeightSemibold, NSForegroundColorAttributeName,
    NSScrollView, NSScrollerStyle, NSStrikethroughStyleAttributeName, NSUnderlineStyleAttributeName, NSView, NSWindowOrderingMode,
};
use objc2_foundation::{NSAttributedString, NSDictionary, NSNumber, NSPoint, NSRect, NSSize, NSString};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use volant_core::tabs::TabId;
use volant_engine::{Family, Font, Fonts, Item, Page};

/// The height of the Reader page's own toolbar, which stays visible above this view.
const TOOLBAR_H: f64 = 60.0;

#[derive(Default)]
pub struct State(RefCell<Inner>);

#[derive(Default)]
struct Inner {
    scroll: Option<Retained<NSScrollView>>,
    view: Option<Retained<EngineView>>,
    tab: Option<TabId>,
    host: String,
    article: Option<volant_core::reader::Article>,
    /// What the current layout was made for: (width, dark, size, serif).
    laid: Option<(i64, bool, u32, bool)>,
}

// ------------------------------------------------------------------ fonts

type FontKey = (Family, u32, u16, bool);

thread_local! {
    static NS_FONTS: RefCell<HashMap<FontKey, Retained<NSFont>>> = RefCell::new(HashMap::new());
}

fn ns_font(f: &Font) -> Retained<NSFont> {
    NS_FONTS.with(|cache| {
        if let Some(font) = cache.borrow().get(&f.key()) {
            return font.clone();
        }
        let size = f.size as f64;
        let weight = unsafe {
            match f.weight {
                0..=449 => NSFontWeightRegular,
                450..=549 => NSFontWeightMedium,
                550..=649 => NSFontWeightSemibold,
                _ => NSFontWeightBold,
            }
        };
        let mut font = match f.family {
            Family::Mono => NSFont::monospacedSystemFontOfSize_weight(size, weight),
            _ => NSFont::systemFontOfSize_weight(size, weight),
        };
        if f.family == Family::Serif {
            // New York, the system's serif.
            if let Some(d) = font.fontDescriptor().fontDescriptorWithDesign(unsafe { NSFontDescriptorSystemDesignSerif }) {
                if let Some(serif) = NSFont::fontWithDescriptor_size(&d, size) {
                    font = serif;
                }
            }
        }
        if f.italic {
            let d = font.fontDescriptor();
            let d = d.fontDescriptorWithSymbolicTraits(d.symbolicTraits() | NSFontDescriptorSymbolicTraits::TraitItalic);
            if let Some(italic) = NSFont::fontWithDescriptor_size(&d, size) {
                font = italic;
            }
        }
        cache.borrow_mut().insert(f.key(), font.clone());
        font
    })
}

fn ns_color(c: volant_engine::Color) -> Retained<NSColor> {
    NSColor::colorWithSRGBRed_green_blue_alpha(c.r as f64 / 255.0, c.g as f64 / 255.0, c.b as f64 / 255.0, c.a as f64 / 255.0)
}

fn attributed(text: &str, font: &NSFont, color: Option<&NSColor>, underline: bool, strike: bool) -> Retained<NSAttributedString> {
    let mut keys: Vec<&NSString> = vec![unsafe { NSFontAttributeName }];
    let mut values: Vec<&AnyObject> = vec![font];
    let one = NSNumber::new_isize(1);
    if let Some(c) = color {
        keys.push(unsafe { NSForegroundColorAttributeName });
        values.push(c);
    }
    if underline {
        keys.push(unsafe { NSUnderlineStyleAttributeName });
        values.push(&one);
    }
    if strike {
        keys.push(unsafe { NSStrikethroughStyleAttributeName });
        values.push(&one);
    }
    let attrs = NSDictionary::from_slices(&keys, &values);
    unsafe { NSAttributedString::initWithString_attributes(NSAttributedString::alloc(), &ns(text), Some(&attrs)) }
}

/// The system's fonts, as the engine measures them.
struct MacFonts;

impl Fonts for MacFonts {
    fn width(&self, text: &str, font: &Font) -> f32 {
        attributed(text, &ns_font(font), None, false, false).size().width as f32
    }
    fn ascent(&self, font: &Font) -> f32 {
        ns_font(font).ascender() as f32
    }
    fn descent(&self, font: &Font) -> f32 {
        -ns_font(font).descender() as f32
    }
}

// ------------------------------------------------------------------ the view

pub struct EngineIvars {
    page: RefCell<Option<Rc<Page>>>,
}

define_class!(
    // SAFETY: NSView has no subclassing requirements; EngineView does not implement Drop.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[ivars = EngineIvars]
    pub struct EngineView;

    impl EngineView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _e: Option<&NSEvent>) -> bool {
            true
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, dirty: NSRect) {
            if let Some(page) = self.ivars().page.borrow().as_ref() {
                paint(page, dirty);
            }
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, e: &NSEvent) {
            let p = self.convertPoint_fromView(e.locationInWindow(), None);
            let href = self.ivars().page.borrow().as_ref().and_then(|page| {
                page.links
                    .iter()
                    .find(|l| p.x >= l.x as f64 && p.x <= (l.x + l.w) as f64 && p.y >= l.y as f64 && p.y <= (l.y + l.h) as f64)
                    .map(|l| l.href.clone())
            });
            if let Some(href) = href.filter(|h| h.starts_with("https://") || h.starts_with("http://")) {
                with(move |b| actions::navigate(b, &href));
            }
        }

        #[unsafe(method(resetCursorRects))]
        fn reset_cursor_rects(&self) {
            let hand = NSCursor::pointingHandCursor();
            if let Some(page) = self.ivars().page.borrow().as_ref() {
                for l in &page.links {
                    self.addCursorRect_cursor(rect(l.x as f64, l.y as f64, l.w as f64, l.h as f64), &hand);
                }
            }
        }
    }
);

impl EngineView {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(EngineIvars { page: RefCell::new(None) });
        unsafe { msg_send![super(this), initWithFrame: rect(0.0, 0.0, 100.0, 100.0)] }
    }
}

fn intersects(r: NSRect, x: f64, y: f64, w: f64, h: f64) -> bool {
    x < r.origin.x + r.size.width && x + w > r.origin.x && y < r.origin.y + r.size.height && y + h > r.origin.y
}

/// Draws the part of the page inside `dirty`.
fn paint(page: &Page, dirty: NSRect) {
    ns_color(page.background).setFill();
    NSBezierPath::fillRect(dirty);
    for item in &page.items {
        match item {
            Item::Rect { x, y, w, h, color } => {
                if intersects(dirty, *x as f64, *y as f64, *w as f64, *h as f64) {
                    ns_color(*color).setFill();
                    NSBezierPath::fillRect(rect(*x as f64, *y as f64, *w as f64, *h as f64));
                }
            }
            Item::Text { x, y, text, font, color, underline, strike } => {
                let size = font.size as f64;
                if !intersects(dirty, *x as f64, *y as f64 - size * 1.3, dirty.size.width + 1e6, size * 1.8) {
                    continue;
                }
                let f = ns_font(font);
                let s = attributed(text, &f, Some(&ns_color(*color)), *underline, *strike);
                // In a flipped view a string's origin is the top of its line; the engine gives the baseline.
                s.drawAtPoint(NSPoint::new(*x as f64, *y as f64 - f.ascender()));
            }
        }
    }
}

// ------------------------------------------------------------------ Reader

/// Called when Reader opens in tab `id` with `article` (from the page at `url`).
pub fn show(b: &Browser, id: TabId, article: volant_core::reader::Article, url: &str) {
    {
        let mut e = b.engine.0.borrow_mut();
        e.tab = Some(id);
        e.host = volant_core::omnibox::host(url).to_string();
        e.article = Some(article);
        e.laid = None;
    }
    sync(b);
}

/// Shows, hides and re-lays out the view to match the tab in front, the
/// window size, the appearance and the Reader settings.
pub fn sync(b: &Browser) {
    let Ok(mut e) = b.engine.0.try_borrow_mut() else { return };
    let active = b.tabs.active;
    let visible = b.settings.engine_reader
        && e.article.is_some()
        && e.tab.is_some()
        && e.tab == active
        && active.map(|id| actions::in_reader(b, id)).unwrap_or(false);
    if !visible {
        if let Some(s) = &e.scroll {
            s.setHidden(true);
        }
        return;
    }
    let Some(web) = active.and_then(|id| b.webs.get(&id)).map(|w| w.web.clone()) else { return };
    let mtm = b.mtm;
    if e.scroll.is_none() {
        let scroll = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), rect(0.0, 0.0, 100.0, 100.0));
        scroll.setHasVerticalScroller(true);
        scroll.setAutohidesScrollers(true);
        scroll.setScrollerStyle(NSScrollerStyle::Overlay);
        let view = EngineView::new(mtm);
        scroll.setDocumentView(Some(&view));
        e.scroll = Some(scroll);
        e.view = Some(view);
    }
    let (Some(scroll), Some(view)) = (e.scroll.clone(), e.view.clone()) else { return };
    // Just above this tab's web view, below anything Volant puts over the content.
    b.ui.content.addSubview_positioned_relativeTo(&scroll, NSWindowOrderingMode::Above, Some(&web));
    let cb = b.ui.content.bounds();
    scroll.setFrame(rect(0.0, TOOLBAR_H, cb.size.width, (cb.size.height - TOOLBAR_H).max(0.0)));
    scroll.setHidden(false);
    let dark = theme::is_dark(mtm);
    let size = b.settings.reader_size;
    let serif = b.settings.reader_font != "sans";
    let width = cb.size.width.floor() as i64;
    let want = (width, dark, size, serif);
    if e.laid != Some(want) {
        let Some(a) = e.article.as_ref() else { return };
        let o = volant_engine::reader::Options { dark, size: size as f32, serif };
        let (html, css) = volant_engine::reader::document(a, &e.host, &o);
        // The Reader page's own top margin already sits above the toolbar line.
        let css = css + "article { margin-top: 0 }";
        let started = std::time::Instant::now();
        let page = volant_engine::render(&html, &css, width as f32, &MacFonts);
        eprintln!("engine: laid out {} items in {:.1} ms", page.items.len(), started.elapsed().as_secs_f64() * 1000.0);
        let bg = ns_color(page.background);
        scroll.setBackgroundColor(&bg);
        scroll.setDrawsBackground(true);
        let height = (page.height as f64).max(scroll.contentSize().height);
        *view.ivars().page.borrow_mut() = Some(Rc::new(page));
        view.setFrameSize(NSSize::new(width as f64, height));
        view.setNeedsDisplay(true);
        if let Some(w) = view.window() {
            w.invalidateCursorRectsForView(&view);
        }
        e.laid = Some(want);
    }
}
