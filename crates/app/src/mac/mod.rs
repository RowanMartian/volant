//! The macOS shell.
//!
//! All browser state lives in one [`Browser`] value owned by the main thread.
//! AppKit and WebKit call back into us from the run loop. Those callbacks use
//! [`with`], which runs the closure right away, or queues it for the next turn of
//! the run loop if the state is already in use (WebKit sometimes notifies
//! synchronously while we're in the middle of changing a web view).

mod actions;
mod blocking;
mod command;
mod delegate;
mod find;
mod menu;
mod pages;
mod theme;
mod ui;
mod views;
mod web;

use objc2::rc::Retained;
use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy, NSImage, NSWindow};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};
use objc2_web_kit::{WKUserContentController, WKWebViewConfiguration};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use volant_core::history::History;
use volant_core::store::Settings;
use volant_core::tabs::{TabId, Tabs};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const COMMIT: Option<&str> = option_env!("VOLANT_COMMIT");

pub struct Paths {
    #[allow(dead_code)]
    pub data: PathBuf,
    pub settings: PathBuf,
    pub session: PathBuf,
    pub history: PathBuf,
    pub lists: PathBuf,
    pub downloads: PathBuf,
}

impl Paths {
    fn new() -> Self {
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/tmp"));
        let data = std::env::var_os("VOLANT_DATA_DIR").map(PathBuf::from).unwrap_or_else(|| home.join("Library/Application Support/Volant"));
        let _ = std::fs::create_dir_all(data.join("lists"));
        Self {
            settings: data.join("settings.txt"),
            session: data.join("session.txt"),
            history: data.join("history.log"),
            lists: data.join("lists"),
            downloads: home.join("Downloads"),
            data,
        }
    }
}

pub struct Download {
    pub obj: Retained<objc2_web_kit::WKDownload>,
    pub name: String,
    pub path: PathBuf,
    pub done: bool,
    pub failed: bool,
    pub finished_at: u64,
}

pub struct Browser {
    pub mtm: MainThreadMarker,
    pub window: Retained<NSWindow>,
    pub ui: ui::Ui,
    pub tabs: Tabs,
    /// Live web views. Sleeping tabs have none.
    pub webs: HashMap<TabId, web::WebTab>,
    pub history: History,
    pub settings: Settings,
    pub paths: Paths,
    pub config: Retained<WKWebViewConfiguration>,
    pub content: Retained<WKUserContentController>,
    pub delegate: Retained<delegate::Delegate>,
    pub web_delegate: Retained<web::WebDelegate>,
    pub icons: HashMap<String, Retained<NSImage>>,
    pub icon_requests: std::collections::HashSet<String>,
    pub command: command::CommandBar,
    pub find: find::FindBar,
    pub downloads: Vec<Download>,
    pub blocking: blocking::State,
    pub session_dirty: bool,
}

thread_local! {
    static STATE: RefCell<Option<Browser>> = const { RefCell::new(None) };
    static PENDING_URLS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

/// Runs `f` with the browser state now if it's free, otherwise on the next turn of the run loop.
pub fn with(f: impl FnOnce(&mut Browser) + 'static) {
    let f = STATE.with(|s| match s.try_borrow_mut() {
        Ok(mut g) => match g.as_mut() {
            Some(b) => {
                f(b);
                None
            }
            None => None,
        },
        Err(_) => Some(f),
    });
    if let Some(f) = f {
        later(move || with(f));
    }
}

/// Like [`with`], but returns a value and gives up (returning `None`) if the state is busy.
pub fn try_with<R>(f: impl FnOnce(&mut Browser) -> R) -> Option<R> {
    STATE.with(|s| s.try_borrow_mut().ok().and_then(|mut g| g.as_mut().map(f)))
}

struct MainOnly<F>(F);
// SAFETY: only ever constructed on the main thread and run on the main queue.
unsafe impl<F> Send for MainOnly<F> {}

/// Queues `f` to run on the main thread after the current event.
pub fn later(f: impl FnOnce() + 'static) {
    let job = MainOnly(f);
    dispatch2::DispatchQueue::main().exec_async(move || {
        let job = job;
        (job.0)()
    });
}

/// Runs `f` on the main thread from any thread.
pub fn on_main(f: impl FnOnce() + Send + 'static) {
    dispatch2::DispatchQueue::main().exec_async(f);
}

pub fn ns(s: &str) -> Retained<NSString> {
    NSString::from_str(s)
}

pub fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(w.max(0.0), h.max(0.0)))
}

pub fn now() -> u64 {
    volant_core::now()
}

pub fn run() {
    let mtm = MainThreadMarker::new().expect("Volant must start on the main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    let delegate = delegate::Delegate::new(mtm);
    app.setDelegate(Some(objc2::runtime::ProtocolObject::from_ref(&*delegate)));
    // The delegate must outlive the run loop; NSApplication only holds it weakly.
    std::mem::forget(delegate.clone());
    app.run();
}

/// Called once AppKit has finished launching.
pub fn start(delegate: Retained<delegate::Delegate>, mtm: MainThreadMarker) {
    let paths = Paths::new();
    let settings = Settings::load(&paths.settings);
    let history = History::open(&paths.history);
    theme::apply_appearance(settings.appearance, mtm);

    let web_delegate = web::WebDelegate::new(mtm);
    let (config, content) = web::make_config(&web_delegate, mtm);
    let window = ui::make_window(&delegate, mtm);
    let ui = ui::Ui::build(&window, &delegate, mtm);
    let browser = Browser {
        mtm,
        window,
        ui,
        tabs: Tabs::new(),
        webs: HashMap::new(),
        history,
        settings,
        paths,
        config,
        content,
        delegate,
        web_delegate,
        icons: HashMap::new(),
        icon_requests: Default::default(),
        command: command::CommandBar::new(mtm),
        find: find::FindBar::new(mtm),
        downloads: Vec::new(),
        blocking: Default::default(),
        session_dirty: false,
    };
    STATE.with(|s| *s.borrow_mut() = Some(browser));

    with(|b| {
        command::attach(b);
        find::attach(b);
        menu::install(b);
        theme::refresh(b);
        actions::restore(b);
        blocking::init(b);
        ui::layout(b);
        b.window.makeKeyAndOrderFront(None);
        delegate::start_timers(b);
        if std::env::args().any(|a| a == "--smoke-test") {
            delegate::smoke_test(b);
        }
    });
    let pending: Vec<String> = PENDING_URLS.with(|p| std::mem::take(&mut *p.borrow_mut()));
    for url in pending {
        with(move |b| actions::open_external(b, &url));
    }
    #[allow(deprecated)]
    NSApplication::sharedApplication(mtm).activateIgnoringOtherApps(true);
}

/// URLs handed to us before the browser finished starting.
pub fn queue_url(url: String) {
    let ready = STATE.with(|s| s.try_borrow().map(|g| g.is_some()).unwrap_or(true));
    if ready {
        with(move |b| actions::open_external(b, &url));
    } else {
        PENDING_URLS.with(|p| p.borrow_mut().push(url));
    }
}
