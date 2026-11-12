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

        #[unsafe(method(shieldMenu:))]
        fn shield_menu_action(&self, sender: &NSMenuItem) {
            let tag = sender.tag();
            with(move |b| actions::shield_menu_action(b, tag));
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
    super::blocking::maybe_update(b);
    super::weather::tick(b);
    super::updater::tick(b);
}

/// `--ublock-selftest`: downloads uBlock Origin's default lists, compiles them
/// all the way into WebKit, then checks on a real page that network blocking,
/// generic and site cosmetic filtering, a procedural filter and a scriptlet all
/// work, and that trusting the site turns them off. Writes ublock.txt to
/// $VOLANT_SMOKE_DIR and quits; CI fails unless it says PASS.
pub fn ublock_selftest(b: &mut Browser) {
    let dir = std::env::var("VOLANT_SMOKE_DIR").unwrap_or_else(|_| "/tmp/volant-smoke".into());
    let _ = std::fs::create_dir_all(&dir);
    b.settings.onboarded = true;
    b.settings.ublock = true;
    b.settings.ublock_lists = volant_core::ublock::default_lists();
    let started = std::time::Instant::now();
    // My filters: one of each kind, all aimed at example.com. `set` runs in the
    // page's world and `set-attr` in an isolated one, both from uBlock Origin.
    b.blocking.my_filters = "example.com##+js(set, volantSelfTest, true)\nexample.com##+js(set-attr, body, data-volant-isolated, true)\nexample.com##div:has-text(volant-procedural-check)\nexample.com##p\n".into();
    // A full refresh, as the timer does it: uBlock Origin's latest release
    // first, then the lists. If launch already started a build, let it finish.
    refresh_when_idle(dir, started, 0);
}

fn refresh_when_idle(dir: String, started: std::time::Instant, polls: u32) {
    super::after(1.0, move || {
        with(move |b| {
            if super::blocking::is_busy(b) && polls < 600 {
                refresh_when_idle(dir, started, polls + 1);
                return;
            }
            super::blocking::download(b, true);
            wait_until_compiled(dir, started, 0);
        })
    });
}

fn wait_until_compiled(dir: String, started: std::time::Instant, polls: u32) {
    super::after(2.0, move || {
        with(move |b| {
            let ready = !super::blocking::is_busy(b) && !b.blocking.lists.is_empty();
            if !ready && polls < 300 {
                wait_until_compiled(dir, started, polls + 1);
                return;
            }
            let compiled_in = started.elapsed();
            let ubo = super::ubo::version(b).to_string();
            let mut report = format!(
                "uBlock Origin: {}\nlists ready after {:.1} s ({})\n{} network rules, {} cosmetic filters, {} scriptlets, {} procedural; {} WebKit lists\nerror: {}\n",
                if ubo.is_empty() { "FAIL: no scriptlets from uBlock Origin's latest release" } else { &ubo },
                compiled_in.as_secs_f64(),
                b.blocking.timings,
                b.blocking.counts.network,
                b.blocking.counts.cosmetic,
                b.blocking.counts.scriptlets,
                b.blocking.counts.procedural,
                b.blocking.lists.len(),
                if b.blocking.error.is_empty() { "none" } else { &b.blocking.error },
            );
            if !ready {
                report.push_str("FAIL: lists never finished compiling\n");
                finish_selftest(&dir, &report);
                return;
            }
            actions::open_tab(b, "https://example.com/", true);
            selftest_page(dir, report, false);
        })
    });
}

const SELFTEST_JS: &str = r#"(async () => {
  const r = {};
  r.scriptlet = window.volantSelfTest === true;
  r.isolated = document.body.getAttribute('data-volant-isolated') === 'true';
  const p = document.querySelector('p'); r.site_cosmetic = !!p && getComputedStyle(p).display === 'none';
  const ad = document.createElement('ins'); ad.className = 'adsbygoogle'; ad.setAttribute('data-ad-client', 'ca-pub-0'); ad.textContent = 'ad'; document.body.appendChild(ad);
  // Added after load, so this also checks that procedural filters follow page changes.
  const pd = document.createElement('div'); pd.textContent = 'volant-procedural-check'; document.body.appendChild(pd);
  await new Promise((ok) => setTimeout(ok, 600));
  r.procedural = getComputedStyle(pd).display === 'none';
  r.generic_cosmetic = getComputedStyle(ad).display === 'none';
  r.network = await fetch('https://www.google-analytics.com/analytics.js', { mode: 'no-cors', cache: 'no-store' }).then(() => 'loaded', () => 'blocked');
  // What the page looks like to Volant's scripts, to explain a failure.
  r.diag = { probe: pd.outerHTML.slice(0, 160), ready: document.readyState,
    styles: Array.from(document.querySelectorAll('style')).map((st) => (st.parentNode && st.parentNode.nodeName) + ':' + st.textContent.length + ':' + st.textContent.slice(-60)) };
  window.__volantSelfTest = JSON.stringify(r);
})(); 'started'"#;

