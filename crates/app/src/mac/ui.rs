//! The window: a sidebar of tabs on the left and the page in a rounded panel
//! inset on the right. No toolbar, no borders, no shadows.

use super::delegate::Delegate;
use super::theme::{self, Rgba};
use super::views::{self, fill, label, plate, set_text, symbol, Flipped, Hit, IconButton, Kind, Weight};
use super::{ns, rect, Browser};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{AnyThread, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSAutoresizingMaskOptions, NSBackingStoreType, NSImage, NSImageView, NSScrollView, NSScrollerStyle, NSTextField,
    NSWindow, NSWindowButton, NSWindowStyleMask, NSWindowTitleVisibility,
};
use objc2_foundation::{NSData, NSPoint, NSSize};
use volant_core::omnibox;
use volant_core::tabs::TabId;

pub const SIDEBAR_W: f64 = 252.0;
pub const INSET: f64 = 8.0;
/// With the sidebar hidden, a slim strip above the page holds the window
/// buttons and the button that brings the sidebar back.
const STRIP_H: f64 = 40.0;
const ROW_H: f64 = 34.0;
const ROW_GAP: f64 = 2.0;
const LIST_TOP: f64 = 136.0;
const FOOTER_H: f64 = 46.0;

/// A small site icon: the favicon when we have one, else a coloured letter tile.
pub struct SiteIcon {
    pub tile: Retained<Flipped>,
    pub letter: Retained<NSTextField>,
    pub img: Retained<NSImageView>,
}

impl SiteIcon {
    pub fn new(x: f64, y: f64, size: f64, mtm: MainThreadMarker) -> Self {
        let tile = plate(rect(x, y, size, size), (size * 0.26).round(), mtm);
        let letter = label("", (size * 0.58).round(), Weight::Bold, Rgba(1.0, 1.0, 1.0, 1.0), mtm);
        letter.setAlignment(objc2_app_kit::NSTextAlignment::Center);
        let lh = (size * 0.58).round() + 4.0;
        letter.setFrame(rect(0.0, ((size - lh) / 2.0).round(), size, lh));
        let img = views::image_view(None, rect(0.0, 0.0, size, size), None, mtm);
        img.setImageScaling(objc2_app_kit::NSImageScaling::ScaleProportionallyUpOrDown);
        tile.addSubview(&letter);
        tile.addSubview(&img);
        Self { tile, letter, img }
    }

    pub fn set(&self, b: &Browser, url: &str) {
        if let Some(img) = icon_for(b, url) {
            self.img.setImage(Some(&img));
            self.img.setHidden(false);
            self.letter.setHidden(true);
            fill(&self.tile, Rgba(0.0, 0.0, 0.0, 0.0));
        } else {
            let host = omnibox::host(url);
            let ch = host.chars().find(|c| c.is_alphanumeric()).map(|c| c.to_uppercase().to_string()).unwrap_or_else(|| "•".into());
            set_text(&self.letter, &ch);
            self.img.setHidden(true);
            self.letter.setHidden(false);
            fill(&self.tile, tile_color(host));
        }
    }
}

const TILE_COLORS: [u32; 8] = [0x1f6bff, 0x7b5cf5, 0xe0457b, 0xf26b3a, 0xe2a400, 0x23a559, 0x0fa5a5, 0x5b6b82];

/// Same colour the internal pages pick for a host.
pub fn tile_color(host: &str) -> Rgba {
    let mut h: u32 = 0;
    for c in host.encode_utf16() {
        h = h.wrapping_mul(31).wrapping_add(c as u32);
    }
    let v = TILE_COLORS[(h % 8) as usize];
    Rgba(((v >> 16) & 255) as f64 / 255.0, ((v >> 8) & 255) as f64 / 255.0, (v & 255) as f64 / 255.0, 1.0)
}

thread_local! {
    static APP_ICON: Option<Retained<NSImage>> = {
        let data = NSData::with_bytes(include_bytes!("../../../../assets/icon-64.png"));
        NSImage::initWithData(NSImage::alloc(), &data)
    };
}

