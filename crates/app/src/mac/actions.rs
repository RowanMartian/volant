//! What the browser does: open, select, close, navigate. Called from clicks,
//! menus and keyboard shortcuts alike.

use super::command::{self, Mode};
use super::views::{Click, Kind};
use super::{find, ns, ui, web, with, Browser};
use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{sel, MainThreadOnly};
use objc2_app_kit::{NSMenu, NSMenuItem, NSWorkspace};
use objc2_foundation::{NSArray, NSError, NSURL};
use objc2_web_kit::{WKWebView, WKWebViewConfiguration};
use volant_core::store::Session;
use volant_core::tabs::TabId;

pub const NEWTAB: &str = "volant://newtab";

/// A click on one of the sidebar's (or a panel's) clickable views.
pub fn hit(kind: Kind, data: u64, click: Click) {
    with(move |b| match (kind, click) {
        (Kind::Tab, Click::Primary) => select(b, data),
        (Kind::Tab, Click::Middle) | (Kind::CloseTab, _) => close_tab(b, data),
        (Kind::NewTab, _) => new_tab(b),
        (Kind::Address, _) => {
            let url = current_url(b);
            command::open(b, Mode::Current, &url);
        }
        (Kind::Back, _) => back(b),
        (Kind::Forward, _) => forward(b),
        (Kind::Reload, _) => reload_or_stop(b),
        (Kind::Settings, _) => open_or_switch(b, "volant://settings"),
        (Kind::Sidebar, _) => toggle_sidebar(b),
        (Kind::Suggestion, _) => command::choose(b, data as usize, false),
        (Kind::Scrim, _) => command::close(b),
        (Kind::Downloads, _) => reveal_download(b),
        (Kind::Reader, _) => toggle_reader(b),
        (Kind::FindPrev, _) => find::step(b, true),
        (Kind::FindNext, _) => find::step(b, false),
        (Kind::FindClose, _) => find::close(b),
        _ => {}
    });
}

pub fn current_url(b: &Browser) -> String {
    b.tabs.active_tab().map(|t| t.url.clone()).filter(|u| u != NEWTAB).unwrap_or_default()
}

/// Tab context menu (right-click on a tab).
pub fn tab_menu(id: TabId) -> Option<Retained<NSMenu>> {
    super::try_with(|b| {
        let pinned = b.tabs.get(id)?.pinned;
        let menu = NSMenu::new(b.mtm);
        let items: [(&str, u64); 5] = [
            (if pinned { "Unpin Tab" } else { "Pin Tab" }, 0),
            ("Duplicate Tab", 1),
            ("Copy Link", 4),
            ("Close Tab", 2),
            ("Close Other Tabs", 3),
        ];
        for (title, op) in items {
            if op == 2 {
                menu.addItem(&NSMenuItem::separatorItem(b.mtm));
            }
            let item = unsafe { NSMenuItem::initWithTitle_action_keyEquivalent(NSMenuItem::alloc(b.mtm), &ns(title), Some(sel!(tabMenu:)), &ns("")) };
            unsafe { item.setTarget(Some(&b.delegate as &AnyObject)) };
            item.setTag((id * 8 + op) as isize);
            menu.addItem(&item);
        }
        Some(menu)
    })
    .flatten()
}

pub fn tab_menu_action(b: &mut Browser, tag: isize) {
    let (id, op) = ((tag as u64) / 8, (tag as u64) % 8);
    match op {
        0 => toggle_pin(b, id),
        1 => {
            if let Some(url) = b.tabs.get(id).map(|t| t.url.clone()) {
                open_tab(b, &url, true);
            }
        }
        2 => close_tab(b, id),
        3 => {
            let others: Vec<TabId> = b.tabs.list.iter().filter(|t| t.id != id && !t.pinned).map(|t| t.id).collect();
            for o in others {
                close_tab(b, o);
            }
        }
        4 => {
            if let Some(url) = b.tabs.get(id).map(|t| t.url.clone()) {
                copy_text(&url);
            }
        }
        _ => {}
    }
}

