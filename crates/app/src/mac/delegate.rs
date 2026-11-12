//! The application delegate: launch, quit, links from other apps, window
//! events, text fields, menus and the housekeeping timer.

use super::{actions, command, find, menu, ui, with, Browser};
use objc2::rc::Retained;
use objc2::runtime::{NSObject, NSObjectProtocol, Sel};
use objc2::{define_class, msg_send, sel, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationDelegate, NSApplicationTerminateReply, NSControl, NSControlTextEditingDelegate, NSMenuItem, NSTextFieldDelegate,
    NSTextView, NSWindowDelegate,
};
use objc2_foundation::{NSArray, NSNotification, NSTimer, NSURL};

define_class!(
    // SAFETY: NSObject has no subclassing requirements; Delegate does not implement Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    pub struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    unsafe impl NSApplicationDelegate for Delegate {
        #[unsafe(method(applicationDidFinishLaunching:))]
        fn did_finish_launching(&self, _n: &NSNotification) {
            let me: Retained<Delegate> = unsafe { Retained::retain(self as *const Self as *mut Self) }.expect("delegate");
            super::start(me, self.mtm());
        }

        #[unsafe(method(applicationShouldTerminate:))]
        fn should_terminate(&self, _app: &NSApplication) -> NSApplicationTerminateReply {
            super::try_with(|b| {
                b.session_dirty = true;
                actions::save_session(b);
            });
            NSApplicationTerminateReply::TerminateNow
        }

        #[unsafe(method(application:openURLs:))]
        fn open_urls(&self, _app: &NSApplication, urls: &NSArray<NSURL>) {
            for u in urls.iter() {
                if let Some(s) = u.absoluteString() {
                    super::queue_url(s.to_string());
                }
            }
        }

        #[unsafe(method(applicationShouldHandleReopen:hasVisibleWindows:))]
        fn reopen(&self, _app: &NSApplication, visible: bool) -> bool {
            if !visible {
                with(|b| b.window.makeKeyAndOrderFront(None));
            }
            true
        }
    }

    unsafe impl NSWindowDelegate for Delegate {
        #[unsafe(method(windowShouldClose:))]
        fn window_should_close(&self, _sender: &objc2_app_kit::NSWindow) -> bool {
            // The red button hides the window; tabs stay as they are. Click the Dock icon to come back.
            with(|b| {
                command::close(b);
                b.window.orderOut(None);
            });
            false
        }

        #[unsafe(method(windowDidResize:))]
        fn did_resize(&self, _n: &NSNotification) {
            with(ui::layout);
        }

        #[unsafe(method(windowDidEnterFullScreen:))]
        fn did_enter_full_screen(&self, _n: &NSNotification) {
            with(ui::layout);
        }

        #[unsafe(method(windowDidExitFullScreen:))]
        fn did_exit_full_screen(&self, _n: &NSNotification) {
            with(ui::layout);
        }

        // Each of these makes AppKit redo the title bar; put the lights back after it.
        #[unsafe(method(windowDidBecomeKey:))]
        fn did_become_key(&self, _n: &NSNotification) {
            ui::relight_soon();
        }

        #[unsafe(method(windowDidResignKey:))]
        fn did_resign_key(&self, _n: &NSNotification) {
            ui::relight_soon();
        }

        #[unsafe(method(windowDidEndLiveResize:))]
        fn did_end_live_resize(&self, _n: &NSNotification) {
            ui::relight_soon();
        }

        #[unsafe(method(windowDidChangeBackingProperties:))]
        fn did_change_backing(&self, _n: &NSNotification) {
            ui::relight_soon();
        }
    }

    unsafe impl NSControlTextEditingDelegate for Delegate {
        #[unsafe(method(controlTextDidChange:))]
        fn text_did_change(&self, n: &NSNotification) {
            let ptr = n.object().map(|o| Retained::as_ptr(&o) as *const u8 as usize).unwrap_or(0);
            with(move |b| {
                if command::is_field(b, ptr) {
                    command::changed(b);
                } else if find::is_field(b, ptr) {
                    find::changed(b);
                }
            });
        }

        #[unsafe(method(control:textView:doCommandBySelector:))]
        fn do_command(&self, control: &NSControl, _tv: &NSTextView, selector: Sel) -> bool {
            let ptr = control as *const NSControl as usize;
            let name = selector.name().to_string_lossy().into_owned();
            super::try_with(|b| {
                if command::is_field(b, ptr) {
                    command::key(b, &name)
                } else if find::is_field(b, ptr) {
                    find::key(b, &name)
                } else {
                    false
                }
            })
            .unwrap_or(false)
        }
    }

    unsafe impl NSTextFieldDelegate for Delegate {}

    impl Delegate {
        #[unsafe(method(menu:))]
        fn menu_action(&self, sender: &NSMenuItem) {
            let tag = sender.tag();
            with(move |b| menu::run(b, tag));
        }

        #[unsafe(method(tabMenu:))]
        fn tab_menu_action(&self, sender: &NSMenuItem) {
            let tag = sender.tag();
            with(move |b| actions::tab_menu_action(b, tag));
        }

        #[unsafe(method(tick:))]
        fn tick(&self, _t: &NSTimer) {
            with(housekeeping);
        }

        #[unsafe(method(validateMenuItem:))]
        fn validate(&self, _item: &NSMenuItem) -> bool {
            true
        }
    }
);

impl Delegate {
    pub fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        unsafe { msg_send![super(this), init] }
    }
}

/// Everything else is event-driven; this is Volant's only regular wake-up.
const TICK_SECS: f64 = 60.0;