pub fn app_icon() -> Option<Retained<NSImage>> {
    APP_ICON.with(|i| i.clone())
}

fn icon_for(b: &Browser, url: &str) -> Option<Retained<NSImage>> {
    if omnibox::is_internal(url) {
        return app_icon();
    }
    b.icons.get(omnibox::host(url)).cloned()
}

pub struct Row {
    pub id: TabId,
    pub hit: Retained<Hit>,
    pub icon: SiteIcon,
    pub title: Retained<NSTextField>,
    pub close: Retained<Hit>,
    pub close_img: Retained<NSImageView>,
    pub pin: Retained<NSImageView>,
}

pub struct Ui {
    pub root: Retained<Flipped>,
    pub sidebar: Retained<Flipped>,
    pub back: IconButton,
    pub forward: IconButton,
    pub reload: IconButton,
    pub address: Retained<Hit>,
    pub address_icon: Retained<NSImageView>,
    pub address_text: Retained<NSTextField>,
    pub reader: IconButton,
    pub new_tab: Retained<Hit>,
    pub new_tab_icon: Retained<NSImageView>,
    pub new_tab_label: Retained<NSTextField>,
    pub scroll: Retained<NSScrollView>,
    pub list: Retained<Flipped>,
    pub rows: Vec<Row>,
    pub settings: IconButton,
    pub toggle: IconButton,
    pub downloads: Retained<Hit>,
    pub downloads_label: Retained<NSTextField>,
    pub update: Retained<Hit>,
    pub update_label: Retained<NSTextField>,
    pub update_dot: Retained<Flipped>,
    pub content: Retained<Flipped>,
    pub progress: Retained<Flipped>,
    pub show_sidebar: IconButton,
}

pub fn make_window(delegate: &Delegate, mtm: MainThreadMarker) -> Retained<NSWindow> {
    let style = NSWindowStyleMask::Titled
        | NSWindowStyleMask::Closable
        | NSWindowStyleMask::Miniaturizable
        | NSWindowStyleMask::Resizable
        | NSWindowStyleMask::FullSizeContentView;
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(NSWindow::alloc(mtm), rect(0.0, 0.0, 1320.0, 860.0), style, NSBackingStoreType::Buffered, false)
    };
    unsafe { window.setReleasedWhenClosed(false) };
    window.setTitle(&ns("Volant"));
    window.setTitlebarAppearsTransparent(true);
    window.setTitleVisibility(NSWindowTitleVisibility::Hidden);
    window.setMovableByWindowBackground(true);
    window.setMinSize(NSSize::new(720.0, 480.0));
    window.center();
    window.setFrameAutosaveName(&ns("VolantMain"));
    window.setDelegate(Some(ProtocolObject::from_ref(delegate)));
    window
}