pub fn copy_text(s: &str) {
    let pb = objc2_app_kit::NSPasteboard::generalPasteboard();
    pb.clearContents();
    unsafe { pb.setString_forType(&ns(s), objc2_app_kit::NSPasteboardTypeString) };
}

/// Opens Volant's tabs at launch: onboarding the first time, then the saved session.
pub fn restore(b: &mut Browser) {
    if !b.settings.onboarded {
        open_tab(b, "volant://welcome", true);
        return;
    }
    let session = Session::load(&b.paths.session);
    if b.settings.restore_session && !session.tabs.is_empty() {
        let now = super::now();
        let mut ids = Vec::new();
        for t in &session.tabs {
            let id = b.tabs.open(&t.url, &t.title, t.pinned, false, now);
            // Every tab but the one in front stays asleep until you visit it.
            if let Some(tab) = b.tabs.get_mut(id) {
                tab.asleep = true;
            }
            ids.push(id);
        }
        let active = ids.get(session.active).copied().unwrap_or(ids[0]);
        select(b, active);
        ui::sync_tabs(b);
    } else {
        open_tab(b, NEWTAB, true);
    }
}

/// Opens `url` in a new tab, in front or behind the current one.
pub fn open_tab(b: &mut Browser, url: &str, front: bool) -> TabId {
    let id = b.tabs.open(url, "", false, true, super::now());
    if front || b.tabs.active.is_none() {
        select(b, id);
    } else {
        let web = web::create(b, id, None);
        web::load(&web, url);
    }
    ui::sync_tabs(b);
    ui::scroll_to_active(b);
    id
}

/// ⌘T: a fresh tab with the command bar open on it.
pub fn new_tab(b: &mut Browser) {
    let reuse = b.tabs.active_tab().filter(|t| t.url == NEWTAB).map(|t| t.id);
    if reuse.is_none() {
        open_tab(b, NEWTAB, true);
    }
    command::open(b, Mode::Current, "");
}

pub fn open_or_switch(b: &mut Browser, url: &str) {
    if let Some(id) = b.tabs.list.iter().find(|t| t.url == url).map(|t| t.id) {
        select(b, id);
    } else {
        open_tab(b, url, true);
    }
}

/// A link from another app.
pub fn open_external(b: &mut Browser, url: &str) {
    open_tab(b, url, true);
    b.window.makeKeyAndOrderFront(None);
}

/// Brings a tab to the front, waking it if it was asleep.
pub fn select(b: &mut Browser, id: TabId) {
    let Some(tab) = b.tabs.get(id) else { return };
    let url = tab.url.clone();
    if let Some(prev) = b.tabs.active.filter(|&p| p != id) {
        if let Some(w) = b.webs.get(&prev) {
            w.web.removeFromSuperview();
        }
    }
    let web = match b.webs.get(&id) {
        Some(w) => w.web.clone(),
        None => {
            let w = web::create(b, id, None);
            web::load(&w, &url);
            w
        }
    };
    if let Some(t) = b.tabs.get_mut(id) {
        t.asleep = false;
    }
    b.tabs.select(id, super::now());
    web.setFrame(b.ui.content.bounds());
    b.ui.content.addSubview(&web);
    find::page_changed(b);
    if !command::is_open(b) {
        b.window.makeFirstResponder(Some(&web));
    }
    ui::sync_tabs(b);
    ui::update_progress(b);
    ui::scroll_to_active(b);
}

pub fn close_tab(b: &mut Browser, id: TabId) {
    if b.tabs.get(id).is_none() {
        return;
    }
    web::release(b, id);
    let next = b.tabs.close(id);
    match next {
        Some(n) => select(b, n),
        None => {
            open_tab(b, NEWTAB, true);
        }
    }
    ui::sync_tabs(b);
}

pub fn close_current(b: &mut Browser) {
    if command::is_open(b) {
        command::close(b);
        return;
    }
    if let Some(id) = b.tabs.active {
        close_tab(b, id);
    }
}