pub fn start_timers(b: &Browser) {
    let timer = unsafe { NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(TICK_SECS, &b.delegate, sel!(tick:), None, true) };
    // Let macOS batch our wake-up with others to save power.
    timer.setTolerance(10.0);
}

fn housekeeping(b: &mut Browser) {
    actions::save_session(b);
    actions::sleep_idle(b);
    super::updater::tick(b);
}

/// `--smoke-test`: drives the real app through its main screens, saves
/// screenshots and a short report to $VOLANT_SMOKE_DIR, then quits. CI runs this.
pub fn smoke_test(b: &Browser) {
    let dir = std::env::var("VOLANT_SMOKE_DIR").unwrap_or_else(|_| "/tmp/volant-smoke".into());
    let _ = std::fs::create_dir_all(&dir);
    let window_id = b.window.windowNumber();
    let report = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let step = move |secs: f64, f: Box<dyn FnOnce(&mut Browser) + Send>| {
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs_f64(secs));
            super::on_main(move || with(f));
        });
    };
    let shot = {
        let dir = dir.clone();
        move |name: &str| {
            let path = format!("{dir}/{name}.png");
            let _ = std::process::Command::new("/usr/sbin/screencapture").args(["-x", "-o", &format!("-l{window_id}"), &path]).status();
        }
    };
    let r = report.clone();
    let mem = move |label: &str| {
        // Memory footprint, the figure Activity Monitor shows (RSS overcounts shared system libraries).
        let pid = std::process::id().to_string();
        let out = std::process::Command::new("/usr/bin/top").args(["-l", "1", "-pid", &pid, "-stats", "mem"]).output().ok();
        let foot = out.and_then(|o| String::from_utf8(o.stdout).ok()).and_then(|s| s.lines().last().map(|l| l.trim().to_string())).unwrap_or_default();
        let line = format!("{label}: Volant app process footprint {foot}\n");
        r.lock().unwrap().push_str(&line);
        eprint!("{line}");
    };
    let since_exec = super::SHOWN.with(|t| t.get()).unwrap_or_default();
    report.lock().unwrap().push_str(&format!("window shown {:.0} ms after the process started\n", since_exec.as_secs_f64() * 1000.0));
    let (s1, s2, s3, s4, s5, s6, s7, s8) = (shot.clone(), shot.clone(), shot.clone(), shot.clone(), shot.clone(), shot.clone(), shot.clone(), shot.clone());
    let s9 = shot;
    let (m1, m2) = (mem.clone(), mem);
    let rep = report.clone();
    let dir2 = dir.clone();
    step(5.0, Box::new(move |_b| {
        m1("idle, welcome page");
        s1("1-welcome");
    }));
    step(6.0, Box::new(|b| {
        b.settings.onboarded = true;
        for u in ["https://example.com/", "https://en.wikipedia.org/wiki/Jet_aircraft", "volant://settings", "volant://history"] {
            actions::open_tab(b, u, false);
        }
        actions::navigate(b, "https://en.wikipedia.org/wiki/Jet_aircraft");
    }));
    step(14.0, Box::new(move |_b| s2("2-browsing")));
    step(15.0, Box::new(|b| command::open(b, command::Mode::Current, "")));
    step(15.5, Box::new(|b| {
        b.command.field.setStringValue(&super::ns("wiki"));
        command::changed(b);
    }));
    step(17.0, Box::new(move |_b| s3("3-command-bar")));
    step(18.0, Box::new(|b| {
        command::close(b);
        actions::open_or_switch(b, "volant://settings");
    }));
    step(21.0, Box::new(move |_b| s4("4-settings")));
    step(22.0, Box::new(|b| {
        super::theme::apply_appearance(volant_core::store::Appearance::Dark, b.mtm);
        super::theme::refresh(b);
        actions::select_index(b, 0);
    }));
    step(25.0, Box::new(move |_b| {
        m2("5 tabs open, 2 loaded");
        s5("5-dark");
    }));
    step(26.0, Box::new(actions::toggle_reader));
    step(29.0, Box::new(move |_b| s6("6-reader")));
    // The home page: favourites, live weather, the edit panel, then the sidebar hidden.
    step(30.0, Box::new(|b| {
        super::theme::apply_appearance(volant_core::store::Appearance::Light, b.mtm);
        super::theme::refresh(b);
        for (u, t) in [("github.com", "GitHub"), ("en.wikipedia.org", "Wikipedia"), ("news.ycombinator.com", "Hacker News"), ("apple.com", "Apple"), ("rust-lang.org", "Rust")] {
            b.favorites.add(u, t);
        }
        b.settings.weather = true;
        b.settings.weather_place = "Toronto, Ontario".into();
        b.settings.weather_at = Some((43.70643, -79.39864));
        actions::open_tab(b, actions::NEWTAB, true);
    }));
    step(35.0, Box::new(move |_b| s7("7-home")));
    step(35.5, Box::new(|b| {
        if let Some(w) = b.tabs.active.and_then(|id| b.webs.get(&id)) {
            unsafe { w.web.evaluateJavaScript_completionHandler(&super::ns("document.getElementById('edit').click()"), None) };
        }
    }));
    step(37.0, Box::new(move |_b| s8("8-home-edit")));
    step(37.5, Box::new(actions::toggle_sidebar));
    step(39.0, Box::new(move |_b| s9("9-no-sidebar")));
    step(40.0, Box::new(move |_b| {
        let _ = std::fs::write(format!("{dir2}/report.txt"), rep.lock().unwrap().as_str());
        let app = NSApplication::sharedApplication(MainThreadMarker::new().unwrap());
        app.terminate(None);
    }));
}

