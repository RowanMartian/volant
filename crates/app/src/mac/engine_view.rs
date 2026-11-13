//! Volant's own engine on screen (experimental, off by default): with
//! Settings → Experimental → "Draw Reader with Volant's own engine", Reader
//! view is laid out by volant-engine and drawn natively by this view, instead
//! of by WebKit.
//!
//! View → Simple Page goes further: the page is loaded, scripted, laid out and
//! drawn by Volant alone. A session thread fetches the HTML, scripts and style
//! sheets with Volant's own HTTP and runs the page's JavaScript with
//! volant-js; this view draws the snapshots it sends, and sends back clicks
//! and typing. While Simple Page is on, the tab belongs to the engine: links,
//! forms (GET and POST), the address bar, Back, Forward and Reload all go to
//! it, with Volant's own history list and cookie jar, and WebKit is left
//! empty (nothing is loaded or run twice) until Simple Page is turned off.
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
    NSMenu, NSMenuItem, NSPasteboard, NSPasteboardTypeString, NSScrollView, NSScrollerStyle, NSStrikethroughStyleAttributeName, NSUnderlineStyleAttributeName, NSView, NSWindowOrderingMode,
};
use objc2_foundation::{NSAttributedString, NSData, NSDictionary, NSNumber, NSPoint, NSRect, NSSize, NSString};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use volant_core::tabs::TabId;
use volant_engine::select::{self, Pos};
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
    /// Simple Page: a whole page drawn by the engine (`toggle_page`).
    page: Option<SimplePage>,
    /// The tab Simple Page is on for (it stays on across navigations).
    sticky: Option<TabId>,
    /// Other tabs in Simple Page, waiting in the background: their history,
    /// where they are, and for the most recent few, the page itself (frozen,
    /// so it comes back at once); the others reload when they come to the front.
    parked: HashMap<TabId, Parked>,
    /// Numbers sessions (each page load gets a new one).
    counter: u64,
    /// A scroll position to return to once the page is laid out (a tab brought back).
    pending_scroll: Option<f64>,
    /// The page's script thread.
    session: Option<Session>,
    /// Simple Page's own history for the tab: addresses, and where we are.
    trail: Vec<String>,
    at: usize,
    /// A page is being fetched (for the reload/stop button).
    loading: bool,
    /// A `#fragment` to scroll to once the page is laid out.
    pending_anchor: Option<String>,
    serial: u64,
    /// The page has a text field focused (typing goes to it).
    focused: bool,
    /// What the current layout was made for: (width, dark, size, serif, simple page).
    laid: Option<(i64, bool, u32, bool, bool)>,
    /// The article's pictures: loading, loaded (with their size) or failed.
    images: HashMap<String, ImageState>,
    relayout_soon: bool,
    /// Find on page: the query and where it was found.
    find_query: String,
    find_hits: Vec<NSRect>,
    find_current: usize,
}

/// A page loaded and run by Volant's own engine: its latest snapshot and linked style sheets.
/// A Simple Page tab in the background.
#[derive(Default)]
struct Parked {
    trail: Vec<String>,
    at: usize,
    live: Option<Live>,
}

/// A background tab's page, kept running (frozen) so switching back is instant.
struct Live {
    session: Session,
    serial: u64,
    page: Option<SimplePage>,
    scroll_y: f64,
    /// When it was parked (the oldest is let go first).
    parked_at: u64,
}

/// How many background Simple Page tabs keep their page alive.
const LIVE_PARKED: usize = 2;

struct SimplePage {
    /// The tab's address it belongs to.
    url: String,
    snap: Snapshot,
    sheets: Vec<String>,
}

use volant_engine::script::{self, Navigation, Snapshot};