pub fn reopen_closed(b: &mut Browser) {
    if let Some(id) = b.tabs.reopen(super::now()) {
        if let Some(t) = b.tabs.get_mut(id) {
            t.asleep = true;
        }
        select(b, id);
    }
}

/// WebKit wants a new window (target=_blank, window.open): give it a tab.
pub fn adopt_popup(b: &mut Browser, config: &WKWebViewConfiguration, url: &str) -> Retained<WKWebView> {
    let id = b.tabs.open(url, "", false, true, super::now());
    let web = web::create(b, id, Some(config));
    let b_ptr = id;
    super::later(move || with(move |b| select(b, b_ptr)));
    ui::sync_tabs(b);
    web
}

pub fn navigate(b: &mut Browser, url: &str) {
    let Some(id) = b.tabs.active else {
        open_tab(b, url, true);
        return;
    };
    if let Some(t) = b.tabs.get_mut(id) {
        t.url = url.to_string();
    }
    let Some(web) = b.webs.get(&id).map(|w| w.web.clone()) else {
        // Asleep: waking it loads the new address.
        select(b, id);
        return;
    };
    web::load(&web, url);
    b.window.makeFirstResponder(Some(&web));
    ui::update_row(b, id);
    ui::update_chrome(b);
}

/// Is this tab showing Reader mode?
pub fn in_reader(b: &Browser, id: TabId) -> bool {
    b.webs.get(&id).and_then(|w| unsafe { w.web.URL() }).and_then(|u| u.absoluteString()).map(|s| s.to_string().starts_with("volant://reader")).unwrap_or(false)
}

/// ⇧⌘R: shows the page's article in Reader, or goes back to the page.
///
/// The page's live DOM (after its scripts ran) is parsed by Volant's own HTML
/// parser on a background thread, then shown as a new history entry, so Back
/// returns to the original page.
pub fn toggle_reader(b: &mut Browser) {
    let Some(id) = b.tabs.active else { return };
    let Some(web) = b.webs.get(&id).map(|w| w.web.clone()) else { return };
    if in_reader(b, id) {
        unsafe { web.goBack() };
        return;
    }
    let url = b.tabs.get(id).map(|t| t.url.clone()).unwrap_or_default();
    if !url.starts_with("http") {
        beep();
        return;
    }
    let block = RcBlock::new(move |res: *mut AnyObject, _e: *mut NSError| {
        let Some(html) = unsafe { res.as_ref() }.and_then(|o| o.downcast_ref::<objc2_foundation::NSString>()).map(|s| s.to_string()) else {
            beep();
            return;
        };
        let url = url.clone();
        std::thread::spawn(move || {
            let article = volant_core::reader::extract(&html, &url);
            super::on_main(move || {
                with(move |b| {
                    let Some(a) = article else { return beep() };
                    // Only if the user is still on that page.
                    if b.tabs.active != Some(id) || b.tabs.get(id).map(|t| t.url != url).unwrap_or(true) {
                        return;
                    }
                    let page = super::pages::reader_page(b, &a, &url);
                    if let Some(w) = b.webs.get(&id) {
                        let base = NSURL::URLWithString(&ns("volant://reader"));
                        unsafe { w.web.loadHTMLString_baseURL(&ns(&page), base.as_deref()) };
                    }
                });
            });
        });
    });
    unsafe { web.evaluateJavaScript_completionHandler(&ns("document.documentElement.outerHTML"), Some(&block)) };
}

fn beep() {
    objc2_app_kit::NSBeep();
}

fn active_web(b: &Browser) -> Option<Retained<WKWebView>> {
    b.tabs.active.and_then(|id| b.webs.get(&id)).map(|w| w.web.clone())
}

pub fn back(b: &mut Browser) {
    if let Some(w) = active_web(b) {
        unsafe { w.goBack() };
    }
}

pub fn forward(b: &mut Browser) {
    if let Some(w) = active_web(b) {
        unsafe { w.goForward() };
    }
}

