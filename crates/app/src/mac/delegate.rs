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
use std::cell::Cell;

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

thread_local! {
    static TICKS: Cell<u64> = const { Cell::new(0) };
}

const TICK_SECS: f64 = 2.0;

pub fn start_timers(b: &Browser) {
    let timer = unsafe { NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(TICK_SECS, &b.delegate, sel!(tick:), None, true) };
    // Let macOS batch our wake-ups with others to save power.
    timer.setTolerance(0.5);
}

fn housekeeping(b: &mut Browser) {
    let n = TICKS.with(|t| {
        t.set(t.get() + 1);
        t.get()
    });
    actions::save_session(b);
    if !b.downloads.is_empty() {
        ui::update_downloads(b);
    }
    if n % 30 == 0 {
        actions::sleep_idle(b);
    }
}

/// `--smoke-test`: drives the real app through its main screens, saves
/// screenshots and a short report to $VOLANT_SMOKE_DIR, then quits. CI runs this.
pub fn smoke_test(b: &Browser) {
    let dir = std::env::var("VOLANT_SMOKE_DIR").unwrap_or_else(|_| "/tmp/volant-smoke".into());
    let _ = std::fs::create_dir_all(&dir);
    let window_id = b.window.windowNumber();
    let started = std::time::Instant::now();
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
        let pid = std::process::id().to_string();
        let out = std::process::Command::new("/bin/ps").args(["-o", "rss=", "-p", &pid]).output().ok();
        let rss = out.and_then(|o| String::from_utf8(o.stdout).ok()).and_then(|s| s.trim().parse::<u64>().ok()).unwrap_or(0);
        let web = std::process::Command::new("/bin/sh")
            .args(["-c", "ps -axo rss=,comm= | grep -i 'com.apple.WebKit' | awk '{s+=$1; n+=1} END {print n\" \"s}'"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .unwrap_or_default();
        let line = format!("{label}: volant RSS {:.1} MB; WebKit helpers (count, total KB): {}\n", rss as f64 / 1024.0, web.trim());
        r.lock().unwrap().push_str(&line);
        eprint!("{line}");
    };
    report.lock().unwrap().push_str(&format!("window shown after {:?}\n", started.elapsed()));
    let (s1, s2, s3, s4, s5) = (shot.clone(), shot.clone(), shot.clone(), shot.clone(), shot);
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
        actions::select_index(b, 4);
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
    step(26.0, Box::new(move |_b| {
        let _ = std::fs::write(format!("{dir2}/report.txt"), rep.lock().unwrap().as_str());
        let app = NSApplication::sharedApplication(MainThreadMarker::new().unwrap());
        app.terminate(None);
    }));
}