impl Ui {
    pub fn build(window: &NSWindow, _delegate: &Delegate, mtm: MainThreadMarker) -> Self {
        let size = window.contentView().map(|v| v.frame().size).unwrap_or(NSSize::new(1320.0, 860.0));
        let (w, h) = (size.width, size.height);
        let root = Flipped::new(rect(0.0, 0.0, w, h), mtm);
        root.mark_root();
        window.setContentView(Some(&root));

        let sidebar = Flipped::new(rect(0.0, 0.0, SIDEBAR_W, h), mtm);
        sidebar.setAutoresizingMask(NSAutoresizingMaskOptions::ViewHeightSizable);
        root.addSubview(&sidebar);

        let nav_x = SIDEBAR_W - 10.0 - 28.0 * 3.0 - 4.0;
        let back = IconButton::new(Kind::Back, "chevron.left", 14.0, rect(nav_x, 13.0, 28.0, 28.0), "Back (⌘[)", mtm);
        let forward = IconButton::new(Kind::Forward, "chevron.right", 14.0, rect(nav_x + 30.0, 13.0, 28.0, 28.0), "Forward (⌘])", mtm);
        let reload = IconButton::new(Kind::Reload, "arrow.clockwise", 13.0, rect(nav_x + 60.0, 13.0, 28.0, 28.0), "Reload (⌘R)", mtm);
        for b in [&back, &forward, &reload] {
            sidebar.addSubview(&b.hit);
        }

        let address = Hit::new(Kind::Address, 0, rect(10.0, 50.0, SIDEBAR_W - 20.0, 36.0), true, mtm);
        if let Some(l) = address.layer() {
            l.setCornerRadius(10.0);
        }
        let address_icon = views::image_view(None, rect(11.0, 10.0, 16.0, 16.0), None, mtm);
        let address_text = label("", 13.0, Weight::Medium, theme::current().text, mtm);
        address_text.setFrame(rect(34.0, 9.0, SIDEBAR_W - 20.0 - 72.0, 18.0));
        let reader = IconButton::new(Kind::Reader, "doc.plaintext", 12.0, rect(SIDEBAR_W - 20.0 - 34.0, 6.0, 26.0, 24.0), "Show Reader (⇧⌘R)", mtm);
        address.addSubview(&address_icon);
        address.addSubview(&address_text);
        address.addSubview(&reader.hit);
        address.setToolTip(Some(&ns("Search or enter an address (⌘L)")));
        sidebar.addSubview(&address);

        let new_tab = Hit::new(Kind::NewTab, 0, rect(10.0, 94.0, SIDEBAR_W - 20.0, 32.0), true, mtm);
        let new_tab_icon = views::image_view(symbol("plus", 13.0, Weight::Medium).as_deref(), rect(10.0, 8.0, 16.0, 16.0), None, mtm);
        let new_tab_label = label("New Tab", 13.0, Weight::Regular, theme::current().text2, mtm);
        new_tab_label.setFrame(rect(34.0, 7.0, 160.0, 18.0));
        new_tab.addSubview(&new_tab_icon);
        new_tab.addSubview(&new_tab_label);
        new_tab.setToolTip(Some(&ns("New Tab (⌘T)")));
        sidebar.addSubview(&new_tab);

        let scroll = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), rect(0.0, LIST_TOP, SIDEBAR_W, h - LIST_TOP - FOOTER_H));
        scroll.setDrawsBackground(false);
        scroll.setHasVerticalScroller(true);
        scroll.setAutohidesScrollers(true);
        scroll.setScrollerStyle(NSScrollerStyle::Overlay);
        let list = Flipped::new(rect(0.0, 0.0, SIDEBAR_W, 10.0), mtm);
        scroll.setDocumentView(Some(&list));
        sidebar.addSubview(&scroll);

        let settings = IconButton::new(Kind::Settings, "gearshape", 14.0, rect(10.0, h - 38.0, 28.0, 28.0), "Settings (⌘,)", mtm);
        let toggle = IconButton::new(Kind::Sidebar, "sidebar.left", 14.0, rect(SIDEBAR_W - 38.0, h - 38.0, 28.0, 28.0), "Hide Sidebar (⌘S)", mtm);
        let downloads = Hit::new(Kind::Downloads, 0, rect(42.0, h - 38.0, SIDEBAR_W - 84.0, 28.0), true, mtm);
        let downloads_label = label("", 11.5, Weight::Medium, theme::current().text2, mtm);
        downloads_label.setFrame(rect(8.0, 6.0, SIDEBAR_W - 100.0, 16.0));
        downloads.addSubview(&downloads_label);
        downloads.setHidden(true);
        let update = Hit::new(Kind::Update, 0, rect(42.0, h - 38.0, SIDEBAR_W - 84.0, 28.0), true, mtm);
        let update_dot = plate(rect(9.0, 11.0, 6.0, 6.0), 3.0, mtm);
        let update_label = label("Update available", 12.0, Weight::Semibold, theme::current().accent, mtm);
        update_label.setFrame(rect(21.0, 6.0, SIDEBAR_W - 110.0, 16.0));
        update.addSubview(&update_dot);
        update.addSubview(&update_label);
        update.setToolTip(Some(&ns("A newer build of Volant is ready. Click to update.")));
        update.setHidden(true);
        for v in [&settings.hit, &toggle.hit, &downloads, &update] {
            sidebar.addSubview(v);
        }

        let content = plate(rect(SIDEBAR_W, INSET, w - SIDEBAR_W - INSET, h - INSET * 2.0), 10.0, mtm);
        if let Some(l) = content.layer() {
            l.setMasksToBounds(true);
        }
        root.addSubview(&content);
        let progress = Flipped::new(rect(0.0, 0.0, 0.0, 2.5), mtm);
        progress.setHidden(true);
        let show_sidebar = IconButton::new(Kind::ShowSidebar, "sidebar.left", 14.0, rect(84.0, 6.0, 28.0, 28.0), "Show Sidebar (⌘S)", mtm);
        show_sidebar.hit.setHidden(true);
        root.addSubview(&show_sidebar.hit);

        Self {
            root,
            sidebar,
            back,
            forward,
            reload,
            address,
            address_icon,
            address_text,
            reader,
            new_tab,
            new_tab_icon,
            new_tab_label,
            scroll,
            list,
            rows: Vec::new(),
            settings,
            toggle,
            downloads,
            downloads_label,
            update,
            update_label,
            update_dot,
            content,
            progress,
            show_sidebar,
        }
    }
}