pub fn reload_or_stop(b: &mut Browser) {
    if let Some(w) = active_web(b) {
        unsafe {
            if w.isLoading() {
                w.stopLoading();
            } else {
                w.reload();
            }
        }
    }
}

pub fn reload(b: &mut Browser, hard: bool) {
    if let Some(w) = active_web(b) {
        unsafe {
            if hard {
                w.reloadFromOrigin();
            } else {
                w.reload();
            }
        }
    }
}

pub fn stop(b: &mut Browser) {
    if let Some(w) = active_web(b) {
        unsafe { w.stopLoading() };
    }
}

pub fn zoom(b: &mut Browser, step: i32) {
    if let Some(w) = active_web(b) {
        unsafe {
            let z = if step == 0 { 1.0 } else { (w.pageZoom() + 0.1 * step as f64).clamp(0.5, 3.0) };
            w.setPageZoom((z * 10.0).round() / 10.0);
        }
    }
}

pub fn toggle_sidebar(b: &mut Browser) {
    b.settings.sidebar = !b.settings.sidebar;
    b.settings.save(&b.paths.settings);
    ui::layout(b);
}

pub fn toggle_pin(b: &mut Browser, id: TabId) {
    let pinned = b.tabs.get(id).map(|t| t.pinned).unwrap_or(false);
    b.tabs.set_pinned(id, !pinned);
    ui::sync_tabs(b);
}

pub fn select_index(b: &mut Browser, i: usize) {
    // ⌘9 always means the last tab, like every other browser.
    let id = if i >= 8 { b.tabs.list.last().map(|t| t.id) } else { b.tabs.list.get(i).map(|t| t.id) };
    if let Some(id) = id {
        select(b, id);
    }
}

pub fn cycle(b: &mut Browser, delta: isize) {
    if let Some(id) = b.tabs.neighbor(delta) {
        select(b, id);
    }
}

fn reveal_download(b: &mut Browser) {
    let Some(d) = b.downloads.iter().rev().find(|d| d.done || !d.failed) else { return };
    if d.path.as_os_str().is_empty() {
        return;
    }
    let url = NSURL::fileURLWithPath(&ns(&d.path.to_string_lossy()));
    NSWorkspace::sharedWorkspace().activateFileViewerSelectingURLs(&NSArray::from_retained_slice(&[url]));
}

pub fn save_session(b: &mut Browser) {
    if b.session_dirty {
        b.tabs.to_session().save(&b.paths.session);
        b.session_dirty = false;
    }
}

const PLAYING_JS: &str = "[...document.querySelectorAll('audio,video')].some(m => !m.paused && !m.muted) || !!document.pictureInPictureElement || !!document.fullscreenElement";

/// Puts tabs to sleep that have sat in the background past the limit. Tabs
/// that are playing audio or video are left alone.
pub fn sleep_idle(b: &mut Browser) {
    let mins = b.settings.suspend_minutes as u64;
    for id in b.tabs.sleepy(super::now(), mins * 60) {
        let Some(w) = b.webs.get(&id) else {
            if let Some(t) = b.tabs.get_mut(id) {
                t.asleep = true;
            }
            continue;
        };
        if unsafe { w.web.isLoading() } {
            continue;
        }
        let block = RcBlock::new(move |res: *mut AnyObject, _e: *mut NSError| {
            let playing = unsafe { res.as_ref() }
                .and_then(|o| o.downcast_ref::<objc2_foundation::NSNumber>())
                .map(|n| n.as_bool())
                .unwrap_or(false);
            if !playing {
                with(move |b| {
                    if b.tabs.active == Some(id) {
                        return;
                    }
                    web::release(b, id);
                    if let Some(t) = b.tabs.get_mut(id) {
                        t.asleep = true;
                    }
                    ui::update_row(b, id);
                });
            }
        });
        unsafe { w.web.evaluateJavaScript_completionHandler(&ns(PLAYING_JS), Some(&block)) };
    }
}