fn selftest_page(dir: String, report: String, trusted: bool) {
    super::after(7.0, move || {
        with(move |b| {
            let Some(w) = b.tabs.active.and_then(|id| b.webs.get(&id)) else { return };
            unsafe { w.web.evaluateJavaScript_completionHandler(&super::ns(SELFTEST_JS), None) };
            super::after(3.0, move || {
                with(move |b| {
                    let Some(w) = b.tabs.active.and_then(|id| b.webs.get(&id)) else { return };
                    let dir2 = dir.clone();
                    let report2 = report.clone();
                    let done = block2::RcBlock::new(move |res: *mut objc2::runtime::AnyObject, _e: *mut objc2_foundation::NSError| {
                        let text = unsafe { res.as_ref() }
                            .and_then(|o| o.downcast_ref::<objc2_foundation::NSString>())
                            .map(|s| s.to_string())
                            .unwrap_or_else(|| "no result".into());
                        let (dir, mut report) = (dir2.clone(), report2.clone());
                        let label = if trusted { "trusted site" } else { "blocking on" };
                        report.push_str(&format!("{label}: {text}\n"));
                        if !trusted {
                            let all_on = ["\"scriptlet\":true", "\"isolated\":true", "\"procedural\":true", "\"site_cosmetic\":true", "\"generic_cosmetic\":true", "\"network\":\"blocked\""]
                                .iter()
                                .all(|k| text.contains(k));
                            report.push_str(if all_on { "blocking: PASS\n" } else { "blocking: FAIL\n" });
                            with(move |b| {
                                super::blocking::set_trusted(b, "example.com");
                                if let Some(w) = b.tabs.active.and_then(|id| b.webs.get(&id)) {
                                    unsafe { w.web.reload() };
                                }
                                selftest_page(dir, report, true);
                            });
                        } else {
                            let all_off = ["\"scriptlet\":false", "\"isolated\":false", "\"procedural\":false", "\"site_cosmetic\":false", "\"network\":\"loaded\""].iter().all(|k| text.contains(k));
                            report.push_str(if all_off { "trusted: PASS\n" } else { "trusted: FAIL\n" });
                            finish_selftest(&dir, &report);
                        }
                    });
                    unsafe { w.web.evaluateJavaScript_completionHandler(&super::ns("window.__volantSelfTest || 'not finished'"), Some(&done)) };
                })
            });
        })
    });
}

fn finish_selftest(dir: &str, report: &str) {
    let verdict = if report.contains("FAIL") || !report.contains("trusted: PASS") { "FAIL" } else { "PASS" };
    let text = format!("{report}{verdict}\n");
    eprint!("{text}");
    let _ = std::fs::write(format!("{dir}/ublock.txt"), text);
    if let Some(mtm) = MainThreadMarker::new() {
        NSApplication::sharedApplication(mtm).terminate(None);
    }
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
    let (s9, s10, s11, s12, s13) = (shot.clone(), shot.clone(), shot.clone(), shot.clone(), shot);
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
    // The blocker's own page, in dark (for the website).
    step(39.5, Box::new(|b| {
        actions::toggle_sidebar(b);
        super::theme::apply_appearance(volant_core::store::Appearance::Dark, b.mtm);
        super::theme::refresh(b);
        actions::open_or_switch(b, "volant://blocker");
    }));
    step(42.5, Box::new(move |_b| s10("10-blocker")));
    // Experimental: the Jet aircraft article in Reader, drawn by Volant's own engine.
    step(43.0, Box::new(|b| {
        super::theme::apply_appearance(volant_core::store::Appearance::Light, b.mtm);
        super::theme::refresh(b);
        b.settings.engine_reader = true;
        actions::select_index(b, 0);
        super::engine_view::sync(b);
    }));
    // Pictures arrive over the network, then the article is laid out again.
    step(47.0, Box::new(move |_b| s11("11-engine-reader")));
    // Find on page, searching the engine's own text.
    step(47.5, Box::new(|b| {
        super::find::open(b);
        b.find.field.setStringValue(&super::ns("turbojet"));
        super::find::changed(b);
    }));
    let rep2 = report.clone();
    step(49.0, Box::new(move |b| {
        let status = b.find.status.stringValue().to_string();
        rep2.lock().unwrap().push_str(&format!("engine find \"turbojet\": {status}\n"));
        s12("12-engine-find");
    }));
    // Select a paragraph and copy it, as a triple-click and ⌘C would.
    let rep3 = report.clone();
    step(49.5, Box::new(move |b| {
        super::find::close(b);
        let copied = super::engine_view::select_and_copy(b, "Frank Whittle").unwrap_or_else(|| "(nothing)".into());
        rep3.lock().unwrap().push_str(&format!("engine copy: {copied:?}\n"));
    }));
    step(51.0, Box::new(move |_b| s13("13-engine-select")));
    step(51.5, Box::new(move |_b| {
        let _ = std::fs::write(format!("{dir2}/report.txt"), rep.lock().unwrap().as_str());
        let app = NSApplication::sharedApplication(MainThreadMarker::new().unwrap());
        app.terminate(None);
    }));
}