/// Moves the window's traffic lights so they line up with the sidebar's buttons.
fn place_traffic_lights(window: &NSWindow, visible: bool, inset_y: f64) {
    let buttons: Vec<Retained<objc2_app_kit::NSButton>> =
        [NSWindowButton::CloseButton, NSWindowButton::MiniaturizeButton, NSWindowButton::ZoomButton].iter().filter_map(|&k| window.standardWindowButton(k)).collect();
    if buttons.len() != 3 {
        return;
    }
    for b in &buttons {
        b.setHidden(!visible);
    }
    if !visible || window.styleMask().contains(NSWindowStyleMask::FullScreen) {
        return;
    }
    let inset_x = 18.0;
    let close = &buttons[0];
    let Some(container) = unsafe { close.superview() }.and_then(|v| unsafe { v.superview() }) else { return };
    let close_frame = close.frame();
    let bar_h = close_frame.size.height + inset_y;
    let mut bar = container.frame();
    bar.size.height = bar_h;
    bar.origin.y = window.frame().size.height - bar_h;
    container.setFrame(bar);
    let gap = buttons[1].frame().origin.x - close_frame.origin.x;
    for (i, b) in buttons.iter().enumerate() {
        b.setFrameOrigin(NSPoint::new(inset_x + gap * i as f64, b.frame().origin.y));
    }
}

/// Positions everything for the current window size.
pub fn layout(b: &mut Browser) {
    let size = b.ui.root.frame().size;
    let (w, h) = (size.width, size.height);
    let show = b.settings.sidebar;
    let ui = &b.ui;
    ui.sidebar.setHidden(!show);
    ui.sidebar.setFrame(rect(0.0, 0.0, SIDEBAR_W, h));
    // Lights line up with the sidebar's buttons, or centre in the strip when it's hidden.
    place_traffic_lights(&b.window, true, if show { 20.0 } else { 13.0 });
    ui.show_sidebar.hit.setHidden(show);
    let full_screen = b.window.styleMask().contains(NSWindowStyleMask::FullScreen);
    ui.show_sidebar.hit.setFrame(rect(if full_screen { 10.0 } else { 84.0 }, 6.0, 28.0, 28.0));
    let (cx, cy) = if show { (SIDEBAR_W, INSET) } else { (INSET, STRIP_H) };
    ui.content.setFrame(rect(cx, cy, w - cx - INSET, h - cy - INSET));
    let cb = ui.content.bounds();
    if let Some(t) = b.tabs.active.and_then(|id| b.webs.get(&id)) {
        t.web.setFrame(cb);
    }
    ui.scroll.setFrame(rect(0.0, LIST_TOP, SIDEBAR_W, h - LIST_TOP - FOOTER_H));
    let fy = h - 38.0;
    ui.settings.hit.setFrame(rect(10.0, fy, 28.0, 28.0));
    ui.toggle.hit.setFrame(rect(SIDEBAR_W - 38.0, fy, 28.0, 28.0));
    ui.downloads.setFrame(rect(42.0, fy, SIDEBAR_W - 84.0, 28.0));
    ui.update.setFrame(rect(42.0, fy, SIDEBAR_W - 84.0, 28.0));
    layout_rows(b);
    update_progress(b);
    super::command::layout(b);
    super::find::layout(b);
}