/// What the view sends the session thread.
enum Cmd {
    Click(u32),
    /// The tab went to the background (true) or came back (false).
    Freeze(bool),
    /// The pointer is over this node (or off the page).
    Hover(Option<u32>),
    /// A `<select>`'s menu: the option picked (by index among its options).
    Choose(u32, usize),
    Type(String),
    Key(&'static str),
    Width(f32, bool),
    /// Runs JavaScript in the page (for tests).
    Eval(String),
}

/// A page's JavaScript world, running on its own thread.
struct Session {
    tx: std::sync::mpsc::Sender<Cmd>,
    url: String,
}

/// Page requests (scripts, style sheets, fetch) over Volant's own HTTP.
struct NetHost;

/// What Volant's engine says it is to web servers (so they send their ordinary pages).
const PAGE_UA: &str = script::USER_AGENT;

impl script::Host for NetHost {
    fn request(&self, req: &script::Request) -> Option<script::Reply> {
        if !(req.url.starts_with("https://") || req.url.starts_with("http://")) {
            return None;
        }
        let mut opts = super::net::options();
        opts.user_agent = PAGE_UA.into();
        opts.limit = 16 << 20;
        opts.deadline = std::time::Duration::from_secs(20);
        opts.method = req.method.to_string();
        opts.body = req.body.map(<[u8]>::to_vec);
        opts.headers = req.headers.to_vec();
        opts.cookie_context = if req.navigation {
            volant_core::cookies::Context::navigation_from(req.initiator, req.method)
        } else {
            match req.initiator {
                Some(page) => volant_core::cookies::Context::subresource(page, req.method),
                None => volant_core::cookies::Context::first_party(),
            }
        };
        let r = if req.credentials { super::net::send(req.url, &opts) } else { super::net::send_anonymous(req.url, &opts) }.ok()?;
        let content_type = r.header("content-type").unwrap_or("").to_string();
        Some(script::Reply { status: r.status, content_type, body: r.body, url: r.url, headers: r.headers })
    }
    fn cookie(&self, url: &str) -> Option<String> {
        let u = volant_core::http::Url::parse(url)?;
        let jar = super::net::cookie_jar()?;
        Some(jar.lock().map(|mut j| j.header(&u, volant_core::cookies::now(), true)).unwrap_or_default())
    }
    fn background(&self) -> Option<script::Fetch> {
        Some(std::sync::Arc::new(|url: &str, page: &str| script::Host::get_for(&NetHost, url, page)))
    }
    fn set_cookie(&self, url: &str, cookie: &str) -> bool {
        let (Some(u), Some(jar)) = (volant_core::http::Url::parse(url), super::net::cookie_jar()) else { return false };
        if let Ok(mut j) = jar.lock() {
            j.set(&u, cookie, volant_core::cookies::now(), true);
        }
        true
    }
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
        if src.starts_with("canvas:") {
            return None; // drawn by the page, sized by its attributes
        }
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
    /// Pages' web fonts, by lower-case family: (weight range, italic, the font).
    static WEB_FONTS: RefCell<HashMap<String, Vec<(u16, u16, bool, Retained<objc2_app_kit::NSFontDescriptor>)>>> = RefCell::new(HashMap::new());
}

#[link(name = "CoreText", kind = "framework")]
extern "C" {
    /// Takes a CFData (TrueType, OpenType, WOFF 2) and returns a +1 CTFontDescriptor, or null.
    fn CTFontManagerCreateFontDescriptorFromData(data: *const std::ffi::c_void) -> *mut std::ffi::c_void;
}

/// The most web font files one page loads.
const MAX_FACES: usize = 16;

/// A downloaded web font, ready to register on the main thread.
struct FaceData {
    family: String,
    weight: (u16, u16),
    italic: bool,
    bytes: Vec<u8>,
}

/// Web fonts arrived: they're registered and the page is laid out again with them.
fn fonts_arrived(b: &mut Browser, faces: Vec<FaceData>) {
    let mut added = 0;
    for f in faces {
        let data = NSData::with_bytes(&f.bytes);
        let raw = unsafe { CTFontManagerCreateFontDescriptorFromData(Retained::as_ptr(&data) as *const std::ffi::c_void) };
        // CTFontDescriptor is toll-free bridged to NSFontDescriptor.
        let Some(desc) = (unsafe { Retained::from_raw(raw as *mut objc2_app_kit::NSFontDescriptor) }) else {
            eprintln!("engine: couldn't use the web font {} ({} KB)", f.family, f.bytes.len() / 1024);
            continue;
        };
        WEB_FONTS.with(|w| w.borrow_mut().entry(f.family.clone()).or_default().push((f.weight.0, f.weight.1, f.italic, desc)));
        volant_engine::fonts::declare_face(&f.family);
        added += 1;
    }
    if added == 0 {
        return;
    }
    NS_FONTS.with(|c| c.borrow_mut().clear());
    if let Ok(mut e) = b.engine.0.try_borrow_mut() {
        e.laid = None;
    }
    sync(b);
}

/// The registered web font closest to `f` (its weight within the face's range, or nearest; its slant).
fn web_font(f: &Font) -> Option<Retained<NSFont>> {
    let Family::Face(i) = f.family else { return None };
    let (name, _) = volant_engine::fonts::face_info(i)?;
    WEB_FONTS.with(|w| {
        let w = w.borrow();
        let faces = w.get(&name)?;
        let distance = |(lo, hi, italic, _): &&(u16, u16, bool, Retained<objc2_app_kit::NSFontDescriptor>)| {
            let off = if f.weight < *lo { lo - f.weight } else { f.weight.saturating_sub(*hi) };
            off as u32 + if *italic != f.italic { 10_000 } else { 0 }
        };
        let best = faces.iter().min_by_key(distance)?;
        NSFont::fontWithDescriptor_size(&best.3, f.size as f64)
    })
}

fn ns_font(f: &Font) -> Retained<NSFont> {
    NS_FONTS.with(|cache| {
        if let Some(font) = cache.borrow().get(&f.key()) {
            return font.clone();
        }
        if let Family::Face(_) = f.family {
            // The page's own font, or (until it loads) its fallback.
            let font = web_font(f).unwrap_or_else(|| ns_font(&Font { family: f.family.generic(), ..*f }));
            cache.borrow_mut().insert(f.key(), font.clone());
            return font;
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
    /// The selection: where it started and where it ends now.
    sel: Cell<Option<(Pos, Pos)>>,
    /// Its highlight rectangles.
    sel_areas: RefCell<Vec<NSRect>>,
    /// Whether the mouse moved since it went down (a drag selects; a click follows links).
    dragged: Cell<bool>,
    /// The address relative links and images resolve against.
    base: RefCell<String>,
    /// Simple Page: maps the laid-out document's nodes to the page's own (for clicks).
    map: RefCell<Vec<u32>>,
    /// Clicks and typing go to a live page's scripts.
    live: Cell<bool>,
    focused: Cell<bool>,
    /// What each `<canvas>` drew, by its box's `src`.
    canvases: RefCell<HashMap<String, std::sync::Arc<volant_engine::canvas::Canvas>>>,
    /// The page node under the pointer, last sent to the page.
    hovered: Cell<Option<u32>>,
    /// The `<select>` whose menu is open, and how its menu lines map to options.
    menu_for: Cell<Option<u32>>,
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
                let view = self.visibleRect();
                paint(page, dirty, (view.origin.y, view.size.height), &self.ivars().images.borrow(), &self.ivars().canvases.borrow(), &self.ivars().hits.borrow(), &self.ivars().sel_areas.borrow());
            }
        }

        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            true
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, e: &NSEvent) {
            if let Some(w) = self.window() {
                w.makeFirstResponder(Some(self));
            }
            self.ivars().dragged.set(false);
            let Some(at) = self.pos_for(e) else { return };
            let shift = e.modifierFlags().contains(objc2_app_kit::NSEventModifierFlags::Shift);
            let page = self.ivars().page.borrow().clone();
            let Some(page) = page else { return };
            let sel = match (e.clickCount(), self.ivars().sel.get()) {
                (_, Some((anchor, _))) if shift => (anchor, at),
                (2, _) => select::word(&page, at),
                (n, _) if n >= 3 => select::paragraph(&page, at),
                _ => (at, at),
            };
            self.set_selection(Some(sel));
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, e: &NSEvent) {
            self.autoscroll(e);
            let Some(at) = self.pos_for(e) else { return };
            if let Some((anchor, _)) = self.ivars().sel.get() {
                self.ivars().dragged.set(true);
                self.set_selection(Some((anchor, at)));
            }
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, e: &NSEvent) {
            // A plain click (no drag, nothing selected) follows a link.
            let selected = self.ivars().sel.get().map(|(a, b)| a != b).unwrap_or(false);
            if self.ivars().dragged.get() || selected || e.clickCount() != 1 {
                return;
            }
            let p = self.convertPoint_fromView(e.locationInWindow(), None);
            if self.ivars().live.get() {
                // The page's own scripts decide what a click does (links included).
                let node = self.ivars().page.borrow().as_ref().and_then(|page| self.hit(page, p));
                let target = node.and_then(|n| self.ivars().map.borrow().get(n as usize).copied());
                if let Some(t) = target {
                    with(move |b| send(b, Cmd::Click(t)));
                }
                return;
            }
            let href = self.ivars().page.borrow().as_ref().and_then(|page| {
                page.links
                    .iter()
                    .find(|l| p.x >= l.x as f64 && p.x <= (l.x + l.w) as f64 && p.y >= l.y as f64 && p.y <= (l.y + l.h) as f64)
                    .map(|l| l.href.clone())
            });
            let base = self.ivars().base.borrow().clone();
            let target = href.and_then(|h| resolve(&base, &h));
            if let Some(url) = target.filter(|h| h.starts_with("https://") || h.starts_with("http://")) {
                with(move |b| actions::navigate(b, &url));
            }
        }

        #[unsafe(method(copy:))]
        fn copy(&self, _sender: Option<&AnyObject>) {
            let page = self.ivars().page.borrow().clone();
            if let (Some(page), Some((a, b))) = (page, self.ivars().sel.get()) {
                let text = select::text(&page, a, b);
                if !text.is_empty() {
                    let pb = NSPasteboard::generalPasteboard();
                    pb.clearContents();
                    pb.setString_forType(&ns(&text), unsafe { NSPasteboardTypeString });
                }
            }
        }

        #[unsafe(method(selectAll:))]
        fn select_all(&self, _sender: Option<&AnyObject>) {
            let all = self.ivars().page.borrow().as_ref().and_then(|p| select::all(p));
            self.set_selection(all);
        }

        #[unsafe(method_id(menuForEvent:))]
        fn menu_for_event(&self, _e: &NSEvent) -> Option<Retained<NSMenu>> {
            let mtm = MainThreadMarker::from(self);
            if let Some(w) = self.window() {
                w.makeFirstResponder(Some(self));
            }
            let menu = NSMenu::new(mtm);
            let copy = unsafe { NSMenuItem::initWithTitle_action_keyEquivalent(NSMenuItem::alloc(mtm), &ns("Copy"), Some(objc2::sel!(copy:)), &ns("")) };
            let all = unsafe { NSMenuItem::initWithTitle_action_keyEquivalent(NSMenuItem::alloc(mtm), &ns("Select All"), Some(objc2::sel!(selectAll:)), &ns("")) };
            menu.addItem(&copy);
            menu.addItem(&all);
            Some(menu)
        }

        #[unsafe(method(validateMenuItem:))]
        fn validate_menu_item(&self, item: &NSMenuItem) -> objc2::runtime::Bool {
            // Copy only when something is selected.
            if item.action() == Some(objc2::sel!(chooseOption:)) {
                return objc2::runtime::Bool::new(item.tag() >= 0);
            }
            let ok = item.action() != Some(objc2::sel!(copy:)) || self.ivars().sel.get().map(|(a, b)| a != b).unwrap_or(false);
            objc2::runtime::Bool::new(ok)
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, e: &NSEvent) {
            let command = e.modifierFlags().contains(objc2_app_kit::NSEventModifierFlags::Command);
            if self.ivars().live.get() && self.ivars().focused.get() && !command {
                // Typing goes into the page's focused field.
                let chars = e.characters().map(|s| s.to_string()).unwrap_or_default();
                let cmd = match chars.as_str() {
                    "\u{7f}" | "\u{8}" => Some(Cmd::Key("Backspace")),
                    "\r" | "\u{3}" => Some(Cmd::Key("Enter")),
                    "\t" => Some(Cmd::Key("Tab")),
                    "\u{1b}" => Some(Cmd::Key("Escape")),
                    "\u{f702}" => Some(Cmd::Key("ArrowLeft")),
                    "\u{f703}" => Some(Cmd::Key("ArrowRight")),
                    "\u{f700}" => Some(Cmd::Key("ArrowUp")),
                    "\u{f701}" => Some(Cmd::Key("ArrowDown")),
                    "\u{f729}" => Some(Cmd::Key("Home")),
                    "\u{f72b}" => Some(Cmd::Key("End")),
                    "\u{f728}" => Some(Cmd::Key("Delete")),
                    c if !c.is_empty() && c.chars().all(|ch| !ch.is_control() && !('\u{f700}'..='\u{f8ff}').contains(&ch)) => Some(Cmd::Type(c.to_string())),
                    _ => None,
                };
                if let Some(cmd) = cmd {
                    with(move |b| send(b, cmd));
                    return;
                }
            }
            if !self.scroll_key(e) {
                let _: () = unsafe { msg_send![super(self), keyDown: e] };
            }
        }

        #[unsafe(method(mouseMoved:))]
        fn mouse_moved(&self, e: &NSEvent) {
            if !self.ivars().live.get() {
                return;
            }
            let p = self.convertPoint_fromView(e.locationInWindow(), None);
            let node = self.ivars().page.borrow().as_ref().and_then(|page| self.hit(page, p));
            let target = node.and_then(|n| self.ivars().map.borrow().get(n as usize).copied());
            if target != self.ivars().hovered.get() {
                self.ivars().hovered.set(target);
                with(move |b| send(b, Cmd::Hover(target)));
            }
        }

        #[unsafe(method(mouseExited:))]
        fn mouse_exited(&self, _e: &NSEvent) {
            if self.ivars().live.get() && self.ivars().hovered.get().is_some() {
                self.ivars().hovered.set(None);
                with(move |b| send(b, Cmd::Hover(None)));
            }
        }

        /// A line of a `<select>`'s menu was picked; its tag is the option's index.
        #[unsafe(method(chooseOption:))]
        fn choose_option(&self, item: &NSMenuItem) {
            if let Some(select) = self.ivars().menu_for.take() {
                let index = item.tag().max(0) as usize;
                with(move |b| send(b, Cmd::Choose(select, index)));
            }
        }

        #[unsafe(method(resetCursorRects))]
        fn reset_cursor_rects(&self) {
            // A text cursor over the page, a hand over links.
            self.addCursorRect_cursor(self.visibleRect(), &NSCursor::IBeamCursor());
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
        let this = Self::alloc(mtm).set_ivars(EngineIvars {
            page: RefCell::new(None),
            images: RefCell::new(HashMap::new()),
            hits: RefCell::new((Vec::new(), 0)),
            sel: Cell::new(None),
            sel_areas: RefCell::new(Vec::new()),
            dragged: Cell::new(false),
            base: RefCell::new(String::new()),
            map: RefCell::new(Vec::new()),
            live: Cell::new(false),
            focused: Cell::new(false),
            canvases: RefCell::new(HashMap::new()),
            hovered: Cell::new(None),
            menu_for: Cell::new(None),
        });
        let this: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: rect(0.0, 0.0, 100.0, 100.0)] };
        // Pointer movement, for :hover and mouseover.
        let opts = objc2_app_kit::NSTrackingAreaOptions::MouseMoved | objc2_app_kit::NSTrackingAreaOptions::MouseEnteredAndExited | objc2_app_kit::NSTrackingAreaOptions::ActiveInKeyWindow | objc2_app_kit::NSTrackingAreaOptions::InVisibleRect;
        let area = unsafe { objc2_app_kit::NSTrackingArea::initWithRect_options_owner_userInfo(objc2_app_kit::NSTrackingArea::alloc(), rect(0.0, 0.0, 0.0, 0.0), opts, Some(&this as &AnyObject), None) };
        this.addTrackingArea(&area);
        this
    }
}

