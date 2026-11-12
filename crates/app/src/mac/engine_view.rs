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
    NSAttributedStringNSStringDrawing, NSBezierPath, NSColor, NSCompositingOperation, NSCursor, NSEvent, NSFont, NSGraphicsContext, NSImage, NSFontAttributeName, NSFontDescriptorSymbolicTraits,
    NSFontDescriptorSystemDesignSerif, NSFontWeightBold, NSFontWeightMedium, NSFontWeightRegular, NSFontWeightSemibold, NSForegroundColorAttributeName,
    NSScrollView, NSScrollerStyle, NSStrikethroughStyleAttributeName, NSUnderlineStyleAttributeName, NSView, NSWindowOrderingMode,
};
use objc2_foundation::{NSAttributedString, NSData, NSDictionary, NSNumber, NSPoint, NSRect, NSSize, NSString};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use volant_core::tabs::TabId;
use volant_engine::{Family, Font, Fonts, Images, Item, Page};

/// The most pictures one article loads.
const MAX_IMAGES: usize = 60;

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
    /// The article's pictures: loading, loaded (with their size) or failed.
    images: HashMap<String, ImageState>,
    relayout_soon: bool,
    /// Find on page: the query and where it was found.
    find_query: String,
    find_hits: Vec<NSRect>,
    find_current: usize,
}

#[derive(Clone, Copy)]
enum ImageState {
    Loading,
    Ready(f32, f32),
    Failed,
}

/// Image sizes for the engine; asks for the ones it hasn't seen yet.
struct MacImages<'a> {
    known: &'a HashMap<String, ImageState>,
    wanted: RefCell<Vec<String>>,
}

impl Images for MacImages<'_> {
    fn size(&self, src: &str) -> Option<(f32, f32)> {
        match self.known.get(src) {
            Some(ImageState::Ready(w, h)) => Some((*w, *h)),
            Some(_) => None,
            None => {
                self.wanted.borrow_mut().push(src.to_string());
                None
            }
        }
    }
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
    images: RefCell<HashMap<String, Retained<NSImage>>>,
    /// Find-on-page matches, and which one is current.
    hits: RefCell<(Vec<NSRect>, usize)>,
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
                paint(page, dirty, &self.ivars().images.borrow(), &self.ivars().hits.borrow());
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
        let this = Self::alloc(mtm).set_ivars(EngineIvars { page: RefCell::new(None), images: RefCell::new(HashMap::new()), hits: RefCell::new((Vec::new(), 0)) });
        unsafe { msg_send![super(this), initWithFrame: rect(0.0, 0.0, 100.0, 100.0)] }
    }
}

fn intersects(r: NSRect, x: f64, y: f64, w: f64, h: f64) -> bool {
    x < r.origin.x + r.size.width && x + w > r.origin.x && y < r.origin.y + r.size.height && y + h > r.origin.y
}

fn rounded(r: NSRect, radius: f64) -> Retained<NSBezierPath> {
    let radius = radius.min(r.size.width / 2.0).min(r.size.height / 2.0).max(0.0);
    NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(r, radius, radius)
}