fn layout_rows(b: &Browser) {
    let ui = &b.ui;
    let mut y = 0.0;
    let mut last_pinned = None;
    for r in &ui.rows {
        let pinned = b.tabs.get(r.id).map(|t| t.pinned).unwrap_or(false);
        if last_pinned == Some(true) && !pinned {
            y += 12.0;
        }
        last_pinned = Some(pinned);
        r.hit.setFrame(rect(10.0, y, SIDEBAR_W - 20.0, ROW_H));
        y += ROW_H + ROW_GAP;
    }
    let vh = ui.scroll.contentSize().height;
    ui.list.setFrame(rect(0.0, 0.0, SIDEBAR_W, y.max(vh)));
}

fn make_row(b: &Browser, id: TabId) -> Row {
    let mtm = b.mtm;
    let t = theme::current();
    let w = SIDEBAR_W - 20.0;
    let hit = Hit::new(Kind::Tab, id, rect(10.0, 0.0, w, ROW_H), true, mtm);
    let icon = SiteIcon::new(10.0, 9.0, 16.0, mtm);
    let title = label("", 13.0, Weight::Regular, t.text, mtm);
    title.setFrame(rect(34.0, 8.0, w - 34.0 - 30.0, 18.0));
    let close = Hit::new(Kind::CloseTab, id, rect(w - 27.0, 6.0, 22.0, 22.0), true, mtm);
    if let Some(l) = close.layer() {
        l.setCornerRadius(6.0);
    }
    let close_img = views::image_view(symbol("xmark", 10.0, Weight::Semibold).as_deref(), rect(0.0, 0.0, 22.0, 22.0), Some(t.text2), mtm);
    close.addSubview(&close_img);
    close.setToolTip(Some(&ns("Close Tab (⌘W)")));
    let pin = views::image_view(symbol("pin.fill", 9.0, Weight::Medium).as_deref(), rect(w - 24.0, 11.0, 12.0, 12.0), Some(t.text3), mtm);
    hit.addSubview(&icon.tile);
    hit.addSubview(&title);
    hit.addSubview(&pin);
    hit.addSubview(&close);
    hit.set_aux(&close);
    Row { id, hit, icon, title, close, close_img, pin }
}

fn fill_row(b: &Browser, r: &Row) {
    let Some(tab) = b.tabs.get(r.id) else { return };
    let t = theme::current();
    let title = tab_title(tab);
    set_text(&r.title, &title);
    r.hit.setToolTip(Some(&ns(&format!("{}\n{}", title, omnibox::display(&tab.url)))));
    r.icon.set(b, &tab.url);
    views::recolor(&r.title, if tab.asleep { t.text2 } else { t.text });
    r.pin.setHidden(!tab.pinned);
    r.hit.set_selected(b.tabs.active == Some(r.id));
}

pub fn tab_title(tab: &volant_core::tabs::Tab) -> String {
    if !tab.title.trim().is_empty() {
        return tab.title.trim().to_string();
    }
    match tab.url.as_str() {
        "volant://newtab" | "" => "New Tab".into(),
        u => omnibox::display(u),
    }
}