impl EngineView {
    /// The text position under the mouse.
    fn pos_for(&self, e: &NSEvent) -> Option<Pos> {
        let p = self.convertPoint_fromView(e.locationInWindow(), None);
        let page = self.ivars().page.borrow().clone()?;
        select::hit(&page, &MacFonts, p.x as f32, p.y as f32)
    }

    fn set_selection(&self, sel: Option<(Pos, Pos)>) {
        self.ivars().sel.set(sel);
        let page = self.ivars().page.borrow().clone();
        let areas = match (page, sel) {
            (Some(page), Some((a, b))) if a != b => {
                select::areas(&page, &MacFonts, a, b).into_iter().map(|r| rect(r.x as f64, r.y as f64, r.w as f64, r.h as f64)).collect()
            }
            _ => Vec::new(),
        };
        *self.ivars().sel_areas.borrow_mut() = areas;
        self.setNeedsDisplay(true);
    }

    /// The node under a point, with fixed and sticky boxes where scrolling has put them.
    fn hit(&self, page: &Page, p: NSPoint) -> Option<volant_core::html::NodeId> {
        let view = self.visibleRect();
        page.hit_scrolled(p.x as f32, p.y as f32, view.origin.y as f32, view.size.height as f32)
    }

    /// Space, arrows, Page Up/Down, Home and End scroll the article.
    fn scroll_key(&self, e: &NSEvent) -> bool {
        let Some(scroll) = self.enclosingScrollView() else { return false };
        let key = e.charactersIgnoringModifiers().map(|s| s.to_string()).unwrap_or_default();
        let shift = e.modifierFlags().contains(objc2_app_kit::NSEventModifierFlags::Shift);
        let clip = scroll.contentView();
        let page_h = clip.bounds().size.height - 40.0;
        let max_y = (self.frame().size.height - clip.bounds().size.height).max(0.0);
        let y = clip.bounds().origin.y;
        let to = match key.chars().next() {
            Some(' ') => y + if shift { -page_h } else { page_h },
            Some('\u{f72d}') => y + page_h,     // Page Down
            Some('\u{f72c}') => y - page_h,     // Page Up
            Some('\u{f701}') => y + 40.0,       // Down
            Some('\u{f700}') => y - 40.0,       // Up
            Some('\u{f729}') => 0.0,            // Home
            Some('\u{f72b}') => max_y,          // End
            _ => return false,
        };
        clip.scrollToPoint(NSPoint::new(0.0, to.clamp(0.0, max_y)));
        scroll.reflectScrolledClipView(&clip);
        true
    }
}

fn intersects(r: NSRect, x: f64, y: f64, w: f64, h: f64) -> bool {
    x < r.origin.x + r.size.width && x + w > r.origin.x && y < r.origin.y + r.size.height && y + h > r.origin.y
}

fn rounded(r: NSRect, radius: f64) -> Retained<NSBezierPath> {
    let radius = radius.min(r.size.width / 2.0).min(r.size.height / 2.0).max(0.0);
    NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(r, radius, radius)
}

/// Draws the part of the page inside `dirty`; `view` is the visible part's top and
/// height, where fixed and sticky boxes are drawn.
fn paint(page: &Page, dirty: NSRect, view: (f64, f64), images: &HashMap<String, Retained<NSImage>>, canvases: &HashMap<String, std::sync::Arc<volant_engine::canvas::Canvas>>, hits: &(Vec<NSRect>, usize), selection: &[NSRect]) {
    ns_color(page.background).setFill();
    NSBezierPath::fillRect(dirty);
    let mut skip = page.layers.iter().map(|l| l.items).collect::<Vec<_>>();
    skip.sort();
    paint_items(page, 0..page.items.len(), &skip, dirty, images, canvases);
    // Fixed and sticky boxes, on top, moved to where scrolling puts them.
    for l in &page.layers {
        let dy = page.layer_offset(l, view.0 as f32, view.1 as f32) as f64;
        NSGraphicsContext::saveGraphicsState_class();
        let t = objc2_foundation::NSAffineTransform::transform();
        t.translateXBy_yBy(0.0, dy);
        objc2_app_kit::NSAffineTransformNSAppKitAdditions::concat(&*t);
        let shifted = NSRect::new(NSPoint::new(dirty.origin.x, dirty.origin.y - dy), dirty.size);
        paint_items(page, l.items.0..l.items.1, &[], shifted, images, canvases);
        NSGraphicsContext::restoreGraphicsState_class();
    }
    paint_marks(dirty, hits, selection);
}