/// Draws the part of the page inside `dirty`.
fn paint(page: &Page, dirty: NSRect, images: &HashMap<String, Retained<NSImage>>, hits: &(Vec<NSRect>, usize)) {
    ns_color(page.background).setFill();
    NSBezierPath::fillRect(dirty);
    for item in &page.items {
        match item {
            Item::Rect { x, y, w, h, color, radius } => {
                if intersects(dirty, *x as f64, *y as f64, *w as f64, *h as f64) {
                    ns_color(*color).setFill();
                    let r = rect(*x as f64, *y as f64, *w as f64, *h as f64);
                    if *radius > 0.0 {
                        rounded(r, *radius as f64).fill();
                    } else {
                        NSBezierPath::fillRect(r);
                    }
                }
            }
            Item::Image { x, y, w, h, src, radius } => {
                if !intersects(dirty, *x as f64, *y as f64, *w as f64, *h as f64) {
                    continue;
                }
                let r = rect(*x as f64, *y as f64, *w as f64, *h as f64);
                NSGraphicsContext::saveGraphicsState_class();
                rounded(r, *radius as f64).addClip();
                match images.get(src) {
                    Some(img) => unsafe {
                        img.drawInRect_fromRect_operation_fraction_respectFlipped_hints(r, NSRect::ZERO, NSCompositingOperation::SourceOver, 1.0, true, None);
                    },
                    // Still on its way: a placeholder where it will go.
                    None => {
                        NSColor::colorWithSRGBRed_green_blue_alpha(0.5, 0.5, 0.52, 0.15).setFill();
                        NSBezierPath::fillRect(r);
                    }
                }
                NSGraphicsContext::restoreGraphicsState_class();
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
    // Find-on-page matches: yellow, the current one orange.
    for (i, r) in hits.0.iter().enumerate() {
        if intersects(dirty, r.origin.x, r.origin.y, r.size.width, r.size.height) {
            let (red, green, blue, a) = if i == hits.1 { (1.0, 0.6, 0.0, 0.55) } else { (1.0, 0.85, 0.0, 0.35) };
            NSColor::colorWithSRGBRed_green_blue_alpha(red, green, blue, a).setFill();
            rounded(*r, 3.0).fill();
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
        e.images.clear();
        e.find_hits.clear();
        if let Some(v) = &e.view {
            v.ivars().images.borrow_mut().clear();
            *v.ivars().hits.borrow_mut() = (Vec::new(), 0);
        }
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
        let images = MacImages { known: &e.images, wanted: RefCell::new(Vec::new()) };
        let page = volant_engine::render(&html, &css, width as f32, &MacFonts, &images);
        let wanted = images.wanted.into_inner();
        let pictures = page.items.iter().filter(|i| matches!(i, Item::Image { .. })).count();
        eprintln!("engine: laid out {} items ({pictures} images) in {:.1} ms", page.items.len(), started.elapsed().as_secs_f64() * 1000.0);
        for src in wanted {
            if e.images.len() < MAX_IMAGES && !e.images.contains_key(&src) && (src.starts_with("https://") || src.starts_with("http://")) {
                e.images.insert(src.clone(), ImageState::Loading);
                load_image(src);
            }
        }
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

/// Downloads a picture (Volant's own HTTP), decodes it, and lays the article out again.
fn load_image(src: String) {
    std::thread::spawn(move || {
        let bytes = super::net::fetch(&src, 24 << 20).ok();
        super::on_main(move || with(move |b| image_arrived(b, &src, bytes)));
    });
}

fn image_arrived(b: &Browser, src: &str, bytes: Option<Vec<u8>>) {
    let Ok(mut e) = b.engine.0.try_borrow_mut() else { return };
    if !e.images.contains_key(src) {
        return; // a different article by now
    }
    let img = bytes.and_then(|d| NSImage::initWithData(NSImage::alloc(), &NSData::with_bytes(&d)));
    let state = match &img {
        Some(i) if i.size().width > 0.0 && i.size().height > 0.0 => ImageState::Ready(i.size().width as f32, i.size().height as f32),
        _ => ImageState::Failed,
    };
    e.images.insert(src.to_string(), state);
    if let (Some(v), Some(i), ImageState::Ready(..)) = (&e.view, img, state) {
        v.ivars().images.borrow_mut().insert(src.to_string(), i);
    }
    // Pictures tend to arrive together: one new layout for a burst of them.
    e.laid = None;
    if !e.relayout_soon {
        e.relayout_soon = true;
        super::after(0.12, || {
            with(|b| {
                if let Ok(mut e) = b.engine.0.try_borrow_mut() {
                    e.relayout_soon = false;
                }
                sync(b);
            })
        });
    }
}

/// Whether the engine's Reader is what's on screen (so Find searches it).
pub fn is_active(b: &Browser) -> bool {
    b.engine.0.try_borrow().map(|e| e.scroll.as_ref().map(|s| !s.isHidden()).unwrap_or(false)).unwrap_or(false)
}

/// Every place `query` appears in the laid-out text (case-insensitive), as rectangles.
fn matches(page: &Page, query: &str) -> Vec<NSRect> {
    let q: Vec<char> = query.chars().flat_map(char::to_lowercase).collect();
    let mut out = Vec::new();
    if q.is_empty() {
        return out;
    }
    for item in &page.items {
        let Item::Text { x, y, text, font, .. } = item else { continue };
        let chars: Vec<char> = text.chars().collect();
        let lower: Vec<char> = chars.iter().map(|c| c.to_lowercase().next().unwrap_or(*c)).collect();
        if lower.len() < q.len() {
            continue;
        }
        let f = ns_font(font);
        for i in 0..=lower.len() - q.len() {
            if lower[i..i + q.len()] == q[..] {
                let before: String = chars[..i].iter().collect();
                let hit: String = chars[i..i + q.len()].iter().collect();
                let x0 = *x as f64 + MacFonts.width(&before, font) as f64;
                let w = MacFonts.width(&hit, font) as f64;
                let top = *y as f64 - f.ascender();
                out.push(rect(x0 - 1.0, top - 1.0, w + 2.0, f.ascender() - f.descender() + 2.0));
            }
        }
    }
    out
}

/// Find on page in the engine's Reader: returns (current match, number of matches).
pub fn find(b: &Browser, query: &str, backwards: bool, fresh: bool) -> (usize, usize) {
    let Ok(mut e) = b.engine.0.try_borrow_mut() else { return (0, 0) };
    let Some(view) = e.view.clone() else { return (0, 0) };
    if fresh || e.find_query != query {
        let page = view.ivars().page.borrow().clone();
        e.find_hits = page.map(|p| matches(&p, query)).unwrap_or_default();
        e.find_query = query.to_string();
        e.find_current = 0;
    } else if !e.find_hits.is_empty() {
        let n = e.find_hits.len();
        e.find_current = if backwards { (e.find_current + n - 1) % n } else { (e.find_current + 1) % n };
    }
    *view.ivars().hits.borrow_mut() = (e.find_hits.clone(), e.find_current);
    view.setNeedsDisplay(true);
    if let Some(r) = e.find_hits.get(e.find_current) {
        // Bring it into view with some room around it.
        view.scrollRectToVisible(rect(r.origin.x, r.origin.y - 80.0, r.size.width, r.size.height + 160.0));
    }
    let total = e.find_hits.len();
    (if total == 0 { 0 } else { e.find_current + 1 }, total)
}

/// Clears Find's highlights.
pub fn clear_find(b: &Browser) {
    if let Ok(mut e) = b.engine.0.try_borrow_mut() {
        e.find_hits.clear();
        e.find_query.clear();
        if let Some(v) = &e.view {
            *v.ivars().hits.borrow_mut() = (Vec::new(), 0);
            v.setNeedsDisplay(true);
        }
    }
}