/// Rebuilds the tab list when tabs were added, removed or moved; otherwise just refreshes it.
pub fn sync_tabs(b: &mut Browser) {
    let ids: Vec<TabId> = b.tabs.list.iter().map(|t| t.id).collect();
    let have: Vec<TabId> = b.ui.rows.iter().map(|r| r.id).collect();
    if ids != have {
        for r in b.ui.rows.drain(..) {
            r.hit.removeFromSuperview();
        }
        let rows: Vec<Row> = ids.iter().map(|&id| make_row(b, id)).collect();
        for r in &rows {
            b.ui.list.addSubview(&r.hit);
        }
        b.ui.rows = rows;
        layout_rows(b);
    }
    for r in &b.ui.rows {
        fill_row(b, r);
    }
    update_chrome(b);
    super::mark_dirty(b);
}

/// Refreshes one tab's row (title, icon) without touching the rest.
pub fn update_row(b: &Browser, id: TabId) {
    if let Some(r) = b.ui.rows.iter().find(|r| r.id == id) {
        fill_row(b, r);
    }
}

pub fn scroll_to_active(b: &Browser) {
    if let Some(r) = b.ui.rows.iter().find(|r| Some(r.id) == b.tabs.active) {
        r.hit.scrollRectToVisible(r.hit.bounds());
    }
}

/// The address pill and back/forward/reload state for the tab in front.
pub fn update_chrome(b: &Browser) {
    let t = theme::current();
    let ui = &b.ui;
    let tab = b.tabs.active_tab();
    let url = tab.map(|t| t.url.clone()).unwrap_or_default();
    let web = b.tabs.active.and_then(|id| b.webs.get(&id));
    let (text, icon, dim) = if url.is_empty() || url == "volant://newtab" {
        ("Search or enter an address".to_string(), "magnifyingglass", true)
    } else if omnibox::is_internal(&url) {
        (tab.map(tab_title).unwrap_or_default(), "magnifyingglass", false)
    } else if let Some(q) = volant_core::search::query_of(&url) {
        (q, "magnifyingglass", false)
    } else if url.starts_with("https://") {
        (omnibox::host(&url).to_string(), "lock.fill", false)
    } else if url.starts_with("http://") {
        (format!("Not Secure — {}", omnibox::host(&url)), "exclamationmark.triangle.fill", false)
    } else {
        (omnibox::display(&url), "doc", false)
    };
    let reading = b.tabs.active.map(|id| super::actions::in_reader(b, id)).unwrap_or(false);
    let reader_shown = reading || url.starts_with("http");
    ui.reader.hit.setHidden(!reader_shown);
    // The text gets the room the Reader button would use when it isn't there.
    ui.address_text.setFrame(rect(34.0, 9.0, SIDEBAR_W - 20.0 - if reader_shown { 72.0 } else { 44.0 }, 18.0));
    views::tint(&ui.reader.img, if reading { t.accent } else { t.text2 });
    ui.reader.hit.setToolTip(Some(&ns(if reading { "Hide Reader (⇧⌘R)" } else { "Show Reader (⇧⌘R)" })));
    set_text(&ui.address_text, &text);
    views::recolor(&ui.address_text, if dim { t.text3 } else { t.text });
    ui.address_icon.setImage(symbol(icon, 11.0, Weight::Semibold).as_deref());
    views::tint(&ui.address_icon, if dim { t.text3 } else { t.text2 });
    ui.back.set_enabled(web.map(|w| unsafe { w.web.canGoBack() }).unwrap_or(false));
    ui.forward.set_enabled(web.map(|w| unsafe { w.web.canGoForward() }).unwrap_or(false));
    let loading = web.map(|w| unsafe { w.web.isLoading() }).unwrap_or(false);
    ui.reload.img.setImage(symbol(if loading { "xmark" } else { "arrow.clockwise" }, if loading { 12.0 } else { 13.0 }, Weight::Medium).as_deref());
    ui.reload.hit.setToolTip(Some(&ns(if loading { "Stop (⌘.)" } else { "Reload (⌘R)" })));
    let title = tab.map(tab_title).unwrap_or_else(|| "Volant".into());
    b.window.setTitle(&ns(&title));
}