/// Draws `range` of the page's items, leaving out those in `skip` (sorted ranges).
fn paint_items(page: &Page, range: std::ops::Range<usize>, skip: &[(usize, usize)], dirty: NSRect, images: &HashMap<String, Retained<NSImage>>, canvases: &HashMap<String, std::sync::Arc<volant_engine::canvas::Canvas>>) {
    let mut i = range.start;
    let mut next = 0;
    while i < range.end {
        while next < skip.len() && skip[next].1 <= i {
            next += 1;
        }
        if next < skip.len() && skip[next].0 <= i {
            i = skip[next].1;
            continue;
        }
        let item = &page.items[i];
        i += 1;
        match item {
            Item::Clip { x, y, w, h, radius } => {
                // overflow: what follows is clipped until the matching Unclip.
                NSGraphicsContext::saveGraphicsState_class();
                rounded(rect(*x as f64, *y as f64, *w as f64, *h as f64), *radius as f64).addClip();
            }
            Item::Unclip => NSGraphicsContext::restoreGraphicsState_class(),
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
                if src.starts_with("canvas:") {
                    if let Some(c) = canvases.get(src) {
                        paint_canvas(c, r, page.background, images);
                    }
                    NSGraphicsContext::restoreGraphicsState_class();
                    continue;
                }
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
            Item::Text { x, y, text, font, color, underline, strike, .. } => {
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

/// The selection and find-on-page matches, over the page.
fn paint_marks(dirty: NSRect, hits: &(Vec<NSRect>, usize), selection: &[NSRect]) {
    // The selection, in the system's highlight colour.
    if !selection.is_empty() {
        NSColor::selectedTextBackgroundColor().colorWithAlphaComponent(0.55).setFill();
        for r in selection {
            if intersects(dirty, r.origin.x, r.origin.y, r.size.width, r.size.height) {
                NSBezierPath::fillRect(*r);
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

/// Draws a `<canvas>`'s display list into its box, scaled from canvas pixels.
fn paint_canvas(c: &volant_engine::canvas::Canvas, r: NSRect, background: volant_engine::Color, images: &HashMap<String, Retained<NSImage>>) {
    use volant_engine::canvas::{Align, Op};
    let (sx, sy) = (r.size.width / c.width.max(1.0) as f64, r.size.height / c.height.max(1.0) as f64);
    let at = |(x, y): (f32, f32)| NSPoint::new(r.origin.x + x as f64 * sx, r.origin.y + y as f64 * sy);
    let polygon = |pts: &[(f32, f32)], closed: bool, path: &NSBezierPath| {
        let mut it = pts.iter();
        if let Some(&p) = it.next() {
            path.moveToPoint(at(p));
            for &q in it {
                path.lineToPoint(at(q));
            }
            if closed {
                path.closePath();
            }
        }
    };
    for op in &c.ops {
        match op {
            Op::Path { subpaths, fill, stroke, line_width } => {
                let path = NSBezierPath::bezierPath();
                for (closed, pts) in subpaths {
                    polygon(pts, *closed || fill.is_some(), &path);
                }
                if let Some(f) = fill {
                    ns_color(*f).setFill();
                    path.fill();
                }
                if let Some(s) = stroke {
                    let stroked = NSBezierPath::bezierPath();
                    for (closed, pts) in subpaths {
                        polygon(pts, *closed, &stroked);
                    }
                    stroked.setLineWidth(*line_width as f64 * sx.min(sy));
                    ns_color(*s).setStroke();
                    stroked.stroke();
                }
            }
            Op::Clear { points } => {
                let path = NSBezierPath::bezierPath();
                polygon(points, true, &path);
                ns_color(background).setFill();
                path.fill();
            }
            Op::Text { x, y, text, size, bold, serif, mono, color, align } => {
                let family = if *mono { volant_engine::Family::Mono } else if *serif { volant_engine::Family::Serif } else { volant_engine::Family::Sans };
                let font = Font { family, size: (*size as f64 * sy) as f32, weight: if *bold { 700 } else { 400 }, italic: false };
                let f = ns_font(&font);
                let s = attributed(text, &f, Some(&ns_color(*color)), false, false);
                let w = s.size().width;
                let p = at((*x, *y));
                let dx = match align {
                    Align::Left => 0.0,
                    Align::Center => w / 2.0,
                    Align::Right => w,
                };
                s.drawAtPoint(NSPoint::new(p.x - dx, p.y - f.ascender()));
            }
            Op::Image { src, x, y, w, h, alpha } => {
                if let Some(img) = images.get(src) {
                    let p = at((*x, *y));
                    let dst = rect(p.x, p.y, *w as f64 * sx, *h as f64 * sy);
                    unsafe { img.drawInRect_fromRect_operation_fraction_respectFlipped_hints(dst, NSRect::ZERO, NSCompositingOperation::SourceOver, *alpha as f64, true, None) };
                }
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
        e.page = None;
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
    // A parked Simple Page tab came to the front: its page runs again.
    if let Some(tab) = active.filter(|t| e.parked.contains_key(t)) {
        let resumed = adopt(&mut e, tab) && e.session.is_some();
        if !resumed {
            if let Some(url) = e.trail.get(e.at).cloned() {
                start_session(&mut e, tab, Navigation::get(url), b.ui.content.bounds().size.width as f32, theme::is_dark(b.mtm), false);
            }
        }
    }
    let on_tab = e.tab.is_some() && e.tab == active;
    let current_url = active.and_then(|id| b.webs.get(&id)).and_then(|w| unsafe { w.web.URL() }).and_then(|u| u.absoluteString()).map(|s| s.to_string());
    // Simple Page owns its tab's navigation (see `open`); WebKit's address doesn't matter there.
    let sticky = e.sticky.is_some() && e.sticky == active;
    if !sticky && on_tab && e.page.is_some() && current_url.as_deref() != e.page.as_ref().map(|p| p.url.as_str()) {
        e.page = None;
    }
    let simple = on_tab && e.page.is_some();
    if let Some(v) = &e.view {
        v.ivars().live.set(simple && e.session.is_some());
        v.ivars().focused.set(simple && e.focused);
    }
    let reader = !simple && b.settings.engine_reader && e.article.is_some() && on_tab && active.map(|id| actions::in_reader(b, id)).unwrap_or(false);
    if !simple && !reader {
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
        // Fixed and sticky boxes move against the page as it scrolls: redraw them.
        let clip = scroll.contentView();
        clip.setPostsBoundsChangedNotifications(true);
        let redraw = view.clone();
        let block = block2::RcBlock::new(move |_: std::ptr::NonNull<objc2_foundation::NSNotification>| {
            if redraw.ivars().page.borrow().as_ref().map(|p| !p.layers.is_empty()).unwrap_or(false) {
                redraw.setNeedsDisplayInRect(redraw.visibleRect());
            }
        });
        let center = objc2_foundation::NSNotificationCenter::defaultCenter();
        let token = unsafe { center.addObserverForName_object_queue_usingBlock(Some(objc2_app_kit::NSViewBoundsDidChangeNotification), Some(&clip), None, &block) };
        std::mem::forget(token);
        e.scroll = Some(scroll);
        e.view = Some(view);
    }
    let (Some(scroll), Some(view)) = (e.scroll.clone(), e.view.clone()) else { return };
    // Just above this tab's web view, below anything Volant puts over the content.
    b.ui.content.addSubview_positioned_relativeTo(&scroll, NSWindowOrderingMode::Above, Some(&web));
    let cb = b.ui.content.bounds();
    // Reader keeps its own toolbar visible above; a simple page takes the whole area.
    let top = if simple { 0.0 } else { TOOLBAR_H };
    scroll.setFrame(rect(0.0, top, cb.size.width, (cb.size.height - top).max(0.0)));
    scroll.setHidden(false);
    let dark = theme::is_dark(mtm);
    let size = b.settings.reader_size;
    let serif = b.settings.reader_font != "sans";
    let width = cb.size.width.floor() as i64;
    let want = (width, dark, size, serif, simple);
    if e.laid != Some(want) {
        let started = std::time::Instant::now();
        let images = MacImages { known: &e.images, wanted: RefCell::new(Vec::new()) };
        let env = volant_engine::Env { width: width as f32, dark };
        let (page, base) = if let Some(p) = &e.page {
            *view.ivars().map.borrow_mut() = p.snap.map.clone();
            *view.ivars().canvases.borrow_mut() = p.snap.canvases.clone();
            (script::render_snapshot(&p.snap, &p.sheets, env, &MacFonts, &images), p.snap.url.clone())
        } else {
            let Some(a) = e.article.as_ref() else { return };
            let o = volant_engine::reader::Options { dark, size: size as f32, serif };
            let (html, css) = volant_engine::reader::document(a, &e.host, &o);
            // The Reader page's own top margin already sits above the toolbar line.
            let sheets = vec![css + "article { margin-top: 0 }"];
            (volant_engine::render_in(&html, &sheets, env, &MacFonts, &images), format!("https://{}/", e.host))
        };
        if simple {
            if let Some(s) = &e.session {
                let _ = s.tx.send(Cmd::Width(width as f32, dark));
            }
        }
        // Pictures a <canvas> drew with drawImage load like any other.
        if let Some(p) = &e.page {
            for c in p.snap.canvases.values() {
                for op in &c.ops {
                    if let volant_engine::canvas::Op::Image { src, .. } = op {
                        let _ = images.size(src);
                    }
                }
            }
        }
        let wanted = images.wanted.into_inner();
        let pictures = page.items.iter().filter(|i| matches!(i, Item::Image { .. })).count();
        eprintln!(
            "engine: {} laid out {} items ({pictures} images) in {:.1} ms",
            if simple { "page" } else { "reader" },
            page.items.len(),
            started.elapsed().as_secs_f64() * 1000.0
        );
        *view.ivars().base.borrow_mut() = base.clone();
        for src in wanted {
            if e.images.len() >= MAX_IMAGES || e.images.contains_key(&src) {
                continue;
            }
            if let Some(data) = src.strip_prefix("data:") {
                // Inline pictures decode straight away.
                let bytes = data.split_once(";base64,").and_then(|(_, b64)| base64(b64));
                e.images.insert(src.clone(), ImageState::Loading);
                let key = src.clone();
                super::later(move || with(move |b| image_arrived(b, &key, bytes)));
            } else if let Some(url) = resolve(&base, &src).filter(|u| u.starts_with("https://") || u.starts_with("http://")) {
                e.images.insert(src.clone(), ImageState::Loading);
                load_image(src, url);
            }
        }
        let bg = ns_color(page.background);
        scroll.setBackgroundColor(&bg);
        scroll.setDrawsBackground(true);
        let height = (page.height as f64).max(scroll.contentSize().height);
        *view.ivars().page.borrow_mut() = Some(Rc::new(page));
        // Positions changed with the layout: start a fresh selection.
        view.set_selection(None);
        view.setFrameSize(NSSize::new(width as f64, height));
        view.setNeedsDisplay(true);
        // A tab brought back: where it was.
        if let Some(y) = e.pending_scroll.take() {
            let clip = scroll.contentView();
            let max = (height - clip.bounds().size.height).max(0.0);
            clip.scrollToPoint(NSPoint::new(0.0, y.clamp(0.0, max)));
            scroll.reflectScrolledClipView(&clip);
        }
        // A #fragment: scroll its target to the top.
        if let (Some(frag), Some(p), Some(laid)) = (e.pending_anchor.take(), e.page.as_ref(), view.ivars().page.borrow().as_ref()) {
            if let Some(y) = script::anchor_y(&p.snap, laid, &frag) {
                let clip = scroll.contentView();
                let max = (height - clip.bounds().size.height).max(0.0);
                clip.scrollToPoint(NSPoint::new(0.0, (y as f64 - 8.0).clamp(0.0, max)));
                scroll.reflectScrolledClipView(&clip);
            }
        }
        if let Some(w) = view.window() {
            w.invalidateCursorRectsForView(&view);
        }
        e.laid = Some(want);
    }
}

/// `href` resolved against `base` (`None` for fragments and scripts).
fn resolve(base: &str, href: &str) -> Option<String> {
    let h = href.trim();
    if h.is_empty() || h.starts_with('#') || h.to_ascii_lowercase().starts_with("javascript:") {
        return None;
    }
    if h.starts_with("data:") {
        return Some(h.to_string());
    }
    volant_core::http::Url::parse(base).and_then(|u| u.join(h)).map(|u| u.as_string())
}

/// Standard base64 (as in `data:` URLs), padding optional.
fn base64(s: &str) -> Option<Vec<u8>> {
    let val = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => return None,
        } as u32)
    };
    let clean: Vec<u8> = s.bytes().filter(|c| !c.is_ascii_whitespace() && *c != b'=').collect();
    let mut out = Vec::with_capacity(clean.len() * 3 / 4);
    for chunk in clean.chunks(4) {
        let mut n = 0u32;
        for (i, &c) in chunk.iter().enumerate() {
            n |= val(c)? << (18 - 6 * i);
        }
        out.push((n >> 16) as u8);
        if chunk.len() > 2 {
            out.push((n >> 8) as u8);
        }
        if chunk.len() > 3 {
            out.push(n as u8);
        }
    }
    Some(out)
}

/// View → Simple Page: loads the page in front with Volant's own engine,
/// scripts and all (or goes back to WebKit).
pub fn toggle_page(b: &mut Browser) {
    let Some(id) = b.tabs.active else { return };
    if handles(b, id) {
        // Back to WebKit, at the page Simple Page had reached; the site stays in WebKit.
        let here = {
            let Ok(mut e) = b.engine.0.try_borrow_mut() else { return };
            adopt(&mut e, id);
            let here = e.trail.get(e.at).cloned();
            leave(&mut e);
            here
        };
        if let Some(url) = here {
            b.sites.choose_for(&url, volant_core::sites::Engine::WebKit);
            b.sites.save(&b.paths.sites);
            if let Some(w) = b.webs.get(&id) {
                super::web::load(&w.web, &url);
            }
        }
        sync(b);
        super::ui::update_chrome(b);
        return;
    }
    let url = b.webs.get(&id).and_then(|w| unsafe { w.web.URL() }).and_then(|u| u.absoluteString()).map(|s| s.to_string()).filter(|u| u.starts_with("http"));
    let Some(url) = url.or_else(|| b.tabs.get(id).map(|t| t.url.clone()).filter(|u| u.starts_with("http"))) else {
        eprintln!("engine: Simple Page needs a web page");
        objc2_app_kit::NSBeep();
        return;
    };
    // The person asked for Simple Page here: the site opens in it from now on.
    b.sites.choose_for(&url, volant_core::sites::Engine::Simple);
    b.sites.save(&b.paths.sites);
    claim(b, id, &url);
}

/// Is Simple Page running this tab (so navigation goes to the engine, not WebKit)?
pub fn handles(b: &Browser, tab: TabId) -> bool {
    b.engine.0.try_borrow().map(|e| e.sticky == Some(tab) || e.parked.contains_key(&tab)).unwrap_or(false)
}

/// For the smoke test: the shown page's fixed and sticky layers, the web font
/// faces registered, and the background tabs whose pages are kept alive.
pub fn probe(b: &Browser) -> (usize, usize, usize) {
    let Ok(e) = b.engine.0.try_borrow() else { return (0, 0, 0) };
    let layers = e.view.as_ref().and_then(|v| v.ivars().page.borrow().as_ref().map(|p| p.layers.len())).unwrap_or(0);
    let fonts = WEB_FONTS.with(|w| w.borrow().values().map(Vec::len).sum());
    let live = e.parked.values().filter(|p| p.live.is_some()).count();
    (layers, fonts, live)
}

/// Scrolls the shown page to `y`, as the scroll wheel would.
pub fn scroll_to(b: &Browser, y: f64) {
    let Ok(e) = b.engine.0.try_borrow() else { return };
    if let Some(s) = &e.scroll {
        let clip = s.contentView();
        clip.scrollToPoint(NSPoint::new(0.0, y));
        s.reflectScrolledClipView(&clip);
    }
}

/// Opens a page in Simple Page (a link, a form, the address bar), as a new history entry.
pub fn open(b: &mut Browser, nav: Navigation) {
    let Some(tab) = b.tabs.active else { return };
    // A page can't take the tab to Volant's own pages, files or scripts in an address;
    // mail and other app links go to their apps.
    let scheme = nav.url.split(':').next().unwrap_or("").to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        if !matches!(scheme.as_str(), "volant" | "file" | "javascript" | "data" | "blob" | "about" | "") {
            if let Some(u) = objc2_foundation::NSURL::URLWithString(&super::ns(&nav.url)) {
                objc2_app_kit::NSWorkspace::sharedWorkspace().openURL(&u);
            }
        } else {
            eprintln!("engine: blocked a page's navigation to {}", nav.url.chars().take(80).collect::<String>());
        }
        return;
    }
    // A link to a web app, or a site the person keeps in WebKit: WebKit takes the tab.
    let to_webkit = nav.method == "GET" && b.sites.choose(&nav.url, true, super::now()) == volant_core::sites::Engine::WebKit;
    if to_webkit {
        let Ok(mut e) = b.engine.0.try_borrow_mut() else { return };
        if !adopt(&mut e, tab) {
            return;
        }
        leave(&mut e);
        drop(e);
        if let Some(t) = b.tabs.get_mut(tab) {
            t.url = nav.url.clone();
        }
        if let Some(w) = b.webs.get(&tab) {
            super::web::load(&w.web, &nav.url);
        }
        sync(b);
        super::ui::update_row(b, tab);
        super::ui::update_chrome(b);
        return;
    }
    let judge = b.sites.chosen(&nav.url) != Some(volant_core::sites::Engine::Simple);
    {
        let Ok(mut e) = b.engine.0.try_borrow_mut() else { return };
        if !adopt(&mut e, tab) {
            return;
        }
        let keep = (e.at + 1).min(e.trail.len());
        e.trail.truncate(keep);
        e.trail.push(nav.url.clone());
        e.at = e.trail.len() - 1;
        start_session(&mut e, tab, nav.clone(), b.ui.content.bounds().size.width as f32, theme::is_dark(b.mtm), judge);
    }
    if let Some(t) = b.tabs.get_mut(tab) {
        t.url = nav.url;
    }
    super::ui::update_row(b, tab);
    super::ui::update_chrome(b);
}

/// Back (-1), Forward (+1) or Reload (0) in Simple Page's history; false if there's nowhere to go.
pub fn travel(b: &mut Browser, step: i32) -> bool {
    let Some(tab) = b.tabs.active else { return false };
    let url = {
        let Ok(mut e) = b.engine.0.try_borrow_mut() else { return false };
        if !adopt(&mut e, tab) {
            return false;
        }
        let to = e.at as i64 + step as i64;
        if to < 0 {
            // Back past Simple Page's first page: to the pages WebKit had before it.
            let web_back = b.webs.get(&tab).map(|w| unsafe { w.web.canGoBack() }).unwrap_or(false);
            if web_back {
                leave(&mut e);
                drop(e);
                sync(b);
                super::ui::update_chrome(b);
            }
            return false;
        }
        if to as usize >= e.trail.len() {
            return false;
        }
        e.at = to as usize;
        let url = e.trail[e.at].clone();
        start_session(&mut e, tab, Navigation::get(url.clone()), b.ui.content.bounds().size.width as f32, theme::is_dark(b.mtm), false);
        url
    };
    if let Some(t) = b.tabs.get_mut(tab) {
        t.url = url;
    }
    super::ui::update_row(b, tab);
    super::ui::update_chrome(b);
    true
}

/// For the toolbar: (can go back, can go forward, loading), if Simple Page runs this tab.
pub fn nav_state(b: &Browser, tab: TabId) -> Option<(bool, bool, bool)> {
    let e = b.engine.0.try_borrow().ok()?;
    if let Some(p) = e.parked.get(&tab) {
        return Some((p.at > 0, p.at + 1 < p.trail.len(), false));
    }
    (e.sticky == Some(tab)).then(|| (e.at > 0, e.at + 1 < e.trail.len(), e.loading))
}

/// Sends the live page a command.
fn send(b: &Browser, cmd: Cmd) {
    if let Ok(e) = b.engine.0.try_borrow() {
        if let Some(s) = &e.session {
            let _ = s.tx.send(cmd);
        }
    }
}

/// Runs JavaScript in the live Simple Page (for the smoke test).
pub fn eval(b: &Browser, code: &str) {
    send(b, Cmd::Eval(code.to_string()));
}

/// Types into the live Simple Page and presses a key, as a person would.
pub fn type_and_press(b: &Browser, text: &str, key: &'static str) {
    send(b, Cmd::Type(text.to_string()));
    send(b, Cmd::Key(key));
}

/// Starts a page's script thread: it fetches, runs and snapshots the page.
/// Starts a page's session thread. With `judge`, the page is checked once its
/// scripts have run, and handed to WebKit if Simple Page can't show it.
fn start_session(e: &mut Inner, tab: TabId, nav: Navigation, width: f32, dark: bool, judge: bool) {
    e.counter += 1;
    e.serial = e.counter;
    let serial = e.serial;
    let (tx, rx) = std::sync::mpsc::channel();
    e.session = Some(Session { tx, url: nav.url.clone() });
    e.loading = true;
    e.focused = false;
    e.images.clear();
    if let Some(v) = &e.view {
        v.ivars().images.borrow_mut().clear();
    }
    let spawned = std::thread::Builder::new()
        .name("volant-page".into())
        .stack_size(script::STACK)
        .spawn(move || {
            // A bug in Volant's engine ends this page's session, not the browser: WebKit takes the page.
            let url = nav.url.clone();
            let run = std::panic::AssertUnwindSafe(move || run_session(serial, tab, nav, width, dark, judge, rx));
            if std::panic::catch_unwind(run).is_err() {
                eprintln!("engine: Simple Page stopped on {url}; WebKit shows it");
                super::on_main(move || with(move |b| fall_back(b, serial, url, true)));
            }
        });
    if spawned.is_err() {
        e.session = None;
        e.loading = false;
    }
}

/// The session thread: one page's whole life in Volant's engine.
fn run_session(serial: u64, tab: TabId, nav: Navigation, width: f32, dark: bool, judge: bool, rx: std::sync::mpsc::Receiver<Cmd>) {
    use std::sync::mpsc::RecvTimeoutError;
    let started = std::time::Instant::now();
    let host = NetHost;
    // The page itself is always checked with the server (a 304 is cheap); its files may come from the cache.
    let mut headers = vec![("Accept".to_string(), "text/html,application/xhtml+xml,*/*;q=0.8".to_string()), ("Cache-Control".to_string(), "max-age=0".to_string())];
    if let Some(t) = &nav.content_type {
        headers.push(("Content-Type".into(), t.clone()));
    }
    if let Some(r) = &nav.referrer {
        script::request_context(&mut headers, r, &nav.method);
    }
    let req = script::Request { method: &nav.method, url: &nav.url, headers: &headers, body: nav.body.as_deref(), credentials: true, initiator: nav.referrer.as_deref(), navigation: true };
    let Some(reply) = script::Host::request(&host, &req) else {
        eprintln!("engine: couldn't load {}", nav.url);
        if nav.method == "GET" {
            // WebKit tries it (it may know a way Volant's HTTP doesn't).
            let url = nav.url.clone();
            super::on_main(move || with(move |b| fall_back(b, serial, url, false)));
        } else {
            super::on_main(move || with(move |b| load_failed(b, serial)));
        }
        return;
    };
    let (status, body) = (reply.status, reply.body);
    // After redirects, the page is where it ended up.
    let url = if reply.url.is_empty() { nav.url.clone() } else { reply.url };
    // Not a web page (a PDF, a picture, a video), or a redirect to a web app: WebKit shows it.
    let ty = reply.content_type.to_ascii_lowercase();
    let page_like = ty.is_empty() || ty.contains("html") || ty.contains("xml");
    if nav.method == "GET" && (!page_like || volant_core::sites::is_app_url(&url)) {
        eprintln!("engine: {url} is {}; WebKit shows it", if page_like { "a web app" } else { "not a web page" });
        super::on_main(move || with(move |b| fall_back(b, serial, url, false)));
        return;
    }
    let html = String::from_utf8_lossy(&body).into_owned();
    let host: Rc<dyn script::Host> = Rc::new(NetHost);
    let env = volant_engine::Env { width, dark };
    let mut rt = script::Runtime::new(&html, &url, env, host.clone());
    eprintln!("engine: loaded {url} ({status}, {} KB) in {:.0} ms", body.len() / 1024, started.elapsed().as_secs_f64() * 1000.0);
    let mut sheets: Vec<(String, String)> = Vec::new();
    let mut shown = 0u64;
    let mut logged = 0usize;
    let mut errors = 0usize;
    // In a background tab: no timers, no work, until it comes back.
    let mut frozen = false;
    // Simple Page judges the page a moment after its scripts ran (they may still be fetching).
    let mut judge_at = judge.then(|| std::time::Instant::now() + std::time::Duration::from_millis(1500));
    // Web fonts the page's style sheets ask for: (the face, the address its urls are relative to).
    let mut faces: Vec<(volant_engine::css::FontFace, String)> = Vec::new();
    let mut faces_seen: std::collections::HashSet<(String, u16, bool)> = Default::default();
    let publish = |rt: &mut script::Runtime, sheets: &mut Vec<(String, String)>, faces: &mut Vec<(volant_engine::css::FontFace, String)>| {
        let snap = rt.snapshot();
        for text in volant_engine::style_sheets(&snap.dom) {
            faces.extend(volant_engine::css::font_faces(&text).into_iter().map(|f| (f, snap.url.clone())));
        }
        // Linked style sheets, fetched once each.
        let mut texts = Vec::new();
        for (href, media) in script::stylesheet_links(&snap.dom, &snap.url) {
            let text = match sheets.iter().find(|(h, _)| *h == href) {
                Some((_, t)) => t.clone(),
                None => {
                    let t = match rt.host().get_for(&href, &snap.url) {
                        Some(r) if r.success() => String::from_utf8_lossy(&r.body).into_owned(),
                        _ => String::new(),
                    };
                    sheets.push((href.clone(), t.clone()));
                    faces.extend(volant_engine::css::font_faces(&t).into_iter().map(|f| (f, href.clone())));
                    t
                }
            };
            texts.push(if media.is_empty() || media == "all" { text } else { format!("@media {media} {{\n{text}\n}}") });
        }
        let focused = rt.focused().is_some();
        let anchor = rt.take_anchor_scroll();
        super::on_main(move || with(move |b| arrived(b, serial, tab, snap, texts, focused, anchor)));
    };
    // Fetches new web fonts on a thread of their own, so the page isn't held up.
    let mut fonts_left = MAX_FACES;
    let mut load_fonts = |faces: &mut Vec<(volant_engine::css::FontFace, String)>, page: &str| {
        let mut batch = Vec::new();
        for (f, base) in faces.drain(..) {
            if fonts_left == 0 || !faces_seen.insert((f.family.clone(), f.weight.0, f.italic)) {
                continue;
            }
            fonts_left -= 1;
            batch.push((f, base));
        }
        if batch.is_empty() {
            return;
        }
        let page = page.to_string();
        let _ = std::thread::Builder::new().name("volant-fonts".into()).spawn(move || {
            let mut out = Vec::new();
            for (f, base) in batch {
                for src in &f.sources {
                    let abs = script::resolve_url(&base, src);
                    let Some(r) = script::Host::get_for(&NetHost, &abs, &page).filter(|r| r.success() && r.body.len() <= 4 << 20) else { continue };
                    let bytes = if volant_core::woff::is_woff(&r.body) { volant_core::woff::to_sfnt(&r.body, 8 << 20) } else { Some(r.body) };
                    if let Some(bytes) = bytes {
                        out.push(FaceData { family: f.family.clone(), weight: f.weight, italic: f.italic, bytes });
                        break;
                    }
                }
            }
            if !out.is_empty() {
                super::on_main(move || with(move |b| fonts_arrived(b, out)));
            }
        });
    };
    // First paint: the page as served, before its scripts.
    publish(&mut rt, &mut sheets, &mut faces);
    load_fonts(&mut faces, &url);
    rt.run_scripts();
    eprintln!("engine: ran {url}'s scripts in {:.0} ms", started.elapsed().as_secs_f64() * 1000.0);
    shown = shown.max(rt.generation());
    publish(&mut rt, &mut sheets, &mut faces);
    load_fonts(&mut faces, &url);
    loop {
        for line in rt.take_console() {
            if line.starts_with("Uncaught ") {
                errors += 1;
            }
            if logged < 40 {
                eprintln!("page: {}", line.chars().take(300).collect::<String>());
                logged += 1;
            }
        }
        if let Some(mut next) = rt.take_navigation() {
            next.referrer = Some(rt.state.borrow().url.clone());
            super::on_main(move || {
                with(move |b| {
                    let current = b.engine.0.try_borrow().map(|e| e.serial == serial).unwrap_or(false);
                    if current && b.tabs.active == Some(tab) {
                        open(b, next);
                    }
                })
            });
        }
        if !frozen && judge_at.is_some_and(|t| std::time::Instant::now() >= t) {
            judge_at = None;
            if let Some(why) = script::assess(&rt.snapshot(), errors) {
                eprintln!("engine: {url}: {why}; WebKit shows it");
                let url = rt.state.borrow().url.clone();
                super::on_main(move || with(move |b| fall_back(b, serial, url, true)));
                return;
            }
        }
        let mut wait = if frozen {
            3_600_000
        } else if rt.has_timers() {
            150
        } else {
            1000
        };
        if let Some(t) = judge_at {
            wait = wait.min(t.saturating_duration_since(std::time::Instant::now()).as_millis() as u64 + 1);
        }
        match rx.recv_timeout(std::time::Duration::from_millis(wait)) {
            Ok(Cmd::Click(n)) => rt.click(n),
            Ok(Cmd::Hover(n)) => {
                rt.hover(n);
            }
            Ok(Cmd::Choose(select, index)) => rt.choose(select, index),
            Ok(Cmd::Type(t)) => rt.type_text(&t),
            Ok(Cmd::Key(k)) => rt.key(k),
            Ok(Cmd::Width(w, d)) => {
                let changed = {
                    let mut st = rt.state.borrow_mut();
                    let was = (st.env.width, st.env.dark);
                    st.env = volant_engine::Env { width: w, dark: d };
                    was != (w, d)
                };
                if changed {
                    let _ = rt.interp.run("dispatchEvent(new Event('resize'))");
                }
            }
            Ok(Cmd::Eval(code)) => {
                if let Err(e) = rt.interp.run(&code) {
                    eprintln!("page eval: {e}");
                }
            }
            Ok(Cmd::Freeze(f)) => {
                frozen = f;
                if !f {
                    shown = u64::MAX; // back in front: show where it got to
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                if !frozen {
                    rt.tick()
                }
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }
        if rt.generation() != shown {
            shown = rt.generation();
            publish(&mut rt, &mut sheets, &mut faces);
            load_fonts(&mut faces, &url);
        }
        // A click or key opened a `<select>`: its menu is native.
        if let Some((select, items)) = rt.take_menu() {
            super::on_main(move || with(move |b| show_menu(b, serial, select, items)));
        }
    }
}

/// A `<select>` was clicked: its options as a native menu, under the control.
fn show_menu(b: &mut Browser, serial: u64, select: u32, items: Vec<script::MenuItem>) {
    let Ok(e) = b.engine.0.try_borrow() else { return };
    if e.serial != serial {
        return;
    }
    let Some(view) = e.view.clone() else { return };
    drop(e);
    let mtm = b.mtm;
    // Where the control is drawn: its box among the laid-out page's hits.
    let place = {
        let map = view.ivars().map.borrow();
        let page = view.ivars().page.borrow();
        page.as_ref().and_then(|page| {
            page.hits
                .iter()
                .filter(|h| map.get(h.node as usize) == Some(&select))
                .max_by(|a, b| (a.w * a.h).partial_cmp(&(b.w * b.h)).unwrap_or(std::cmp::Ordering::Equal))
                .map(|h| (h.x as f64, (h.y + h.h) as f64, h.w as f64))
        })
    };
    let Some((x, y, w)) = place else { return };
    let menu = NSMenu::new(mtm);
    menu.setAutoenablesItems(true);
    let mut index = 0isize;
    let mut chosen = None;
    for it in &items {
        let item = unsafe { NSMenuItem::initWithTitle_action_keyEquivalent(NSMenuItem::alloc(mtm), &ns(&it.label), Some(objc2::sel!(chooseOption:)), &ns("")) };
        if it.header {
            item.setTag(-1);
            item.setEnabled(false);
        } else {
            item.setTag(if it.disabled { -1 - index } else { index });
            unsafe { item.setTarget(Some(&view)) };
            if it.disabled {
                item.setEnabled(false);
            } else {
                item.setIndentationLevel(if items.iter().any(|m| m.header) { 1 } else { 0 });
            }
            if it.selected {
                item.setState(objc2_app_kit::NSControlStateValueOn);
                chosen = Some(item.clone());
            }
            index += 1;
        }
        menu.addItem(&item);
    }
    menu.setMinimumWidth(w);
    view.ivars().menu_for.set(Some(select));
    // The menu blocks until it closes, so it opens outside the browser state;
    // a pick arrives through chooseOption:.
    super::later(move || {
        menu.popUpMenuPositioningItem_atLocation_inView(chosen.as_deref(), NSPoint::new(x, y), Some(&view));
        if view.ivars().menu_for.get() == Some(select) {
            view.ivars().menu_for.set(None);
        }
    });
}

/// Simple Page can't show this page: WebKit loads it instead. With `learn`,
/// the site goes to WebKit for a while (see `volant_core::sites`).
fn fall_back(b: &mut Browser, serial: u64, url: String, learn: bool) {
    let tab = {
        let Ok(mut e) = b.engine.0.try_borrow_mut() else { return };
        if e.serial != serial {
            return;
        }
        let Some(tab) = e.sticky else { return };
        leave(&mut e);
        tab
    };
    if learn {
        b.sites.failed(&url, super::now());
        b.sites.save(&b.paths.sites);
    }
    if let Some(t) = b.tabs.get_mut(tab) {
        t.url = url.clone();
    }
    if let Some(w) = b.webs.get(&tab) {
        super::web::load(&w.web, &url);
    }
    sync(b);
    super::ui::update_row(b, tab);
    super::ui::update_chrome(b);
}

/// Simple Page stops running the tab it's on.
fn leave(e: &mut Inner) {
    e.sticky = None;
    e.session = None; // dropping the sender ends the thread
    e.page = None;
    e.focused = false;
    e.trail.clear();
    e.at = 0;
    e.loading = false;
}

/// Brings a parked tab's history into the running slot (parking the tab that had it).
/// Doesn't start its page; the caller does.
/// If the tab's page was kept alive, it's running again afterwards (`e.session` is set).
fn adopt(e: &mut Inner, tab: TabId) -> bool {
    let Some(p) = e.parked.remove(&tab) else { return e.sticky == Some(tab) };
    if let Some(old) = e.sticky.filter(|&o| o != tab) {
        let trail = std::mem::take(&mut e.trail);
        if !trail.is_empty() {
            // The page stays alive, frozen, with where it was scrolled to.
            let scroll_y = e.scroll.as_ref().map(|s| s.contentView().bounds().origin.y).unwrap_or(0.0);
            e.counter += 1;
            let parked_at = e.counter;
            let live = e.session.take().map(|session| {
                let _ = session.tx.send(Cmd::Freeze(true));
                Live { session, serial: e.serial, page: e.page.take(), scroll_y, parked_at }
            });
            e.parked.insert(old, Parked { trail, at: e.at, live });
            // Only the most recent few stay alive.
            loop {
                let alive: Vec<(TabId, u64)> = e.parked.iter().filter_map(|(t, p)| p.live.as_ref().map(|l| (*t, l.parked_at))).collect();
                if alive.len() <= LIVE_PARKED {
                    break;
                }
                let Some(&(oldest, _)) = alive.iter().min_by_key(|(_, at)| *at) else { break };
                if let Some(p) = e.parked.get_mut(&oldest) {
                    p.live = None; // its thread ends
                }
            }
        }
    }
    leave(e);
    e.sticky = Some(tab);
    e.tab = Some(tab);
    e.trail = p.trail;
    e.at = p.at.min(e.trail.len().saturating_sub(1));
    if let Some(live) = p.live.filter(|l| l.page.is_some()) {
        let _ = live.session.tx.send(Cmd::Freeze(false));
        e.session = Some(live.session);
        e.serial = live.serial;
        e.page = live.page;
        e.laid = None;
        e.pending_scroll = Some(live.scroll_y);
        e.images.clear();
        if let Some(v) = &e.view {
            v.ivars().images.borrow_mut().clear();
        }
    }
    true
}

/// Should Simple Page draw this address (see Settings and `volant_core::sites`)?
pub fn wants(b: &Browser, url: &str) -> bool {
    b.sites.choose(url, b.settings.simple_default, super::now()) == volant_core::sites::Engine::Simple
}

/// Simple Page takes tab `tab` to `url`: now if it's in front, else when it comes to the front.
/// WebKit's copy of the tab, if any, stays empty.
pub fn claim(b: &mut Browser, tab: TabId, url: &str) {
    let mine = handles(b, tab);
    if let Some(t) = b.tabs.get_mut(tab) {
        t.url = url.to_string();
    }
    if !mine {
        if let Some(w) = b.webs.get(&tab) {
            unsafe {
                w.web.stopLoading();
                w.web.loadHTMLString_baseURL(&super::ns(""), None);
            }
        }
    }
    if b.tabs.active != Some(tab) {
        if let Ok(mut e) = b.engine.0.try_borrow_mut() {
            let entry = e.parked.entry(tab).or_default();
            let keep = (entry.at + 1).min(entry.trail.len());
            entry.trail.truncate(keep);
            entry.trail.push(url.to_string());
            entry.at = entry.trail.len() - 1;
            entry.live = None; // a new page: the old one goes
        }
        return;
    }
    if mine {
        open(b, Navigation::get(url.to_string()));
        return;
    }
    let judge = b.sites.chosen(url) != Some(volant_core::sites::Engine::Simple);
    let (width, dark) = (b.ui.content.bounds().size.width as f32, theme::is_dark(b.mtm));
    {
        let Ok(mut e) = b.engine.0.try_borrow_mut() else { return };
        e.parked.insert(tab, Parked { trail: vec![url.to_string()], at: 0, live: None });
        adopt(&mut e, tab);
        start_session(&mut e, tab, Navigation::get(url.to_string()), width, dark, judge);
    }
    super::ui::update_row(b, tab);
    super::ui::update_chrome(b);
}

/// The tab closed: Simple Page forgets it.
pub fn forget(b: &Browser, tab: TabId) {
    if let Ok(mut e) = b.engine.0.try_borrow_mut() {
        e.parked.remove(&tab);
        if e.sticky == Some(tab) {
            leave(&mut e);
        }
    }
}

/// The page couldn't be fetched: stop showing it as loading.
fn load_failed(b: &mut Browser, serial: u64) {
    if let Ok(mut e) = b.engine.0.try_borrow_mut() {
        if e.serial == serial {
            e.loading = false;
        }
    }
    objc2_app_kit::NSBeep();
    super::ui::update_chrome(b);
}

/// A snapshot from the session thread.
fn arrived(b: &mut Browser, serial: u64, tab: TabId, snap: Snapshot, sheets: Vec<String>, focused: bool, anchor: bool) {
    let visited;
    {
        let Ok(mut e) = b.engine.0.try_borrow_mut() else { return };
        if e.serial != serial || e.sticky != Some(tab) {
            return; // an older page
        }
        // Where the page really is (after redirects, or a script's pushState).
        let url = snap.url.clone();
        if let Some(s) = e.session.as_mut() {
            s.url = url.clone();
        }
        let at = e.at;
        if let Some(entry) = e.trail.get_mut(at) {
            if *entry != url {
                *entry = url.clone();
            }
        }
        e.loading = false;
        visited = Some((url.clone(), snap.title.clone()));
        // A different page starts at its top (or at its #fragment); a #link on the page scrolls there.
        let base = |u: &str| u.split('#').next().unwrap_or(u).to_string();
        let new_page = e.page.as_ref().map(|p| base(&p.url) != base(&url)).unwrap_or(true);
        let fragment = url.split_once('#').map(|(_, f)| f.to_string());
        if anchor || (new_page && fragment.is_some()) {
            e.pending_anchor = Some(fragment.clone().unwrap_or_default());
        }
        if new_page {
            if let Some(scroll) = &e.scroll {
                let clip = scroll.contentView();
                clip.scrollToPoint(NSPoint::new(0.0, 0.0));
                scroll.reflectScrolledClipView(&clip);
            }
        }
        e.tab = Some(tab);
        e.page = Some(SimplePage { url, snap, sheets });
        e.focused = focused;
        e.laid = None;
        e.find_hits.clear();
        if let Some(v) = &e.view {
            *v.ivars().hits.borrow_mut() = (Vec::new(), 0);
        }
    }
    sync(b);
    // The tab shows the page's address and title, and Volant's history records the visit.
    let Some((url, title)) = visited else { return };
    let Some(t) = b.tabs.get_mut(tab) else { return };
    let changed = t.url != url || (!title.is_empty() && t.title != title);
    if !changed {
        super::ui::update_chrome(b);
        return;
    }
    let new_url = t.url != url;
    t.url = url.clone();
    if !title.is_empty() {
        t.title = title.clone();
    }
    if new_url {
        b.history.visit(&url, &title, super::now());
    } else {
        b.history.set_title(&url, &title);
    }
    super::mark_dirty(b);
    super::ui::update_row(b, tab);
    super::ui::update_chrome(b);
}

/// Downloads a picture (Volant's own HTTP), decodes it, and lays the page out again.
/// `key` is the address as the page wrote it; `url` is where it really is.
fn load_image(key: String, url: String) {
    std::thread::spawn(move || {
        let bytes = super::net::fetch(&url, 24 << 20).ok();
        super::on_main(move || with(move |b| image_arrived(b, &key, bytes)));
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

/// For the smoke test: selects the paragraph that contains `needle` (as a
/// triple-click would), copies it through the view's own Copy, and returns
/// what landed on the pasteboard.
pub fn select_and_copy(b: &Browser, needle: &str) -> Option<String> {
    let view = b.engine.0.try_borrow().ok()?.view.clone()?;
    let page = view.ivars().page.borrow().clone()?;
    let item = page.items.iter().position(|i| matches!(i, Item::Text { text, .. } if text.contains(needle)))?;
    let sel = select::paragraph(&page, Pos { item, offset: 0 });
    view.set_selection(Some(sel));
    if let Some(r) = view.ivars().sel_areas.borrow().first() {
        view.scrollRectToVisible(rect(r.origin.x, r.origin.y - 60.0, r.size.width, r.size.height + 200.0));
    }
    let _: () = unsafe { msg_send![&*view, copy: std::ptr::null::<AnyObject>()] };
    NSPasteboard::generalPasteboard().stringForType(unsafe { NSPasteboardTypeString }).map(|s| s.to_string())
}