/// The thin loading line along the top of the page.
pub fn update_progress(b: &Browser) {
    let ui = &b.ui;
    let web = b.tabs.active.and_then(|id| b.webs.get(&id));
    let (loading, p) = web.map(|w| unsafe { (w.web.isLoading(), w.web.estimatedProgress()) }).unwrap_or((false, 0.0));
    let w = ui.content.bounds().size.width;
    if loading && p < 1.0 {
        ui.progress.setFrame(rect(0.0, 0.0, (w * p.max(0.08)).round(), 2.5));
        ui.progress.setHidden(false);
        if unsafe { ui.progress.superview() }.is_none() {
            ui.content.addSubview(&ui.progress);
        } else {
            // Keep it above the web view.
            ui.content.addSubview(&ui.progress);
        }
    } else {
        ui.progress.setHidden(true);
    }
}

/// Refreshes the download line once a second, only while something is downloading.
pub fn watch_downloads(b: &mut Browser) {
    update_downloads(b);
    let busy = b.downloads.iter().any(|d| !d.done && !d.failed);
    let recent = b.downloads.iter().any(|d| super::now().saturating_sub(d.finished_at) < 61);
    if busy || recent {
        super::after(if busy { 1.0 } else { 5.0 }, || super::with(watch_downloads));
    }
}

pub fn update_downloads(b: &Browser) {
    let ui = &b.ui;
    let now = super::now();
    let active: Vec<&super::Download> = b.downloads.iter().filter(|d| !d.done && !d.failed).collect();
    let text = if let Some(d) = active.first() {
        let p = objc2_foundation::NSProgressReporting::progress(&*d.obj);
        let frac = p.fractionCompleted();
        let more = if active.len() > 1 { format!(" +{}", active.len() - 1) } else { String::new() };
        Some(format!("↓  {}  {}%{}", d.name, (frac * 100.0).round() as i64, more))
    } else {
        b.downloads.iter().rev().find(|d| now.saturating_sub(d.finished_at) < 60).map(|d| {
            if d.failed {
                format!("Couldn't download {}", d.name)
            } else {
                format!("✓  {}", d.name)
            }
        })
    };
    match text {
        Some(t) => {
            set_text(&ui.downloads_label, &t);
            ui.downloads.setHidden(false);
        }
        None => ui.downloads.setHidden(true),
    }
    // Downloads take the slot first; otherwise it tells you about a new Volant.
    let offer = b.update.installing || b.update.available.is_some();
    ui.update.setHidden(!offer || !ui.downloads.isHidden());
    set_text(&ui.update_label, if b.update.installing { "Updating Volant…" } else { "Update available" });
}

/// Applies the current theme's colours everywhere.
pub fn repaint(b: &mut Browser) {
    let t = theme::current();
    b.window.setBackgroundColor(Some(&theme::ns(t.side)));
    let ui = &b.ui;
    fill(&ui.root, t.side);
    fill(&ui.content, t.content);
    fill(&ui.progress, t.accent);
    ui.address.set_fill(Some(t.fill));
    for btn in [&ui.back, &ui.forward, &ui.reload, &ui.settings, &ui.toggle, &ui.reader, &ui.show_sidebar] {
        views::tint(&btn.img, t.text2);
        btn.hit.restyle();
    }
    views::tint(&ui.new_tab_icon, t.text2);
    views::recolor(&ui.new_tab_label, t.text2);
    views::recolor(&ui.downloads_label, t.text2);
    views::recolor(&ui.update_label, t.accent);
    fill(&ui.update_dot, t.accent);
    ui.update.restyle();
    ui.new_tab.restyle();
    ui.downloads.restyle();
    for r in &ui.rows {
        views::tint(&r.close_img, t.text2);
        views::tint(&r.pin, t.text3);
        r.hit.restyle();
        r.close.restyle();
        fill_row(b, r);
    }
    for w in b.webs.values() {
        super::web::paint_background(&w.web);
    }
    update_chrome(b);
    super::command::repaint(b);
    super::find::repaint(b);
}
